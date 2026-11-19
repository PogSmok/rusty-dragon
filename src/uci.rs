//! UCI (Universal Chess Interface): communicating with a chess GUI over standard input and output.
//!
//! Specification: <https://www.wbec-ridderkerk.nl/html/UCIProtocol.html>

use std::fmt;
use std::io::{self, BufRead, Write};
use std::mem;
use std::ops::ControlFlow;
use std::panic;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::board::{FenError, Move, Position};
use crate::search::{
    self, Control, JobThread, Outcome, Request, SearchThreads, SendOnDrop, TranspositionTable,
};
use crate::{ENGINE_AUTHOR, ENGINE_NAME, ENGINE_VERSION, eval, perft};

mod command;

pub use command::{Command, Go, ParseError};

/// UTF-8 BOM (byte order mark), which some GUIs (graphical user interfaces) send first.
const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// Reads commands from the input `open_input` returns until `quit` or the end of input, writing
/// replies to `output`.
///
/// Invalid commands are reported to the GUI and skipped.
/// `quit` stops a running `go perft`; the end of input lets it finish.
///
/// # Errors
///
/// Returns an error only if reading or writing fails, or the search thread cannot start.
#[expect(clippy::missing_panics_doc, reason = "the session keeps a sender alive")]
pub fn run<R: BufRead>(
    open_input: impl FnOnce() -> R + Send + 'static,
    output: impl Write,
) -> io::Result<()> {
    let (events, inbox) = mpsc::channel();
    let reader = events.clone();
    // Detached: after `quit` it may stay blocked on input, and it ends with the process.
    thread::spawn(move || {
        let reader = EndOnPanic(reader);
        read_lines(open_input(), &reader.0);
    });

    // Dropping the session, on any return, stops a perft that's still running.
    let mut session = Session::new(output, events)?;
    writeln!(session.out, "{ENGINE_NAME} {ENGINE_VERSION} by {ENGINE_AUTHOR}")?;
    session.out.flush()?;

    loop {
        let event = inbox.recv().expect("the session should keep a sender alive");
        // How the session ends, and whether a running perft is stopped or left to finish.
        let end = match event {
            Event::Line(line, received) => {
                match session.execute(&String::from_utf8_lossy(&line), received)? {
                    ControlFlow::Continue(()) => None,
                    // `quit` asks to exit as soon as possible.
                    ControlFlow::Break(()) => Some((Ok(()), true)),
                }
            }
            Event::BadInput(e) => {
                session.send_info(format_args!("unreadable input: {e}"))?;
                None
            }
            Event::PerftDone(id) => {
                session.finish_perft(id)?;
                None
            }
            Event::SearchUpdate => {
                session.poll_search()?;
                None
            }
            // A piped script expects its last perft to be counted,
            // and its last search to end at the limit it was given.
            // A search without a limit that ends it, such as `go infinite`, is stopped at once.
            Event::InputEnd(result) => Some((result, false)),
        };
        if let Some((result, stop)) = end {
            // Every `go` gets its answer, however the session ends.
            session.end_perft(stop)?;
            let stop_search =
                stop || session.search.as_ref().is_some_and(RunningSearch::is_endless);
            session.end_search(stop_search)?;
            session.release_pending()?;
            session.out.flush()?;
            return result;
        }
        session.out.flush()?;
    }
}

/// What the session loop waits for.
#[derive(Debug)]
enum Event {
    /// A line of input, not yet decoded, and when it was read.
    Line(Vec<u8>, Instant),
    /// Input that couldn't be read, such as text a Windows console can't convert to UTF-8.
    ///
    /// Reading carries on after it.
    BadInput(io::Error),
    /// The input ended (`Ok`), or reading it failed with an unrecoverable error (`Err`).
    InputEnd(io::Result<()>),
    /// The perft thread with this id is about to exit.
    PerftDone(u64),
    /// The search thread sent something, or exited.
    SearchUpdate,
}

/// Ends the session if the input thread panics.
#[derive(Debug)]
struct EndOnPanic(Sender<Event>);

impl Drop for EndOnPanic {
    fn drop(&mut self) {
        if thread::panicking() {
            let error = io::Error::other("the input thread panicked");
            _ = self.0.send(Event::InputEnd(Err(error)));
        }
    }
}

/// Unreadable chunks in a row after which the input is taken to be broken for good, rather
/// than reporting the same error forever.
const MAX_BAD_READS: u32 = 16;

