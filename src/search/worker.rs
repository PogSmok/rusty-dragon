//! One search thread: iterative deepening over a fail-soft PVS (principal variation search).

use std::mem;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use super::memory::Aligned;
use super::ordering::{self, History, MovePicker, PieceTo};
use super::see::{self, SEE_VALUE, see_ge};
use super::time::TimeManager;
use super::tt::{Bound, Hit, TranspositionTable};
use super::{Control, Info, Limits, MAX_DEPTH, Outcome};
use crate::board::{CheckInfo, Move, MoveList, PieceKind, Position};
use crate::eval::{self, Score};

/// Plies a line can reach from the root, extensions and quiescence search included.
pub(super) const MAX_PLY: usize = 128;

const INFINITE: i32 = Score::INFINITE.value();
const MATE_IN_MAX_PLY: i32 = Score::MATE_IN_MAX_PLY.value();
const DRAW: i32 = Score::DRAW.value();
const MAX_EVAL: i32 = Score::MAX_EVAL.value();

/// The static evaluation of a node in check, which has none.
pub(super) const NO_EVAL: i32 = -INFINITE - 1;

/// Nodes between checks of the stop flag, the clock and the node limit.
const POLL_INTERVAL: u64 = 1024;

/// What every thread of one search reads.
///
/// On cache lines of its own, so the main thread's writes to
/// what lies next to it never slow the other threads' reads.
#[repr(align(128))]
pub(super) struct Shared<'a> {
    pub root: &'a Position,
    /// Keys of the game's positions before the root, possibly ending with the root's.
    pub history: &'a [u64],
    /// The legal moves searched at the root, never empty.
    pub root_moves: &'a MoveList,
    /// Whether `root_moves` leaves some legal moves out, as `searchmoves` asks.
    pub restricted: bool,
    pub limits: &'a Limits,
    pub tt: &'a TranspositionTable,
    pub control: &'a Control,
    /// Nodes searched by each thread, published every [`POLL_INTERVAL`].
    pub nodes: &'a [NodeCounter],
    /// When the search began, for reporting: the clock may restart after `ponderhit`.
    pub start: Instant,
}

/// The natural logarithm of `x`, which must be at least 1, at compile time, where `f64::ln`
/// is not available.
const fn const_ln(x: f64) -> f64 {
    // x = mantissa * 2^exponent with the mantissa in [1, 2), and ln(mantissa) = 2 atanh(ratio)
    // for ratio = (mantissa - 1) / (mantissa + 1), below 1/3, so the series converges quickly.
    let (mut mantissa, mut exponent) = (x, 0.0);
    while mantissa >= 2.0 {
        mantissa /= 2.0;
        exponent += 1.0;
    }
    let ratio = (mantissa - 1.0) / (mantissa + 1.0);
    let (mut term, mut sum, mut power) = (ratio, 0.0, 1.0);
    while power < 40.0 {
        sum += term / power;
        term *= ratio * ratio;
        power += 2.0;
    }
    exponent * std::f64::consts::LN_2 + 2.0 * sum
}

/// LMR (late move reductions): base reductions, by depth and move number.
///
/// Bytes, so the table takes 4 KiB of cache rather than 16.
static REDUCTIONS: [[u8; 64]; 64] = {
    let mut table = [[0; 64]; 64];
    let mut depth = 1;
    while depth < 64 {
        let mut moves = 1;
        while moves < 64 {
            #[expect(clippy::cast_precision_loss, reason = "below 64, exact")]
            let (d, m) = (const_ln(depth as f64), const_ln(moves as f64));
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "positive and below 10"
            )]
            let r = (0.75 + d * m / 2.25) as u8;
            table[depth][moves] = r;
            moves += 1;
        }
        depth += 1;
    }
    table
};

#[inline]
fn reduction(depth: i32, move_count: i32) -> i32 {
    let index =
        |n: i32| usize::try_from(n.clamp(0, 63)).expect("clamped value should fit in a `usize`");
    i32::from(REDUCTIONS[index(depth)][index(move_count)])
}

/// A thread's node count, on cache lines of its own, so one thread publishing its count
/// does not slow the others.
pub(super) type NodeCounter = Aligned<AtomicU64>;

/// What a search thread keeps from one search to the next: its history
/// tables, and buffers it would otherwise allocate for every search.
///
/// Aligned to two cache lines, the most a CPU prefetches together, so
/// threads next to each other in a slice never write the same line.
/// The buffers written at every node are aligned the same way, so no
/// other allocation, another thread's included, shares their lines.
#[repr(align(128))]
pub(super) struct ThreadData {
    pub(super) history: History,
    pv: PvTable,
    /// The frames of a line, from the root.
    frames: Box<Aligned<[Frame; MAX_PLY + 3]>>,
    /// Zobrist keys, after [`KEY_PADDING`] unused ones.
    keys: Vec<u64>,
}

impl ThreadData {
    pub(super) fn new() -> Self {
        Self {
            history: History::new(),
            pv: PvTable::new(),
            frames: Box::new(Aligned([Frame::EMPTY; MAX_PLY + 3])),
            keys: Vec::new(),
        }
    }
}

/// Unused keys at the bottom of a thread's key stack, and at least as many free at its top:
///
/// two cache lines' worth, so the keys a search writes, at every node, never share a line with
/// another allocation, another thread's included.
const KEY_PADDING: usize = 16;

/// The first moves tried at a node that did not cut, which a later cutoff penalizes.
///
/// Holding a few dozen bounds the history updates after a cutoff.
/// Later moves gain little from them.
type Tried = MoveList<32>;

fn ply_score(ply: usize) -> i32 {
    i32::try_from(ply).expect("ply should fit in an `i32`, being below `MAX_PLY`")
}

fn mate_in(ply: usize) -> i32 {
    Score::mate_in(ply_score(ply)).value()
}

fn mated_in(ply: usize) -> i32 {
    Score::mated_in(ply_score(ply)).value()
}

/// A mate score counted from the node at `ply` instead of the root.
fn score_to_tt(score: i32, ply: usize) -> i32 {
    if score >= MATE_IN_MAX_PLY {
        score + ply_score(ply)
    } else if score <= -MATE_IN_MAX_PLY {
        score - ply_score(ply)
    } else {
        score
    }
}

/// Undoes [`score_to_tt`] for a node at `ply`.
fn score_from_tt(score: i32, ply: usize) -> i32 {
    if score >= MATE_IN_MAX_PLY {
        score - ply_score(ply)
    } else if score <= -MATE_IN_MAX_PLY {
        score + ply_score(ply)
    } else {
        score
    }
}

/// Whether the side to move has a piece besides pawns and the king.
///
/// Without one, zugzwang is common: passing would often be the best move.
fn has_non_pawn_material(pos: &Position) -> bool {
    let us = pos.side_to_move();
    !pos.pieces_of_color(us)
        .difference(pos.pieces(us, PieceKind::Pawn))
        .difference(pos.pieces(us, PieceKind::King))
        .is_empty()
}

/// Material a capture or promotion gains at once, the captured piece being worth `captured`.
fn immediate_gain(captured: i32, mv: Move) -> i32 {
    captured + see::promotion_gain(mv)
}

