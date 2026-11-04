use std::fmt;

use super::{
    Bitboard, CastlingRights, Color, File, Piece, PieceKind, Rank, Square, bishop_attacks,
    king_attacks, knight_attacks, pawn_attacks, rook_attacks,
};

mod fen;

pub use fen::FenError;

/// Full game state.
#[derive(Clone, PartialEq, Eq, Debug)]
#[must_use]
pub struct Position {
    /// Squares occupied by each piece kind (both colors), indexed by `PieceKind`.
    by_kind: [Bitboard; 6],
    /// Squares occupied by each color, indexed by `Color`.
    by_color: [Bitboard; 2],
    /// Piece on each square, indexed by `Square`. Kept in sync with the bitboards.
    board_mailbox: [Option<Piece>; 64],

    side_to_move: Color,
    castling_rights: CastlingRights,
    /// Square a pawn skipped over on the previous double push, if any.
    en_passant: Option<Square>,
    /// Plies since the last capture or pawn move (fifty-move rule).
    halfmove_clock: u16,
    /// Starts at 1 and increments after Black's move.
    fullmove_number: u16,
}

impl Position {
    /// No pieces, White to move, no castling rights, move 1.
    const EMPTY: Self = Self {
        by_kind: [Bitboard::EMPTY; 6],
        by_color: [Bitboard::EMPTY; 2],
        board_mailbox: [None; 64],
        side_to_move: Color::White,
        castling_rights: CastlingRights::NONE,
        en_passant: None,
        halfmove_clock: 0,
        fullmove_number: 1,
    };

    // Board writes. These three are the only code that modifies `by_kind`, `by_color`
    // and `board_mailbox`, so the representations can never drift apart.

    /// Puts `piece` on the empty square `square`.
    #[inline]
    fn put_piece(&mut self, square: Square, piece: Piece) {
        debug_assert!(self.board_mailbox[square.index()].is_none(), "square already occupied");

        let bb = Bitboard::from_square(square);
        self.by_color[piece.color().index()] |= bb;
        self.by_kind[piece.kind().index()] |= bb;
        self.board_mailbox[square.index()] = Some(piece);

        self.debug_check_square(square);
    }

    /// Removes and returns the piece on `square`, or `None` if it is empty.
    #[inline]
    #[cfg_attr(not(test), expect(dead_code, reason = "used by make_move, not written yet"))]
    fn remove_piece(&mut self, square: Square) -> Option<Piece> {
        let piece = self.board_mailbox[square.index()].take()?;

        let bb = !Bitboard::from_square(square);
        self.by_color[piece.color().index()] &= bb;
        self.by_kind[piece.kind().index()] &= bb;

        self.debug_check_square(square);
        Some(piece)
    }

    /// Moves the piece on `from` to the empty square `to`.
    #[inline]
    #[cfg_attr(not(test), expect(dead_code, reason = "used by make_move, not written yet"))]
    fn move_piece(&mut self, from: Square, to: Square) {
        debug_assert!(self.board_mailbox[to.index()].is_none(), "destination already occupied");

        let piece = self.board_mailbox[from.index()].take().expect("no piece on source square");
        let mask = Bitboard::from_square(from) | Bitboard::from_square(to);
        self.by_color[piece.color().index()] ^= mask;
        self.by_kind[piece.kind().index()] ^= mask;
        self.board_mailbox[to.index()] = Some(piece);

        self.debug_check_square(from);
        self.debug_check_square(to);
    }

    #[inline]
    fn debug_check_square(&self, square: Square) {
        if cfg!(debug_assertions) {
            self.assert_square_consistent(square);
        }
    }

    /// Panics if the bitboards and the mailbox disagree about `square`.
    fn assert_square_consistent(&self, square: Square) {
        let colors = self.by_color.iter().filter(|bb| bb.contains(square)).count();
        let kinds = self.by_kind.iter().filter(|bb| bb.contains(square)).count();
        if let Some(piece) = self.board_mailbox[square.index()] {
            let matches = self.pieces(piece.color(), piece.kind()).contains(square);
            assert!(
                colors == 1 && kinds == 1 && matches,
                "{square}: mailbox has {piece:?}, bitboards don't"
            );
        } else {
            assert!(colors == 0 && kinds == 0, "{square}: mailbox is empty, bitboards aren't");
        }
    }

