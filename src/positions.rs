//! Well-known positions, as FEN, shared by tests and anything else that needs a standard set.
//!
//! The perft positions come from the Chess Programming Wiki
//! (<https://www.chessprogramming.org/Perft_Results>), which lists their node counts.

/// The standard starting position.
///
/// Source: Chess Programming Wiki, "Perft Results", Initial Position
/// (<https://www.chessprogramming.org/Perft_Results#initial-position>).
pub const STARTPOS: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

/// "Kiwipete", a position with castling, en passant, promotions and pins within a few plies.
///
/// Source: Chess Programming Wiki, "Perft Results", Position 2
/// (<https://www.chessprogramming.org/Perft_Results#position-2>).
pub const KIWIPETE: &str = "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1";

/// An endgame with en passant captures that would expose the king.
///
/// Source: Chess Programming Wiki, "Perft Results", Position 3
/// (<https://www.chessprogramming.org/Perft_Results#position-3>).
pub const POSITION_3: &str = "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1";

/// Promotions, underpromotions and lost castling rights.
///
/// Source: Chess Programming Wiki, "Perft Results", Position 4
/// (<https://www.chessprogramming.org/Perft_Results#position-4>).
pub const POSITION_4: &str = "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1";

/// [`POSITION_4`] with the colors swapped, so its counts are the same.
///
/// Source: Chess Programming Wiki, "Perft Results", Position 4, mirrored version
/// (<https://www.chessprogramming.org/Perft_Results#position-4>).
pub const POSITION_4_MIRRORED: &str =
    "r2q1rk1/pP1p2pp/Q4n2/bbp1p3/Np6/1B3NBn/pPPP1PPP/R3K2R b KQ - 0 1";

/// A promotion that captures and gives check.
///
/// Source: Chess Programming Wiki, "Perft Results", Position 5
/// (<https://www.chessprogramming.org/Perft_Results#position-5>).
pub const POSITION_5: &str = "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8";

/// A symmetrical middlegame.
///
/// Source: Chess Programming Wiki, "Perft Results", Position 6
/// (<https://www.chessprogramming.org/Perft_Results#position-6>).
pub const POSITION_6: &str =
    "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10";

/// White to move with 218 legal moves, the most any reachable position can have.
/// Composed by A. Dickins (1968).
///
/// Source: Chess Programming Wiki, "Encoding Moves", Move Index
/// (<https://www.chessprogramming.org/Encoding_Moves#move-index>).
pub const MOST_LEGAL_MOVES: &str = "R6R/3Q4/1Q4Q1/4Q3/2Q4Q/Q4Q2/pp1Q4/kBNN1KB1 w - - 0 1";

/// Fool's mate, 1. f3 e5 2. g4 Qh4#: White is checkmated, with no legal moves.
pub const FOOLS_MATE: &str = "rnb1kbnr/pppp1ppp/8/4p3/6Pq/5P2/PPPPP2P/RNBQKBNR w KQkq - 1 3";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Position;

    #[test]
    fn every_position_is_valid() {
        for fen in [
            STARTPOS,
            KIWIPETE,
            POSITION_3,
            POSITION_4,
            POSITION_4_MIRRORED,
            POSITION_5,
            POSITION_6,
            MOST_LEGAL_MOVES,
            FOOLS_MATE,
        ] {
            let pos = Position::from_fen(fen).unwrap_or_else(|e| panic!("{fen}: {e}"));
            assert_eq!(pos.to_fen(), fen, "round trip");
        }
    }
}
