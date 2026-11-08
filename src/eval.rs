//! Static evaluation
//!
//! The evaluation is PeSTO's tapered material and piece-square tables.

mod psqt;
mod score;
mod tapered;

use std::fmt;

pub(crate) use psqt::{phase_weight, piece_square};
pub use score::Score;
pub use tapered::Tapered;

use crate::board::{Color, Position};
pub use psqt::MAX_PHASE;

/// The position's score for the side to move.
pub fn evaluate(pos: &Position) -> Score {
    score(pos, phase(pos), pos.is_insufficient_material())
}

fn score(pos: &Position, phase: i32, insufficient_material: bool) -> Score {
    if insufficient_material {
        return Score::DRAW;
    }
    let white = blend(pos.psqt(), phase);
    match pos.side_to_move() {
        Color::White => white,
        Color::Black => -white,
    }
}

/// Game phase from 0, bare kings and pawns, to [`MAX_PHASE`], the starting material.
/// `Position` keeps the uncapped sum as pieces come and go, so this is one read.
fn phase(pos: &Position) -> i32 {
    i32::from(pos.phase()).min(MAX_PHASE)
}

/// Mixes the middlegame and endgame values in proportion to `phase`, from White's view.
fn blend(psqt: Tapered, phase: i32) -> Score {
    let mg = i32::from(psqt.mg()) * phase;
    let eg = i32::from(psqt.eg()) * (MAX_PHASE - phase);
    // Rounds toward zero, the same for both colors, so a mirrored position scores the same.
    Score::new((mg + eg) / MAX_PHASE)
}

/// How [`evaluate`] reached its score, for debugging the tables.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Breakdown {
    /// Material and piece-square total, from White's view.
    pub psqt: Tapered,
    /// From 0, bare kings and pawns, to the starting material's [`MAX_PHASE`].
    pub phase: i32,
    pub insufficient_material: bool,
    /// The result, for the side to move.
    pub score: Score,
}

impl Breakdown {
    #[must_use]
    pub fn of(pos: &Position) -> Self {
        let phase = phase(pos);
        let insufficient_material = pos.is_insufficient_material();
        let score = score(pos, phase, insufficient_material);
        Self { psqt: pos.psqt(), phase, insufficient_material, score }
    }
}