    /// Panics if the bitboards and the mailbox disagree anywhere on the board.
    #[cfg_attr(not(test), expect(dead_code, reason = "only used by tests"))]
    fn assert_consistent(&self) {
        for square in Square::ALL {
            self.assert_square_consistent(square);
        }
    }

    /// Whether any piece of color `by` attacks `square`.
    #[must_use]
    pub fn is_attacked(&self, square: Square, by: Color) -> bool {
        let occupied = self.occupied();
        let queens = self.pieces(by, PieceKind::Queen);
        let diagonal = self.pieces(by, PieceKind::Bishop) | queens;
        let straight = self.pieces(by, PieceKind::Rook) | queens;
        // A pawn `by` attacks `square` iff pawn of opposite color on `square` attacks it back
        let attackers = (pawn_attacks(by.opposite(), square) & self.pieces(by, PieceKind::Pawn))
            | (knight_attacks(square) & self.pieces(by, PieceKind::Knight))
            | (king_attacks(square) & self.pieces(by, PieceKind::King))
            | (bishop_attacks(square, occupied) & diagonal)
            | (rook_attacks(square, occupied) & straight);
        !attackers.is_empty()
    }

    #[inline]
    #[must_use]
    pub const fn piece_at(&self, square: Square) -> Option<Piece> {
        self.board_mailbox[square.index()]
    }

    #[inline]
    pub const fn pieces(&self, color: Color, kind: PieceKind) -> Bitboard {
        self.by_color[color.index()].intersection(self.by_kind[kind.index()])
    }

    #[inline]
    pub const fn pieces_of_color(&self, color: Color) -> Bitboard {
        self.by_color[color.index()]
    }

    /// Squares holding pieces of `kind`, both colors.
    #[inline]
    pub const fn pieces_of_kind(&self, kind: PieceKind) -> Bitboard {
        self.by_kind[kind.index()]
    }

    #[inline]
    pub const fn occupied(&self) -> Bitboard {
        self.by_color[Color::White.index()].union(self.by_color[Color::Black.index()])
    }

    #[inline]
    pub const fn side_to_move(&self) -> Color {
        self.side_to_move
    }

    #[inline]
    pub const fn castling_rights(&self) -> CastlingRights {
        self.castling_rights
    }

    #[inline]
    #[must_use]
    pub const fn en_passant(&self) -> Option<Square> {
        self.en_passant
    }

    #[inline]
    #[must_use]
    pub const fn halfmove_clock(&self) -> u16 {
        self.halfmove_clock
    }

    #[inline]
    #[must_use]
    pub const fn fullmove_number(&self) -> u16 {
        self.fullmove_number
    }
}

