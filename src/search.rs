//! Finding the best move.
//!
//! Runs `worker` (src/search/worker.rs) in parallel.
//! Several threads search the same position, sharing only the TT (transposition table).
//! The first thread keeps time, reports and gives the result.

mod job_thread;
mod memory;
mod ordering;
mod see;
mod time;
mod tt;
mod worker;

use std::fmt;
use std::ptr::NonNull;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

pub(crate) use job_thread::{JobThread, SendOnDrop};
pub use tt::TranspositionTable;

use crate::board::{Color, Move, MoveList, Position};
use crate::eval::Score;
use memory::for_each_chunk_in_parallel;
use time::TimeManager;
use worker::{NodeCounter, Shared, ThreadData, Worker};

/// Deepest iteration a search runs, in plies.
pub const MAX_DEPTH: u32 = 100;

/// Most threads a search may use.
pub const MAX_THREADS: usize = 1024;

/// Stack size for each search thread in bytes.
pub const STACK_SIZE: usize = 16 << 20;

/// What a search is asked to do: which root moves to search, and when to stop.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Request {
    /// Only these root moves, which must be legal. Empty for every legal move.
    pub searchmoves: Vec<Move>,
    /// When to stop the search.
    pub limits: Limits,
    /// Defined by UCI's `Move Overhead` option. Subtracted from the time allotted to each move.
    pub move_overhead: Duration,
    /// Timestamp when `go` arrived, which the clock counts from.
    ///
    /// `None` to count from when the search starts.
    pub start: Option<Instant>,
}

/// When to stop searching, as `go` gives it. `None` fields impose no limit.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Limits {
    /// Time left on each side's clock, indexed by `Color`.
    ///
    /// A negative time, which GUIs may send once a clock runs out, is clamped to 0.
    pub time: [Option<Duration>; 2],
    /// Inc (increment): time added per move for each side, indexed by `Color`.
    pub inc: [Option<Duration>; 2],
    /// Moves until the next time control.
    pub movestogo: Option<u32>,
    /// Search no deeper than this many plies, clamped to `1..=`[`MAX_DEPTH`].
    pub depth: Option<u32>,
    /// Stop after approximately this many nodes, counted over all threads.
    pub nodes: Option<u64>,
    /// Stop once a mate in at most this many moves is found.
    pub mate: Option<u32>,
    /// Search for exactly this duration.
    pub movetime: Option<Duration>,
    /// Ignore the clock; only `stop` or another limit ends the search.
    pub infinite: bool,
}

impl Limits {
    /// Whether a search with `us` to move ends by itself,
    /// rather than running until it is told to stop.
    ///
    /// Only `us`'s clock counts.
    /// Pondering does not count: it runs until `ponderhit`, then by the clock.
    /// Nor does `mate` alone: without a mate to find, it would search to [`MAX_DEPTH`].
    #[must_use]
    pub fn is_bounded(&self, us: Color) -> bool {
        !self.infinite
            && (self.depth.is_some()
                || self.nodes.is_some()
                || self.movetime.is_some()
                || self.time[us.index()].is_some())
    }
}

/// The search threads, kept between searches.
///
/// Maintaining the threads avoids the delay of starting new ones for every search.
/// Each thread also keeps its data, in particular its history tables,
/// so every search starts from what earlier searches learned.
pub struct SearchThreads {
    /// One per thread, the calling thread's data is the first element in the vector.
    data: Vec<ThreadData>,
    /// Threads spawned to search alongside the calling thread, which is not one of them.
    ///
    /// `spawned_threads[i]` searches with `data[i + 1]`.
    spawned_threads: Vec<SpawnedThread>,
    /// The requested number of search threads, clamped to `1..=MAX_THREADS`.
    ///
    /// If `requested == data.len()`, all threads were successfully spawned.
    /// Otherwise, some threads could not be spawned.
    requested: usize,
    /// Whether no search has run since the threads were made or cleared.
    untouched: bool,
}

