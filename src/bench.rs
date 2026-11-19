//! Bench: searching a fixed set of positions to a fixed depth.
//!
//! On one thread, with the tables cleared before each position, the search is deterministic.
//! The total node count then identifies the search: any change to what it does changes the count.
//! The time it takes measures the speed.

use std::fmt;
use std::io;
use std::panic;
use std::thread;
use std::time::{Duration, Instant};

use crate::board::Position;
use crate::positions::{KIWIPETE, POSITION_3, POSITION_4, POSITION_5, POSITION_6, STARTPOS};
use crate::search::{self, Control, Limits, Outcome, Request, SearchThreads, TranspositionTable};

/// Depth searched when none is given, in plies.
pub const DEFAULT_DEPTH: u32 = 15;

/// Size of the transposition table, in MiB.
const HASH_MIB: usize = 16;

/// The positions searched, as FEN: openings, middlegames and endgames.
///
/// The perft positions are followed by the 24 positions of the Bratko-Kopec Test (1982).
/// (<https://www.chessprogramming.org/Bratko-Kopec_Test>).
pub const POSITIONS: [&str; 30] = [
    STARTPOS,
    KIWIPETE,
    POSITION_3,
    POSITION_4,
    POSITION_5,
    POSITION_6,
    "1k1r4/pp1b1R2/3q2pp/4p3/2B5/4Q3/PPP2B2/2K5 b - - 0 1",
    "3r1k2/4npp1/1ppr3p/p6P/P2PPPP1/1NR5/5K2/2R5 w - - 0 1",
    "2q1rr1k/3bbnnp/p2p1pp1/2pPp3/PpP1P1P1/1P2BNNP/2BQ1PRK/7R b - - 0 1",
    "rnbqkb1r/p3pppp/1p6/2ppP3/3N4/2P5/PPP1QPPP/R1B1KB1R w KQkq - 0 1",
    "r1b2rk1/2q1b1pp/p2ppn2/1p6/3QP3/1BN1B3/PPP3PP/R4RK1 w - - 0 1",
    "2r3k1/pppR1pp1/4p3/4P1P1/5P2/1P4K1/P1P5/8 w - - 0 1",
    "1nk1r1r1/pp2n1pp/4p3/q2pPp1N/b1pP1P2/B1P2R2/2P1B1PP/R2Q2K1 w - - 0 1",
    "4b3/p3kp2/6p1/3pP2p/2pP1P2/4K1P1/P3N2P/8 w - - 0 1",
    "2kr1bnr/pbpq4/2n1pp2/3p3p/3P1P1B/2N2N1Q/PPP3PP/2KR1B1R w - - 0 1",
    "3rr1k1/pp3pp1/1qn2np1/8/3p4/PP1R1P2/2P1NQPP/R1B3K1 b - - 0 1",
    "2r1nrk1/p2q1ppp/bp1p4/n1pPp3/P1P1P3/2PBB1N1/4QPPP/R4RK1 w - - 0 1",
    "r3r1k1/ppqb1ppp/8/4p1NQ/8/2P5/PP3PPP/R3R1K1 b - - 0 1",
    "r2q1rk1/4bppp/p2p4/2pP4/3pP3/3Q4/PP1B1PPP/R3R1K1 w - - 0 1",
    "rnb2r1k/pp2p2p/2pp2p1/q2P1p2/8/1Pb2NP1/PB2PPBP/R2Q1RK1 w - - 0 1",
    "2r3k1/1p2q1pp/2b1pr2/p1pp4/6Q1/1P1PP1R1/P1PN2PP/5RK1 w - - 0 1",
    "r1bqkb1r/4npp1/p1p4p/1p1pP1B1/8/1B6/PPPN1PPP/R2Q1RK1 w kq - 0 1",
    "r2q1rk1/1ppnbppp/p2p1nb1/3Pp3/2P1P1P1/2N2N1P/PPB1QP2/R1B2RK1 b - - 0 1",
    "r1bq1rk1/pp2ppbp/2np2p1/2n5/P3PP2/N1P2N2/1PB3PP/R1B1QRK1 b - - 0 1",
    "3rr3/2pq2pk/p2p1pnp/8/2QBPP2/1P6/P5PP/4RRK1 b - - 0 1",
    "r4k2/pb2bp1r/1p1qp2p/3pNp2/3P1P2/2N3P1/PPP1Q2P/2KRR3 w - - 0 1",
    "3rn2k/ppb2rpp/2ppqp2/5N2/2P1P3/1P5Q/PB3PPP/3RR1K1 w - - 0 1",
    "2r2rk1/1bqnbpp1/1p1ppn1p/pP6/N1P1P3/P2B1N1P/1B2QPP1/R2R2K1 b - - 0 1",
    "r1bqk2r/pp2bppp/2p5/3pP3/P2Q1P2/2N1B3/1PP3PP/R4RK1 b kq - 0 1",
    "r2qnrnk/p2b2b1/1p1p2pp/2pPpp2/1PP1P3/PRNBB3/3QNPPP/5RK1 w - - 0 1",
];

