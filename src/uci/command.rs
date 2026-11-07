//! Parsing the commands a GUI sends to the engine.

use std::error::Error;
use std::fmt;
use std::iter::Peekable;
use std::str::{FromStr, SplitWhitespace};
use std::time::Duration;

use crate::board::Color;

/// A command from the GUI. Arguments borrow from the input line.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Command<'a> {
    /// `uci`: identify the engine and list its options.
    Uci,
    /// `debug on` or `debug off`.
    Debug(bool),
    /// `isready`: answered with `readyok` once every earlier command has been read.
    /// Work running in the background, like a search or `go perft`, keeps running.
    IsReady,
    /// `setoption name <id> [value <x>]`. Both the name and the value may contain spaces.
    SetOption { name: &'a str, value: Option<&'a str> },
    /// `register`, for engines that need registration.
    Register,
    /// `ucinewgame`: the next search is from a different game.
    UciNewGame,
    /// `position [startpos | fen <fen>] [moves <move>...]`.
    Position {
        /// `None` for the starting position.
        fen: Option<&'a str>,
        /// Whitespace-separated moves in UCI notation, played from the setup position.
        moves: &'a str,
    },
    /// `go [<limit>...]`: start searching the current position within the given limits.
    Go(Go<'a>),
    /// `go perft <depth>`: count leaf nodes per root move instead of searching.
    Perft(u32),
    /// `stop`: end the search as soon as possible and report `bestmove`.
    Stop,
    /// `ponderhit`: the opponent played the move being pondered on.
    PonderHit,
    /// `quit`: exit the program as soon as possible.
    Quit,
    /// `d`: print the board and its FEN. Not part of UCI.
    Display,
}

/// Search limits of a `go` command. A limit left out is `None`, or `false`/empty.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Go<'a> {
    /// Only search these root moves, in UCI notation.
    pub searchmoves: Vec<&'a str>,
    /// Search the position after the predicted reply while the opponent thinks.
    pub ponder: bool,
    /// Time left on each side's clock, indexed by `Color`. Negative times are clamped to 0.
    pub time: [Option<Duration>; 2],
    /// Increment per move for each side, indexed by `Color`.
    pub inc: [Option<Duration>; 2],
    /// Moves until the next time control.
    pub movestogo: Option<u32>,
    /// Search this many plies deep.
    pub depth: Option<u32>,
    /// Search at most this many nodes.
    pub nodes: Option<u64>,
    /// Search for a mate in this many moves.
    pub mate: Option<u32>,
    /// Search exactly this long.
    pub movetime: Option<Duration>,
    /// Search until `stop`.
    pub infinite: bool,
}

/// Every keyword of `go` (besides the leading `perft`), which ends a `searchmoves` list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GoKeyword {
    SearchMoves,
    Ponder,
    WTime,
    BTime,
    WInc,
    BInc,
    MovesToGo,
    Depth,
    Nodes,
    Mate,
    MoveTime,
    Infinite,
}

impl FromStr for GoKeyword {
    type Err = ();

    fn from_str(token: &str) -> Result<Self, ()> {
        Ok(match token {
            "searchmoves" => Self::SearchMoves,
            "ponder" => Self::Ponder,
            "wtime" => Self::WTime,
            "btime" => Self::BTime,
            "winc" => Self::WInc,
            "binc" => Self::BInc,
            "movestogo" => Self::MovesToGo,
            "depth" => Self::Depth,
            "nodes" => Self::Nodes,
            "mate" => Self::Mate,
            "movetime" => Self::MoveTime,
            "infinite" => Self::Infinite,
            _ => return Err(()),
        })
    }
}

/// Error returned when parsing a command.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ParseError<'a> {
    /// The line has no known command.
    Unknown(&'a str),
    /// `debug` is not followed by `on` or `off`.
    DebugMode,
    /// `setoption` lacks `name <id>`.
    OptionName,
    /// `position` is not followed by `startpos` or `fen`.
    PositionSetup,
    /// `go perft` lacks a valid depth.
    PerftDepth,
}