impl SearchThreads {
    /// `count` threads, the calling one included, clamped to `1..=MAX_THREADS`.
    ///
    /// The actual number of threads used can be lower if the OS fails to create some.
    #[must_use]
    pub fn new(count: usize) -> Self {
        let requested = count.clamp(1, MAX_THREADS);
        let spawned_threads: Vec<SpawnedThread> =
            (1..requested).map_while(SpawnedThread::spawn).collect();
        let data = (0..=spawned_threads.len()).map(|_| ThreadData::new()).collect();
        Self { data, spawned_threads, requested, untouched: true }
    }

    /// The threads that run, the calling one included.
    #[must_use]
    pub fn count(&self) -> usize {
        self.data.len()
    }

    /// Remakes the threads for `count`, as [`SearchThreads::new`] does.
    ///
    /// Threads already made for `count` are kept, along with what they learned, even if some
    /// threads could not be spawned: trying again would fail the same way.
    pub fn resize(&mut self, count: usize) {
        if count.clamp(1, MAX_THREADS) != self.requested {
            // The old threads are dropped first, so both sets never occupy memory at once.
            self.spawned_threads = Vec::new();
            self.data = Vec::new();
            *self = Self::new(count);
        }
    }

    /// Forgets everything the threads learned, for a new game.
    pub fn clear(&mut self) {
        if self.untouched {
            return;
        }

        for_each_chunk_in_parallel(&mut self.data, 8, |data| {
            for data in data {
                data.history.clear();
            }
        });
        self.untouched = true;
    }
}

impl fmt::Debug for SearchThreads {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SearchThreads")
            .field("count", &self.count())
            .field("spawned_threads", &self.spawned_threads.len())
            .finish_non_exhaustive()
    }
}

/// One search for a spawned thread: the shared state it reads, and its own data.
///
/// [`search`] keeps both alive, and lends the data to this thread alone.
struct Job {
    shared: NonNull<Shared<'static>>,
    data: NonNull<ThreadData>,
    id: usize,
}

// SAFETY: `Shared` is `Sync` and `ThreadData` is `Send`; `search` keeps both alive and the
// data unaliased for as long as the spawned thread uses them.
unsafe impl Send for Job {}

/// A thread spawned to search alongside the calling thread, waiting for jobs.
struct SpawnedThread {
    thread: JobThread<Job>,
    /// One message per job finished, even by panicking. Closed if the thread is gone.
    done: Receiver<()>,
}

impl SpawnedThread {
    fn spawn(id: usize) -> Option<Self> {
        let (finished, done) = mpsc::channel();
        let thread = JobThread::spawn(format!("search thread {id}"), move |job: Job| {
            // Dropped last, once the worker is done with what the job lends it.
            let _done = SendOnDrop::new(finished.clone(), ());
            // SAFETY: `search` keeps `shared` alive until it hears from `finished`.
            let shared = unsafe { job.shared.as_ref() };
            // SAFETY: `search` keeps `data` alive the same way, and lends it to this thread alone.
            let data = unsafe { &mut *job.data.as_ptr() };
            Worker::new(shared, data, job.id, None).iterative_deepening(&mut |_| {});
        })
        .ok()?;
        Some(Self { thread, done })
    }

    /// Hands `job` to the thread. Returns whether it took it.
    fn start(&self, job: Job) -> bool {
        self.thread.send(job).is_ok()
    }
}

/// When dropped, even by a panic, stops the search and waits for every spawned thread given a job.
///
/// The spawned threads use what the search borrows until they are done.
struct SpawnedThreadsGuard<'a> {
    control: &'a Control,
    started: Vec<&'a SpawnedThread>,
}

impl Drop for SpawnedThreadsGuard<'_> {
    fn drop(&mut self) {
        self.control.stop();
        for spawned in &self.started {
            // An error means the spawned thread is gone, which only reduces speed.
            _ = spawned.done.recv();
        }
    }
}

