//! History tables and move ordering.
//!
//! In the main search, moves are tried in the following order, higher scores first within each
//! step:
//!
//! 1. The TT (transposition table) move: the best move an earlier search of the same position
//!    stored in the transposition table.
//!    It is tried before any other move is generated, and only if it is legal.
//! 2. Captures that do not lose material, and promotions to a queen.
//!    A capture loses material when its SEE (static exchange evaluation) is negative.
//!    Each is scored by the SEE value of the captured piece multiplied by [`VICTIM_WEIGHT`],
//!    with the move's entry in the capture history table (described below) then added.
//! 3. Quiet moves, that is, moves that neither capture nor promote.
//!    Each is scored by the move's entry in the main history table multiplied by 2, with its
//!    entries in the continuation history table (described below) then added, one for each of
//!    the moves played one, two and four plies earlier.
//! 4. Captures that lose material, scored as in step 2.
//! 5. Promotions to a knight, bishop or rook, scored as in step 2.
//!
//! The history tables score moves by how well they have done in the search so far:
//!
//! - Main history is indexed by the side to move, the origin square and the destination square
//!   of a quiet move.
//! - Capture history is indexed by the moved piece, the destination square and the kind of
//!   piece captured, a pawn for a promotion that captures nothing.
//! - Continuation history is indexed by the piece and destination square of an earlier move,
//!   then those of the quiet move being scored.
//!   It tells how well that move has answered the earlier one.
//!
//! An entry rises when its move causes a beta cutoff, and falls when its move was tried before
//! another move that caused one.
//! Each change grows with the remaining depth, and shrinks as the entry nears its limit of
//! ±16,384 ([`MAX_HISTORY`]), so recent results outweigh old ones.
//!
//! In quiescence search, the TT move comes first if it is a capture or a promotion, or if the
//! side to move is in check.
//! Captures and promotions of every kind follow, scored as in step 2 without checking SEE.
//! In check, the quiet evasions come last, scored by their main history entry alone.

use std::hint;
use std::mem::MaybeUninit;
use std::num::NonZeroU16;
use std::slice;

use super::memory::ZeroedBox;
use super::see::{SEE_VALUE, see_ge, victim, victim_value};
use crate::board::{Color, Move, MoveGenerator, MoveList, PieceKind, Position};

/// Captures and queen promotions that do not lose material score this plus their history.
const GOOD_TACTICAL: i32 = 1_000_000_000;
/// Captures that lose material score this plus their history.
///
/// Quiet moves score their history alone, far inside the gap between the two tactical bands.
const BAD_TACTICAL: i32 = -1_000_000_000;
/// Underpromotions score this plus their history, below every other move.
const UNDERPROMOTION: i32 = -1_500_000_000;

/// History scores saturate at this magnitude.
pub(crate) const MAX_HISTORY: i32 = 16_384;

/// Bonus for a move that caused a cutoff `depth` plies from the leaves.
pub(crate) fn history_bonus(depth: i32) -> i32 {
    (150 * depth - 90).clamp(0, 1500)
}

/// Malus for the moves tried before the one that caused a cutoff.
///
/// It is larger than the bonus, since a move that did not cut is more reliably known not to.
pub(crate) fn history_malus(depth: i32) -> i32 {
    (180 * depth - 60).clamp(0, 1800)
}

/// Moves `entry` by `bonus`, less as it nears `limit`, so recent results outweigh old ones.
fn gravity(entry: i32, bonus: i32, limit: i32) -> i32 {
    let bonus = bonus.clamp(-limit, limit);
    entry + bonus - entry * bonus.abs() / limit
}

/// Applies [`gravity`] to a stored entry, keeping it within `limit`, which is at most `i16::MAX`.
#[inline]
fn update(entry: &mut i16, bonus: i32, limit: i32) {
    let updated = gravity(i32::from(*entry), bonus, limit);
    *entry = i16::try_from(updated).expect("gravity should keep the entry within `i16`");
}

/// Pawn structures the correction history tells apart, a power of two.
const CORRECTION_SIZE: usize = 1 << 14;
/// Correction entries saturate at this.
const CORRECTION_LIMIT: i32 = 16_384;
/// Correction entries count this many units per centipawn.
const CORRECTION_GRAIN: i32 = 128;

/// A piece and the square it moved to: what continuation history is keyed by, both for the
/// earlier move and the one being judged.
///
/// Stored as its index plus one, so `Option<PieceTo>` takes two bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct PieceTo(NonZeroU16);

impl PieceTo {
    const COUNT: usize = 12 * 64;

