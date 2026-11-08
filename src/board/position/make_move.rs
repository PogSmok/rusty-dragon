//! Applying a move to a position.

use super::Position;
use crate::board::{Castle, CastlingRights, Color, Move, Piece, PieceKind};

/// `RIGHTS_LOST[square]`: castling rights lost when a piece moves from or to `square`
static RIGHTS_LOST: [CastlingRights; 64] = {
    let mut table = [CastlingRights::NONE; 64];
    let mut i = 0;
    while i < Castle::ALL.len() {
        let castle = Castle::ALL[i];
        let (king, rook) = (castle.king_from.index(), castle.rook_from.index());
        table[king] = table[king].with(castle.right);
        table[rook] = table[rook].with(castle.right);
        i += 1;
    }
    table
};

impl Position {
    /// Plays `mv`, which must be legal in this position.
    pub fn make_move(&mut self, mv: Move) {
        let us = self.side_to_move;
        let (from, to) = (mv.from(), mv.to());
        let piece = self.piece_at(from).expect("no piece on source square");
        debug_assert_eq!(piece.color(), us, "{mv} moves the opponent's piece");

        self.set_en_passant(None);
        self.halfmove_clock = if piece.kind() == PieceKind::Pawn || mv.is_capture() {
            0
        } else {
            self.halfmove_clock.saturating_add(1)
        };

        if mv.is_en_passant() {
            // The captured pawn is just behind the destination, from the mover's side.
            self.remove_piece(to.wrapping_offset(-us.pawn_push()));
        } else if mv.is_capture() {
            let captured = self.remove_piece(to);
            debug_assert!(captured.is_some(), "{mv} captures on an empty square");
        }

        if let Some(promotion) = mv.promotion() {
            self.remove_piece(from);
            self.put_piece(to, Piece::new(us, promotion));
        } else {
            self.move_piece(from, to);
        }

        if mv.is_castle() {
            let [kingside, queenside] = Castle::of(us);
            let castle = if to == kingside.king_to { kingside } else { queenside };
            self.move_piece(castle.rook_from, castle.rook_to);
        }

        let lost = RIGHTS_LOST[from.index()].with(RIGHTS_LOST[to.index()]);
        self.set_castling_rights(self.castling_rights.without(lost));

        if us == Color::Black {
            self.fullmove_number = self.fullmove_number.saturating_add(1);
        }
        self.flip_side_to_move();

        // After switching sides, since the opponent is the one who would capture.
        if mv.is_double_push() {
            // The square the pawn skipped, one push from where it started.
            let skipped = from.wrapping_offset(us.pawn_push());
            if self.has_legal_en_passant(skipped) {
                self.set_en_passant(Some(skipped));
            }
        }

        debug_assert!(
            !self.is_attacked(self.king_square(us), self.side_to_move),
            "{mv} left the king in check"
        );
        self.debug_check_incremental();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::positions::{KIWIPETE, POSITION_3, POSITION_4, STARTPOS};

    /// Plays UCI `moves` from `fen` and returns the resulting FEN.
    fn play(fen: &str, moves: &[&str]) -> String {
        let mut pos = Position::from_fen(fen).expect("valid FEN");
        for uci in moves {
            let mv = pos.parse_uci_move(uci).unwrap_or_else(|| panic!("{uci} is not legal"));
            pos.make_move(mv);
            pos.assert_consistent();
        }
        pos.to_fen()
    }

    #[test]
    fn double_push_sets_en_passant_and_counters() {
        // A black pawn on d4 can capture on e3.
        assert_eq!(
            play("4k3/8/8/8/3p4/8/4P3/4K3 w - - 0 1", &["e2e4"]),
            "4k3/8/8/8/3pP3/8/8/4K3 b - e3 0 1"
        );
        // A black pawn on d4 could, but dxe3 would expose its king on a4 to the rook on h4.
        assert_eq!(
            play("4K3/8/8/8/k2p3R/8/4P3/8 w - - 0 1", &["e2e4"]),
            "4K3/8/8/8/k2pP2R/8/8/8 b - - 0 1"
        );
        // No black pawn can, so no en passant square.
        assert_eq!(
            play(STARTPOS, &["e2e4"]),
            "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1"
        );
        assert_eq!(
            play("4k3/2p5/8/3P4/8/8/8/4K3 b - - 0 1", &["c7c5"]),
            "4k3/8/8/2pP4/8/8/8/4K3 w - c6 0 2"
        );
        assert_eq!(
            play(STARTPOS, &["e2e4", "c7c5", "g1f3"]),
            "rnbqkbnr/pp1ppppp/8/2p5/4P3/5N2/PPPP1PPP/RNBQKB1R b KQkq - 1 2"
        );
    }

    #[test]
    fn en_passant_removes_the_captured_pawn() {
        let fen = "rnbqkbnr/ppp1p1pp/8/3pPp2/8/8/PPPP1PPP/RNBQKBNR w KQkq f6 0 3";
        assert_eq!(
            play(fen, &["e5f6"]),
            "rnbqkbnr/ppp1p1pp/5P2/3p4/8/8/PPPP1PPP/RNBQKBNR b KQkq - 0 3"
        );
    }

    #[test]
    fn castling_moves_the_rook_and_clears_rights() {
        let fen = "r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1";
        assert_eq!(play(fen, &["e1g1"]), "r3k2r/8/8/8/8/8/8/R4RK1 b kq - 1 1");
        // Opposite sides, since a rook on d1 or f1 would attack the other king's path.
        assert_eq!(play(fen, &["e1c1", "e8g8"]), "r4rk1/8/8/8/8/8/8/2KR3R w - - 2 2");
        assert_eq!(play(fen, &["e1g1", "e8c8"]), "2kr3r/8/8/8/8/8/8/R4RK1 w - - 2 2");
        // White's rook on d1 attacks d8, which Black's king would pass through.
        let pos = Position::from_fen(&play(fen, &["e1c1"])).expect("valid FEN");
        assert_eq!(pos.parse_uci_move("e8c8"), None);
    }

    #[test]
    fn rook_moves_and_captures_clear_rights() {
        let fen = "r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1";
        // Rook leaves h1: only White's kingside right goes.
        assert_eq!(play(fen, &["h1h2"]), "r3k2r/8/8/8/8/8/7R/R3K3 b Qkq - 1 1");
        // Rook takes the rook on a8: both queenside rights go.
        assert_eq!(play(fen, &["a1a8"]), "R3k2r/8/8/8/8/8/8/4K2R b Kk - 0 1");
    }

    #[test]
    fn promotion_replaces_the_pawn() {
        let fen = "1n2k3/P7/8/8/8/8/8/4K3 w - - 5 40";
        assert_eq!(play(fen, &["a7a8q"]), "Qn2k3/8/8/8/8/8/8/4K3 b - - 0 40");
        assert_eq!(play(fen, &["a7b8n"]), "1N2k3/8/8/8/8/8/8/4K3 b - - 0 40");
    }

    /// Checks the kept Zobrist key at every node `depth` plies below `pos`, against the one
    /// computed from scratch and the one of the same position read back from its FEN.
    fn check_zobrist_keys(pos: &Position, depth: u32) {
        pos.assert_consistent();
        let from_fen = Position::from_fen(&pos.to_fen()).expect("valid FEN");
        assert_eq!(pos.zobrist_key(), from_fen.zobrist_key(), "{}", pos.to_fen());
        if depth > 0 {
            for &mv in &pos.legal_moves() {
                let mut child = pos.clone();
                child.make_move(mv);
                check_zobrist_keys(&child, depth - 1);
            }
        }
    }

    #[test]
    fn zobrist_key_stays_in_sync_through_every_kind_of_move() {
        for fen in [STARTPOS, KIWIPETE, POSITION_3, POSITION_4] {
            check_zobrist_keys(&Position::from_fen(fen).expect("valid FEN"), 3);
        }
    }

    #[test]
    fn transpositions_share_a_zobrist_key_whatever_the_move_counters() {
        // The Zobrist key `make_move` keeps, not one computed from scratch.
        let zobrist_key = |moves: &[&str]| {
            let mut pos = Position::startpos();
            for uci in moves {
                pos.make_move(pos.parse_uci_move(uci).expect("legal"));
            }
            pos.zobrist_key()
        };
        assert_eq!(
            zobrist_key(&["e2e4", "e7e5", "g1f3", "b8c6"]),
            zobrist_key(&["g1f3", "b8c6", "e2e4", "e7e5"])
        );
        // Knights out and back: the start position again, with different counters.
        assert_eq!(
            zobrist_key(&["g1f3", "g8f6", "f3g1", "f6g8"]),
            Position::startpos().zobrist_key()
        );
    }

    #[test]
    fn zobrist_key_tells_apart_side_castling_and_en_passant() {
        let zobrist_key = |fen| Position::from_fen(fen).expect("valid FEN").zobrist_key();
        // The pawn on e5 can take en passant on either d6 or f6.
        let board = "r3k2r/8/8/3pPp2/8/8/8/R3K2R";
        let zobrist_keys = [
            zobrist_key(&format!("{board} w KQkq d6 0 1")),
            zobrist_key(&format!("{board} w KQkq f6 0 1")),
            zobrist_key(&format!("{board} w KQkq - 0 1")),
            zobrist_key(&format!("{board} w Kkq d6 0 1")),
            zobrist_key(&format!("{board} b KQkq - 0 1")),
        ];
        for (i, a) in zobrist_keys.iter().enumerate() {
            for b in &zobrist_keys[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn black_move_increments_fullmove_number() {
        let fen = "4k3/8/8/8/8/8/8/4K3 b - - 7 12";
        assert_eq!(play(fen, &["e8d8"]), "3k4/8/8/8/8/8/8/4K3 w - - 8 13");
    }
}