/// Signals from outside a running search.
#[derive(Debug, Default)]
pub struct Control {
    stop: AtomicBool,
    ponder: AtomicBool,
    /// When the opponent played the pondered move, which the clock runs from.
    ponderhit_at: OnceLock<Instant>,
}

impl Control {
    /// Controls for a search.
    ///
    /// If `ponder`, it starts by pondering: thinking on the opponent's time, without a clock.
    #[must_use]
    pub fn new(ponder: bool) -> Self {
        Self {
            stop: AtomicBool::new(false),
            ponder: AtomicBool::new(ponder),
            ponderhit_at: OnceLock::new(),
        }
    }

    /// Ends the search as soon as it has a move.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// The opponent played the pondered move at timestamp `at`.
    pub fn ponderhit(&self, at: Instant) {
        _ = self.ponderhit_at.set(at);
        self.ponder.store(false, Ordering::Relaxed);
    }

    /// When the clock started running for a search that began by pondering.
    /// If search did not begin by pondering returns `None`.
    /// See [`Control::ponderhit`].
    #[must_use]
    pub fn ponderhit_at(&self) -> Option<Instant> {
        self.ponderhit_at.get().copied()
    }

    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn is_pondering(&self) -> bool {
        self.ponder.load(Ordering::Relaxed)
    }
}

/// Progress after each finished iteration, or the line an unfinished one switched to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Info {
    /// Plies the reported iteration searched to, which is unfinished if `lowerbound` is set.
    pub depth: u32,
    /// Seldepth (selective depth): deepest ply reached, quiescence search included.
    pub seldepth: u32,
    /// The score of `pv`, from the point of view of the side to move.
    pub score: Score,
    /// Whether `score` only bounds the line from below.
    pub lowerbound: bool,
    /// Nodes searched by all threads.
    pub nodes: u64,
    /// Time elapsed since the search began, including any time spent pondering.
    pub time: Duration,
    /// Hashfull: per mille of the TT (transposition table) used by this search.
    pub hashfull: usize,
    /// PV (principal variation): the line expected, starting with the best move.
    pub pv: Vec<Move>,
}

impl Info {
    /// NPS (nodes per second).
    #[must_use]
    pub fn nps(&self) -> u64 {
        let micros = self.time.as_micros().max(1);
        u64::try_from(u128::from(self.nodes) * 1_000_000 / micros).unwrap_or(u64::MAX)
    }
}

/// UCI notation: `info depth <d> seldepth <d> ... pv <moves>`.
impl fmt::Display for Info {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "info depth {} seldepth {} multipv 1 score {}{} nodes {} nps {} hashfull {} time {} pv",
            self.depth,
            self.seldepth,
            self.score,
            if self.lowerbound { " lowerbound" } else { "" },
            self.nodes,
            self.nps(),
            self.hashfull,
            self.time.as_millis(),
        )?;
        for mv in &self.pv {
            write!(f, " {mv}")?;
        }
        Ok(())
    }
}

/// What a search found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Outcome {
    /// `None` only when there is no legal move.
    pub best: Option<Move>,
    /// The reply expected to `best`, to search on the opponent's time.
    pub ponder: Option<Move>,
    /// `None` if no iteration finished, which only happens without a legal move.
    pub score: Option<Score>,
    /// Plies of the last finished iteration.
    pub depth: u32,
}