    /// The piece `mv` moves in `pos`, before any promotion, and its destination.
    #[inline]
    pub(crate) fn of(pos: &Position, mv: Move) -> Self {
        let piece = pos.piece_at(mv.from()).expect("the source square should hold a piece");
        let slot = piece.color().index() * 6 + piece.kind().index();
        let index = slot * 64 + mv.to().index();
        debug_assert!(index < Self::COUNT, "piece and square index must be below `COUNT`");
        let index = u16::try_from(index).expect("index should fit in a `u16`, being below `COUNT`");
        Self(NonZeroU16::MIN.saturating_add(index))
    }

    /// The index, below [`PieceTo::COUNT`].
    ///
    /// The compiler is told this, so indexing the history tables by it needs no bounds check.
    #[inline]
    const fn index(self) -> usize {
        let index = self.0.get() as usize - 1;
        // SAFETY: only `of` builds a `PieceTo`, from a color index below 2, a kind index below
        // 6 and a square index below 64, so `index` is below 12 * 64.
        unsafe { hint::assert_unchecked(index < Self::COUNT) };
        index
    }
}

/// How much more a victim's SEE value counts than capture history in [`History::tactical`].
pub(crate) const VICTIM_WEIGHT: i32 = 7;

/// What the search learns about moves and evaluations, per thread.
///
/// The tables are fixed-size arrays, so indexing them by squares and colors needs no bounds
/// checks. Their entries are `i16`, which every entry fits, so they take half the cache of `i32`.
pub(crate) struct History {
    /// By side, from and to, for quiet moves.
    main: ZeroedBox<[[[i16; 64]; 64]; 2]>,
    /// By moved piece and destination, and the kind captured, for captures.
    capture: ZeroedBox<[[i16; 6]; PieceTo::COUNT]>,
    /// By an earlier move's piece and destination, then this move's.
    continuation: ZeroedBox<[[i16; PieceTo::COUNT]; PieceTo::COUNT]>,
    /// By side, then a hash of the pawns: how far the static evaluation tends to be off, in
    /// [`CORRECTION_GRAIN`] units per centipawn.
    correction: ZeroedBox<[i16; 2 * CORRECTION_SIZE]>,
}

impl History {
    pub(crate) fn new() -> Self {
        Self {
            main: ZeroedBox::new(),
            capture: ZeroedBox::new(),
            continuation: ZeroedBox::new(),
            correction: ZeroedBox::new(),
        }
    }

    /// Forgets everything learned.
    pub(crate) fn clear(&mut self) {
        self.main.as_flattened_mut().as_flattened_mut().fill(0);
        self.capture.as_flattened_mut().fill(0);
        self.continuation.as_flattened_mut().fill(0);
        self.correction.fill(0);
    }

    /// Whether nothing has been learned.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        let main = self.main.as_flattened().as_flattened().iter();
        main.chain(self.capture.as_flattened())
            .chain(self.correction.iter())
            .chain(self.continuation.as_flattened())
            .all(|&n| n == 0)
    }

    #[inline]
    pub(crate) fn main(&self, us: Color, mv: Move) -> i32 {
        i32::from(self.main[us.index()][mv.from().index()][mv.to().index()])
    }

    pub(crate) fn update_main(&mut self, us: Color, mv: Move, bonus: i32) {
        update(&mut self.main[us.index()][mv.from().index()][mv.to().index()], bonus, MAX_HISTORY);
    }

    /// The indices of the moved piece and destination, and of the kind captured: a pawn for a
    /// promotion without a capture.
    fn capture_index(pos: &Position, mv: Move) -> (usize, usize) {
        let victim = victim(pos, mv).unwrap_or(PieceKind::Pawn);
        (PieceTo::of(pos, mv).index(), victim.index())
    }

    #[inline]
    pub(crate) fn capture(&self, pos: &Position, mv: Move) -> i32 {
        let (piece_to, victim) = Self::capture_index(pos, mv);
        i32::from(self.capture[piece_to][victim])
    }

    pub(crate) fn update_capture(&mut self, pos: &Position, mv: Move, bonus: i32) {
        let (piece_to, victim) = Self::capture_index(pos, mv);
        update(&mut self.capture[piece_to][victim], bonus, MAX_HISTORY);
    }

    #[inline]
    pub(crate) fn continuation(&self, earlier: PieceTo, now: PieceTo) -> i32 {
        i32::from(self.continuation[earlier.index()][now.index()])
    }

    pub(crate) fn update_continuation(&mut self, earlier: PieceTo, now: PieceTo, bonus: i32) {
        update(&mut self.continuation[earlier.index()][now.index()], bonus, MAX_HISTORY);
    }

    /// How good a quiet move looks after the earlier moves `contexts`.
    pub(crate) fn quiet(&self, pos: &Position, mv: Move, contexts: &[Option<PieceTo>]) -> i32 {
        let now = PieceTo::of(pos, mv);
        let continuation: i32 =
            contexts.iter().flatten().map(|&earlier| self.continuation(earlier, now)).sum();
        2 * self.main(pos.side_to_move(), mv) + continuation
    }

    /// How good a capture or promotion looks: its victim first, then its history.
    pub(crate) fn tactical(&self, pos: &Position, mv: Move) -> i32 {
        VICTIM_WEIGHT * victim_value(pos, mv) + self.capture(pos, mv)
    }

    /// By the side to move, then the hash of the pawns.
    fn correction_index(pos: &Position) -> usize {
        let pawns = |color| pos.pieces(color, PieceKind::Pawn).0;
        let hash = pawns(Color::White).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ pawns(Color::Black).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        let index = usize::try_from(hash >> (64 - CORRECTION_SIZE.trailing_zeros()))
            .expect("index should fit in a `usize`, having fewer bits");
        pos.side_to_move().index() * CORRECTION_SIZE + index
    }

    /// Centipawns to add to the static evaluation of `pos`.
    #[inline]
    pub(crate) fn correction(&self, pos: &Position) -> i32 {
        i32::from(self.correction[Self::correction_index(pos)]) / CORRECTION_GRAIN
    }

    /// Learns that a search of `pos`, `depth` plies deep, scored `error` centipawns away from
    /// its static evaluation.
    pub(crate) fn update_correction(&mut self, pos: &Position, depth: i32, error: i32) {
        let bonus = (error * depth * CORRECTION_GRAIN / 8)
            .clamp(-CORRECTION_LIMIT / 4, CORRECTION_LIMIT / 4);
        update(&mut self.correction[Self::correction_index(pos)], bonus, CORRECTION_LIMIT);
    }
}

