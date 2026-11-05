//! Perft against published node counts.

use rusty_dragon::board::Position;
use rusty_dragon::perft::parallel_perft;

/// Perft suite from the Chess Programming Wiki (<https://www.chessprogramming.org/Perft_Results>).
/// Each position is a FEN string and a list of node counts at depths 1 through 6.
const WIKI: &[(&str, &[u64])] = &[
    // Initial position.
    (
        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
        &[20, 400, 8_902, 197_281, 4_865_609, 119_060_324],
    ),
    // Kiwipete.
    (
        "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq -",
        &[48, 2_039, 97_862, 4_085_603, 193_690_690, 8_031_647_685],
    ),
    // Position 3.
    ("8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1", &[14, 191, 2_812, 43_238, 674_624, 11_030_083]),
    // Position 4.
    (
        "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
        &[6, 264, 9_467, 422_333, 15_833_292, 706_045_033],
    ),
    // Position 4 (mirrored).
    (
        "r2q1rk1/pP1p2pp/Q4n2/bbp1p3/Np6/1B3NBn/pPPP1PPP/R3K2R b KQ - 0 1",
        &[6, 264, 9_467, 422_333, 15_833_292, 706_045_033],
    ),
    // Position 5.
    (
        "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8",
        &[44, 1_486, 62_379, 2_103_487, 89_941_194, 3_048_196_529],
    ),
    // Position 6.
    (
        "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10",
        &[46, 2_079, 89_890, 3_894_594, 164_075_551, 6_923_051_137],
    ),
];

/// Martin Sedlak's perft collection (<https://www.talkchess.com/forum/viewtopic.php?t=47318>).
/// Each entry is a tuple of (FEN, depth, count).
const SPECIAL_RULES: &[(&str, u32, u64)] = &[
    // En passant would expose the king.
    ("3k4/3p4/8/K1P4r/8/8/8/8 b - - 0 1", 6, 1_134_888),
    ("8/8/4k3/8/2p5/8/B2P2K1/8 w - - 0 1", 6, 1_015_133),
    // En passant capture gives check.
    ("8/8/1k6/2b5/2pP4/8/5K2/8 b - d3 0 1", 6, 1_440_467),
    ("8/5k2/8/2Pp4/2B5/1K6/8/8 w - d6 0 1", 6, 1_440_467),
    // Castling gives check.
    ("5k2/8/8/8/8/8/8/4K2R w K - 0 1", 6, 661_072),
    ("3k4/8/8/8/8/8/8/R3K3 w Q - 0 1", 6, 803_711),
    // Castling rights are lost and regained correctly.
    ("r3k2r/1b4bq/8/8/8/8/7B/R3K2R w KQkq - 0 1", 4, 1_274_206),
    ("r3k2r/8/3Q4/8/8/5q2/8/R3K2R b KQkq - 0 1", 4, 1_720_476),
    // Promotions out of and into check, including underpromotion.
    ("2K2r2/4P3/8/8/8/8/8/3k4 w - - 0 1", 6, 3_821_001),
    ("8/8/1P2K3/8/2n5/1q6/8/5k2 b - - 0 1", 5, 1_004_658),
    ("4k3/1P6/8/8/8/8/K7/8 w - - 0 1", 6, 217_342),
    ("8/P1k5/K7/8/8/8/8/8 w - - 0 1", 6, 92_683),
    // Stalemate and checkmate.
    ("K1k5/8/P7/8/8/8/8/8 w - - 0 1", 6, 2_217),
    ("8/k1P5/8/1K6/8/8/8/8 w - - 0 1", 7, 567_584),
    ("8/8/2k5/5q2/5n2/8/5K2/8 b - - 0 1", 4, 23_527),
];

fn check(fen: &str, depth: u32, expected: u64) {
    let pos = Position::from_fen(fen).expect("valid FEN");
    assert_eq!(parallel_perft(&pos, depth), expected, "perft({depth}) of {fen}");
}

#[test]
fn wiki_positions() {
    for &(fen, counts) in WIKI {
        for (depth, &expected) in (1..).zip(counts) {
            check(fen, depth, expected);
        }
    }
}

#[test]
fn special_rules() {
    for &(fen, depth, expected) in SPECIAL_RULES {
        check(fen, depth, expected);
    }
}