/// What a node leaves for the nodes around it.
#[derive(Clone, Copy)]
struct Frame {
    /// Set by the parent, which knows whether its move gave check.
    in_check: bool,
    /// Corrected, or [`NO_EVAL`] in check.
    static_eval: i32,
    /// The move played from here, [`Move::NULL`] for a null move.
    played: Move,
    /// `played` for continuation history; `None` for a null move.
    played_piece_to: Option<PieceTo>,
    /// The move a singular extension search leaves out, or [`Move::NULL`].
    excluded: Move,
    /// TT PV flag: whether the node is or was on the principal variation.
    tt_pv: bool,
    /// Double and triple extensions on the line to here.
    double_extensions: i32,
    /// Cutoffs at this ply under the current grandparent.
    cutoffs: i32,
    /// Set while the node is searched again to verify a null-move cutoff.
    ///
    /// The verification does not reward the move that led here, since the node's own search will.
    verifying: bool,
}

impl Frame {
    const EMPTY: Self = Self {
        in_check: false,
        static_eval: NO_EVAL,
        played: Move::NULL,
        played_piece_to: None,
        excluded: Move::NULL,
        tt_pv: false,
        double_extensions: 0,
        cutoffs: 0,
        verifying: false,
    };
}

/// PV (principal variation) table: the line expected below each ply. Only PV nodes touch it.
struct PvTable {
    /// The line below each ply, on cache lines of its own.
    lines: Vec<Aligned<[Move; MAX_PLY]>>,
    /// Bytes, so the lengths take two cache lines rather than sixteen.
    len: [u8; MAX_PLY],
}

const _: () = assert!(MAX_PLY <= u8::MAX as usize, "a line's length must fit in a `u8`");

impl PvTable {
    fn new() -> Self {
        Self { lines: vec![Aligned([Move::NULL; MAX_PLY]); MAX_PLY], len: [0; MAX_PLY] }
    }

    #[inline]
    fn clear(&mut self, ply: usize) {
        self.len[ply] = 0;
    }

    /// `mv` followed by the line below it.
    #[inline]
    fn update(&mut self, ply: usize, mv: Move) {
        let (head, tail) = self.lines.split_at_mut(ply + 1);
        let child_len = self.len[ply + 1];
        let line = &mut head[ply].0;
        line[0] = mv;
        line[1..=usize::from(child_len)].copy_from_slice(&tail[0].0[..usize::from(child_len)]);
        self.len[ply] = child_len + 1;
    }

    fn line(&self, ply: usize) -> &[Move] {
        &self.lines[ply].0[..usize::from(self.len[ply])]
    }
}

/// The result of the last iteration that finished.
struct Completed {
    depth: i32,
    score: i32,
    /// The principal variation of the iteration.
    pv: Vec<Move>,
}

/// How much longer than planned to think, by how many iterations in a row kept the best move.
const STABILITY_FACTOR: [f64; 6] = [2.2, 1.6, 1.3, 1.1, 0.95, 0.85];

pub(super) struct Worker<'a> {
    shared: &'a Shared<'a>,
    /// 0 for the main thread, which reports, keeps time and decides when the search ends.
    id: usize,
    /// The main thread's clock.
    time: Option<TimeManager>,
    /// Whether the main thread still thinks on the opponent's time.
    pondering: bool,
    nodes: u64,
    /// Seldepth (selective depth): deepest ply reached this iteration.
    seldepth: usize,
    /// Keys of every position from the game's start to the current node.
    keys: &'a mut Vec<u64>,
    /// Index in `keys` of the root.
    root_index: usize,
    /// Index in `keys` of the position after the latest null move, or of the first key.
    ///
    /// No repetition is counted across it.
    null_floor: usize,
    /// NMP (null-move pruning) minimum ply: no null move before it, while one is verified.
    nmp_min_ply: usize,
    frames: &'a mut [Frame; MAX_PLY + 3],
    pv: &'a mut PvTable,
    /// This thread's history tables, kept from one search to the next.
    history: &'a mut History,
    /// Set once the search must end: every score after it is meaningless.
    stopped: bool,
    completed: Option<Completed>,
    /// The root move that failed high in this iteration's aspiration window: better than the
    /// last iteration's best, so searched first and played if the iteration does not finish.
    fail_high: Option<FailHigh>,
    root_depth: i32,
}

/// A root move that failed high: its line, starting with it, and the score it beat.
struct FailHigh {
    pv: Vec<Move>,
    score: i32,
}

impl<'a> Worker<'a> {
    pub(super) fn new(
        shared: &'a Shared<'a>,
        data: &'a mut ThreadData,
        id: usize,
        time: Option<TimeManager>,
    ) -> Self {
        let ThreadData { history, pv, frames, keys } = data;
        keys.clear();
        keys.resize(KEY_PADDING, 0);
        keys.extend_from_slice(shared.history);
        let root_key = shared.root.zobrist_key();
        if shared.history.last() != Some(&root_key) {
            keys.push(root_key);
        }
        // Room for the deepest line, so the stack never grows during the search, and the
        // padding after it.
        keys.reserve(MAX_PLY + KEY_PADDING);
        let frames = &mut frames.0;
        let root_index = keys.len() - 1;
        frames.fill(Frame::EMPTY);
        frames[0].in_check = shared.root.in_check();
        pv.len.fill(0);
        Self {
            shared,
            id,
            time,
            pondering: shared.control.is_pondering(),
            nodes: 0,
            seldepth: 0,
            keys,
            root_index,
            null_floor: KEY_PADDING,
            nmp_min_ply: 0,
            frames,
            pv,
            history,
            stopped: false,
            completed: None,
            fail_high: None,
            root_depth: 0,
        }
    }

    const fn is_main(&self) -> bool {
        self.id == 0
    }

    /// Searches one ply deeper each iteration until a limit or `stop`.
    ///
    /// The main thread reports each finished iteration and stops the others when it ends.
    pub(super) fn iterative_deepening(&mut self, report: &mut dyn FnMut(&Info)) {
        let max_depth = self.shared.limits.depth.map_or(MAX_DEPTH, |d| d.clamp(1, MAX_DEPTH));
        let max_depth = i32::try_from(max_depth).expect("max depth should fit in an `i32`");
        // Odd-numbered spawned threads search a ply ahead of the main thread,
        // so the threads cover more of the tree between them.
        let lead = i32::from(self.id % 2 == 1);
        let mut score = 0;
        let mut stability = 0;
        let mut previous_best = None;

        for depth in (1 + lead).min(max_depth)..=max_depth {
            self.poll();
            if self.stopped {
                break;
            }
            self.root_depth = depth;
            self.seldepth = 0;
            self.fail_high = None;
            let result = self.aspiration(depth, score);
            if self.stopped {
                // A move that beat the last best is the better guess, unfinished or not,
                // and the score it beat says more of it than the last best's score.
                // The depth stays that of the last finished iteration.
                let mut switched = false;
                if let (Some(fail_high), Some(completed)) =
                    (self.fail_high.take(), &mut self.completed)
                {
                    if completed.pv[0] != fail_high.pv[0] {
                        completed.pv = fail_high.pv;
                        completed.score = fail_high.score;
                        switched = true;
                    }
                }
                // The GUI's last line should be the move about to be played.
                if switched && self.is_main() {
                    let depth = u32::try_from(depth).unwrap_or(0);
                    report(&Info { depth, lowerbound: true, ..self.info() });
                }
                break;
            }
            let mut pv = self.pv.line(0).to_vec();
            if pv.is_empty() {
                pv.push(self.shared.root_moves[0]);
            }
            let best = pv.first().copied();
            // Only a finished iteration has a score to drop from.
            let dropped = self.completed.is_some() && result < score - 30;
            score = result;
            stability = if best == previous_best { stability + 1 } else { 0 };
            previous_best = best;
            self.completed = Some(Completed { depth, score, pv });

            if self.is_main() {
                report(&self.info());
                if self.iteration_ends_search(score, stability, dropped) {
                    break;
                }
            }
        }
        if self.is_main() {
            self.shared.control.stop();
        }
    }