/// How a picker scores the moves it has not handed out.
#[derive(Clone, Copy)]
enum Scoring {
    /// Every kind of move, after the moves played one, two and four plies before, where known.
    Main([Option<PieceTo>; 3]),
    /// Captures and promotions by victim and capture history, without checking their
    /// exchanges. In check, quiet evasions follow.
    Tactical,
}

/// Scores at or above this are captures and promotions handed out before the quiet moves.
const GOOD_TACTICAL_MIN: i32 = GOOD_TACTICAL / 2;

// A move's score is its band plus its history, which must keep it inside the band, clear of
// `GOOD_TACTICAL_MIN`, so the bands never overlap and the good captures are told apart.
const _: () = {
    // A quiet move's main history twice plus three continuations, or a capture's victim and
    // its history.
    let quiet = 5 * MAX_HISTORY;
    let tactical = VICTIM_WEIGHT * SEE_VALUE[PieceKind::Queen.index()] + MAX_HISTORY;
    let max_in_band = if quiet > tactical { quiet } else { tactical };
    assert!(GOOD_TACTICAL + max_in_band < i32::MAX);
    assert!(GOOD_TACTICAL - max_in_band >= GOOD_TACTICAL_MIN);
    assert!(max_in_band < GOOD_TACTICAL_MIN);
    assert!(-max_in_band > BAD_TACTICAL + max_in_band);
    assert!(BAD_TACTICAL - max_in_band > UNDERPROMOTION + max_in_band);
    assert!(UNDERPROMOTION - max_in_band > i32::MIN);
};

/// A move handed out by [`MovePicker::next`], and what the picker learned of it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Picked {
    pub mv: Move,
    /// What [`History`] says of the move: [`History::tactical`] for a capture or promotion,
    /// [`History::quiet`] for another move, or [`History::main`] for a quiet evasion in
    /// quiescence search. `None` for the TT move, which is not scored.
    pub history: Option<i32>,
    /// Whether the exchange the move starts is known to lose at most a pawn: a capture whose
    /// SEE the picker found to lose nothing, or a queen promotion, which at worst loses the
    /// pawn. Always `false` for captures-only ordering, which does not check exchanges.
    pub safe_exchange: bool,
}

/// Where a picker is in handing out its moves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    /// Staged: the TT move, once checked legal, before any generation.
    TtMove,
    GenerateTacticals,
    /// Captures and promotions not banded below the quiet moves.
    GoodTacticals,
    GenerateQuiets,
    Quiets,
    /// Losing captures, then underpromotions.
    BadTacticals,
    /// Every move given up front: the TT move, if among them, before any is scored.
    ListTtMove,
    ListScore,
    List,
    Done,
}