/// Prints the board with rank 8 at the top, FEN letters for pieces and `.` for empty squares.
impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for rank in Rank::ALL.into_iter().rev() {
            write!(f, "{} ", rank.to_char())?;
            for file in File::ALL {
                let c =
                    self.piece_at(Square::from_file_rank(file, rank)).map_or('.', Piece::to_char);
                write!(f, " {c}")?;
            }
            writeln!(f)?;
        }
        f.write_str("   a b c d e f g h")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE_KNIGHT: Piece = Piece::WhiteKnight;
    const BLACK_PAWN: Piece = Piece::BlackPawn;

    #[test]
    fn size_is_compact() {
        assert_eq!(size_of::<Position>(), 136);
    }

    #[test]
    fn empty_has_no_pieces() {
        let pos = Position::EMPTY;
        assert!(pos.occupied().is_empty());
        assert!(Square::ALL.iter().all(|&sq| pos.piece_at(sq).is_none()));
        pos.assert_consistent();
    }

    #[test]
    fn put_piece_updates_every_view() {
        let mut pos = Position::EMPTY;
        pos.put_piece(Square::G1, WHITE_KNIGHT);

        let g1 = Bitboard::from_square(Square::G1);
        assert_eq!(pos.piece_at(Square::G1), Some(WHITE_KNIGHT));
        assert_eq!(pos.pieces(Color::White, PieceKind::Knight), g1);
        assert_eq!(pos.pieces_of_color(Color::White), g1);
        assert_eq!(pos.pieces_of_kind(PieceKind::Knight), g1);
        assert_eq!(pos.occupied(), g1);
        assert!(pos.pieces_of_color(Color::Black).is_empty());
    }

    #[test]
    fn remove_piece_returns_it_and_clears_every_view() {
        let mut pos = Position::EMPTY;
        pos.put_piece(Square::E7, BLACK_PAWN);

        assert_eq!(pos.remove_piece(Square::E7), Some(BLACK_PAWN));
        assert_eq!(pos, Position::EMPTY);
    }

    #[test]
    fn remove_piece_on_empty_square_is_none() {
        let mut pos = Position::EMPTY;
        assert_eq!(pos.remove_piece(Square::E4), None);
        assert_eq!(pos, Position::EMPTY);
    }

    #[test]
    fn move_piece_updates_both_squares() {
        let mut pos = Position::EMPTY;
        pos.put_piece(Square::G1, WHITE_KNIGHT);
        pos.move_piece(Square::G1, Square::F3);

        assert_eq!(pos.piece_at(Square::G1), None);
        assert_eq!(pos.piece_at(Square::F3), Some(WHITE_KNIGHT));
        assert_eq!(pos.occupied(), Bitboard::from_square(Square::F3));
    }

    #[test]
    #[should_panic(expected = "no piece on source square")]
    fn move_piece_from_empty_square_panics() {
        let mut pos = Position::EMPTY;
        pos.move_piece(Square::E2, Square::E4);
    }

    #[test]
    #[should_panic(expected = "mailbox is empty, bitboards aren't")]
    fn assert_consistent_detects_corruption() {
        let mut pos = Position::EMPTY;
        pos.by_kind[PieceKind::Queen.index()] = Bitboard::from_square(Square::D1);
        pos.by_color[Color::White.index()] = Bitboard::from_square(Square::D1);
        pos.assert_consistent();
    }

    #[test]
    fn is_attacked_by_each_piece_kind() {
        use Square::*;
        let with = |pieces: &[(Square, Piece)]| {
            let mut pos = Position::EMPTY;
            for &(square, piece) in pieces {
                pos.put_piece(square, piece);
            }
            pos
        };
        let attacked = |pos: &Position, square| pos.is_attacked(square, Color::White);

        let pos = with(&[(E4, Piece::WhitePawn)]);
        assert!(attacked(&pos, D5) && attacked(&pos, F5));
        assert!(!attacked(&pos, E5) && !attacked(&pos, D3));
        assert!(!pos.is_attacked(D5, Color::Black));

        let pos = with(&[(E5, Piece::BlackPawn)]);
        assert!(pos.is_attacked(D4, Color::Black) && pos.is_attacked(F4, Color::Black));
        assert!(!pos.is_attacked(D6, Color::Black));

        let pos = with(&[(B1, Piece::WhiteKnight)]);
        assert!(attacked(&pos, C3) && !attacked(&pos, B3));

        let pos = with(&[(E1, Piece::WhiteKing)]);
        assert!(attacked(&pos, D2) && !attacked(&pos, E3));

        let pos = with(&[(A8, Piece::WhiteRook)]);
        assert!(attacked(&pos, F8) && !attacked(&pos, B7));

        let pos = with(&[(D1, Piece::WhiteQueen)]);
        assert!(attacked(&pos, D8) && attacked(&pos, H5) && !attacked(&pos, E3));

        let pos = with(&[(H1, Piece::WhiteBishop), (F3, Piece::BlackKnight)]);
        assert!(attacked(&pos, F3));
        assert!(!attacked(&pos, E4));
    }

    #[test]
    fn display() {
        let mut pos = Position::EMPTY;
        pos.put_piece(Square::G1, WHITE_KNIGHT);
        pos.put_piece(Square::E7, BLACK_PAWN);

        let expected = concat!(
            "8  . . . . . . . .\n",
            "7  . . . . p . . .\n",
            "6  . . . . . . . .\n",
            "5  . . . . . . . .\n",
            "4  . . . . . . . .\n",
            "3  . . . . . . . .\n",
            "2  . . . . . . . .\n",
            "1  . . . . . . N .\n",
            "   a b c d e f g h",
        );
        assert_eq!(pos.to_string(), expected);
    }
}