/// Searches `pos` as `request` asks on every thread of `threads`,
/// the calling one included, calling `report` after each iteration.
///
/// `history` holds the keys of the positions played before `pos`, to
/// recognize repetitions; it may also end with the key of `pos` itself.
///
/// Returns once a limit is reached or `control` stops it, but never before it has a move.
/// Without a limit, it runs until stopped or [`MAX_DEPTH`] is searched.
#[expect(clippy::missing_panics_doc, reason = "`SearchThreads` always has the calling thread")]
pub fn search(
    pos: &Position,
    history: &[u64],
    request: &Request,
    threads: &mut SearchThreads,
    tt: &mut TranspositionTable,
    control: &Control,
    report: &mut dyn FnMut(&Info),
) -> Outcome {
    let start = request.start.unwrap_or_else(Instant::now);
    let limits = &request.limits;
    let legal = pos.legal_moves();
    if legal.is_empty() {
        return Outcome { best: None, ponder: None, score: None, depth: 0 };
    }
    let mut root_moves = MoveList::new();
    for &mv in legal.iter().filter(|mv| request.searchmoves.contains(mv)) {
        root_moves.push(mv);
    }
    // If no searchmove is legal, every legal move is searched, as UCI specifies.
    if root_moves.is_empty() {
        root_moves = legal.clone();
    }
    let restricted = root_moves.len() < legal.len();

    tt.new_search();
    threads.untouched = false;
    let tt = &*tt;
    // If `ponderhit` arrived before the search began, the clock has been running since then.
    let clock_start = control.ponderhit_at().unwrap_or(start);
    let time = TimeManager::new(
        limits,
        request.move_overhead,
        pos.side_to_move(),
        clock_start,
        root_moves.len() == 1,
    );
    let nodes: Vec<NodeCounter> = (0..threads.count()).map(|_| NodeCounter::default()).collect();
    let shared = Shared {
        root: pos,
        history,
        root_moves: &root_moves,
        restricted,
        limits,
        tt,
        control,
        nodes: &nodes,
        start,
    };
    let SearchThreads { data, spawned_threads, .. } = threads;
    let (main_data, spawned_data) =
        data.split_first_mut().expect("there should be at least one search thread");

    // Declared after `shared` and the data, so it is dropped before them
    let mut guard =
        SpawnedThreadsGuard { control, started: Vec::with_capacity(spawned_threads.len()) };
    let shared_ptr = NonNull::from(&shared).cast::<Shared<'static>>();
    for ((id, spawned), data) in (1..).zip(spawned_threads.iter()).zip(spawned_data) {
        if spawned.start(Job { shared: shared_ptr, data: NonNull::from(data), id }) {
            guard.started.push(spawned);
        }
    }
    let mut main = Worker::new(&shared, main_data, 0, Some(time));
    main.iterative_deepening(report);
    let outcome = main.outcome();
    // The spawned threads run until stopped.
    // One that panicked only reduced speed: the main thread's result stands.
    drop(guard);
    outcome
}

#[cfg(test)]
mod tests {
    use std::{panic, thread};

    use super::*;
    use crate::positions::{FOOLS_MATE, KIWIPETE, STARTPOS};

    /// Runs `f` on a thread with the stack [`search`] needs
    fn on_search_stack<T: Send>(f: impl FnOnce() -> T + Send) -> T {
        thread::scope(|scope| {
            let searcher = thread::Builder::new().stack_size(STACK_SIZE).spawn_scoped(scope, f);
            searcher
                .expect("the search thread should start")
                .join()
                .unwrap_or_else(|e| panic::resume_unwind(e))
        })
    }

    /// Searches `fen` within `limits` on `threads` threads, returning the outcome and the reports.
    fn search_fen(fen: &str, limits: &Limits, threads: usize) -> (Outcome, Vec<Info>) {
        search_request(fen, &Request { limits: *limits, ..Request::default() }, threads)
    }

    fn search_request(fen: &str, request: &Request, threads: usize) -> (Outcome, Vec<Info>) {
        let pos = Position::from_fen(fen).expect("FEN should be valid");
        let mut tt = TranspositionTable::new(4);
        let mut threads = SearchThreads::new(threads);
        let control = Control::default();
        on_search_stack(|| {
            let mut infos = Vec::new();
            let tt = &mut tt;
            let outcome = search(&pos, &[], request, &mut threads, tt, &control, &mut |info| {
                infos.push(info.clone());
            });
            (outcome, infos)
        })
    }

    fn depth(depth: u32) -> Limits {
        Limits { depth: Some(depth), ..Limits::default() }
    }