    /// Whether the main thread should not start another iteration.
    fn iteration_ends_search(&mut self, score: i32, stability: usize, dropped: bool) -> bool {
        let limits = self.shared.limits;
        if let Some(mate) = limits.mate {
            let moves = Score::from_value(score).mate_moves();
            if moves.is_some_and(|n| n > 0 && n.unsigned_abs() <= mate) {
                return true;
            }
        }
        self.update_ponder();
        if self.pondering {
            return false;
        }
        let factor = STABILITY_FACTOR[stability.min(STABILITY_FACTOR.len() - 1)]
            * if dropped { 1.3 } else { 1.0 };
        self.time.as_ref().is_some_and(|time| time.soft_expired(factor))
    }

    /// Searches the root in a window around the last score, widening it on each fail.
    fn aspiration(&mut self, depth: i32, previous: i32) -> i32 {
        let mut delta = 20;
        let (mut alpha, mut beta) = if depth >= 4 {
            ((previous - delta).max(-INFINITE), (previous + delta).min(INFINITE))
        } else {
            (-INFINITE, INFINITE)
        };
        let root = self.shared.root;
        loop {
            let score = self.negamax::<true>(root, depth, alpha, beta, 0, false);
            if self.stopped {
                return score;
            }
            if score <= alpha {
                beta = (alpha + beta) / 2;
                alpha = (score - delta).max(-INFINITE);
                // An earlier fail high, searched first, has just failed low: it is no better.
                self.fail_high = None;
            } else if score >= beta {
                beta = (score + delta).min(INFINITE);
                let pv = self.pv.line(0);
                if !pv.is_empty() {
                    self.fail_high = Some(FailHigh { pv: pv.to_vec(), score });
                }
            } else {
                return score;
            }
            delta += delta / 2;
        }
    }

    /// The best move and the reply expected, from the last finished iteration.
    pub(super) fn outcome(&self) -> Outcome {
        self.shared.nodes[self.id].0.store(self.nodes, Ordering::Relaxed);
        let completed = self.completed.as_ref();
        let pv = completed.map_or(&[][..], |c| &c.pv);
        let best = pv.first().copied().or_else(|| self.shared.root_moves.first().copied());
        let ponder = pv.get(1).copied().or_else(|| best.and_then(|mv| self.ponder_from_tt(mv)));
        Outcome {
            best,
            ponder,
            score: completed.map(|c| Score::from_value(c.score)),
            depth: completed.map_or(0, |c| u32::try_from(c.depth).unwrap_or(0)),
            nodes: self.total_nodes(),
        }
    }

    /// The reply the transposition table remembers after `best`, if it is legal.
    fn ponder_from_tt(&self, best: Move) -> Option<Move> {
        let mut child = self.shared.root.clone();
        child.make_move(best);
        let mv = self.shared.tt.probe(child.zobrist_key())?.mv?;
        child.is_legal(mv).then_some(mv)
    }

    fn total_nodes(&self) -> u64 {
        self.shared.nodes.iter().map(|n| n.0.load(Ordering::Relaxed)).sum()
    }

    fn info(&self) -> Info {
        let completed = self.completed.as_ref().expect("an iteration should have finished");
        self.shared.nodes[self.id].0.store(self.nodes, Ordering::Relaxed);
        Info {
            depth: u32::try_from(completed.depth).unwrap_or(0),
            seldepth: u32::try_from(self.seldepth).unwrap_or(0),
            score: Score::from_value(completed.score),
            lowerbound: false,
            nodes: self.total_nodes(),
            time: self.shared.start.elapsed(),
            hashfull: self.shared.tt.hashfull(),
            pv: completed.pv.clone(),
        }
    }

    /// Restarts the main thread's clock once the opponent plays the pondered move.
    fn update_ponder(&mut self) {
        if self.pondering && !self.shared.control.is_pondering() {
            self.pondering = false;
            if let Some(time) = &mut self.time {
                // The ponderhit may only now be seen as set.
                time.restart(self.shared.control.ponderhit_at().unwrap_or_else(Instant::now));
            }
        }
    }

    /// Publishes the node count, and sets `stopped` if the search must end.
    ///
    /// The main thread also ends it on its limits, but only once an iteration has given it a move.
    fn poll(&mut self) {
        self.shared.nodes[self.id].0.store(self.nodes, Ordering::Relaxed);
        if self.is_main() {
            if self.completed.is_none() {
                return;
            }
            self.update_ponder();
            let limits = self.shared.limits;
            let out_of_time =
                !self.pondering && self.time.as_ref().is_some_and(TimeManager::hard_expired);
            let out_of_nodes = limits.nodes.is_some_and(|n| self.total_nodes() >= n);
            if out_of_time || out_of_nodes {
                self.shared.control.stop();
            }
        }
        if self.shared.control.is_stopped() {
            self.stopped = true;
        }
    }

    /// Counts a node. Returns whether the search must end.
    #[inline]
    fn visit(&mut self, ply: usize) -> bool {
        self.nodes += 1;
        self.seldepth = self.seldepth.max(ply);
        if self.nodes % POLL_INTERVAL == 0 {
            self.poll();
        }
        self.stopped
    }

    /// A draw score, one point either side of zero depending on the node count, so drawn lines
    /// do not all tie.
    fn draw_score(&self) -> i32 {
        DRAW - 1 + 2 * i32::from(self.nodes & 2 != 0)
    }

    /// Whether the node is drawn by insufficient material, the fifty-move rule or repetition.
    ///
    /// Repeating a position from the search counts: the
    /// side that chose to repeat it can do so again.
    /// One from before the root only counts on its third occurrence,
    /// as the rules say, since the opponent may never repeat it again.
    fn is_draw(&self, pos: &Position, in_check: bool) -> bool {
        if pos.is_insufficient_material() {
            return true;
        }
        // Checkmate on the hundredth ply still wins.
        if pos.halfmove_clock() >= 100 {
            return !in_check || !pos.legal_moves().is_empty();
        }
        let keys = self.keys.as_slice();
        let current = keys.len() - 1;
        let key = keys[current];
        debug_assert_eq!(key, pos.zobrist_key(), "the last key must be the position's");
        let reach = usize::from(pos.halfmove_clock()).min(current - self.null_floor);
        let mut repeated_before_root = false;
        for index in (4..=reach).step_by(2).map(|back| current - back) {
            if keys[index] == key {
                if index > self.root_index || repeated_before_root {
                    return true;
                }
                repeated_before_root = true;
            }
        }
        false
    }