impl fmt::Display for ParseError<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(line) => write!(f, "unknown command \"{line}\""),
            Self::DebugMode => write!(f, "expected \"debug on\" or \"debug off\""),
            Self::OptionName => write!(f, "expected \"setoption name <id> [value <x>]\""),
            Self::PositionSetup => write!(f, "expected \"position startpos\" or \"position fen\""),
            Self::PerftDepth => write!(f, "expected \"go perft <depth>\""),
        }
    }
}

impl Error for ParseError<'_> {}

impl<'a> Command<'a> {
    /// Parses one line of input.
    /// Unknown tokens before the command are skipped, and unknown arguments are ignored.
    /// e.g "hello world\nuci" parses as `Command::Uci`.
    pub fn parse(line: &'a str) -> Result<Self, ParseError<'a>> {
        let mut rest = line;
        while let Some((token, args)) = split_token(rest) {
            // The one place that maps keywords to commands.
            // Every command is covered by `every_command_parses` in the tests.
            return Ok(match token {
                "uci" => Self::Uci,
                "debug" => parse_debug(args)?,
                "isready" => Self::IsReady,
                "setoption" => parse_setoption(args)?,
                "register" => Self::Register,
                "ucinewgame" => Self::UciNewGame,
                "position" => parse_position(args)?,
                "go" => parse_go(args)?,
                "stop" => Self::Stop,
                "ponderhit" => Self::PonderHit,
                "quit" => Self::Quit,
                "d" => Self::Display,
                _ => {
                    rest = args;
                    continue;
                }
            });
        }
        Err(ParseError::Unknown(line.trim()))
    }
}

fn parse_debug(args: &str) -> Result<Command<'_>, ParseError<'_>> {
    match split_token(args) {
        Some(("on", _)) => Ok(Command::Debug(true)),
        Some(("off", _)) => Ok(Command::Debug(false)),
        _ => Err(ParseError::DebugMode),
    }
}

fn parse_setoption(args: &str) -> Result<Command<'_>, ParseError<'_>> {
    let Some(("name", rest)) = split_token(args) else { return Err(ParseError::OptionName) };
    let (name, value) = match split_at_keyword(rest, "value") {
        Some((name, value)) => (name.trim(), Some(value.trim())),
        None => (rest.trim(), None),
    };
    if name.is_empty() {
        return Err(ParseError::OptionName);
    }
    Ok(Command::SetOption { name, value })
}

fn parse_position(args: &str) -> Result<Command<'_>, ParseError<'_>> {
    let (setup, moves) = split_at_keyword(args, "moves").unwrap_or((args, ""));
    let fen = match split_token(setup) {
        Some(("startpos", _)) => None,
        Some(("fen", fen)) => Some(fen.trim()),
        _ => return Err(ParseError::PositionSetup),
    };
    Ok(Command::Position { fen, moves })
}

type Tokens<'a> = Peekable<SplitWhitespace<'a>>;

fn parse_go(args: &str) -> Result<Command<'_>, ParseError<'_>> {
    let mut tokens = args.split_whitespace().peekable();
    if tokens.next_if_eq(&"perft").is_some() {
        return number(&mut tokens).map(Command::Perft).ok_or(ParseError::PerftDepth);
    }

    let mut go = Go::default();
    let (white, black) = (Color::White.index(), Color::Black.index());
    while let Some(token) = tokens.next() {
        let Ok(keyword) = token.parse::<GoKeyword>() else { continue };
        match keyword {
            GoKeyword::SearchMoves => {
                while let Some(mv) = tokens.next_if(|token| token.parse::<GoKeyword>().is_err()) {
                    go.searchmoves.push(mv);
                }
            }
            GoKeyword::Ponder => go.ponder = true,
            GoKeyword::WTime => go.time[white] = millis(&mut tokens),
            GoKeyword::BTime => go.time[black] = millis(&mut tokens),
            GoKeyword::WInc => go.inc[white] = millis(&mut tokens),
            GoKeyword::BInc => go.inc[black] = millis(&mut tokens),
            GoKeyword::MovesToGo => go.movestogo = number(&mut tokens),
            GoKeyword::Depth => go.depth = number(&mut tokens),
            GoKeyword::Nodes => go.nodes = number(&mut tokens),
            GoKeyword::Mate => go.mate = number(&mut tokens),
            GoKeyword::MoveTime => go.movetime = millis(&mut tokens),
            GoKeyword::Infinite => go.infinite = true,
        }
    }
    Ok(Command::Go(go))
}

/// Consumes the next token if it parses as a `T`
fn number<T: FromStr>(tokens: &mut Tokens<'_>) -> Option<T> {
    let n = tokens.peek()?.parse().ok()?;
    tokens.next();
    Some(n)
}

/// Consumes a duration in milliseconds. GUIs may send negative times once a clock runs out.
fn millis(tokens: &mut Tokens<'_>) -> Option<Duration> {
    number::<i64>(tokens).map(|ms| Duration::from_millis(u64::try_from(ms).unwrap_or(0)))
}

/// Splits off the first whitespace-delimited token. The rest keeps its leading whitespace.
/// `None` if `s` is blank.
fn split_token(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    Some(s.find(char::is_whitespace).map_or((s, ""), |end| s.split_at(end)))
}

/// Splits `s` around its first token equal to `keyword`, which is left out of both halves.
fn split_at_keyword<'a>(s: &'a str, keyword: &str) -> Option<(&'a str, &'a str)> {
    let mut rest = s;
    while let Some((token, after)) = split_token(rest) {
        if token == keyword {
            return Some((&s[..s.len() - after.len() - token.len()], after));
        }
        rest = after;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Command<'_> {
        Command::parse(line).unwrap_or_else(|e| panic!("{line:?}: {e}"))
    }

    fn go(line: &str) -> Go<'_> {
        match parse(line) {
            Command::Go(go) => go,
            other => panic!("{line:?} parsed as {other:?}"),
        }
    }

    #[test]
    fn simple_commands() {
        assert_eq!(parse("register later"), Command::Register);
        assert_eq!(parse("debug off"), Command::Debug(false));
    }

    /// A line for each kind of command.
    /// A new command doesn't compile until it has a line here.
    /// Add that line to [`EXAMPLES`] too: nothing can check that it's there.
    fn example(command: &Command<'_>) -> &'static str {
        match command {
            Command::Uci => "uci",
            Command::Debug(_) => "debug on",
            Command::IsReady => "isready",
            Command::SetOption { .. } => "setoption name Hash",
            Command::Register => "register",
            Command::UciNewGame => "ucinewgame",
            Command::Position { .. } => "position startpos",
            Command::Go(_) => "go",
            Command::Perft(_) => "go perft 1",
            Command::Stop => "stop",
            Command::PonderHit => "ponderhit",
            Command::Quit => "quit",
            Command::Display => "d",
        }
    }

    const EXAMPLES: [&str; 13] = [
        "uci",
        "debug on",
        "isready",
        "setoption name Hash",
        "register",
        "ucinewgame",
        "position startpos",
        "go",
        "go perft 1",
        "stop",
        "ponderhit",
        "quit",
        "d",
    ];

    #[test]
    fn every_command_parses() {
        for line in EXAMPLES {
            assert_eq!(example(&parse(line)), line);
        }
    }

    #[test]
    fn whitespace_and_unknown_tokens_are_skipped() {
        assert_eq!(parse("  \tisready \r\n"), Command::IsReady);
        assert_eq!(parse("joho debug on"), Command::Debug(true));
        assert_eq!(Command::parse("hello world\n"), Err(ParseError::Unknown("hello world")));
        assert_eq!(Command::parse(" "), Err(ParseError::Unknown("")));
        // Commands are case sensitive.
        assert_eq!(Command::parse("UCI"), Err(ParseError::Unknown("UCI")));
    }

    #[test]
    fn rejects_malformed_arguments() {
        assert_eq!(Command::parse("debug"), Err(ParseError::DebugMode));
        assert_eq!(Command::parse("debug maybe"), Err(ParseError::DebugMode));
        assert_eq!(Command::parse("setoption"), Err(ParseError::OptionName));
        assert_eq!(Command::parse("setoption name"), Err(ParseError::OptionName));
        assert_eq!(Command::parse("setoption name value 1"), Err(ParseError::OptionName));
        assert_eq!(Command::parse("position"), Err(ParseError::PositionSetup));
        assert_eq!(Command::parse("position moves e2e4"), Err(ParseError::PositionSetup));
        assert_eq!(Command::parse("go perft"), Err(ParseError::PerftDepth));
        assert_eq!(Command::parse("go perft x"), Err(ParseError::PerftDepth));
    }

    #[test]
    fn setoption_names_and_values_may_have_spaces() {
        assert_eq!(
            parse("setoption name Clear Hash"),
            Command::SetOption { name: "Clear Hash", value: None }
        );
        assert_eq!(
            parse("setoption  name  Book File  value  C:\\my books\\a.bin "),
            Command::SetOption { name: "Book File", value: Some("C:\\my books\\a.bin") }
        );
        assert_eq!(
            parse("setoption name Hash value"),
            Command::SetOption { name: "Hash", value: Some("") }
        );
        // "value" only counts as a whole token.
        assert_eq!(
            parse("setoption name Values value 3"),
            Command::SetOption { name: "Values", value: Some("3") }
        );
    }

    #[test]
    fn position_setups() {
        let fen = "4k3/8/8/8/8/8/8/4K3 w - - 0 1";
        assert_eq!(parse("position startpos"), Command::Position { fen: None, moves: "" });
        assert_eq!(
            parse("position startpos moves e2e4  e7e5"),
            Command::Position { fen: None, moves: " e2e4  e7e5" }
        );
        assert_eq!(
            parse(&format!("position fen {fen}")),
            Command::Position { fen: Some(fen), moves: "" }
        );
        assert_eq!(
            parse(&format!("position  fen  {fen}  moves e1d1")),
            Command::Position { fen: Some(fen), moves: " e1d1" }
        );
        assert_eq!(parse("position startpos moves"), Command::Position { fen: None, moves: "" });
    }

    #[test]
    fn go_without_limits() {
        assert_eq!(go("go"), Go::default());
    }

    #[test]
    fn go_clocks() {
        let go = go("go wtime 300000 btime -50 winc 2000 binc 2000 movestogo 40");
        let (white, black) = (Color::White.index(), Color::Black.index());
        assert_eq!(go.time[white], Some(Duration::from_secs(300)));
        assert_eq!(go.time[black], Some(Duration::ZERO), "negative times clamp to 0");
        assert_eq!(go.inc, [Some(Duration::from_secs(2)); 2]);
        assert_eq!(go.movestogo, Some(40));
    }

    #[test]
    fn go_fixed_limits() {
        let go = go("go depth 12 nodes 1000000 mate 3 movetime 500 ponder infinite");
        assert_eq!(go.depth, Some(12));
        assert_eq!(go.nodes, Some(1_000_000));
        assert_eq!(go.mate, Some(3));
        assert_eq!(go.movetime, Some(Duration::from_millis(500)));
        assert!(go.ponder && go.infinite);
    }

    #[test]
    fn go_searchmoves_end_at_the_next_keyword() {
        let go = go("go searchmoves e2e4 d2d4 depth 5");
        assert_eq!(go.searchmoves, ["e2e4", "d2d4"]);
        assert_eq!(go.depth, Some(5));
    }

    #[test]
    fn go_ignores_bad_values_without_losing_keywords() {
        let go = go("go depth infinite nodes -3 foo movetime 10");
        assert_eq!(go.depth, None);
        assert!(go.infinite);
        assert_eq!(go.nodes, None);
        assert_eq!(go.movetime, Some(Duration::from_millis(10)));
    }

    #[test]
    fn go_perft() {
        assert_eq!(parse("go perft 5"), Command::Perft(5));
    }

    #[test]
    fn split_token_keeps_the_separator() {
        assert_eq!(split_token("  go depth 3"), Some(("go", " depth 3")));
        assert_eq!(split_token("quit"), Some(("quit", "")));
        assert_eq!(split_token(" \t\n"), None);
    }

    #[test]
    fn split_at_keyword_matches_whole_tokens() {
        assert_eq!(split_at_keyword("a b moves c d", "moves"), Some(("a b ", " c d")));
        assert_eq!(split_at_keyword("amoves b", "moves"), None);
        assert_eq!(split_at_keyword("moves", "moves"), Some(("", "")));
    }
}