    fn best(fen: &str, limits: &Limits) -> (String, Score) {
        let (outcome, _) = search_fen(fen, limits, 1);
        (
            outcome.best.expect("search should find a move").to_string(),
            outcome.score.expect("search should give a score"),
        )
    }

    #[test]
    fn reports_every_iteration() {
        let (outcome, infos) = search_fen(STARTPOS, &depth(5), 1);
        assert_eq!(infos.iter().map(|i| i.depth).collect::<Vec<_>>(), [1, 2, 3, 4, 5]);
        let last = infos.last().expect("search should report at least once");
        assert_eq!(outcome.best, last.pv.first().copied());
        assert_eq!(outcome.depth, 5);
        assert!(last.pv.len() >= 2 && outcome.ponder == Some(last.pv[1]), "{last}");
        assert!(last.nodes > 0 && last.seldepth >= 5);
        let line = last.to_string();
        assert!(line.starts_with("info depth 5 seldepth "), "{line}");
        assert!(line.contains(" multipv 1 score cp "), "{line}");
        // The principal variation is a legal line.
        let mut pos = Position::startpos();
        for mv in &last.pv {
            assert!(pos.legal_moves().contains(mv), "{line}");
            pos.make_move(*mv);
        }
    }

    #[test]
    fn finds_mate_in_one() {
        assert_eq!(
            best("6k1/5ppp/8/8/8/8/8/R5K1 w - - 0 1", &depth(3)),
            ("a1a8".into(), Score::mate_in(1))
        );
    }

    /// Nf6+ gxf6 Bxf7#.
    const MATE_IN_TWO: &str = "r2qkb1r/pp2nppp/3p4/2pNN1B1/2BnP3/3P4/PPP2PPP/R2bK2R w KQkq - 1 1";

    /// Whether playing `pv` from `fen` ends in checkmate.
    fn mates(fen: &str, pv: &[Move]) -> bool {
        let mut pos = Position::from_fen(fen).expect("FEN should be valid");
        for mv in pv {
            assert!(pos.legal_moves().contains(mv), "{mv} in {}", pos.to_fen());
            pos.make_move(*mv);
        }
        pos.in_check() && pos.legal_moves().is_empty()
    }

    #[test]
    fn finds_mate_in_two() {
        let (outcome, infos) = search_fen(MATE_IN_TWO, &depth(6), 1);
        assert_eq!(outcome.score, Some(Score::mate_in(3)));
        let pv = &infos.last().expect("search should report at least once").pv;
        assert_eq!(pv.len(), 3);
        assert!(mates(MATE_IN_TWO, pv));
    }

    #[test]
    fn finds_a_quiet_sacrifice_in_zugzwang() {
        // Morphy: Ra6, and every Black move loses, bxa6 to b7 and a bishop move to Rxa7.
        // The quiet rook move is ordered late, so reductions must not bury it.
        let fen = "kbK5/pp6/1P6/8/8/8/8/R7 w - - 0 1";
        let (outcome, infos) = search_fen(fen, &depth(8), 1);
        assert_eq!(outcome.best.map(|mv| mv.to_string()), Some("a1a6".into()));
        assert_eq!(outcome.score, Some(Score::mate_in(3)));
        assert!(mates(fen, &infos.last().expect("search should report at least once").pv));
    }

    #[test]
    fn sees_being_mated() {
        // Only the a-pawn can move, and then Qxh2 mates.
        let (_, score) = best("6k1/8/8/8/8/5n1q/P4P1P/7K w - - 0 1", &depth(4));
        assert_eq!(score, Score::mated_in(2));
    }

    #[test]
    fn wins_material() {
        // A knight against a queen, until the knight takes it and the king takes back: even.
        let (mv, score) = best("3qk3/4p3/2N5/8/8/8/4P3/4K3 w - - 0 1", &depth(4));
        assert_eq!(mv, "c6d8");
        assert!(score > Score::new(-100), "{score}");
        assert_eq!(best("4k3/8/8/3q4/8/8/8/3RK3 w - - 0 1", &depth(4)).0, "d1d5");
    }