/// Sends each line of `input` until it ends or the session is gone.
fn read_lines(mut input: impl BufRead, events: &Sender<Event>) {
    let mut line = Vec::new();
    let mut first = true;
    let mut bad_reads = 0;
    let end = loop {
        let read = input.read_until(b'\n', &mut line);
        if read.is_ok() {
            bad_reads = 0;
        }
        match read {
            // What's left after a read error at the very end is still sent.
            Ok(0) if line.is_empty() => break Ok(()),
            Ok(_) => {
                let mut bytes = mem::take(&mut line);
                // Ignore byte order marks
                if mem::take(&mut first) && bytes.starts_with(UTF8_BOM) {
                    bytes.drain(..UTF8_BOM.len());
                }
                if events.send(Event::Line(bytes, Instant::now())).is_err() {
                    return;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::InvalidData && bad_reads < MAX_BAD_READS => {
                bad_reads += 1;
                if events.send(Event::BadInput(e)).is_err() {
                    return;
                }
            }
            Err(e) => break Err(e),
        }
    };
    _ = events.send(Event::InputEnd(end));
}

/// State kept between commands.
#[derive(Debug)]
struct Session<W> {
    /// Where replies to the GUI are written.
    out: W,
    /// The position set by the last `position` command.
    position: Position,
    /// Zobrist keys of the game's positions that can still be repeated, `position` included.
    history: Vec<u64>,
    /// The answer to a `go infinite` or `go ponder`, held back until the GUI asks for it.
    pending: Option<Pending>,
    /// `go perft` running on its own thread.
    perft: Option<RunningPerft>,
    /// Id for the next perft, to tell its `PerftDone` from older ones.
    next_perft_id: u64,
    /// The `go` the search thread is running.
    search: Option<RunningSearch>,
    /// The search thread, which runs every `go` in turn.
    searcher: Searcher,
    /// Lets the perft and search threads wake the session loop.
    events: Sender<Event>,
    /// Defined by the `Move Overhead` option. Subtracted from the time allotted to each move.
    move_overhead: Duration,
}

impl<W> Drop for Session<W> {
    fn drop(&mut self) {
        if let Some(perft) = &self.perft {
            perft.stop.store(true, Ordering::Relaxed);
        }
        if let Some(search) = &self.search {
            search.control.stop();
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    best: Option<Move>,
    ponder: Option<Move>,
    /// If true, only `stop` sends the held-back `bestmove`; `ponderhit` does not.
    infinite: bool,
}

/// Move overhead bounds and default, in ms (milliseconds).
const DEFAULT_MOVE_OVERHEAD_MS: u64 = 30;
const MAX_MOVE_OVERHEAD_MS: u64 = 5000;
/// Largest `Hash`, in MiB (mebibytes).
const MAX_HASH_MIB: usize = 1 << 16;

/// Work for the search thread, done in the order sent.
#[derive(Debug)]
enum SearchJob {
    /// Run this search.
    ///
    /// Boxed to limit the size of `SearchJob`.
    Search(Box<SearchTask>),
    /// Forget everything learned: the transposition table and each thread's own tables.
    NewGame,
    /// Empty the transposition table.
    ClearHash,
    /// Resize the transposition table to this many MiB.
    Hash(usize),
    /// Search on this many threads from now on.
    Threads(usize),
    /// Answer with [`Reply::Ready`], once every earlier job is done.
    Ready,
}

/// One `go`: everything the search thread needs to run it.
#[derive(Debug)]
struct SearchTask {
    /// The position to search.
    position: Position,
    /// Zobrist keys of the game's positions that can still be repeated, `position` included.
    history: Vec<u64>,
    /// Which root moves to search, and when to stop.
    request: Request,
    /// Signals from the session while the search runs, such as `stop` and `ponderhit`.
    control: Arc<Control>,
}

/// What the search thread sends to the session.
#[derive(Debug)]
enum Reply {
    /// A line of output, such as `info depth ...`.
    Info(String),
    /// The search ended. Nothing more of it follows.
    Done(Outcome),
    /// The search ended without an outcome due to an error.
    ///
    /// A waiting session also sees this when the thread is gone.
    Failed,
    /// Every job before [`SearchJob::Ready`] is done.
    Ready,
}

/// The search thread, and how it answers.
#[derive(Debug)]
struct Searcher {
    thread: JobThread<SearchJob>,
    replies: Receiver<Reply>,
}

impl Searcher {
    /// Starts the thread, which wakes the session through `events` whenever it replies.
    fn spawn(events: Sender<Event>) -> io::Result<Self> {
        let (reply_to, replies) = mpsc::channel();
        let reply = move |reply: Reply| {
            _ = reply_to.send(reply);
            _ = events.send(Event::SearchUpdate);
        };
        let mut engine = Engine::new();
        let thread = JobThread::spawn("search".into(), move |job| engine.run(job, &reply))?;
        Ok(Self { thread, replies })
    }
}

/// What the search thread keeps from one search to the next.
struct Engine {
    tt: TranspositionTable,
    /// The table's size in MiB.
    hash_mib: usize,
    threads: SearchThreads,
}

impl Engine {
    fn new() -> Self {
        let hash_mib = TranspositionTable::DEFAULT_MIB;
        Self { tt: TranspositionTable::new(hash_mib), hash_mib, threads: SearchThreads::new(1) }
    }

    fn run(&mut self, job: SearchJob, reply: &impl Fn(Reply)) {
        match job {
            SearchJob::Search(task) => {
                let SearchTask { position, history, request, control } = *task;
                let mut report = |info: &search::Info| reply(Reply::Info(info.to_string()));
                let outcome = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                    let (threads, tt) = (&mut self.threads, &mut self.tt);
                    search::search(
                        &position,
                        &history,
                        &request,
                        threads,
                        tt,
                        &control,
                        &mut report,
                    )
                }));
                reply(outcome.map_or(Reply::Failed, Reply::Done));
            }
            SearchJob::NewGame => {
                self.tt.clear();
                self.threads.clear();
            }
            SearchJob::ClearHash => self.tt.clear(),
            SearchJob::Hash(mib) => {
                if let Some(size) = self.resize_hash(mib) {
                    let warning = format!(
                        "info string not enough memory for {mib} MiB of hash, using {size}"
                    );
                    reply(Reply::Info(warning));
                }
            }
            SearchJob::Threads(count) => self.threads.resize(count),
            SearchJob::Ready => reply(Reply::Ready),
        }
    }

    /// Makes an empty table of `mib` MiB, unless the table is already that size.
    ///
    /// If `mib` does not fit, it tries the old size, then the smallest.
    /// Returns the size it made, if that is not `mib`.
    fn resize_hash(&mut self, mib: usize) -> Option<usize> {
        // GUIs resend the size before every game: the same one keeps the table.
        if mib == self.hash_mib {
            return None;
        }
        let old = self.hash_mib;
        // The old table is freed first, so both need not fit at once.
        self.tt = TranspositionTable::new(1);
        let (size, tt) = match TranspositionTable::try_new(mib) {
            Some(tt) => (mib, tt),
            None => match TranspositionTable::try_new(old) {
                Some(tt) => (old, tt),
                None => (1, TranspositionTable::new(1)),
            },
        };
        self.tt = tt;
        self.hash_mib = size;
        (size != mib).then_some(size)
    }
}

/// A `go` the search thread is running: how the session stops it, and how it answers.
#[derive(Debug)]
struct RunningSearch {
    /// Shared with the search, which `stop` and `ponderhit` reach through it.
    control: Arc<Control>,
    /// `go infinite`: the answer waits for `stop`, even if the search ends sooner.
    infinite: bool,
    /// Has a limit that ends it, such as a depth or the clock, once pondering is over.
    bounded: bool,
    /// Played if the search fails to give a move: the first searchmove, or the first legal move.
    fallback: Option<Move>,
}

impl RunningSearch {
    /// Whether it runs until told to stop: left alone at the end of input, it would never end.
    fn is_endless(&self) -> bool {
        self.control.is_pondering() || !self.bounded
    }
}

/// A `go perft` counting on its own thread.
#[derive(Debug)]
struct RunningPerft {
    /// Unique identifier used for `PerftDone`.
    id: u64,
    /// Set to end the count early.
    stop: Arc<AtomicBool>,
    /// The thread, which returns the count below each root move, or the error that ended it.
    handle: JoinHandle<Result<perft::Divide, perft::PerftError>>,
}

/// Error from a command that parsed but can't be carried out.
#[derive(Clone, PartialEq, Eq, Debug)]
enum PositionError<'a> {
    Fen(FenError),
    /// The move, in UCI notation, isn't legal where it's played.
    IllegalMove(&'a str),
}

impl fmt::Display for PositionError<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fen(e) => write!(f, "invalid FEN: {e}"),
            Self::IllegalMove(mv) => write!(f, "illegal move \"{mv}\""),
        }
    }
}