/// Hands out moves best first, lazily.
///
/// A staged picker generates in stages: the TT (transposition table) move before any generation,
/// then captures, then quiet moves only once they are reached, since a cutoff often comes before.
/// Within a stage, moves are picked by selection sort.
pub(crate) struct MovePicker<'p> {
    pos: &'p Position,
    /// Shared by the stages, which then find the checkers and pins once.
    ///
    /// Built when first needed, which a list of moves given up front never is.
    generator: Option<MoveGenerator<'p>>,
    moves: MoveList,
    /// The score of the move at the same index, written when it is scored.
    ///
    /// Only the slots from a stage's next move to its end are read, all scored by then.
    scores: [MaybeUninit<i32>; MoveList::CAPACITY],
    stage: Stage,
    /// The next tactical move to hand out, or the next move for a list.
    next: usize,
    /// Where the tactical moves end and the quiet ones start.
    tactical_end: usize,
    next_quiet: usize,
    tt_move: Option<Move>,
    scoring: Scoring,
    /// Whether captures are scored good without their SEE, which is checked once one is picked:
    ///
    /// a cutoff often comes before the rest matter.
    lazy_see: bool,
    /// Whether quiet moves are left out from now on.
    skip_quiets: bool,
    /// Whether the tactical moves are already generated, by [`MovePicker::begin_probcut`].
    pregenerated: bool,
    /// The TT move and scoring [`MovePicker::end_probcut`] restores.
    main: (Option<Move>, Scoring),
}

impl<'p> MovePicker<'p> {
    fn with(pos: &'p Position, stage: Stage, tt_move: Option<Move>, scoring: Scoring) -> Self {
        Self {
            pos,
            generator: None,
            moves: MoveList::new(),
            scores: [const { MaybeUninit::uninit() }; MoveList::CAPACITY],
            stage,
            next: 0,
            tactical_end: 0,
            next_quiet: 0,
            tt_move,
            scoring,
            lazy_see: stage == Stage::TtMove,
            skip_quiets: false,
            pregenerated: false,
            main: (tt_move, scoring),
        }
    }

    /// Orders every legal move, generating them in stages.
    ///
    /// `tt_move` may be any move: it is handed out only if legal.
    /// `contexts` are the moves played one, two and four plies before, where known.
    pub(crate) fn new(
        pos: &'p Position,
        tt_move: Option<Move>,
        contexts: [Option<PieceTo>; 3],
    ) -> Self {
        Self::with(pos, Stage::TtMove, tt_move, Scoring::Main(contexts))
    }

    /// Orders `moves`, legal in `pos`, as [`MovePicker::new`] orders every move.
    pub(crate) fn list(
        pos: &'p Position,
        moves: &MoveList,
        tt_move: Option<Move>,
        contexts: [Option<PieceTo>; 3],
    ) -> Self {
        let mut picker = Self::with(pos, Stage::ListTtMove, tt_move, Scoring::Main(contexts));
        picker.moves.append(moves);
        picker
    }

    /// Orders the legal captures and promotions, by victim and
    /// capture history, and in check the quiet evasions after them.
    ///
    /// Staged as [`MovePicker::new`] is: `tt_move`, if legal and
    /// of a kind searched, comes before any move is generated. The exchanges are not checked.
    pub(crate) fn quiescence(pos: &'p Position, tt_move: Option<Move>, in_check: bool) -> Self {
        let tt_move = tt_move.filter(|mv| in_check || mv.is_tactical());
        let mut picker = Self::with(pos, Stage::TtMove, tt_move, Scoring::Tactical);
        picker.skip_quiets = !in_check;
        picker
    }

    /// Switches a staged picker that has not started to the legal captures and promotions: it
    /// generates them now, and hands them out as [`MovePicker::quiescence`] orders them,
    /// `tt_move` first. [`MovePicker::end_probcut`] switches back, keeping them generated.
    pub(crate) fn begin_probcut(&mut self, tt_move: Option<Move>) {
        debug_assert_eq!(self.stage, Stage::TtMove, "picker must be staged and not yet started");
        Self::generator(&mut self.generator, self.pos).push_tactical(&mut self.moves);
        self.pregenerated = true;
        self.main = (self.tt_move, self.scoring);
        (self.tt_move, self.scoring) = (tt_move, Scoring::Tactical);
        self.stage = Stage::ListTtMove;
    }

    /// Restores the order the picker had before [`MovePicker::begin_probcut`], from its first
    /// move.
    pub(crate) fn end_probcut(&mut self) {
        (self.tt_move, self.scoring) = self.main;
        self.next = 0;
        self.stage = Stage::TtMove;
    }