    #[test]
    fn prefers_mate_to_stalemate() {
        // Qg2, Qf7 and others stalemate; Qf8 mates.
        let (mv, score) = best("7k/8/6K1/8/8/8/5Q2/8 w - - 0 1", &depth(4));
        assert_eq!((mv.as_str(), score), ("f2f8", Score::mate_in(1)));
    }

    #[test]
    fn no_legal_move_gives_no_move() {
        let (outcome, infos) = search_fen(FOOLS_MATE, &depth(3), 1);
        assert_eq!(outcome, Outcome { best: None, ponder: None, score: None, depth: 0 });
        assert_eq!(infos, []);
    }

    #[test]
    fn searches_only_the_searchmoves() {
        let pos = Position::startpos();
        let a3 = pos.parse_uci_move("a2a3").expect("move should be legal");
        let request = Request { searchmoves: vec![a3], limits: depth(4), ..Request::default() };
        let (outcome, _) = search_request(STARTPOS, &request, 1);
        assert_eq!(outcome.best, Some(a3));
    }

    #[test]
    fn stopped_before_starting_still_has_a_move() {
        let pos = Position::from_fen(KIWIPETE).expect("FEN should be valid");
        let control = Control::default();
        control.stop();
        let mut tt = TranspositionTable::new(1);
        let mut threads = SearchThreads::new(2);
        let outcome = on_search_stack(|| {
            search(&pos, &[], &Request::default(), &mut threads, &mut tt, &control, &mut |_| {})
        });
        assert!(pos.legal_moves().contains(&outcome.best.expect("search should find a move")));
        assert_eq!(outcome.depth, 1);
    }

    #[test]
    fn node_limit_ends_the_search() {
        let limits = Limits { nodes: Some(20_000), ..Limits::default() };
        let (outcome, infos) = search_fen(KIWIPETE, &limits, 1);
        assert!(outcome.best.is_some());
        let last = infos.last().expect("search should report at least once");
        assert!(last.nodes < 200_000, "{last}");
    }

    #[test]
    fn movetime_ends_the_search() {
        let limits = Limits { movetime: Some(Duration::from_millis(50)), ..Limits::default() };
        let start = Instant::now();
        let (outcome, _) = search_fen(KIWIPETE, &limits, 1);
        assert!(outcome.best.is_some());
        assert!(start.elapsed() < Duration::from_secs(2), "{:?}", start.elapsed());
    }

    #[test]
    fn the_clock_runs_from_when_go_arrived() {
        // A `go` that waited behind earlier jobs past its whole budget still gets its move, from
        // one iteration.
        let Some(start) = Instant::now().checked_sub(Duration::from_secs(10)) else { return };
        let clock = Some(Duration::from_secs(2));
        let limits = Limits { time: [clock; 2], ..Limits::default() };
        let request = Request { limits, start: Some(start), ..Request::default() };
        let (outcome, infos) = search_request(KIWIPETE, &request, 1);
        assert!(outcome.best.is_some());
        assert_eq!(infos.len(), 1);
        assert!(infos[0].time >= Duration::from_secs(10));
    }

    #[test]
    fn a_ponderhit_before_the_search_starts_the_clock_then() {
        // `go ponder` waited 10 s, the opponent's time, before the ponderhit just now: the
        // clock runs from the ponderhit, so there is time for more than one iteration.
        let Some(start) = Instant::now().checked_sub(Duration::from_secs(10)) else { return };
        let pos = Position::from_fen(KIWIPETE).expect("FEN should be valid");
        let clock = Some(Duration::from_secs(2));
        let limits = Limits { time: [clock; 2], ..Limits::default() };
        let request = Request { limits, start: Some(start), ..Request::default() };
        let control = Control::new(true);
        control.ponderhit(Instant::now());
        let mut tt = TranspositionTable::new(1);
        let mut threads = SearchThreads::new(1);
        let mut iterations = 0;
        let tt = &mut tt;
        on_search_stack(|| {
            search(&pos, &[], &request, &mut threads, tt, &control, &mut |_| iterations += 1)
        });
        assert!(iterations > 1, "{iterations}");
    }