impl<W: Write> Session<W> {
    /// Fails if the search thread cannot start.
    fn new(out: W, events: Sender<Event>) -> io::Result<Self> {
        let position = Position::startpos();
        Ok(Self {
            out,
            history: vec![position.zobrist_key()],
            position,
            pending: None,
            perft: None,
            next_perft_id: 0,
            search: None,
            searcher: Searcher::spawn(events.clone())?,
            events,
            move_overhead: Duration::from_millis(DEFAULT_MOVE_OVERHEAD_MS),
        })
    }

    /// Carries out one line of input, read at `received`. Breaks on `quit`.
    fn execute(&mut self, line: &str, received: Instant) -> io::Result<ControlFlow<()>> {
        if line.trim().is_empty() {
            return Ok(ControlFlow::Continue(()));
        }
        let command = match Command::parse(line) {
            Ok(command) => command,
            Err(e) => {
                self.send_info(e)?;
                return Ok(ControlFlow::Continue(()));
            }
        };

        match command {
            Command::Uci => {
                writeln!(self.out, "id name {ENGINE_NAME} {ENGINE_VERSION}")?;
                writeln!(self.out, "id author {ENGINE_AUTHOR}")?;
                writeln!(
                    self.out,
                    "option name Hash type spin default {} min 1 max {MAX_HASH_MIB}",
                    TranspositionTable::DEFAULT_MIB
                )?;
                writeln!(
                    self.out,
                    "option name Threads type spin default 1 min 1 max {}",
                    search::MAX_THREADS
                )?;
                writeln!(
                    self.out,
                    "option name Move Overhead type spin default {DEFAULT_MOVE_OVERHEAD_MS} \
                     min 0 max {MAX_MOVE_OVERHEAD_MS}"
                )?;
                writeln!(self.out, "option name Ponder type check default false")?;
                writeln!(self.out, "option name Clear Hash type button")?;
                writeln!(self.out, "uciok")?;
            }
            // There is no debug output yet.
            Command::Debug(_) | Command::Register => {}
            Command::IsReady => {
                self.wait_until_ready()?;
                writeln!(self.out, "readyok")?;
            }
            Command::SetOption { name, value } => self.set_option(name, value)?,
            // A held-back answer is kept: every `go` gets its `bestmove` on `stop` at the latest.
            // A running search is left alone, and the table and history are cleared once it ends.
            Command::UciNewGame => {
                self.position = Position::startpos();
                self.history = vec![self.position.zobrist_key()];
                self.queue(SearchJob::NewGame)?;
            }
            Command::Position { fen, moves } => {
                if let Err(e) = self.set_position(fen, moves) {
                    self.send_info(e)?;
                }
            }
            Command::Go(go) => {
                self.end_perft(true)?;
                self.go(&go, received)?;
            }
            Command::Perft(depth) => {
                // Like `go`, a perft answers any earlier search first.
                self.end_search(true)?;
                self.release_pending()?;
                self.start_perft(depth)?;
            }
            Command::Stop => {
                self.end_perft(true)?;
                self.end_search(true)?;
                self.release_pending()?;
            }
            Command::PonderHit => {
                if let Some(search) = &self.search {
                    search.control.ponderhit(received);
                }
                if let Some(pending) = self.pending.take_if(|pending| !pending.infinite) {
                    self.send_bestmove(pending.best, pending.ponder)?;
                }
            }
            Command::Quit => return Ok(ControlFlow::Break(())),
            Command::Display => {
                writeln!(self.out, "{}\n\nFen: {}", self.position, self.position.to_fen())?;
            }
            Command::Eval => writeln!(self.out, "{}", eval::Breakdown::of(&self.position))?,
        }
        Ok(ControlFlow::Continue(()))
    }

