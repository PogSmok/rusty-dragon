use std::env;
use std::process::ExitCode;
use std::time::Instant;

use rusty_dragon::board::Position;
use rusty_dragon::perft;
use rusty_dragon::{ENGINE_NAME, ENGINE_VERSION};

const USAGE: &str = "usage: rusty-dragon perft <depth> [fen]";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.split_first() {
        None => {
            println!("{ENGINE_NAME} {ENGINE_VERSION}");
            ExitCode::SUCCESS
        }
        Some((command, rest)) if command == "perft" => run_perft(rest),
        Some(_) => fail(USAGE),
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
    let split = perft::parallel_divide(&pos, depth);
    let nodes: u64 = if depth == 0 { 1 } else { split.iter().map(|&(_, n)| n).sum() };
    let elapsed = start.elapsed();

    for (mv, n) in &split {
        println!("{mv}: {n}");
    }
    #[expect(clippy::cast_precision_loss, reason = "only for display")]
    let nps = nodes as f64 / elapsed.as_secs_f64().max(f64::EPSILON);
    println!("\nNodes: {nodes}\nTime: {:.3} s\nNPS: {nps:.0}", elapsed.as_secs_f64());
    ExitCode::SUCCESS
}

#[expect(clippy::print_stderr, reason = "command-line errors go to stderr")]
fn fail(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::FAILURE
}