    /// The generator, built on first use.
    ///
    /// It takes the fields rather than `self`, so the move list can be borrowed alongside.
    fn generator<'g>(
        generator: &'g mut Option<MoveGenerator<'p>>,
        pos: &'p Position,
    ) -> &'g MoveGenerator<'p> {
        generator.get_or_insert_with(|| MoveGenerator::new(pos))
    }

    /// Leaves out the quiet moves not handed out yet, except the TT move.
    ///
    /// A staged picker then does not generate them.
    pub(crate) fn skip_quiets(&mut self) {
        self.skip_quiets = true;
    }

    fn score(&self, mv: Move, history: &History) -> i32 {
        let pos = self.pos;
        match self.scoring {
            Scoring::Main(contexts) => {
                if let Some(promotion) = mv.promotion() {
                    let base =
                        if promotion == PieceKind::Queen { GOOD_TACTICAL } else { UNDERPROMOTION };
                    base + history.tactical(pos, mv)
                } else if mv.is_capture() {
                    let good = self.lazy_see || see_ge(pos, mv, 0);
                    let base = if good { GOOD_TACTICAL } else { BAD_TACTICAL };
                    base + history.tactical(pos, mv)
                } else {
                    history.quiet(pos, mv, &contexts)
                }
            }
            Scoring::Tactical if mv.is_tactical() => GOOD_TACTICAL + history.tactical(pos, mv),
            Scoring::Tactical => history.main(pos.side_to_move(), mv),
        }
    }

    /// Scores the moves in `start..end`.
    fn score_range(&mut self, start: usize, end: usize, history: &History) {
        for i in start..end {
            let score = self.score(self.moves[i], history);
            self.scores[i].write(score);
        }
    }

    /// The scores of the moves in `start..end`, which must all be scored.
    #[inline]
    fn scored(&self, start: usize, end: usize) -> &[i32] {
        debug_assert!(end <= self.moves.len(), "`end` must not be past the moves");
        let slots = &self.scores[start..end];
        // SAFETY: only ranges of scored moves are passed: each stage scores
        // its whole range before reading it, and swaps only within it.
        // `MaybeUninit<i32>` has the layout of `i32`.
        unsafe { slice::from_raw_parts(slots.as_ptr().cast::<i32>(), slots.len()) }
    }

    /// Moves the best move of `start..end`, all scored, to `start`, and returns its score, or
    /// `None` if the range is empty. Of several equally good moves, the first wins.
    ///
    /// One branchless pass, which vectorizes: it takes the maximum of keys holding the score in
    /// the high half and the inverted index in the low half, so the first of equal scores has
    /// the largest key.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the key's halves are an `i32` score and a `u32` index"
    )]
    #[inline]
    fn select(&mut self, start: usize, end: usize) -> Option<i32> {
        let scores = self.scored(start, end);
        let key = scores
            .iter()
            .zip(0_u32..)
            .map(|(&score, index)| (i64::from(score) << 32) | i64::from(!index))
            .max()?;
        let best = start + !(key as u32) as usize;
        self.moves.as_mut_slice().swap(start, best);
        self.scores.swap(start, best);
        Some((key >> 32) as i32)
    }

    /// The TT move, unscored.
    const fn tt_picked(mv: Move) -> Picked {
        Picked { mv, history: None, safe_exchange: false }
    }

    /// A scored move, from its band and score.
    const fn picked(&self, mv: Move, band: i32, score: i32) -> Picked {
        let main = matches!(self.scoring, Scoring::Main(_));
        Picked { mv, history: Some(score - band), safe_exchange: main && band == GOOD_TACTICAL }
    }

    /// The band of a listed move scored `score`.
    ///
    /// The move's kind decides it, except for a capture,
    /// whose score shows whether its exchange was good.
    fn list_band(&self, mv: Move, score: i32) -> i32 {
        match (self.scoring, mv.promotion()) {
            (Scoring::Tactical, _) if mv.is_tactical() => GOOD_TACTICAL,
            (Scoring::Tactical, _) => 0,
            (Scoring::Main(_), Some(PieceKind::Queen)) => GOOD_TACTICAL,
            (Scoring::Main(_), Some(_)) => UNDERPROMOTION,
            (Scoring::Main(_), None) if !mv.is_capture() => 0,
            (Scoring::Main(_), None) if score >= GOOD_TACTICAL_MIN => GOOD_TACTICAL,
            (Scoring::Main(_), None) => BAD_TACTICAL,
        }
    }

    /// The best move not yet handed out.
    #[expect(clippy::too_many_lines, reason = "one state machine reads best in one piece")]
    pub(crate) fn next(&mut self, history: &History) -> Option<Picked> {
        loop {
            match self.stage {
                Stage::TtMove => {
                    self.stage = Stage::GenerateTacticals;
                    let tt_move = self.tt_move;
                    let generator = Self::generator(&mut self.generator, self.pos);
                    match tt_move {
                        Some(mv) if generator.is_legal(mv) => return Some(Self::tt_picked(mv)),
                        _ => self.tt_move = None,
                    }
                }
                Stage::GenerateTacticals => {
                    if !self.pregenerated {
                        let generator = Self::generator(&mut self.generator, self.pos);
                        generator.push_tactical(&mut self.moves);
                    }
                    self.tactical_end = self.moves.len();
                    self.score_range(0, self.tactical_end, history);
                    self.stage = Stage::GoodTacticals;
                }
                Stage::GoodTacticals => match self.select(self.next, self.tactical_end) {
                    Some(score) if score >= GOOD_TACTICAL_MIN => {
                        let mv = self.moves[self.next];
                        if Some(mv) == self.tt_move {
                            self.next += 1;
                            continue;
                        }
                        // In the main search, a capture losing material waits with the bad
                        // ones: its score moves to their band, and the next best is picked.
                        if matches!(self.scoring, Scoring::Main(_))
                            && mv.promotion().is_none()
                            && !see_ge(self.pos, mv, 0)
                        {
                            self.scores[self.next].write(score - GOOD_TACTICAL + BAD_TACTICAL);
                            continue;
                        }
                        self.next += 1;
                        return Some(self.picked(mv, GOOD_TACTICAL, score));
                    }
                    _ => self.stage = Stage::GenerateQuiets,
                },
                Stage::GenerateQuiets => {
                    if self.skip_quiets {
                        self.stage = Stage::BadTacticals;
                        continue;
                    }
                    Self::generator(&mut self.generator, self.pos).push_quiet(&mut self.moves);
                    self.score_range(self.tactical_end, self.moves.len(), history);
                    self.next_quiet = self.tactical_end;
                    self.stage = Stage::Quiets;
                }
                Stage::Quiets => {
                    let picked = if self.skip_quiets {
                        None
                    } else {
                        self.select(self.next_quiet, self.moves.len())
                    };
                    let Some(score) = picked else {
                        self.stage = Stage::BadTacticals;
                        continue;
                    };
                    let mv = self.moves[self.next_quiet];
                    self.next_quiet += 1;
                    if Some(mv) != self.tt_move {
                        return Some(self.picked(mv, 0, score));
                    }
                }
                Stage::BadTacticals => {
                    let Some(score) = self.select(self.next, self.tactical_end) else {
                        self.stage = Stage::Done;
                        continue;
                    };
                    let mv = self.moves[self.next];
                    self.next += 1;
                    // Every promotion left is an underpromotion.
                    let band = if mv.promotion().is_some() { UNDERPROMOTION } else { BAD_TACTICAL };
                    if Some(mv) != self.tt_move {
                        return Some(self.picked(mv, band, score));
                    }
                }
                Stage::ListTtMove => {
                    self.stage = Stage::ListScore;
                    let tt_index =
                        self.tt_move.and_then(|tt| self.moves.iter().position(|&mv| mv == tt));
                    if let Some(index) = tt_index {
                        self.moves.as_mut_slice().swap(0, index);
                        self.next = 1;
                        return Some(Self::tt_picked(self.moves[0]));
                    }
                }
                Stage::ListScore => {
                    self.score_range(self.next, self.moves.len(), history);
                    self.stage = Stage::List;
                }
                Stage::List => {
                    let Some(score) = self.select(self.next, self.moves.len()) else {
                        self.stage = Stage::Done;
                        continue;
                    };
                    let mv = self.moves[self.next];
                    self.next += 1;
                    return Some(self.picked(mv, self.list_band(mv, score), score));
                }
                Stage::Done => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::positions::KIWIPETE;

    fn drain(mut picker: MovePicker<'_>, history: &History) -> Vec<String> {
        let mut order = Vec::new();
        while let Some(picked) = picker.next(history) {
            order.push(picked.mv.to_string());
        }
        order
    }

    /// The order of the staged picker, after checking the list picker agrees on everything but
    /// ties.
    fn order(fen: &str, tt_move: Option<&str>, history: &History) -> Vec<String> {
        let pos = Position::from_fen(fen).expect("FEN should be valid");
        let tt_move = tt_move.map(|uci| pos.parse_uci_move(uci).expect("move should be legal"));
        let staged = drain(MovePicker::new(&pos, tt_move, [None; 3]), history);
        let listed = drain(MovePicker::list(&pos, &pos.legal_moves(), tt_move, [None; 3]), history);
        let mut sorted = (staged.clone(), listed);
        sorted.0.sort();
        sorted.1.sort();
        assert_eq!(sorted.0, sorted.1);
        staged
    }

    #[test]
    fn picked_moves_carry_their_history_and_exchange() {
        // The rook takes the queen on d5 safely; it takes the pawn on a7, which the rook on a8
        // defends, at a loss.
        let fen = "r3k3/p7/4p3/3q4/4P3/8/8/R2RK3 w - - 0 1";
        let pos = Position::from_fen(fen).expect("FEN should be valid");
        let mv = |uci| pos.parse_uci_move(uci).expect("move should be legal");
        let mut history = History::new();
        history.update_capture(&pos, mv("a1a7"), -700);
        history.update_main(Color::White, mv("e1f2"), 900);
        let check = |picker: MovePicker<'_>| {
            let picked = drain_picked(picker, &history);
            let find = |uci: &str| *picked.iter().find(|p| p.mv.to_string() == uci).expect(uci);
            assert_eq!(find("e1e2").history, None, "the TT move must have no history");
            let good = find("d1d5");
            assert_eq!(good.history, Some(history.tactical(&pos, good.mv)));
            assert!(good.safe_exchange);
            let bad = find("a1a7");
            assert_eq!(bad.history, Some(history.tactical(&pos, bad.mv)));
            assert!(!bad.safe_exchange);
            assert_eq!(find("e1f2").history, Some(history.quiet(&pos, mv("e1f2"), &[None; 3])));
            assert!(!find("e1f2").safe_exchange);
        };
        check(MovePicker::new(&pos, Some(mv("e1e2")), [None; 3]));
        check(MovePicker::list(&pos, &pos.legal_moves(), Some(mv("e1e2")), [None; 3]));
    }

    fn drain_picked(mut picker: MovePicker<'_>, history: &History) -> Vec<Picked> {
        let mut all = Vec::new();
        while let Some(next) = picker.next(history) {
            all.push(next);
        }
        all
    }

    #[test]
    fn quiescence_hands_out_the_tactical_moves_or_every_evasion() {
        let history = History::new();
        let pos = Position::from_fen(KIWIPETE).expect("FEN should be valid");
        let quiet = pos.parse_uci_move("e1g1");
        let capture = pos.parse_uci_move("e2a6").expect("move should be legal");
        let mut moves = drain(MovePicker::quiescence(&pos, quiet, false), &history);
        assert!(!moves.contains(&"e1g1".to_string()), "a quiet TT move must not be searched");
        let mut expected: Vec<String> =
            pos.legal_tactical_moves().iter().map(Move::to_string).collect();
        moves.sort();
        expected.sort();
        assert_eq!(moves, expected);
        let moves = drain(MovePicker::quiescence(&pos, Some(capture), false), &history);
        assert_eq!(moves[0], "e2a6");
        assert_eq!(moves.len(), expected.len());
        // In check, every evasion, the captures first.
        let checked =
            Position::from_fen("4k3/8/8/8/1b6/8/2P5/4K3 w - - 0 1").expect("FEN should be valid");
        let evasions = drain(MovePicker::quiescence(&checked, None, true), &history);
        assert_eq!(evasions.len(), checked.legal_moves().len());
    }

    #[test]
    fn hands_out_every_move_once() {
        let pos = Position::from_fen(KIWIPETE).expect("FEN should be valid");
        let mut expected: Vec<String> = pos.legal_moves().iter().map(Move::to_string).collect();
        expected.sort();
        for tt_move in [None, Some("e2a6"), Some("e1g1")] {
            let mut moves = order(KIWIPETE, tt_move, &History::new());
            if let Some(tt_move) = tt_move {
                assert_eq!(moves[0], tt_move);
            }
            moves.sort();
            assert_eq!(moves, expected);
        }
    }

    #[test]
    fn an_illegal_tt_move_is_left_out() {
        let pos = Position::startpos();
        let other = Position::from_fen(KIWIPETE).expect("FEN should be valid");
        let illegal =
            other.parse_uci_move("e2a6").expect("move should be legal in its own position");
        let moves = drain(MovePicker::new(&pos, Some(illegal), [None; 3]), &History::new());
        assert_eq!(moves.len(), 20);
        assert!(!moves.contains(&"e2a6".to_string()));
    }

    #[test]
    fn a_picker_back_from_probcut_hands_out_every_move() {
        let pos = Position::from_fen(KIWIPETE).expect("FEN should be valid");
        let history = History::new();
        let tt_move = pos.parse_uci_move("e1g1");
        let mut picker = MovePicker::new(&pos, tt_move, [None; 3]);
        picker.begin_probcut(pos.parse_uci_move("e2a6"));
        let mut captures = 0;
        while picker.next(&history).is_some() {
            captures += 1;
        }
        assert_eq!(captures, pos.legal_tactical_moves().len());
        picker.end_probcut();
        let mut moves = drain(picker, &history);
        assert_eq!(moves[0], "e1g1");
        let mut expected: Vec<String> = pos.legal_moves().iter().map(Move::to_string).collect();
        moves.sort();
        expected.sort();
        assert_eq!(moves, expected);
    }

    #[test]
    fn skipping_quiets_keeps_the_tactical_moves() {
        let pos = Position::from_fen(KIWIPETE).expect("FEN should be valid");
        let history = History::new();
        let mut picker = MovePicker::new(&pos, None, [None; 3]);
        let first = picker.next(&history).expect("picker should yield a capture");
        picker.skip_quiets();
        let mut rest = vec![first.mv];
        while let Some(picked) = picker.next(&history) {
            rest.push(picked.mv);
        }
        let tactical = pos.legal_tactical_moves();
        assert_eq!(rest.len(), tactical.len());
        assert!(rest.iter().all(|mv| tactical.contains(mv)));
    }

    #[test]
    fn tt_move_then_captures_by_value_then_quiets_by_history_then_losing_captures() {
        // White can take the queen on d5 with the pawn or the rook, which the e6 pawn defends,
        // and the rook can take the pawn on a7, which the rook on a8 defends.
        let fen = "r3k3/p7/4p3/3q4/4P3/8/8/R2RK3 w - - 0 1";
        let pos = Position::from_fen(fen).expect("FEN should be valid");
        let mv = |uci| pos.parse_uci_move(uci).expect("move should be legal");
        let mut history = History::new();
        history.update_main(Color::White, mv("e1f2"), 1000);
        history.update_main(Color::White, mv("a1b1"), 500);
        // Capture history breaks ties between equal victims, not past a bigger one.
        history.update_capture(&pos, mv("d1d5"), 1000);
        let order = order(fen, Some("e1e2"), &history);
        assert_eq!(order[..5], ["e1e2", "d1d5", "e4d5", "e1f2", "a1b1"]);
        assert_eq!(order.last().map(String::as_str), Some("a1a7"), "{order:?}");
    }

    #[test]
    fn continuation_history_follows_the_earlier_move() {
        let mut pos = Position::startpos();
        let e4 = pos.parse_uci_move("e2e4").expect("move should be legal");
        let earlier = PieceTo::of(&pos, e4);
        pos.make_move(e4);
        let e5 = pos.parse_uci_move("e7e5").expect("move should be legal");
        let mut history = History::new();
        history.update_continuation(earlier, PieceTo::of(&pos, e5), 800);
        assert_eq!(history.quiet(&pos, e5, &[Some(earlier)]), 800);
        assert_eq!(history.quiet(&pos, e5, &[None]), 0);
    }

    #[test]
    fn histories_saturate() {
        let mut history = History::new();
        let pos = Position::startpos();
        let mv = pos.parse_uci_move("e2e4").expect("move should be legal");
        let earlier = PieceTo::of(&pos, mv);
        for _ in 0..1000 {
            history.update_main(Color::White, mv, 1700);
            history.update_continuation(earlier, earlier, 1700);
        }
        assert!((MAX_HISTORY - 100..=MAX_HISTORY).contains(&history.main(Color::White, mv)));
        assert!(history.continuation(earlier, earlier) <= MAX_HISTORY);
        for _ in 0..1000 {
            history.update_main(Color::White, mv, -1700);
        }
        assert!(history.main(Color::White, mv) <= -MAX_HISTORY + 100);
        assert_eq!(history.main(Color::Black, mv), 0);
    }

    #[test]
    fn correction_learns_the_evaluation_error() {
        let mut history = History::new();
        let pos = Position::startpos();
        assert_eq!(history.correction(&pos), 0);
        for _ in 0..100 {
            history.update_correction(&pos, 8, 40);
        }
        let correction = history.correction(&pos);
        assert!(
            correction > 20 && correction <= CORRECTION_LIMIT / CORRECTION_GRAIN,
            "{correction}"
        );
        // Another pawn structure, or the other side to move, learned nothing.
        let mut after = pos.clone();
        after.make_move(pos.parse_uci_move("e2e4").expect("move should be legal"));
        assert_eq!(history.correction(&after), 0);
    }
}
