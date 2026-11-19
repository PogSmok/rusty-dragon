//! SEE (static exchange evaluation): what a capture wins once every recapture on its square is
//! played out, cheapest piece first, without searching.

use crate::board::{Move, Piece, PieceKind, Position, bishop_attacks, rook_attacks};

/// SEE piece values, by `PieceKind`.
///
/// The king's is 0: a legal king capture cannot be answered, so the king is never the piece lost.
pub(crate) const SEE_VALUE: [i32; 6] = [100, 300, 300, 500, 900, 0];

/// The kind of piece `mv` captures: a pawn for en passant, `None` for a non-capture.
#[inline]
pub(crate) fn victim(pos: &Position, mv: Move) -> Option<PieceKind> {
    if mv.is_en_passant() { Some(PieceKind::Pawn) } else { pos.piece_at(mv.to()).map(Piece::kind) }
}

/// The SEE value of the piece `mv` captures, 0 for none.
#[inline]
pub(crate) fn victim_value(pos: &Position, mv: Move) -> i32 {
    victim(pos, mv).map_or(0, |kind| SEE_VALUE[kind.index()])
}

/// What a promotion gains in SEE value, the new piece over the pawn, or 0 for another move.
#[inline]
pub(crate) fn promotion_gain(mv: Move) -> i32 {
    mv.promotion().map_or(0, |kind| SEE_VALUE[kind.index()] - SEE_VALUE[PieceKind::Pawn.index()])
}