    /// Sets up the position from `fen`, or the starting position if `None`, and plays `moves`.
    ///
    /// A bad FEN leaves the current position unchanged.
    ///  An illegal move ends the list but keeps the moves before it.
    fn set_position<'a>(
        &mut self,
        fen: Option<&str>,
        moves: &'a str,
    ) -> Result<(), PositionError<'a>> {
        self.position = match fen {
            None => Position::startpos(),
            Some(fen) => Position::from_fen(fen).map_err(PositionError::Fen)?,
        };
        self.history = vec![self.position.zobrist_key()];
        for uci in moves.split_whitespace() {
            let mv = self.position.parse_uci_move(uci).ok_or(PositionError::IllegalMove(uci))?;
            self.position.make_move(mv);
            // No position before a capture or pawn move can come again.
            if self.position.halfmove_clock() == 0 {
                self.history.clear();
            }
            self.history.push(self.position.zobrist_key());
        }
        Ok(())
    }

    /// Applies `setoption`. Names are matched ignoring case.
    fn set_option(&mut self, name: &str, value: Option<&str>) -> io::Result<()> {
        let is = |option: &str| name.eq_ignore_ascii_case(option);
        let value = value.unwrap_or("");
        let invalid = |session: &mut Self| {
            session.send_info(format_args!("invalid value \"{value}\" for option \"{name}\""))
        };
        // The table and the threads change on the search thread, after a running search.
        if is("Hash") {
            match value.parse::<usize>() {
                Ok(mib) if (1..=MAX_HASH_MIB).contains(&mib) => self.queue(SearchJob::Hash(mib))?,
                _ => invalid(self)?,
            }
        } else if is("Threads") {
            match value.parse::<usize>() {
                Ok(n) if (1..=search::MAX_THREADS).contains(&n) => {
                    self.queue(SearchJob::Threads(n))?;
                }
                _ => invalid(self)?,
            }
        } else if is("Move Overhead") {
            match value.parse::<u64>() {
                Ok(ms) if ms <= MAX_MOVE_OVERHEAD_MS => {
                    self.move_overhead = Duration::from_millis(ms);
                }
                _ => invalid(self)?,
            }
        } else if is("Ponder") {
            // The GUI decides when to ponder; the option only tells it the engine can.
            if !value.eq_ignore_ascii_case("true") && !value.eq_ignore_ascii_case("false") {
                invalid(self)?;
            }
        } else if is("Clear Hash") {
            self.queue(SearchJob::ClearHash)?;
        } else {
            self.send_info(format_args!("unknown option \"{name}\""))?;
        }
        Ok(())
    }

    /// Starts searching the current position on its own thread, after answering any earlier `go`.
    ///
    /// The clock runs from `received`.
    /// The answer comes when the search ends, or for `infinite` and `ponder`,
    /// when the GUI asks for it.
    fn go(&mut self, go: &Go<'_>, received: Instant) -> io::Result<()> {
        // The GUI should have sent `stop` first.
        self.end_search(true)?;
        self.release_pending()?;

        let legal = self.position.legal_moves();
        let mut searchmoves = Vec::new();
        for &uci in &go.searchmoves {
            match legal.find_uci(uci) {
                Some(mv) => searchmoves.push(mv),
                None => self.send_info(format_args!("ignoring illegal searchmove \"{uci}\""))?,
            }
        }
        // With no legal searchmove, every move is searched.
        let fallback = searchmoves.first().or_else(|| legal.first()).copied();
        let request = Request {
            searchmoves,
            limits: go.limits,
            move_overhead: self.move_overhead,
            start: Some(received),
        };
        self.start_search(request, go.ponder, fallback)
    }

    /// Hands `job` to the search thread, which only fails if the thread is gone.
    fn queue(&mut self, job: SearchJob) -> io::Result<()> {
        if self.searcher.thread.send(job).is_err() {
            self.send_info("the search thread is gone")?;
        }
        Ok(())
    }

    fn start_search(
        &mut self,
        request: Request,
        ponder: bool,
        fallback: Option<Move>,
    ) -> io::Result<()> {
        let control = Arc::new(Control::new(ponder));
        let infinite = request.limits.infinite;
        let bounded = request.limits.is_bounded(self.position.side_to_move());
        let job = SearchJob::Search(Box::new(SearchTask {
            position: self.position.clone(),
            history: self.history.clone(),
            request,
            control: Arc::clone(&control),
        }));

        // Every `go` gets its answer, even without a search.
        if self.searcher.thread.send(job).is_err() {
            self.send_info("could not start the search: the search thread is gone")?;
            if infinite || ponder {
                self.pending = Some(Pending { best: fallback, ponder: None, infinite });
                return Ok(());
            }
            return self.send_bestmove(fallback, None);
        }
        self.search = Some(RunningSearch { control, infinite, bounded, fallback });
        Ok(())
    }

    /// Prints the search thread's info lines until a reply of another kind comes, and returns it.
    ///
    /// Waits for that reply, or returns [`Reply::Failed`] if the thread is gone.
    fn recv_reply(&mut self) -> io::Result<Reply> {
        loop {
            match self.searcher.replies.recv() {
                Ok(Reply::Info(line)) => writeln!(self.out, "{line}")?,
                Ok(reply) => return Ok(reply),
                Err(mpsc::RecvError) => return Ok(Reply::Failed),
            }
        }
    }

    /// Like [`Self::recv_reply`], but returns `None` instead of waiting if that reply has not come.
    fn try_recv_reply(&mut self) -> io::Result<Option<Reply>> {
        loop {
            match self.searcher.replies.try_recv() {
                Ok(Reply::Info(line)) => writeln!(self.out, "{line}")?,
                Ok(reply) => return Ok(Some(reply)),
                Err(TryRecvError::Empty) => return Ok(None),
                Err(TryRecvError::Disconnected) => return Ok(Some(Reply::Failed)),
            }
        }
    }

    /// Waits for the search thread to carry out every earlier command.
    fn wait_until_ready(&mut self) -> io::Result<()> {
        if self.search.is_none() && self.searcher.thread.send(SearchJob::Ready).is_ok() {
            // No search runs, so no `Done` comes first.
            while !matches!(self.recv_reply()?, Reply::Ready | Reply::Failed) {}
        }
        Ok(())
    }

    /// Prints what the search thread sent, and the running search's answer if it has ended.
    fn poll_search(&mut self) -> io::Result<()> {
        match self.try_recv_reply()? {
            Some(Reply::Done(outcome)) => self.finish_search(Some(outcome), true),
            Some(Reply::Failed) if self.search.is_some() => self.finish_search(None, true),
            _ => Ok(()),
        }
    }

    /// Waits for the running search to end, if any, after telling it to `stop`, and answers it.
    fn end_search(&mut self, stop: bool) -> io::Result<()> {
        let Some(search) = &self.search else { return Ok(()) };
        if stop {
            search.control.stop();
        }
        let outcome = loop {
            match self.recv_reply()? {
                Reply::Done(outcome) => break Some(outcome),
                Reply::Failed => break None,
                Reply::Ready | Reply::Info(_) => {}
            }
        };
        self.finish_search(outcome, false)
    }

    /// Answers the search that just ended with `outcome`, which is `None` if it failed.
    ///
    /// With `hold`, keeps the answer for later, since the GUI has not asked for it yet.
    fn finish_search(&mut self, outcome: Option<Outcome>, hold: bool) -> io::Result<()> {
        let search = self.search.take().expect("a search should be running");
        let (best, ponder) = if let Some(outcome) = outcome {
            (outcome.best, outcome.ponder)
        } else {
            self.send_info("search failed")?;
            (search.fallback, None)
        };
        if hold && (search.infinite || search.control.is_pondering()) {
            self.pending = Some(Pending { best, ponder, infinite: search.infinite });
            Ok(())
        } else {
            self.send_bestmove(best, ponder)
        }
    }

    /// Sends the held-back `bestmove`, if any.
    fn release_pending(&mut self) -> io::Result<()> {
        match self.pending.take() {
            Some(pending) => self.send_bestmove(pending.best, pending.ponder),
            None => Ok(()),
        }
    }

    /// Counts on its own thread, so `isready` and `stop` are still answered.
    ///
    /// The result is printed once it's done.
    fn start_perft(&mut self, depth: u32) -> io::Result<()> {
        self.end_perft(true)?;
        let id = self.next_perft_id;
        self.next_perft_id += 1;
        let stop = Arc::new(AtomicBool::new(false));
        let handle = thread::spawn({
            let stop = Arc::clone(&stop);
            let position = self.position.clone();
            let notify = SendOnDrop::new(self.events.clone(), Event::PerftDone(id));
            move || {
                let _notify = notify;
                perft::parallel_divide(&position, depth, &stop)
            }
        });
        self.perft = Some(RunningPerft { id, stop, handle });
        Ok(())
    }

    /// Reports perft `id` if it's the one running.
    ///
    /// The thread sends `PerftDone` as it exits, so joining it doesn't wait.
    fn finish_perft(&mut self, id: u64) -> io::Result<()> {
        match self.perft.take_if(|perft| perft.id == id) {
            Some(perft) => self.report_perft(perft),
            None => Ok(()),
        }
    }

    /// Waits for the running perft, if any, after telling it to `stop`.
    fn end_perft(&mut self, stop: bool) -> io::Result<()> {
        match self.perft.take() {
            Some(perft) => {
                if stop {
                    perft.stop.store(true, Ordering::Relaxed);
                }
                self.report_perft(perft)
            }
            None => Ok(()),
        }
    }

    /// Waits for `perft`, then prints the count below each root move and the total.
    fn report_perft(&mut self, perft: RunningPerft) -> io::Result<()> {
        match perft.handle.join() {
            Ok(Ok(split)) => writeln!(self.out, "{split}"),
            Ok(Err(e)) => self.send_info(e),
            Err(_) => self.send_info("perft failed"),
        }
    }

    /// Sends `bestmove` (null move `0000` when there is no legal move) and the ponder move, if any.
    fn send_bestmove(&mut self, best: Option<Move>, ponder: Option<Move>) -> io::Result<()> {
        match (best, ponder) {
            (Some(mv), Some(reply)) => writeln!(self.out, "bestmove {mv} ponder {reply}"),
            (Some(mv), None) => writeln!(self.out, "bestmove {mv}"),
            (None, _) => writeln!(self.out, "bestmove 0000"),
        }
    }

    /// Sends a message for the GUI to show, which it does not otherwise interpret.
    ///
    /// Control characters become spaces, so an echoed input message cannot start a line of its own.
    fn send_info(&mut self, message: impl fmt::Display) -> io::Result<()> {
        let text: String =
            message.to_string().chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        writeln!(self.out, "info string {text}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::positions::{FOOLS_MATE, STARTPOS};

    /// Runs a session on `input` and returns its output, without the greeting line, the
    /// search's progress lines or its ponder moves, which vary from run to run.
    fn session(input: &str) -> String {
        let mut replies = String::new();
        for line in session_raw(input).lines() {
            if line.starts_with("info ") && !line.starts_with("info string ") {
                continue;
            }
            let line = match line.split_once(" ponder ") {
                Some((bestmove, _)) if line.starts_with("bestmove ") => bestmove,
                _ => line,
            };
            replies.push_str(line);
            replies.push('\n');
        }
        replies
    }

    /// Runs a session on `input` and returns all its output but the greeting line.
    fn session_raw(input: &str) -> String {
        session_bytes(input.as_bytes())
    }

    /// Like [`session`], for input that may not be valid UTF-8.
    fn session_bytes(input: &[u8]) -> String {
        let mut output = Vec::new();
        let input = input.to_vec();
        run(move || io::Cursor::new(input), &mut output)
            .expect("writing to a `Vec` should not fail");
        let output = String::from_utf8(output).expect("output should be UTF-8");
        let (greeting, replies) =
            output.split_once('\n').expect("output should start with a greeting line");
        assert_eq!(greeting, format!("{ENGINE_NAME} {ENGINE_VERSION} by {ENGINE_AUTHOR}"));
        replies.to_string()
    }

    #[test]
    fn handshake() {
        let out = session("uci\nisready\n");
        let expected_start =
            format!("id name {ENGINE_NAME} {ENGINE_VERSION}\nid author {ENGINE_AUTHOR}\n");
        assert!(out.starts_with(&expected_start), "{out}");
        assert!(out.ends_with("\nuciok\nreadyok\n"), "{out}");
        for option in
            ["Hash type spin", "Threads type spin", "Move Overhead", "Ponder", "Clear Hash"]
        {
            assert!(out.contains(&format!("\noption name {option}")), "{option}: {out}");
        }
    }

    #[test]
    fn quit_ends_the_session() {
        assert_eq!(session("isready\nquit\nisready\n"), "readyok\n");
    }

    #[test]
    fn handles_crlf_and_missing_final_newline() {
        assert_eq!(session("isready\r\n\r\nisready"), "readyok\nreadyok\n");
    }

    #[test]
    fn reports_bad_input_and_continues() {
        assert_eq!(session("foo\nisready\n"), "info string unknown command \"foo\"\nreadyok\n");
    }

    #[test]
    fn invalid_utf8_only_loses_the_bad_bytes() {
        assert_eq!(
            session_bytes(b"\xFF isready\nsetoption name B\xFCcher\n"),
            "readyok\ninfo string unknown option \"B\u{FFFD}cher\"\n"
        );
    }

    #[test]
    fn skips_a_byte_order_mark() {
        assert_eq!(session_bytes(b"\xEF\xBB\xBFisready\n"), "readyok\n");
    }

    #[test]
    fn position_with_moves() {
        let out = session("position startpos moves e2e4 e7e5 g1f3\nd\n");
        assert!(
            out.ends_with("Fen: rnbqkbnr/pppp1ppp/8/4p3/4P3/5N2/PPPP1PPP/RNBQKB1R b KQkq - 1 2\n")
        );
    }

    #[test]
    fn illegal_moves_keep_the_moves_before_them() {
        let out = session("position startpos moves e2e4 e7e5 e2e4 g1f3\nd\n");
        assert!(out.starts_with("info string illegal move \"e2e4\"\n"));
        assert!(
            out.ends_with("Fen: rnbqkbnr/pppp1ppp/8/4p3/4P3/8/PPPP1PPP/RNBQKBNR w KQkq - 0 2\n")
        );
    }

    #[test]
    fn bad_fen_keeps_the_previous_position() {
        let out = session("position startpos moves e2e4\nposition fen 8/8/8 w - - 0 1\nd\n");
        assert!(out.starts_with("info string invalid FEN: expected 8 ranks, got 3\n"));
        assert!(out.ends_with("Fen: rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1\n"));
    }

    #[test]
    fn eval_prints_the_breakdown() {
        let out = session("position startpos moves e2e4\neval\n");
        assert!(out.starts_with("Middlegame: "), "{out}");
        let mut pos = Position::startpos();
        pos.make_move(pos.parse_uci_move("e2e4").expect("move should be legal"));
        let expected = eval::evaluate(&pos);
        assert!(out.ends_with(&format!("Evaluation: {expected} (side to move)\n")), "{out}");
    }

    #[test]
    fn ucinewgame_resets_the_position() {
        let out = session("position startpos moves e2e4\nucinewgame\nd\n");
        assert!(out.ends_with(&format!("Fen: {STARTPOS}\n")));
    }

    #[test]
    fn go_answers_with_a_legal_move() {
        let out = session("position startpos\ngo wtime 1000 btime 1000\n");
        let mv =
            out.trim_end().strip_prefix("bestmove ").expect("output should end with a bestmove");
        assert!(Position::startpos().parse_uci_move(mv).is_some(), "{mv}");
    }

    #[test]
    fn go_respects_searchmoves() {
        let out = session("go depth 3 searchmoves a1a8 g1f3 e2e4\n");
        let (warning, bestmove) = out.split_once('\n').expect("output should have two lines");
        assert_eq!(warning, "info string ignoring illegal searchmove \"a1a8\"");
        assert!(["bestmove g1f3\n", "bestmove e2e4\n"].contains(&bestmove), "{out}");
    }

    #[test]
    fn go_reports_each_iteration_then_the_best_move() {
        let out = session_raw("position startpos moves e2e4\ngo depth 4\n");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 5, "{out}");
        for (depth, line) in lines[..4].iter().enumerate() {
            assert!(line.starts_with(&format!("info depth {} ", depth + 1)), "{line}");
        }
        // The answer is the last line's principal variation: its best move and the reply.
        let pv = lines[3].split_once(" pv ").expect("info should have a pv").1;
        let mut pv = pv.split(' ');
        let (best, reply) = (
            pv.next().expect("pv should have a best move"),
            pv.next().expect("pv should have a reply"),
        );
        assert_eq!(lines[4], format!("bestmove {best} ponder {reply}"));
    }

    #[test]
    fn go_finds_a_mate() {
        let out = session_raw("position fen 6k1/5ppp/8/8/8/8/8/R5K1 w - - 0 1\ngo depth 3\n");
        assert!(out.contains(" score mate 1 "), "{out}");
        assert!(out.ends_with("bestmove a1a8\n"), "{out}");
    }

    #[test]
    fn go_on_the_clock_answers_in_time() {
        let out = session("go wtime 300 btime 300 winc 0 binc 0\n");
        assert!(out.starts_with("bestmove "), "{out}");
        let out = session("go movetime 20\ngo nodes 500\ngo mate 1\n");
        assert_eq!(out.lines().filter(|line| line.starts_with("bestmove ")).count(), 3, "{out}");
    }

    #[test]
    fn ponderhit_puts_the_search_on_the_clock() {
        // After ponderhit, the clock ends the search by itself, without `stop`.
        assert_eq!(
            session("go ponder wtime 200 btime 200 searchmoves e2e4\nponderhit\n"),
            "bestmove e2e4\n"
        );
    }

    #[test]
    fn options_are_set_or_reported() {
        let out = session(
            "setoption name Hash value 4\nsetoption name threads value 3\n\
             setoption name Move Overhead value 0\nsetoption name Ponder value true\n\
             setoption name Clear Hash\ngo depth 5\n",
        );
        assert!(out.starts_with("bestmove "), "{out}");
        assert_eq!(
            session(
                "setoption name Hash value 0\nsetoption name Threads value lots\n\
                 setoption name Move Overhead value -1\nsetoption name Ponder value maybe\n"
            ),
            "info string invalid value \"0\" for option \"Hash\"\n\
             info string invalid value \"lots\" for option \"Threads\"\n\
             info string invalid value \"-1\" for option \"Move Overhead\"\n\
             info string invalid value \"maybe\" for option \"Ponder\"\n"
        );
    }

    #[test]
    fn the_game_history_counts_for_repetitions() {
        // A king and knight against everything: hopeless, but after the knights went out and
        // back twice, Nf3 puts the position after the first move on the board a third time.
        let fen = "rnbqkbnr/pppppppp/8/8/8/8/8/4K1N1 w kq - 0 1";
        let shuffle = "g1f3 g8f6 f3g1 f6g8";
        let search = |moves: &str| session_raw(&format!("position fen {fen}{moves}\ngo depth 4\n"));
        // Draws score a point off zero either way.
        let drawn = |out: &str| {
            let last_info = out
                .lines()
                .rev()
                .find(|line| line.starts_with("info depth"))
                .expect("output should have an info depth line");
            [" score cp -1 ", " score cp 0 ", " score cp 1 "].iter().any(|s| last_info.contains(s))
        };
        let out = search(&format!(" moves {shuffle} {shuffle}"));
        assert!(out.ends_with("bestmove g1f3\n") || out.contains("bestmove g1f3 "), "{out}");
        assert!(drawn(&out), "{out}");
        // Twice on the board is no draw yet, and Black need not repeat a third time.
        assert!(!drawn(&search(&format!(" moves {shuffle}"))));
        assert!(!drawn(&search("")));
    }

    #[test]
    fn only_the_clock_of_the_side_to_move_bounds_the_search() {
        // Black to move with only White's clock: a search without limit, stopped by the end of
        // the input instead of left to run.
        let out = session("position startpos moves e2e4\ngo wtime 1000\n");
        assert!(out.starts_with("bestmove "), "{out}");
    }

    #[test]
    fn go_without_legal_moves_plays_the_null_move() {
        assert_eq!(session(&format!("position fen {FOOLS_MATE}\ngo depth 1\n")), "bestmove 0000\n");
    }

    #[test]
    fn infinite_search_waits_for_stop() {
        assert_eq!(
            session("go infinite searchmoves e2e4\nisready\nponderhit\nstop\nstop\n"),
            "readyok\nbestmove e2e4\n"
        );
    }

    #[test]
    fn ponder_search_waits_for_ponderhit_or_stop() {
        assert_eq!(
            session("go ponder searchmoves e2e4\nisready\nponderhit\nponderhit\n"),
            "readyok\nbestmove e2e4\n"
        );
        assert_eq!(session("go ponder searchmoves d2d4\nstop\n"), "bestmove d2d4\n");
    }

    #[test]
    fn go_answers_a_pending_search_first() {
        assert_eq!(
            session("go infinite searchmoves e2e4\ngo searchmoves d2d4\n"),
            "bestmove e2e4\nbestmove d2d4\n"
        );
    }

    #[test]
    fn end_of_input_sends_the_held_back_answer() {
        assert_eq!(session("go infinite searchmoves e2e4\n"), "bestmove e2e4\n");
    }

    #[test]
    fn a_new_position_still_answers_the_pending_search() {
        assert_eq!(
            session("go infinite searchmoves e2e4\nucinewgame\nposition startpos\nstop\n"),
            "bestmove e2e4\n"
        );
    }

    #[test]
    fn table_and_thread_changes_wait_for_the_running_search() {
        // Sent mid-search, they neither stop it nor hold up `stop`, and the next search runs
        // with them.
        assert_eq!(
            session(
                "go infinite searchmoves e2e4\nsetoption name Hash value 2\n\
                 setoption name Threads value 2\nucinewgame\nsetoption name Clear Hash\n\
                 isready\nstop\nisready\ngo depth 3 searchmoves d2d4\n"
            ),
            "readyok\nbestmove e2e4\nreadyok\nbestmove d2d4\n"
        );
    }

    #[test]
    fn perft_divides_by_root_move() {
        let out = session("position startpos moves e2e4\ngo perft 2\n");
        assert_eq!(out.lines().count(), 20 + 2);
        assert!(out.contains("e7e5: 29\n"));
        assert!(out.ends_with("\nNodes searched: 600\n"));
        assert_eq!(session("go perft 0\n"), "\nNodes searched: 1\n");
    }

    #[test]
    fn perft_runs_while_commands_are_answered() {
        // Far too deep to finish before the next command is read.
        assert_eq!(session("go perft 9\nisready\nstop\n"), "readyok\ninfo string perft stopped\n");
        assert_eq!(
            session("go perft 9\ngo searchmoves e2e4\n"),
            "info string perft stopped\nbestmove e2e4\n"
        );
    }

    #[test]
    fn quit_stops_a_running_perft() {
        assert_eq!(session("go perft 9\nquit\n"), "info string perft stopped\n");
    }

    #[test]
    fn echoed_input_cant_forge_a_line() {
        assert_eq!(
            session("foo\rbestmove a1a1\n"),
            "info string unknown command \"foo bestmove a1a1\"\n"
        );
    }

    #[test]
    fn perft_reports_absurd_depths() {
        assert_eq!(
            session(&format!("go perft {}\n", perft::MAX_DEPTH + 1)),
            format!("info string {}\n", perft::PerftError::TooDeep)
        );
    }

    #[test]
    fn unknown_options_are_reported() {
        assert_eq!(
            session("setoption name Hashes value 64\n"),
            "info string unknown option \"Hashes\"\n"
        );
    }
}