    /// The transposition table entry for `pos`, its score counted from the root.
    fn probe(&self, pos: &Position, ply: usize) -> Option<Hit> {
        let hit = self.shared.tt.probe(pos.zobrist_key())?;
        Some(Hit { score: score_from_tt(hit.score, ply), ..hit })
    }

    /// The static evaluation corrected by the correction history, and the score to prune
    /// with, which is the table's score when its bound says the true score is closer.
    ///
    /// `pos` must have enough material to mate.
    fn evaluate(&self, pos: &Position, hit: Option<Hit>) -> (i32, i32) {
        let raw = eval::evaluate_sufficient(pos).value();
        let corrected = (raw + self.history.correction(pos)).clamp(-MAX_EVAL, MAX_EVAL);
        let eval = match hit {
            Some(hit)
                if hit.score.abs() < MATE_IN_MAX_PLY
                    && hit.bound.cuts(hit.score, corrected, corrected) =>
            {
                hit.score
            }
            _ => corrected,
        };
        (corrected, eval)
    }

    /// Whether the static evaluation beats the one from this side's previous move, or the one
    /// before if that was in check.
    fn improving(&self, ply: usize, static_eval: i32) -> bool {
        if static_eval == NO_EVAL {
            return false;
        }
        for back in [2, 4] {
            if ply >= back && self.frames[ply - back].static_eval != NO_EVAL {
                return static_eval > self.frames[ply - back].static_eval;
            }
        }
        true
    }

    /// The moves played one, two and four plies before the node at `ply`.
    fn contexts(&self, ply: usize) -> [Option<PieceTo>; 3] {
        let at =
            |back: usize| if ply >= back { self.frames[ply - back].played_piece_to } else { None };
        [at(1), at(2), at(4)]
    }

    /// Plays `mv` from the node at `ply`, keeping the key history and the child's frame in step.
    #[inline]
    fn play(&mut self, pos: &Position, mv: Move, ply: usize, gives_check: bool) -> Position {
        self.frames[ply].played_piece_to = Some(PieceTo::of(pos, mv));
        self.frames[ply + 1].double_extensions = self.frames[ply].double_extensions;
        self.play_quiescent(pos, mv, ply, gives_check)
    }

    /// Plays `mv` from the node at `ply` as [`Worker::play`] does, but
    /// sets only the move played and whether the child is in check.
    ///
    /// The rest of the frames is left as it was, so no main search node may follow below.
    #[inline]
    fn play_quiescent(
        &mut self,
        pos: &Position,
        mv: Move,
        ply: usize,
        gives_check: bool,
    ) -> Position {
        self.frames[ply].played = mv;
        self.frames[ply + 1].in_check = gives_check;
        let mut child = pos.clone();
        child.make_move(mv);
        self.keys.push(child.zobrist_key());
        child
    }

    /// Passes the turn from the node at `ply`, keeping the key history and the child's frame in
    /// step as [`Worker::play`] does.
    #[inline]
    fn play_null(&mut self, pos: &Position, ply: usize) -> Position {
        self.frames[ply].played = Move::NULL;
        self.frames[ply].played_piece_to = None;
        self.frames[ply + 1].in_check = false;
        self.frames[ply + 1].double_extensions = self.frames[ply].double_extensions;
        let mut child = pos.clone();
        child.make_null_move();
        self.keys.push(child.zobrist_key());
        child
    }

    #[inline]
    fn unplay(&mut self) {
        self.keys.pop();
    }