/// SEE GE (greater or equal): whether `mv`, legal in `pos`, wins at least `threshold` once the
/// exchange it starts is played out, with each side free to stop recapturing when that loses.
///
/// Pins are ignored, and so are promotions by later captures in the exchange.
/// Castling counts as winning nothing.
/// A promotion wins the new piece over the pawn, and puts the new piece at stake.
#[must_use]
pub(crate) fn see_ge(pos: &Position, mv: Move, threshold: i32) -> bool {
    if mv.is_castle() {
        return threshold <= 0;
    }
    let promotion = mv.promotion();
    let (from, to) = (mv.from(), mv.to());
    // En passant takes the pawn beside the destination, not on it.
    let captured_square = if mv.is_en_passant() { mv.en_passant_victim() } else { to };

    // What we win if the exchange ends now, relative to the threshold. Below 0, we cannot reach it.
    let mut swap = victim_value(pos, mv) + promotion_gain(mv) - threshold;
    if swap < 0 {
        return false;
    }
    // The piece on the square once the move is made, which the opponent may take.
    let mover = promotion.unwrap_or_else(|| {
        pos.piece_at(from).expect("the source square should hold a piece").kind()
    });
    // What the opponent wins back by taking our piece, relative to the threshold.
    swap = SEE_VALUE[mover.index()] - swap;
    if swap <= 0 {
        return true;
    }

    let (diagonal, straight) = (pos.diagonal_sliders(), pos.straight_sliders());
    let mut occupied = pos.occupied().without_square(from).without_square(captured_square);
    let mut attackers = pos.attackers_to(to, occupied);
    let mut side = pos.side_to_move();
    // Whether the side that moved first comes out at or above the threshold so far.
    let mut reaches_threshold = true;

    loop {
        side = side.opposite();
        attackers &= occupied;
        let ours = attackers & pos.pieces_of_color(side);
        if ours.is_empty() {
            break;
        }
        reaches_threshold = !reaches_threshold;

        // The cheapest attacker takes next.
        let Some(kind) =
            PieceKind::ALL.into_iter().find(|&kind| ours.intersects(pos.pieces_of_kind(kind)))
        else {
            break;
        };
        if kind == PieceKind::King {
            // The king may only take if nothing can take it back.
            let theirs = attackers & pos.pieces_of_color(side.opposite());
            return if theirs.is_empty() { reaches_threshold } else { !reaches_threshold };
        }
        swap = SEE_VALUE[kind.index()] - swap;
        if swap < i32::from(reaches_threshold) {
            break;
        }
        let capturer = (ours & pos.pieces_of_kind(kind))
            .lsb()
            .expect("a capturer should exist, as found above");
        occupied = occupied.without_square(capturer);
        // Sliders lined up behind the capturer now see the square.
        if matches!(kind, PieceKind::Pawn | PieceKind::Bishop | PieceKind::Queen) {
            attackers |= bishop_attacks(to, occupied) & diagonal;
        }
        if matches!(kind, PieceKind::Rook | PieceKind::Queen) {
            attackers |= rook_attacks(to, occupied) & straight;
        }
    }
    reaches_threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The highest threshold `uci` reaches in `fen`, found by trying them all.
    fn see(fen: &str, uci: &str) -> i32 {
        let pos = Position::from_fen(fen).expect("FEN should be valid");
        let mv = pos.parse_uci_move(uci).expect("move should be legal");
        let value = (-2000..=2000)
            .rev()
            .find(|&t| see_ge(&pos, mv, t))
            .expect("some threshold should pass");
        // Every threshold up to it is reached, none above.
        assert!((-2000..=value).all(|t| see_ge(&pos, mv, t)));
        value
    }

    #[test]
    fn free_pieces_win_their_value() {
        // The pawn on e5 is undefended.
        assert_eq!(see("1k1r4/1pp4p/p7/4p3/8/P5P1/1PP4P/2K1R3 w - - 0 1", "e1e5"), 100);
        assert_eq!(see("4k3/8/8/3q4/8/8/8/3RK3 w - - 0 1", "d1d5"), 900);
    }

    #[test]
    fn defended_pieces_cost_the_capturer() {
        // Nxe5 dxe5: a knight for a pawn.
        assert_eq!(see("4k3/8/3p4/4p3/8/3N4/8/4K3 w - - 0 1", "d3e5"), -200);
        // Pawn takes knight defended by a pawn: still a knight for a pawn.
        assert_eq!(see("4k3/8/3p4/4n3/3P4/8/8/4K3 w - - 0 1", "d4e5"), 200);
    }

    #[test]
    fn x_rays_join_the_exchange() {
        // Rxd5 Rxd5 Rxd5: the doubled rooks win the pawn, since Black's rook cannot take twice.
        assert_eq!(see("3rk3/8/8/3p4/8/8/3R4/3RK3 w - - 0 1", "d2d5"), 100);
        // With Black's queen behind its rook, the exchange loses a rook for a pawn.
        assert_eq!(see("3qk3/3r4/8/3p4/8/8/3R4/3RK3 w - - 0 1", "d2d5"), -400);
    }

    #[test]
    fn a_king_cannot_take_a_defended_piece() {
        // Rxe2+ cannot be answered by Kxe2 while the bishop on b5 defends e2.
        assert_eq!(see("k3r3/8/8/1b6/8/8/4P3/4K3 b - - 0 1", "e8e2"), 100);
        // Without the bishop, the king takes the rook.
        assert_eq!(see("k3r3/8/8/8/8/8/4P3/4K3 b - - 0 1", "e8e2"), -400);
    }

    #[test]
    fn en_passant_wins_the_pawn_beside_the_destination() {
        assert_eq!(see("4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 1", "e5d6"), 100);
        // The c7 pawn takes back.
        assert_eq!(see("4k3/2p5/8/3pP3/8/8/8/4K3 w - d6 0 1", "e5d6"), 0);
        // With the d5 pawn gone, the rook on d1 backs up the capture against the rook on d8.
        assert_eq!(see("3rk3/8/8/3pP3/8/8/8/3RK3 w - d6 0 1", "e5d6"), 100);
    }

    #[test]
    fn promotions_win_the_new_piece_over_the_pawn() {
        // A free queen promotion, and one that takes a rook too.
        assert_eq!(see("4k3/1P6/8/8/8/8/8/4K3 w - - 0 1", "b7b8q"), 800);
        assert_eq!(see("2r1k3/1P6/8/8/8/8/8/4K3 w - - 0 1", "b7c8q"), 1300);
        // The rook on a8 takes the new queen: the pawn is lost.
        assert_eq!(see("r3k3/1P6/8/8/8/8/8/4K3 w - - 0 1", "b7b8q"), -100);
        // A knight is worth less to lose: promoting to it there still loses only the pawn.
        assert_eq!(see("r3k3/1P6/8/8/8/8/8/4K3 w - - 0 1", "b7b8n"), -100);
    }

    #[test]
    fn quiet_moves_onto_attacked_squares_lose_the_piece() {
        // The knight steps where a pawn takes it.
        assert_eq!(see("4k3/8/3p4/8/8/3N4/8/4K3 w - - 0 1", "d3e5"), -300);
        assert_eq!(see("4k3/8/8/8/8/3N4/8/4K3 w - - 0 1", "d3e5"), 0);
    }
}