/// The totals over every position.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Summary {
    /// Nodes searched.
    pub nodes: u64,
    /// Time spent searching, without the time spent clearing the tables.
    pub time: Duration,
}

impl Summary {
    /// NPS (nodes per second).
    #[must_use]
    pub fn nps(&self) -> u64 {
        let micros = self.time.as_micros().max(1);
        u64::try_from(u128::from(self.nodes) * 1_000_000 / micros).unwrap_or(u64::MAX)
    }
}

/// `<nodes> nodes <nps> nps`.
impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} nodes {} nps", self.nodes, self.nps())
    }
}

/// Searches each of [`POSITIONS`] to `depth` plies, clamped to `1..=`[`search::MAX_DEPTH`].
///
/// The search runs on one thread, with the stack it needs, and a transposition table of 16 MiB.
/// Both tables are cleared before each position.
/// After each position, `report` is called with its index in [`POSITIONS`] and the outcome.
///
/// # Errors
///
/// Returns an error if the search thread cannot start.
pub fn run(depth: u32, report: impl FnMut(usize, &Outcome) + Send) -> io::Result<Summary> {
    thread::scope(|scope| {
        let bench = thread::Builder::new()
            .name("bench".into())
            .stack_size(search::STACK_SIZE)
            .spawn_scoped(scope, || search_all(depth, report))?;
        Ok(bench.join().unwrap_or_else(|e| panic::resume_unwind(e)))
    })
}

/// [`run`], on the calling thread.
fn search_all(depth: u32, mut report: impl FnMut(usize, &Outcome)) -> Summary {
    let mut tt = TranspositionTable::new(HASH_MIB);
    let mut threads = SearchThreads::new(1);
    let limits = Limits { depth: Some(depth), ..Limits::default() };
    let request = Request { limits, ..Request::default() };
    let mut summary = Summary { nodes: 0, time: Duration::ZERO };
    for (index, fen) in POSITIONS.iter().enumerate() {
        let pos = Position::from_fen(fen).expect("bench positions should be valid");
        // Each count then depends on its own position alone.
        tt.clear();
        threads.clear();
        // A finished search leaves its control stopped.
        let control = Control::default();
        let start = Instant::now();
        let outcome =
            search::search(&pos, &[], &request, &mut threads, &mut tt, &control, &mut |_| {});
        summary.time += start.elapsed();
        summary.nodes += outcome.nodes;
        report(index, &outcome);
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_position_is_valid_with_a_legal_move() {
        for fen in POSITIONS {
            let pos = Position::from_fen(fen).unwrap_or_else(|e| panic!("{fen}: {e}"));
            assert_eq!(pos.to_fen(), fen, "round trip");
            assert!(!pos.legal_moves().is_empty(), "{fen}");
        }
    }

    /// Runs a bench to `depth`, returning the summary and each position's node count.
    fn bench(depth: u32) -> (Summary, Vec<u64>) {
        let mut nodes = Vec::new();
        let summary = run(depth, |index, outcome| {
            assert_eq!(index, nodes.len());
            let fen = POSITIONS[index];
            let pos = Position::from_fen(fen).expect("FEN should be valid");
            let best = outcome.best.expect("every position should have a move");
            assert!(pos.legal_moves().contains(&best), "{best} in {fen}");
            assert_eq!(outcome.depth, depth, "{fen}");
            nodes.push(outcome.nodes);
        })
        .expect("the bench thread should start");
        (summary, nodes)
    }

    #[test]
    fn counts_every_position_the_same_on_every_run() {
        let (summary, nodes) = bench(4);
        assert_eq!(nodes.len(), POSITIONS.len());
        assert!(nodes.iter().all(|&n| n > 0), "{nodes:?}");
        assert_eq!(summary.nodes, nodes.iter().sum::<u64>());
        let (again, nodes_again) = bench(4);
        assert_eq!((again.nodes, nodes_again), (summary.nodes, nodes));
    }

    /// Node count of the bench at [`DEFAULT_DEPTH`].
    /// Modify ONLY when code change is modifying the search behaviour.
    const SIGNATURE: u64 = 5_954_244;

    #[test]
    fn default_bench_matches_the_signature() {
        let summary = run(DEFAULT_DEPTH, |_, _| {}).expect("the bench thread should start");
        assert_eq!(
            summary.nodes, SIGNATURE,
            "the bench signature changed to {}: update `SIGNATURE` if the search was meant to \
             change",
            summary.nodes
        );
    }

    #[test]
    fn summary_formats_nodes_then_nps() {
        let summary = Summary { nodes: 3000, time: Duration::from_millis(1500) };
        assert_eq!(summary.nps(), 2000);
        assert_eq!(summary.to_string(), "3000 nodes 2000 nps");
        assert_eq!(Summary { nodes: 5, time: Duration::ZERO }.nps(), 5_000_000);
    }
}
