//! Universal Chess Interface
//! Specification: <https://www.wbec-ridderkerk.nl/html/UCIProtocol.html>

use std::fmt;
use std::io::{self, BufRead, Write};
use std::mem;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

use crate::board::{FenError, Move, Position};
use crate::{ENGINE_AUTHOR, ENGINE_NAME, ENGINE_VERSION, perft};

mod command;

pub use command::{Command, Go, ParseError};

const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// Reads commands from the input `open_input` returns until `quit` or the end of input, writing
/// replies to `output`. Fails only if reading or writing fails; bad commands are reported to
/// the GUI and skipped. `quit` stops a running `go perft`; the end of input lets it finish.
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
    let mut session = Session::new(output, events);
    writeln!(session.out, "{ENGINE_NAME} {ENGINE_VERSION} by {ENGINE_AUTHOR}")?;
    session.out.flush()?;

    loop {
        let event = inbox.recv().expect("the session keeps a sender alive");
        // How the session ends, and whether a running perft is stopped or left to finish.
        let end = match event {
            Event::Line(line) => match session.execute(&String::from_utf8_lossy(&line))? {
                ControlFlow::Continue(()) => None,
                // `quit` asks to exit as soon as possible.
                ControlFlow::Break(()) => Some((Ok(()), true)),
            },
            Event::BadInput(e) => {
                session.info(format_args!("unreadable input: {e}"))?;
                None
            }
            Event::PerftDone(id) => {
                session.finish_perft(id)?;
                None
            }
            // A piped script expects its last perft to be counted.
            Event::InputEnd(result) => Some((result, false)),
        };
        if let Some((result, stop)) = end {
            // Every `go` gets its answer, however the session ends.
            session.end_perft(stop)?;
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
    /// A line of input, not yet decoded.
    Line(Vec<u8>),
    /// Input that couldn't be read, such as text a Windows console can't convert to UTF-8.
    /// Reading carries on after it.
    BadInput(io::Error),
    /// The input ended, or reading it failed for good.
    InputEnd(io::Result<()>),
    /// The perft thread with this id is about to exit. Its result is only reported once: this
    /// may be a perft that was already stopped and reported.
    PerftDone(u64),
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
                if events.send(Event::Line(bytes)).is_err() {
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
    out: W,
    position: Position,
    /// The answer to a `go infinite` or `go ponder`, held back until the GUI asks for it.
    pending: Option<Pending>,
    /// `go perft` running on its own thread.
    perft: Option<RunningPerft>,
    /// Id for the next perft, to tell its `PerftDone` from older ones.
    next_perft_id: u64,
    /// Lets the perft thread wake the session loop.
    events: Sender<Event>,
}

/// A perft left running would keep every core busy after the session is gone.
impl<W> Drop for Session<W> {
    fn drop(&mut self) {
        if let Some(perft) = &self.perft {
            perft.stop.store(true, Ordering::Relaxed);
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    best: Option<Move>,
    /// Only `stop` releases the move, not `ponderhit`.
    infinite: bool,
}

#[derive(Debug)]
struct RunningPerft {
    id: u64,
    stop: Arc<AtomicBool>,
    handle: JoinHandle<Result<perft::Divide, perft::PerftError>>,
}

/// Sends `PerftDone` when the perft thread exits, even by panicking.
#[derive(Debug)]
struct NotifyOnExit {
    id: u64,
    events: Sender<Event>,
}

impl Drop for NotifyOnExit {
    fn drop(&mut self) {
        _ = self.events.send(Event::PerftDone(self.id));
    }
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
    fn new(out: W, events: Sender<Event>) -> Self {
        Self {
            out,
            position: Position::startpos(),
            pending: None,
            perft: None,
            next_perft_id: 0,
            events,
        }
    }

    /// Carries out one line of input. Breaks on `quit`.
    fn execute(&mut self, line: &str) -> io::Result<ControlFlow<()>> {
        if line.trim().is_empty() {
            return Ok(ControlFlow::Continue(()));
        }
        let command = match Command::parse(line) {
            Ok(command) => command,
            Err(e) => {
                self.info(e)?;
                return Ok(ControlFlow::Continue(()));
            }
        };

        match command {
            Command::Uci => {
                writeln!(self.out, "id name {ENGINE_NAME} {ENGINE_VERSION}")?;
                writeln!(self.out, "id author {ENGINE_AUTHOR}")?;
                writeln!(self.out, "uciok")?;
            }
            // There is no debug output yet.
            Command::Debug(_) | Command::Register => {}
            Command::IsReady => writeln!(self.out, "readyok")?,
            Command::SetOption { name, .. } => {
                self.info(format_args!("unknown option \"{name}\""))?;
            }
            // A held-back answer is kept: every `go` gets its `bestmove`, on `stop` at the latest.
            Command::UciNewGame => self.position = Position::startpos(),
            Command::Position { fen, moves } => {
                if let Err(e) = self.set_position(fen, moves) {
                    self.info(e)?;
                }
            }
            Command::Go(go) => {
                self.end_perft(true)?;
                self.go(&go)?;
            }
            Command::Perft(depth) => self.start_perft(depth)?,
            Command::Stop => {
                self.end_perft(true)?;
                self.release_pending()?;
            }
            Command::PonderHit => {
                if let Some(pending) = self.pending.take_if(|pending| !pending.infinite) {
                    self.bestmove(pending.best)?;
                }
            }
            Command::Quit => return Ok(ControlFlow::Break(())),
            Command::Display => {
                writeln!(self.out, "{}\n\nFen: {}", self.position, self.position.to_fen())?;
            }
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
        for uci in moves.split_whitespace() {
            let mv = self.position.parse_uci_move(uci).ok_or(PositionError::IllegalMove(uci))?;
            self.position.make_move(mv);
        }
        Ok(())
    }

    /// There is no search yet: plays the first legal move of `searchmoves`
    /// TODO: Implement a search and return the best move found.
    fn go(&mut self, go: &Go<'_>) -> io::Result<()> {
        // The GUI should have sent `stop` first.
        self.release_pending()?;

        let legal = self.position.legal_moves();
        let mut first_searchmove = None;
        for &uci in &go.searchmoves {
            match legal.find_uci(uci) {
                Some(mv) => {
                    first_searchmove.get_or_insert(mv);
                }
                None => self.info(format_args!("ignoring illegal searchmove \"{uci}\""))?,
            }
        }
        // With no legal searchmove, every move is searched.
        let best = first_searchmove.or_else(|| legal.first().copied());

        if go.infinite || go.ponder {
            self.pending = Some(Pending { best, infinite: go.infinite });
            Ok(())
        } else {
            self.bestmove(best)
        }
    }

    /// Sends the held-back `bestmove`, if any.
    fn release_pending(&mut self) -> io::Result<()> {
        match self.pending.take() {
            Some(pending) => self.bestmove(pending.best),
            None => Ok(()),
        }
    }

    /// Counts on its own thread, so `isready` and `stop` are still answered.
    /// The result is printed once it's done.
    fn start_perft(&mut self, depth: u32) -> io::Result<()> {
        self.end_perft(true)?;
        let id = self.next_perft_id;
        self.next_perft_id += 1;
        let stop = Arc::new(AtomicBool::new(false));
        let handle = thread::spawn({
            let stop = Arc::clone(&stop);
            let position = self.position.clone();
            let notify = NotifyOnExit { id, events: self.events.clone() };
            move || {
                let _notify = notify;
                perft::parallel_divide(&position, depth, &stop)
            }
        });
        self.perft = Some(RunningPerft { id, stop, handle });
        Ok(())
    }

    /// Reports perft `id` if it's the one running.
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
            Ok(Err(e)) => self.info(e),
            Err(_) => self.info("perft failed"),
        }
    }

    /// `bestmove`, with the null move `0000` when there is no legal move.
    fn bestmove(&mut self, best: Option<Move>) -> io::Result<()> {
        match best {
            Some(mv) => writeln!(self.out, "bestmove {mv}"),
            None => writeln!(self.out, "bestmove 0000"),
        }
    }

    /// A message for the GUI to show, which it doesn't otherwise interpret.
    /// Control characters become spaces: messages echo input, and a `\r` in it could make a GUI
    /// read the rest as a line of its own, such as a forged `bestmove`.
    fn info(&mut self, message: impl fmt::Display) -> io::Result<()> {
        let text: String =
            message.to_string().chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        writeln!(self.out, "info string {text}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::positions::{FOOLS_MATE, STARTPOS};

    /// Runs a session on `input` and returns its output, without the greeting line.
    fn session(input: &str) -> String {
        session_bytes(input.as_bytes())
    }

    /// Like [`session`], for input that may not be valid UTF-8.
    fn session_bytes(input: &[u8]) -> String {
        let mut output = Vec::new();
        let input = input.to_vec();
        run(move || io::Cursor::new(input), &mut output).expect("writing to a Vec can't fail");
        let output = String::from_utf8(output).expect("output is UTF-8");
        let (greeting, replies) = output.split_once('\n').expect("greeting line");
        assert_eq!(greeting, format!("{ENGINE_NAME} {ENGINE_VERSION} by {ENGINE_AUTHOR}"));
        replies.to_string()
    }

    #[test]
    fn handshake() {
        assert_eq!(
            session("uci\nisready\n"),
            format!(
                "id name {ENGINE_NAME} {ENGINE_VERSION}\nid author {ENGINE_AUTHOR}\nuciok\nreadyok\n"
            )
        );
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
    fn ucinewgame_resets_the_position() {
        let out = session("position startpos moves e2e4\nucinewgame\nd\n");
        assert!(out.ends_with(&format!("Fen: {STARTPOS}\n")));
    }

    #[test]
    fn go_answers_with_a_legal_move() {
        let out = session("position startpos\ngo wtime 1000 btime 1000\n");
        let mv = out.trim_end().strip_prefix("bestmove ").expect("bestmove");
        assert!(Position::startpos().parse_uci_move(mv).is_some(), "{mv}");
    }

    #[test]
    fn go_respects_searchmoves() {
        assert_eq!(
            session("go searchmoves a1a8 g1f3 e2e4\n"),
            "info string ignoring illegal searchmove \"a1a8\"\nbestmove g1f3\n"
        );
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
            session("setoption name Hash value 64\n"),
            "info string unknown option \"Hash\"\n"
        );
    }
}