    /// Alpha-beta search of `pos`, `depth` plies deep, in the window `alpha..beta`.
    ///
    /// `PV` (principal variation) nodes may score inside the window; others only test against it.
    /// A cut node is a non-PV node expected to fail high.
    #[expect(clippy::too_many_lines, reason = "one search routine reads best in one piece")]
    fn negamax<const PV: bool>(
        &mut self,
        pos: &Position,
        mut depth: i32,
        mut alpha: i32,
        mut beta: i32,
        ply: usize,
        cut_node: bool,
    ) -> i32 {
        let root = ply == 0;
        if PV {
            self.pv.clear(ply);
        }
        if depth <= 0 {
            return self.qsearch::<PV>(pos, alpha, beta, ply);
        }
        if self.visit(ply) {
            return 0;
        }

        let in_check = self.frames[ply].in_check;
        debug_assert_eq!(in_check, pos.in_check(), "the parent's `gives_check` must be correct");
        let excluded = self.frames[ply].excluded;
        let singular_search = excluded != Move::NULL;
        if root {
            // Nothing above the root resets these, as each node does for its grandchildren.
            self.frames[0].double_extensions = 0;
            self.frames[1].cutoffs = 0;
        } else {
            if self.is_draw(pos, in_check) {
                return self.draw_score();
            }
            if ply >= MAX_PLY - 1 {
                return if in_check { DRAW } else { self.evaluate(pos, None).0 };
            }
            // Mate distance pruning.
            alpha = alpha.max(mated_in(ply));
            beta = beta.min(mate_in(ply + 1));
            if alpha >= beta {
                return alpha;
            }
        }

        // A singular search re-searches this node without its best move, so it leaves the
        // table alone.
        let us = pos.side_to_move();
        let hit = if singular_search { None } else { self.probe(pos, ply) };
        let tt_move = if root {
            // A move that just failed high, else the last iteration's best,
            // which a spawned thread may have overwritten in the table.
            // With searchmoves, the table may suggest a move outside them.
            self.fail_high
                .as_ref()
                .map(|fail_high| fail_high.pv[0])
                .or_else(|| self.completed.as_ref().map(|c| c.pv[0]))
                .or(hit.and_then(|h| h.mv))
                .filter(|mv| self.shared.root_moves.contains(mv))
        } else if singular_search {
            Some(excluded)
        } else {
            hit.and_then(|h| h.mv)
        };
        let tt_capture = tt_move.is_some_and(Move::is_tactical);
        if !singular_search {
            self.frames[ply].tt_pv = PV || hit.is_some_and(|h| h.pv);
        }
        let tt_pv = self.frames[ply].tt_pv;
        // Near the fifty-move limit, a stored score may miss a coming draw.
        if !PV && pos.halfmove_clock() < 96 {
            if let Some(hit) = hit {
                if hit.depth >= depth && hit.bound.cuts(hit.score, alpha, beta) {
                    // On a key collision the move is another position's.
                    // Any move indexes the history safely, so two bitboard
                    // tests that rule out nearly every foreign move are enough,
                    // rather than a full legality check on so common a path.
                    let plausible = |mv: Move| {
                        pos.pieces_of_color(us).contains(mv.from())
                            && !pos.occupied().contains(mv.to())
                    };
                    let rewarded = tt_move
                        .filter(|&mv| hit.score >= beta && !mv.is_tactical() && plausible(mv));
                    if let Some(mv) = rewarded {
                        self.history.update_main(us, mv, ordering::history_bonus(depth));
                    }
                    return hit.score;
                }
            }
        }

        let (static_eval, eval) = if in_check {
            (NO_EVAL, NO_EVAL)
        } else if singular_search {
            let static_eval = self.frames[ply].static_eval;
            (static_eval, static_eval)
        } else if root && pos.is_insufficient_material() {
            // Every other node has ruled this out in `is_draw`. A dead draw has nothing to correct.
            (DRAW, DRAW)
        } else {
            self.evaluate(pos, hit)
        };
        self.frames[ply].static_eval = static_eval;
        self.frames[ply + 2].cutoffs = 0;
        let improving = self.improving(ply, static_eval);
        let non_pawn_material = has_non_pawn_material(pos);

        if !PV && !in_check && !singular_search {
            // Razoring: far below alpha, trust a quiescence search that fails low.
            if eval < alpha - 300 - 250 * depth * depth {
                // This node's work is done: counted, checked for draws, probed, evaluated.
                let evals = Some((static_eval, eval));
                let score =
                    self.qsearch_from::<false, false>(pos, alpha - 1, alpha, ply, hit, evals);
                if self.stopped {
                    return 0;
                }
                // The quiescence result is stored only when it cuts.
                // Otherwise this node goes on to search and stores its own,
                // deeper result, which a depth-0 entry must not replace.
                // Nor does it replace a deeper entry already there.
                if score < alpha && score.abs() < MATE_IN_MAX_PLY {
                    if hit.is_none_or(|hit| hit.depth <= 0) {
                        let pv = hit.is_some_and(|hit| hit.pv);
                        let score_tt = score_to_tt(score, ply);
                        let entry =
                            Hit { mv: None, score: score_tt, depth: 0, bound: Bound::Upper, pv };
                        self.shared.tt.store(pos.zobrist_key(), entry);
                    }
                    return score;
                }
            }

            // Reverse futility pruning.
            let margin =
                (80 - 20 * i32::from(cut_node && hit.is_none())) * (depth - i32::from(improving));
            if !tt_pv
                && depth < 10
                && eval >= beta
                && eval < MATE_IN_MAX_PLY
                && tt_move.is_none_or(Move::is_tactical)
                && eval - margin >= beta
            {
                return beta + (eval - beta) / 3;
            }

            // Null-move pruning, verified by a real search at high depth.
            if cut_node
                && eval >= beta
                && static_eval >= beta - 20 * depth + 200
                && self.frames[ply - 1].played != Move::NULL
                && ply >= self.nmp_min_ply
                && beta > -MATE_IN_MAX_PLY
                && non_pawn_material
            {
                let r = 4 + depth / 3 + ((eval - beta) / 200).min(3);
                self.shared.tt.prefetch(pos.key_after_null_move());
                let child = self.play_null(pos, ply);
                let floor = mem::replace(&mut self.null_floor, self.keys.len() - 1);
                let score =
                    -self.negamax::<false>(&child, depth - r, -beta, -beta + 1, ply + 1, false);
                self.null_floor = floor;
                self.unplay();
                if self.stopped {
                    return 0;
                }
                // A mate found after passing proves nothing.
                if score >= beta && score < MATE_IN_MAX_PLY {
                    if self.nmp_min_ply > 0 || depth < 14 {
                        return score;
                    }
                    self.nmp_min_ply = ply + usize::try_from(3 * (depth - r) / 4).unwrap_or(0);
                    let window = (depth - r, beta - 1, beta);
                    let verified = self.search_again(pos, window, ply, false, |frame| {
                        frame.verifying = true;
                    });
                    self.nmp_min_ply = 0;
                    if self.stopped {
                        return 0;
                    }
                    if verified >= beta {
                        return score;
                    }
                }
            }
        }

        // Internal iterative reduction.
        if tt_move.is_none() && ((PV && depth >= 4) || (cut_node && depth >= 8)) {
            depth -= 2;
        }

        // ProbCut: a capture beating beta by a margin in a much
        // shallower search likely beats it in the full one.
        // No capture gains more than a queen, so a higher threshold rules every one out.
        let contexts = self.contexts(ply);
        // One picker for the node, lent to ProbCut first if it runs.
        let mut picker = if root {
            MovePicker::list(pos, self.shared.root_moves, tt_move, contexts)
        } else {
            MovePicker::new(pos, tt_move, contexts)
        };
        // Shared by ProbCut and the move loop: a node ProbCut does not cut searches its moves.
        let check_info = pos.check_info();
        let probcut_beta = beta + 200 - 60 * i32::from(improving);
        if !PV
            && !in_check
            && !singular_search
            && depth >= 5
            && beta.abs() < MATE_IN_MAX_PLY
            && probcut_beta - static_eval <= SEE_VALUE[PieceKind::Queen.index()]
            && !hit.is_some_and(|h| h.depth >= depth - 3 && h.score < probcut_beta)
        {
            // Lent to ProbCut, and given back with the captures generated.
            picker.begin_probcut(tt_move.filter(|&mv| mv.is_tactical()));
            let cut = self.probcut(
                pos,
                ply,
                (depth, beta, probcut_beta),
                cut_node,
                &check_info,
                &mut picker,
            );
            if let Some(score) = cut {
                return score;
            }
            picker.end_probcut();
        }

        let prune = !root && non_pawn_material;
        let late_move_limit = (3 + depth * depth) / (2 - i32::from(improving));
        let mut best_score = -INFINITE;
        let mut best_move = None;
        let mut move_count = 0;
        let mut skip_quiets = false;
        let mut quiets_tried = Tried::empty();
        let mut tacticals_tried = Tried::empty();

        while let Some(picked) = picker.next(self.history) {
            let mv = picked.mv;
            if mv == excluded {
                continue;
            }
            let quiet = !mv.is_tactical();
            if quiet && skip_quiets {
                continue;
            }
            let gives_check = pos.gives_check_with(mv, &check_info);
            move_count += 1;
            let mut new_depth = depth - 1;
            let base_reduction = reduction(depth, move_count) + i32::from(!improving);
            // The picker scored every move but the table's by its history.
            let stat = picked.history.unwrap_or_else(|| {
                if quiet {
                    self.history.quiet(pos, mv, &contexts)
                } else {
                    self.history.tactical(pos, mv)
                }
            });

            if prune && best_score > -MATE_IN_MAX_PLY {
                // Late move pruning.
                if move_count >= late_move_limit && !skip_quiets {
                    skip_quiets = true;
                    picker.skip_quiets();
                }
                // The depth the move would likely be searched to, after its reduction.
                let lmr_depth = new_depth - base_reduction;
                if !quiet || gives_check {
                    if !gives_check && !in_check && lmr_depth < 7 {
                        // `stat` is the tactical history: the weighted victim plus the capture
                        // history. Only the capture history counts here.
                        let victim = see::victim_value(pos, mv);
                        let capture_history = stat - ordering::VICTIM_WEIGHT * victim;
                        let futility = static_eval
                            + 250
                            + 200 * lmr_depth
                            + immediate_gain(victim, mv)
                            + capture_history / 8;
                        if futility <= alpha {
                            continue;
                        }
                    }
                    if !picked.safe_exchange && !see_ge(pos, mv, -180 * depth) {
                        continue;
                    }
                } else {
                    if lmr_depth < 6 && stat < -3500 * depth {
                        continue;
                    }
                    let lmr_depth = lmr_depth + stat / 7000;
                    if !in_check && lmr_depth < 10 && static_eval + 100 + 110 * lmr_depth <= alpha {
                        continue;
                    }
                    let lmr_depth = lmr_depth.max(0);
                    if !see_ge(pos, mv, -25 * lmr_depth * lmr_depth) {
                        continue;
                    }
                }
            }

            // Singular extensions: extend the table's move when every other move falls well
            // short of its score, and cut when another move beats beta too (multi-cut).
            let mut extension = 0;
            if let Some(hit) = hit {
                if !root
                    && Some(mv) == tt_move
                    && depth >= 6 + i32::from(tt_pv)
                    && hit.depth >= depth - 3
                    && hit.bound != Bound::Upper
                    && hit.score.abs() < MATE_IN_MAX_PLY
                    && ply_score(ply) < 2 * self.root_depth
                {
                    let singular_beta = hit.score - (1 + i32::from(tt_pv && !PV)) * depth;
                    let window = (new_depth / 2, singular_beta - 1, singular_beta);
                    let score = self.search_again(pos, window, ply, cut_node, |frame| {
                        frame.excluded = mv;
                    });
                    if self.stopped {
                        return 0;
                    }
                    if score < singular_beta {
                        extension = 1;
                        if !PV
                            && score < singular_beta - 20
                            && self.frames[ply].double_extensions <= 8
                        {
                            extension = 2 + i32::from(!tt_capture && score < singular_beta - 100);
                        }
                    } else if score >= beta && score.abs() < MATE_IN_MAX_PLY {
                        return score;
                    } else if hit.score >= beta || cut_node {
                        extension = -2;
                    }
                }
            }
            new_depth += extension;

            // Prefetched after the singular search, whose nodes would evict it, so the child's
            // table entry loads while the move is made.
            self.shared.tt.prefetch(pos.key_after(mv));
            let child = self.play(pos, mv, ply, gives_check);
            self.frames[ply + 1].double_extensions += i32::from(extension >= 2);
            // A child searched only with a null window leaves no line, so the line is the move
            // alone.
            if PV {
                self.pv.clear(ply + 1);
            }

            // Late move reductions, with a re-search at full depth, or a ply deeper or
            // shallower, when the reduced search beats alpha.
            let mut score = -INFINITE;
            if depth >= 2 && move_count > 1 + i32::from(root) {
                let r = base_reduction - i32::from(tt_pv) - i32::from(gives_check)
                    + 2 * i32::from(cut_node)
                    + i32::from(tt_capture)
                    + i32::from(self.frames[ply + 1].cutoffs > 3)
                    - (stat - 4000) / 10_000;
                let reduced = (new_depth - r).min(new_depth + 1).max(1);
                score = -self.negamax::<false>(&child, reduced, -alpha - 1, -alpha, ply + 1, true);
                if score > alpha && reduced < new_depth {
                    new_depth += i32::from(score > best_score + 40 + 2 * new_depth)
                        - i32::from(score < best_score + 10);
                    if new_depth > reduced {
                        score = -self.negamax::<false>(
                            &child,
                            new_depth,
                            -alpha - 1,
                            -alpha,
                            ply + 1,
                            !cut_node,
                        );
                    }
                    // Learn whether the move held up at full depth, unless the search was
                    // stopped and the score means nothing.
                    if quiet && !self.stopped && (score >= beta || score <= alpha) {
                        let bonus = if score >= beta {
                            ordering::history_bonus(new_depth)
                        } else {
                            -ordering::history_malus(new_depth)
                        };
                        self.update_continuations(pos, mv, &contexts, bonus);
                    }
                }
            } else if !PV || move_count > 1 {
                score = -self.negamax::<false>(
                    &child,
                    new_depth,
                    -alpha - 1,
                    -alpha,
                    ply + 1,
                    !cut_node,
                );
            }
            // A move that beat beta with a null window cuts as it is, except at the root, whose
            // line and exact score are wanted.
            if PV && (move_count == 1 || (score > alpha && (root || score < beta))) {
                score = -self.negamax::<true>(&child, new_depth, -beta, -alpha, ply + 1, false);
            }
            self.unplay();
            if self.stopped {
                return 0;
            }

            if score > best_score {
                best_score = score;
                if score > alpha {
                    best_move = Some(mv);
                    if PV {
                        self.pv.update(ply, mv);
                    }
                    if score >= beta {
                        self.frames[ply].cutoffs += 1;
                        break;
                    }
                    alpha = score;
                }
            }
            if Some(mv) != best_move {
                // Past the first few dozen, the rest go unpenalized.
                let tried = if quiet { &mut quiets_tried } else { &mut tacticals_tried };
                _ = tried.try_push(mv);
            }
        }

        // No move at all: mate or stalemate.
        // Or, in a singular search, the excluded move was the only one.
        if move_count == 0 {
            return if singular_search {
                alpha
            } else if in_check {
                mated_in(ply)
            } else {
                DRAW
            };
        }

        if let Some(mv) = best_move.filter(|_| best_score >= beta) {
            let depth = depth + i32::from(best_score > beta + 100);
            let tried = (quiets_tried.as_slice(), tacticals_tried.as_slice());
            self.reward_cutoff(pos, depth, mv, &contexts, tried);
        } else if best_move.is_none() && ply > 0 && !singular_search && !self.frames[ply].verifying
        {
            // Every move failed low: the opponent's quiet move that led here was a good one.
            // A re-search of the node, which usually fails low by design, says nothing new.
            let previous = self.frames[ply - 1];
            if previous.played != Move::NULL && !previous.played.is_tactical() {
                let bonus = ordering::history_bonus(depth);
                self.history.update_main(us.opposite(), previous.played, bonus);
                let earlier = ply.checked_sub(2).and_then(|p| self.frames[p].played_piece_to);
                if let (Some(now), Some(earlier)) = (previous.played_piece_to, earlier) {
                    self.history.update_continuation(earlier, now, bonus);
                }
            }
        }

        // Pull a deep fail high toward beta: a big margin is less reliable.
        if best_score >= beta && best_score.abs() < MATE_IN_MAX_PLY && alpha.abs() < MATE_IN_MAX_PLY
        {
            best_score = (best_score * depth + beta) / (depth + 1);
        }

        let bound = if best_score >= beta {
            Bound::Lower
        } else if PV && best_move.is_some() {
            Bound::Exact
        } else {
            Bound::Upper
        };

        // A root limited to some moves has a score the position itself does not have,
        // so it is kept out of the table and the correction history.
        let restricted_root = root && self.shared.restricted;

        // Learn the evaluation's error, unless a capture decided the node or the bound leaves
        // the true score on the evaluation's side.
        let bound_shows_error = match bound {
            Bound::Exact => true,
            Bound::Lower => best_score > static_eval,
            Bound::Upper => best_score < static_eval,
        };
        if !in_check
            && !singular_search
            && !restricted_root
            && best_score.abs() < MATE_IN_MAX_PLY
            && best_move.is_none_or(|mv| !mv.is_tactical())
            && bound_shows_error
        {
            self.history.update_correction(pos, depth, best_score - static_eval);
        }

        if !singular_search && !restricted_root {
            let score = score_to_tt(best_score, ply);
            let entry = Hit { mv: best_move, score, depth, bound, pv: tt_pv };
            self.shared.tt.store(pos.zobrist_key(), entry);
        }
        best_score
    }

