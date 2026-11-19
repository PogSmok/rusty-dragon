use std::env;
use std::io;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use rusty_dragon::board::Position;
use rusty_dragon::{bench, perft, search, uci};

const USAGE: &str = "usage: rusty-dragon [perft <depth> [fen] | bench [depth]]";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.split_first() {
        None => run_uci(),
        Some((command, rest)) if command == "perft" => run_perft(rest),
        Some((command, rest)) if command == "bench" => run_bench(rest),
        Some(_) => fail(USAGE),
    }
}

/// No arguments: talk UCI over stdin and stdout until `quit`.
fn run_uci() -> ExitCode {
    match uci::run(|| io::stdin().lock(), io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&format!("UCI I/O error: {e}")),
    }
}

/// `perft <depth> [fen]`: prints the node count of each root move, then the total.
fn run_perft(args: &[String]) -> ExitCode {
    let Some((depth, fen)) = args.split_first() else { return fail(USAGE) };
    let Ok(depth) = depth.parse::<u32>() else { return fail(USAGE) };
    let pos = if fen.is_empty() {
        Position::startpos()
    } else {
        match Position::from_fen(&fen.join(" ")) {
            Ok(pos) => pos,
            Err(e) => return fail(&format!("invalid FEN: {e}")),
        }
    };

    let start = Instant::now();
    // Nothing sets the flag: Ctrl-C ends the whole process.
    let split = match perft::parallel_divide(&pos, depth, &AtomicBool::new(false)) {
        Ok(split) => split,
        Err(e) => return fail(&e.to_string()),
    };
    let elapsed = start.elapsed();

    #[expect(clippy::cast_precision_loss, reason = "only for display")]
    let nps = split.nodes() as f64 / elapsed.as_secs_f64().max(f64::EPSILON);
    println!("{split}\nTime: {:.3} s\nNPS: {nps:.0}", elapsed.as_secs_f64());
    ExitCode::SUCCESS
}

/// `bench [depth]`: prints the node count and best move of each bench position, then the totals.
fn run_bench(args: &[String]) -> ExitCode {
    let depth = match args {
        [] => bench::DEFAULT_DEPTH,
        [depth] => match depth.parse::<u32>() {
            Ok(depth) if (1..=search::MAX_DEPTH).contains(&depth) => depth,
            _ => return fail(USAGE),
        },
        _ => return fail(USAGE),
    };

    let count = bench::POSITIONS.len();
    let summary = bench::run(depth, |index, outcome| {
        let best = outcome.best.map_or_else(|| "0000".into(), |mv| mv.to_string());
        println!("Position {:>2}/{count}: {} nodes, best move {best}", index + 1, outcome.nodes);
    });
    let summary = match summary {
        Ok(summary) => summary,
        Err(e) => return fail(&format!("could not start the bench: {e}")),
    };
    println!("\nTime: {:.3} s\n{summary}", summary.time.as_secs_f64());
    ExitCode::SUCCESS
}

#[expect(clippy::print_stderr, reason = "command-line errors go to stderr")]
fn fail(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::FAILURE
}