    #[test]
    fn mate_limit_ends_the_search() {
        let limits = Limits { mate: Some(1), ..Limits::default() };
        let (outcome, infos) = search_fen("6k1/5ppp/8/8/8/8/8/R5K1 w - - 0 1", &limits, 1);
        assert_eq!(outcome.score, Some(Score::mate_in(1)));
        assert!(infos.len() < 10, "{}", infos.len());
    }

    #[test]
    fn threads_agree_on_a_forced_mate() {
        let (outcome, infos) = search_fen(MATE_IN_TWO, &depth(8), 4);
        assert_eq!(outcome.score, Some(Score::mate_in(3)));
        assert!(mates(MATE_IN_TWO, &infos.last().expect("search should report at least once").pv));
    }

    #[test]
    fn threads_finish_a_busy_search_with_a_legal_line() {
        let (outcome, infos) = search_fen(KIWIPETE, &depth(7), 3);
        assert_eq!(outcome.depth, 7);
        let mut pos = Position::from_fen(KIWIPETE).expect("FEN should be valid");
        for mv in &infos.last().expect("search should report at least once").pv {
            assert!(pos.legal_moves().contains(mv));
            pos.make_move(*mv);
        }
    }

    #[test]
    fn is_bounded_only_with_a_limit() {
        let white = Color::White;
        assert!(!Limits::default().is_bounded(white));
        assert!(depth(3).is_bounded(white));
        assert!(!Limits { infinite: true, ..depth(3) }.is_bounded(white));
        assert!(!Limits { mate: Some(2), ..Limits::default() }.is_bounded(white));
        // Only the clock of the side to move limits the search.
        let clock = Limits { time: [None, Some(Duration::from_secs(1))], ..Limits::default() };
        assert!(clock.is_bounded(Color::Black));
        assert!(!clock.is_bounded(white));
    }

    #[test]
    fn search_threads_keep_their_history_until_cleared() {
        let mut threads = SearchThreads::new(3);
        assert_eq!(threads.count(), 3);
        assert_eq!(SearchThreads::new(0).count(), 1);
        let pos = Position::from_fen(KIWIPETE).expect("FEN should be valid");
        let mut tt = TranspositionTable::new(1);
        let control = Control::default();
        let request = Request { limits: depth(4), ..Request::default() };
        let tt = &mut tt;
        on_search_stack(|| search(&pos, &[], &request, &mut threads, tt, &control, &mut |_| {}));
        // The spawned threads may barely start before the main thread is done.
        assert!(!threads.data[0].history.is_empty());
        // The same count keeps them; another makes them anew.
        threads.resize(3);
        assert!(!threads.data[0].history.is_empty());
        threads.clear();
        assert!(threads.data.iter().all(|data| data.history.is_empty()));
        threads.resize(2);
        assert_eq!((threads.count(), threads.spawned_threads.len()), (2, 1));
    }

    #[test]
    fn info_formats_as_uci() {
        let pos = Position::startpos();
        let info = Info {
            depth: 3,
            seldepth: 7,
            score: Score::mate_in(5),
            lowerbound: false,
            nodes: 3000,
            time: Duration::from_millis(1500),
            hashfull: 12,
            pv: vec![pos.parse_uci_move("e2e4").expect("move should be legal")],
        };
        assert_eq!(
            info.to_string(),
            "info depth 3 seldepth 7 multipv 1 score mate 3 nodes 3000 nps 2000 hashfull 12 \
             time 1500 pv e2e4"
        );
        let bound = Info { score: Score::new(40), lowerbound: true, ..info };
        assert!(bound.to_string().contains(" score cp 40 lowerbound nodes "), "{bound}");
    }
}