    /// Searches the node at `ply` again with
    /// `(depth, alpha, beta)`, after `setup` has changed its frame.
    ///
    /// What the search leaves in the frames would mislead the node's
    /// own moves and the parent's cutoff count, so it restores them.
    /// Kept out of `negamax`, and not inlined, so it takes no room in every search frame.
    #[inline(never)]
    fn search_again(
        &mut self,
        pos: &Position,
        (depth, alpha, beta): (i32, i32, i32),
        ply: usize,
        cut_node: bool,
        setup: impl FnOnce(&mut Frame),
    ) -> i32 {
        let frame = self.frames[ply];
        setup(&mut self.frames[ply]);
        let score = self.negamax::<false>(pos, depth, alpha, beta, ply, cut_node);
        self.frames[ply] = frame;
        self.frames[ply + 2].cutoffs = 0;
        score
    }

    /// ProbCut: a capture beating beta by a margin in a much shallower search likely beats it
    /// in the full one. Returns the node's score, if a capture beats the margin.
    ///
    /// It searches the captures of `picker`, which must have begun ProbCut with
    /// [`MovePicker::begin_probcut`]. `check_info` is the node's [`Position::check_info`].
    /// Kept out of `negamax`, and not inlined, so it takes no room in every search frame.
    #[inline(never)]
    fn probcut(
        &mut self,
        pos: &Position,
        ply: usize,
        (depth, beta, probcut_beta): (i32, i32, i32),
        cut_node: bool,
        check_info: &CheckInfo,
        picker: &mut MovePicker<'_>,
    ) -> Option<i32> {
        let threshold = probcut_beta - self.frames[ply].static_eval;
        let tt_pv = self.frames[ply].tt_pv;
        while let Some(picked) = picker.next(self.history) {
            let mv = picked.mv;
            if mv.promotion().is_some_and(|kind| kind != PieceKind::Queen)
                || !see_ge(pos, mv, threshold)
            {
                continue;
            }
            self.shared.tt.prefetch(pos.key_after(mv));
            let child = self.play(pos, mv, ply, pos.gives_check_with(mv, check_info));
            let mut score =
                -self.qsearch::<false>(&child, -probcut_beta, -probcut_beta + 1, ply + 1);
            if score >= probcut_beta {
                score = -self.negamax::<false>(
                    &child,
                    depth - 4,
                    -probcut_beta,
                    -probcut_beta + 1,
                    ply + 1,
                    !cut_node,
                );
            }
            self.unplay();
            if self.stopped {
                return Some(0);
            }
            if score >= probcut_beta {
                let entry = Hit {
                    mv: Some(mv),
                    score: score_to_tt(score, ply),
                    depth: depth - 3,
                    bound: Bound::Lower,
                    pv: tt_pv,
                };
                self.shared.tt.store(pos.zobrist_key(), entry);
                if score.abs() < MATE_IN_MAX_PLY {
                    return Some(score - (probcut_beta - beta));
                }
            }
        }
        None
    }