impl fmt::Display for Breakdown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Middlegame: {} (White's view)", self.psqt.mg())?;
        writeln!(f, "Endgame: {} (White's view)", self.psqt.eg())?;
        writeln!(f, "Phase: {}/{MAX_PHASE}", self.phase)?;
        if self.insufficient_material {
            writeln!(f, "Insufficient material: draw")?;
        }
        write!(f, "Evaluation: {} (side to move)", self.score)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::positions::{
        FOOLS_MATE, KIWIPETE, MOST_LEGAL_MOVES, POSITION_3, POSITION_4, POSITION_4_MIRRORED,
        POSITION_5, POSITION_6, STARTPOS,
    };

    fn eval(fen: &str) -> Score {
        evaluate(&Position::from_fen(fen).expect("valid FEN"))
    }

    /// `fen` with the board turned upside down and the colors swapped
    fn mirror(fen: &str) -> String {
        let fields: Vec<&str> = fen.split(' ').collect();
        let swap_case = |s: &str| -> String {
            s.chars()
                .map(|c| {
                    if c.is_ascii_uppercase() {
                        c.to_ascii_lowercase()
                    } else {
                        c.to_ascii_uppercase()
                    }
                })
                .collect()
        };
        let board = fields[0].split('/').rev().map(swap_case).collect::<Vec<_>>().join("/");
        let side = if fields[1] == "w" { "b" } else { "w" };
        let mut castling: Vec<char> = swap_case(fields[2]).chars().collect();
        castling.sort_by_key(|&c| "KQkq-".find(c));
        let castling: String = castling.into_iter().collect();
        let en_passant = match fields[3].as_bytes() {
            [file, b'3'] => format!("{}6", char::from(*file)),
            [file, b'6'] => format!("{}3", char::from(*file)),
            _ => fields[3].to_string(),
        };
        let mut mirrored = vec![board, side.to_string(), castling, en_passant];
        mirrored.extend(fields[4..].iter().map(|s| (*s).to_string()));
        mirrored.join(" ")
    }

    /// Scores at every node `depth` plies below `pos` match those of the mirrored positions.
    fn check_symmetry(pos: &Position, depth: u32) {
        let fen = pos.to_fen();
        assert_eq!(evaluate(pos), eval(&mirror(&fen)), "{fen}");
        assert_eq!(Breakdown::of(pos).score, evaluate(pos), "{fen}");
        if depth > 0 {
            for &mv in &pos.legal_moves() {
                let mut child = pos.clone();
                child.make_move(mv);
                check_symmetry(&child, depth - 1);
            }
        }
    }

    #[test]
    fn mirror_swaps_sides() {
        assert_eq!(mirror(STARTPOS), STARTPOS.replace(" w ", " b "));
        assert_eq!(mirror(POSITION_4), POSITION_4_MIRRORED);
    }

    #[test]
    fn mirrored_positions_score_the_same() {
        for fen in [STARTPOS, KIWIPETE, POSITION_3, POSITION_4, POSITION_5, POSITION_6] {
            check_symmetry(&Position::from_fen(fen).expect("valid FEN"), 2);
        }
        assert_eq!(eval(MOST_LEGAL_MOVES), eval(&mirror(MOST_LEGAL_MOVES)));
    }

    #[test]
    fn start_position_is_level() {
        assert_eq!(eval(STARTPOS), Score::DRAW);
    }

    #[test]
    fn the_side_a_piece_up_is_better() {
        // White has an extra knight; the score flips with the side to move.
        let fen = "4k3/pppp4/8/8/8/8/PPPP4/2N1K3 w - - 0 1";
        assert!(eval(fen) > Score::new(200));
        assert_eq!(eval(&fen.replace(" w ", " b ")), -eval(fen));
        // Queen against rook.
        assert!(eval("3rk3/8/8/8/8/8/8/3QK3 w - - 0 1") > Score::new(300));
    }

    #[test]
    fn phase_tapers_from_middlegame_to_endgame() {
        let start = Position::startpos();
        assert_eq!(phase(&start), MAX_PHASE);
        let kings_and_pawns =
            Position::from_fen("4k3/pppp4/8/8/8/8/PPPP4/4K3 w - - 0 1").expect("valid FEN");
        assert_eq!(phase(&kings_and_pawns), 0);
        // Nine queens can't push the phase past the cap.
        assert_eq!(phase(&Position::from_fen(MOST_LEGAL_MOVES).expect("valid FEN")), MAX_PHASE);

        let psqt = Tapered::new(100, -40);
        assert_eq!(blend(psqt, MAX_PHASE), Score::new(100));
        assert_eq!(blend(psqt, 0), Score::new(-40));
        assert_eq!(blend(psqt, MAX_PHASE / 2), Score::new(30));
    }

    #[test]
    fn insufficient_material_is_a_draw() {
        assert_eq!(eval("4k3/8/8/8/8/8/8/2B1K3 w - - 0 1"), Score::DRAW);
        assert_eq!(eval("4k3/8/8/8/8/8/8/2N1K3 b - - 0 1"), Score::DRAW);
        assert_eq!(eval("4k3/8/8/8/8/8/8/4K3 w - - 0 1"), Score::DRAW);
        // Bishops all on light squares can never mate.
        let same_color_bishops = "2b1k3/8/8/8/8/8/8/4KB2 w - - 0 1";
        assert_eq!(eval(same_color_bishops), Score::DRAW);
        let breakdown = Breakdown::of(&Position::from_fen(same_color_bishops).expect("valid"));
        assert_eq!(breakdown.score, Score::DRAW);
        assert!(breakdown.to_string().contains("Insufficient material: draw"));
    }

    #[test]
    fn breakdown_shows_each_step() {
        let breakdown = Breakdown::of(&Position::from_fen(FOOLS_MATE).expect("valid FEN"));
        assert_eq!(breakdown.score, evaluate(&Position::from_fen(FOOLS_MATE).expect("valid")));
        let text = breakdown.to_string();
        assert!(text.contains("Phase: 24/24"), "{text}");
        assert!(text.ends_with(&format!("Evaluation: {} (side to move)", breakdown.score)));
    }
}
