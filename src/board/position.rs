use std::fmt;

use super::{
    Bitboard, CastlingRights, Color, File, Piece, PieceKind, Rank, Square, bishop_attacks,
    king_attacks, knight_attacks, pawn_attacks, rook_attacks, zobrist,
};

mod fen;
mod make_move;
mod movegen;

pub use fen::FenError;

/// Full game state.
#[derive(Clone, PartialEq, Eq, Debug)]
#[must_use]
pub struct Position {
    /// Zobrist key of everything below but the move counters.
    /// Kept in sync by the board and state writes, the only code that changes what it covers.
    /// First, so the derived `PartialEq` rejects unequal positions on one comparison.
    zobrist_key: u64,

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
    /// No pieces, White to move, no castling rights, move 1. Its Zobrist key is 0.
    const EMPTY: Self = Self {
        zobrist_key: 0,
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
    // and `board_mailbox`, so the representations and the pieces' part of the Zobrist key can
    // never drift apart.

    /// Puts `piece` on the empty square `square`.
    #[inline]
    fn put_piece(&mut self, square: Square, piece: Piece) {
        debug_assert!(self.board_mailbox[square.index()].is_none(), "square already occupied");

        let bb = Bitboard::from_square(square);
        self.by_color[piece.color().index()] |= bb;
        self.by_kind[piece.kind().index()] |= bb;
        self.board_mailbox[square.index()] = Some(piece);
        self.zobrist_key ^= zobrist::piece(piece, square);

        self.debug_check_square(square);
    }

    /// Removes and returns the piece on `square`, or `None` if it is empty.
    #[inline]
    fn remove_piece(&mut self, square: Square) -> Option<Piece> {
        let piece = self.board_mailbox[square.index()].take()?;

        let bb = !Bitboard::from_square(square);
        self.by_color[piece.color().index()] &= bb;
        self.by_kind[piece.kind().index()] &= bb;
        self.zobrist_key ^= zobrist::piece(piece, square);

        self.debug_check_square(square);
        Some(piece)
    }

    /// Moves the piece on `from` to the empty square `to`.
    #[inline]
    fn move_piece(&mut self, from: Square, to: Square) {
        debug_assert!(self.board_mailbox[to.index()].is_none(), "destination already occupied");

        let piece = self.board_mailbox[from.index()].take().expect("no piece on source square");
        let mask = Bitboard::from_square(from) | Bitboard::from_square(to);
        self.by_color[piece.color().index()] ^= mask;
        self.by_kind[piece.kind().index()] ^= mask;
        self.board_mailbox[to.index()] = Some(piece);
        self.zobrist_key ^= zobrist::piece(piece, from) ^ zobrist::piece(piece, to);

        self.debug_check_square(from);
        self.debug_check_square(to);
    }

    /// Hands the move to the other side.
    #[inline]
    fn flip_side_to_move(&mut self) {
        self.side_to_move = self.side_to_move.opposite();
        self.zobrist_key ^= zobrist::black_to_move();
    }

    #[inline]
    fn set_side_to_move(&mut self, side: Color) {
        if side != self.side_to_move {
            self.flip_side_to_move();
        }
    }

    #[inline]
    fn set_castling_rights(&mut self, rights: CastlingRights) {
        self.zobrist_key ^= zobrist::castling(self.castling_rights) ^ zobrist::castling(rights);
        self.castling_rights = rights;
    }

    #[inline]
    fn set_en_passant(&mut self, square: Option<Square>) {
        self.zobrist_key ^= zobrist::en_passant(self.en_passant) ^ zobrist::en_passant(square);
        self.en_passant = square;
    }

    /// The Zobrist key computed from scratch, which the kept one must always equal.
    /// Read from the bitboards, while the board writes hash what they put in the mailbox,
    /// so a check against it also compares the two.
    fn compute_zobrist_key(&self) -> u64 {
        let mut zobrist_key = zobrist::castling(self.castling_rights);
        for piece in Piece::ALL {
            for square in self.pieces(piece.color(), piece.kind()) {
                zobrist_key ^= zobrist::piece(piece, square);
            }
        }
        if self.side_to_move == Color::Black {
            zobrist_key ^= zobrist::black_to_move();
        }
        zobrist_key ^ zobrist::en_passant(self.en_passant)
    }

    #[inline]
    fn debug_check_square(&self, square: Square) {
        if cfg!(debug_assertions) {
            self.assert_square_consistent(square);
        }
    }

    #[inline]
    fn debug_check_zobrist_key(&self) {
        if cfg!(debug_assertions) {
            self.assert_zobrist_key_current();
        }
    }

    /// Panics if the kept Zobrist key differs from the one computed from scratch.
    fn assert_zobrist_key_current(&self) {
        assert_eq!(self.zobrist_key, self.compute_zobrist_key(), "Zobrist key is out of date");
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

    /// Panics if the bitboards and the mailbox disagree anywhere on the board, or the Zobrist
    /// key is out of date.
    #[cfg_attr(not(test), expect(dead_code, reason = "only used by tests"))]
    fn assert_consistent(&self) {
        for square in Square::ALL {
            self.assert_square_consistent(square);
        }
        self.assert_zobrist_key_current();
    }

    #[inline]
    pub fn attackers(&self, square: Square, by: Color, occupied: Bitboard) -> Bitboard {
        let queens = self.pieces_of_kind(PieceKind::Queen);
        let diagonal = self.pieces_of_kind(PieceKind::Bishop) | queens;
        let straight = self.pieces_of_kind(PieceKind::Rook) | queens;
        // A pawn `by` attacks `square` iff a pawn of the other color on `square` attacks it back.
        let attackers = (pawn_attacks(by.opposite(), square)
            & self.pieces_of_kind(PieceKind::Pawn))
            | (knight_attacks(square) & self.pieces_of_kind(PieceKind::Knight))
            | (king_attacks(square) & self.pieces_of_kind(PieceKind::King))
            | (bishop_attacks(square, occupied) & diagonal)
            | (rook_attacks(square, occupied) & straight);
        attackers & self.pieces_of_color(by)
    }

    /// Whether any piece of color `by` attacks `square`.
    #[inline]
    #[must_use]
    pub fn is_attacked(&self, square: Square, by: Color) -> bool {
        !self.attackers(square, by, self.occupied()).is_empty()
    }

    /// Square of `color`'s king. Every position has exactly one king per side.
    #[inline]
    pub fn king_square(&self, color: Color) -> Square {
        self.pieces(color, PieceKind::King).lsb().expect("each side has a king")
    }

    /// Whether the side to move is in check.
    #[must_use]
    pub fn in_check(&self) -> bool {
        let us = self.side_to_move;
        self.is_attacked(self.king_square(us), us.opposite())
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

    /// Equal for positions with the same pieces, side to move, castling rights and
    /// en passant square, whatever the move counters.
    /// Distinct positions almost always get distinct Zobrist keys, but a collision is possible.
    ///
    /// Due to collissions a move found by Zobrist key, such as a transposition table's best move,
    /// has to be checked for legality before it's played.
    /// Engines tested with far more frequent collisions than 64-bit keys give showed no
    /// measurable loss of strength (Hyatt and Cozzie, 2005).
    #[inline]
    #[must_use]
    pub const fn zobrist_key(&self) -> u64 {
        self.zobrist_key
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
        assert_eq!(size_of::<Position>(), 144);
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