    /// Adds `bonus` to quiet `mv`'s continuation history after each move in `contexts`.
    fn update_continuations(
        &mut self,
        pos: &Position,
        mv: Move,
        contexts: &[Option<PieceTo>],
        bonus: i32,
    ) {
        let now = PieceTo::of(pos, mv);
        for &earlier in contexts.iter().flatten() {
            self.history.update_continuation(earlier, now, bonus);
        }
    }

    /// Rewards `mv` for a cutoff `depth` plies deep, and penalizes the moves of its kind tried
    /// before it.
    fn reward_cutoff(
        &mut self,
        pos: &Position,
        depth: i32,
        mv: Move,
        contexts: &[Option<PieceTo>],
        (quiets_tried, tacticals_tried): (&[Move], &[Move]),
    ) {
        let us = pos.side_to_move();
        let bonus = ordering::history_bonus(depth);
        let malus = ordering::history_malus(depth);
        if mv.is_tactical() {
            self.history.update_capture(pos, mv, bonus);
        } else {
            self.history.update_main(us, mv, bonus);
            self.update_continuations(pos, mv, contexts, bonus);
            for &tried in quiets_tried {
                self.history.update_main(us, tried, -malus);
                self.update_continuations(pos, tried, contexts, -malus);
            }
        }
        for &tried in tacticals_tried {
            self.history.update_capture(pos, tried, -malus);
        }
    }

    /// Qsearch (quiescence search): searches captures and promotions until the position is
    /// quiet, or every evasion in check, so the evaluation never judges a position in the middle
    /// of an exchange.
    fn qsearch<const PV: bool>(
        &mut self,
        pos: &Position,
        alpha: i32,
        beta: i32,
        ply: usize,
    ) -> i32 {
        if PV {
            self.pv.clear(ply);
        }
        if self.visit(ply) {
            return 0;
        }
        let in_check = self.frames[ply].in_check;
        debug_assert_eq!(in_check, pos.in_check(), "the parent's `gives_check` must be correct");
        if self.is_draw(pos, in_check) {
            return self.draw_score();
        }
        if ply >= MAX_PLY - 1 {
            return if in_check { DRAW } else { self.evaluate(pos, None).0 };
        }

        let hit = self.probe(pos, ply);
        self.qsearch_from::<PV, true>(pos, alpha, beta, ply, hit, None)
    }

    /// [`Worker::qsearch`] for a node at `ply` already counted, checked for draws and probed.
    ///
    /// `evals` are its static evaluation and the score to prune with, if known.
    /// With `STORE`, the result is stored in the table. Without it, nothing is stored.
    fn qsearch_from<const PV: bool, const STORE: bool>(
        &mut self,
        pos: &Position,
        mut alpha: i32,
        beta: i32,
        ply: usize,
        hit: Option<Hit>,
        evals: Option<(i32, i32)>,
    ) -> i32 {
        let in_check = self.frames[ply].in_check;
        // Near the fifty-move limit, a stored score may miss a coming draw.
        if !PV && pos.halfmove_clock() < 96 {
            if let Some(hit) = hit {
                if hit.bound.cuts(hit.score, alpha, beta) {
                    return hit.score;
                }
            }
        }

        let (static_eval, mut best_score) = if in_check {
            (NO_EVAL, -INFINITE)
        } else {
            // Standing pat: the side to move need not capture.
            let (static_eval, eval) = evals.unwrap_or_else(|| self.evaluate(pos, hit));
            if eval >= beta {
                return if eval.abs() < MATE_IN_MAX_PLY { (eval + beta) / 2 } else { eval };
            }
            alpha = alpha.max(eval);
            (static_eval, eval)
        };

        let mut picker = MovePicker::quiescence(pos, hit.and_then(|h| h.mv), in_check);
        // Built at the first move weighed: many quiescence nodes have none.
        let mut check_info = None;
        let futility_base = static_eval + 200;
        // Recaptures are never futile: they restore the material balance the evaluation saw.
        let recapture_square = ply
            .checked_sub(1)
            .map(|p| self.frames[p].played)
            .filter(|&mv| mv != Move::NULL)
            .map(Move::to);
        let mut best_move = None;
        let mut move_count = 0;

        while let Some(picked) = picker.next(self.history) {
            let mv = picked.mv;
            move_count += 1;
            if !in_check && mv.promotion().is_some_and(|kind| kind != PieceKind::Queen) {
                continue;
            }
            // Past a couple of evasions, quiet ones rarely matter.
            if in_check && best_score > -MATE_IN_MAX_PLY && move_count > 2 && !mv.is_tactical() {
                continue;
            }
            let check_info = check_info.get_or_insert_with(|| pos.check_info());
            let gives_check = pos.gives_check_with(mv, check_info);
            if !in_check && best_score > -MATE_IN_MAX_PLY {
                // The threshold the exchange is known to reach, once checked.
                let mut reached = None;
                if !gives_check && mv.promotion().is_none() && Some(mv.to()) != recapture_square {
                    let futility = futility_base + immediate_gain(see::victim_value(pos, mv), mv);
                    if futility <= alpha {
                        best_score = best_score.max(futility);
                        continue;
                    }
                    let threshold = alpha - futility_base;
                    if !see_ge(pos, mv, threshold) {
                        best_score = best_score.max(alpha.min(futility_base));
                        continue;
                    }
                    reached = Some(threshold);
                }
                if reached.is_none_or(|threshold| threshold < -80) && !see_ge(pos, mv, -80) {
                    continue;
                }
            }

            // The move will be searched, so start loading its child's table entry.
            self.shared.tt.prefetch(pos.key_after(mv));
            let child = self.play_quiescent(pos, mv, ply, gives_check);
            let score = -self.qsearch::<PV>(&child, -beta, -alpha, ply + 1);
            self.unplay();
            if self.stopped {
                return 0;
            }

            if score > best_score {
                best_score = score;
                if score > alpha {
                    best_move = Some(mv);
                    if PV {
                        self.pv.update(ply, mv);
                    }
                    if score >= beta {
                        break;
                    }
                    alpha = score;
                }
            }
        }

        if in_check && move_count == 0 {
            return mated_in(ply);
        }
        if STORE {
            let bound = if best_score >= beta { Bound::Lower } else { Bound::Upper };
            let score = score_to_tt(best_score, ply);
            // A PV flag from an earlier visit stays: this one may be off the PV only for now.
            let pv = PV || hit.is_some_and(|hit| hit.pv);
            let entry = Hit { mv: best_move, score, depth: 0, bound, pv };
            self.shared.tt.store(pos.zobrist_key(), entry);
        }
        best_score
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mate_scores_round_trip_through_the_table() {
        for score in [mate_in(5), mated_in(8), 150, -3, DRAW] {
            for ply in [0, 1, 17, MAX_PLY - 1] {
                assert_eq!(score_from_tt(score_to_tt(score, ply), ply), score);
            }
        }
        // Mate in 5 from the root, seen at ply 3, is mate in 2 from that node.
        assert_eq!(score_to_tt(mate_in(5), 3), mate_in(2));
    }

    /// The knights out and back, twice: every position repeats every four plies.
    const SHUFFLE: [&str; 8] = ["g1f3", "g8f6", "f3g1", "f6g8", "g1f3", "g8f6", "f3g1", "f6g8"];

    /// Whether a worker that searches from `root`, after the game `history`, and has played
    /// `line` in its search, sees the position it reaches as drawn.
    fn drawn(root: &Position, history: &[u64], line: &[&str]) -> bool {
        let tt = TranspositionTable::new(0);
        let control = Control::default();
        let limits = Limits::default();
        let shared = Shared {
            root,
            history,
            root_moves: &MoveList::new(),
            restricted: false,
            limits: &limits,
            tt: &tt,
            control: &control,
            nodes: &[],
            start: Instant::now(),
        };
        let mut data = ThreadData::new();
        let worker = Worker::new(&shared, &mut data, 0, None);
        let mut pos = root.clone();
        for uci in line {
            pos.make_move(pos.parse_uci_move(uci).expect("move should be legal"));
            worker.keys.push(pos.zobrist_key());
        }
        worker.is_draw(&pos, pos.in_check())
    }

    /// The position after the first `plies` moves of [`SHUFFLE`], with the game's keys so far.
    fn after_shuffle(plies: usize) -> (Position, Vec<u64>) {
        let mut pos = Position::startpos();
        let mut history = vec![pos.zobrist_key()];
        for uci in &SHUFFLE[..plies] {
            pos.make_move(pos.parse_uci_move(uci).expect("move should be legal"));
            history.push(pos.zobrist_key());
        }
        (pos, history)
    }

    #[test]
    fn a_repetition_inside_the_search_is_a_draw() {
        let (root, history) = after_shuffle(0);
        assert!(!drawn(&root, &history, &SHUFFLE[..4]), "the root must need a threefold");
        assert!(drawn(&root, &history, &SHUFFLE[..5]), "repeating Nf3 must be drawn");
    }

    #[test]
    fn a_position_from_before_the_root_needs_a_threefold_repetition() {
        // The start position has been on the board twice, then three times.
        let (twice, history) = after_shuffle(4);
        assert!(!drawn(&twice, &history, &[]));
        let (three_times, history) = after_shuffle(8);
        assert!(drawn(&three_times, &history, &[]));
        assert!(!drawn(&three_times, &[], &[]), "without the history, nothing must repeat");
        // Nf3 from the root reaches a position that was on the board once, then twice, before.
        let (root, history) = after_shuffle(4);
        assert!(!drawn(&root, &history, &["g1f3"]));
        let (root, history) = after_shuffle(8);
        assert!(drawn(&root, &history, &["g1f3"]));
    }

    #[test]
    fn fifty_moves_draw_unless_mated() {
        let fen = |fen| Position::from_fen(fen).expect("FEN should be valid");
        assert!(drawn(&fen("4k3/8/8/8/8/8/R7/4K3 w - - 100 80"), &[], &[]));
        assert!(!drawn(&fen("4k3/8/8/8/8/8/R7/4K3 w - - 99 80"), &[], &[]));
        // Checkmated on the hundredth ply: the mate stands.
        assert!(!drawn(&fen("R3k3/8/4K3/8/8/8/8/8 b - - 100 80"), &[], &[]));
        assert!(drawn(&fen("4k3/8/8/8/8/8/8/2B1K3 w - - 0 1"), &[], &[]));
    }

    #[test]
    fn reductions_grow_with_depth_and_move_number() {
        assert_eq!(reduction(1, 1), 0);
        assert!(reduction(10, 30) > reduction(3, 4));
        assert_eq!(reduction(500, 500), reduction(63, 63));
    }

    #[test]
    fn non_pawn_material_ignores_pawns_and_king() {
        let has =
            |fen| has_non_pawn_material(&Position::from_fen(fen).expect("FEN should be valid"));
        assert!(!has("4k3/pppp4/8/8/8/8/PPPP4/4K3 w - - 0 1"));
        assert!(has("4k3/8/8/8/8/8/8/2N1K3 w - - 0 1"));
        assert!(!has("4k3/8/8/8/8/8/8/2N1K3 b - - 0 1"), "a lone king must not count");
    }
}
