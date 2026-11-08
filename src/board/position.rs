use std::fmt;

use super::{
    Bitboard, CastlingRights, Color, File, Piece, PieceKind, Rank, Square, bishop_attacks,
    king_attacks, knight_attacks, pawn_attacks, rook_attacks, zobrist,
};
use crate::eval::{self, Tapered};

mod fen;
mod make_move;
mod movegen;

pub use fen::FenError;

/// Each side starts with 16 pieces, and none can be added.
/// The piece-square total's `i16` halves rely on it, so FEN parsing rejects more and the board
/// writes check it in debug builds.
pub(crate) const MAX_PIECES_PER_SIDE: u32 = 16;

/// Full game state.
#[derive(Clone, PartialEq, Eq, Debug)]
#[repr(C)]
#[must_use]
pub struct Position {
    /// Zobrist key of the pieces, side to move, castling rights and en passant square.
    /// Kept in sync by the board and state writes, the only code that changes what it covers.
    /// First, so the derived `PartialEq` rejects unequal positions on one comparison.
    zobrist_key: u64,
    /// Squares occupied by each piece kind (both colors), indexed by `PieceKind`.
    by_kind: [Bitboard; 6],
    /// Squares occupied by each color, indexed by `Color`.
    /// Right after `by_kind`, since `pieces(color, kind)` reads both.
    by_color: [Bitboard; 2],
    /// Material and piece-square total from White's view. Kept in sync by the board writes.
    psqt: Tapered,
    side_to_move: Color,
    castling_rights: CastlingRights,
    /// Square a pawn skipped over on the previous double push, if any.
    en_passant: Option<Square>,
    /// Game phase of the pieces on the board, uncapped: see [`eval::phase_weight`].
    /// Kept in sync by the board writes. It fills what would otherwise be a padding byte.
    phase: u8,
    /// Plies since the last capture or pawn move (fifty-move rule).
    halfmove_clock: u16,
    /// Starts at 1 and increments after Black's move.
    fullmove_number: u16,
    /// Piece on each square, indexed by `Square`. Kept in sync with the bitboards.
    board_mailbox: [Option<Piece>; 64],
}

impl Position {
    /// No pieces, White to move, no castling rights, move 1. Its Zobrist key and
    /// piece-square total are 0.
    const EMPTY: Self = Self {
        zobrist_key: 0,
        by_kind: [Bitboard::EMPTY; 6],
        by_color: [Bitboard::EMPTY; 2],
        psqt: Tapered::ZERO,
        side_to_move: Color::White,
        castling_rights: CastlingRights::NONE,
        en_passant: None,
        phase: 0,
        halfmove_clock: 0,
        fullmove_number: 1,
        board_mailbox: [None; 64],
    };

    /// Puts `piece` on the empty square `square`.
    #[inline]
    fn put_piece(&mut self, square: Square, piece: Piece) {
        debug_assert!(self.board_mailbox[square.index()].is_none(), "square already occupied");
        debug_assert!(
            self.by_color[piece.color().index()].popcount() < MAX_PIECES_PER_SIDE,
            "a side would have more than {MAX_PIECES_PER_SIDE} pieces"
        );

        let bb = Bitboard::from_square(square);
        self.by_color[piece.color().index()] |= bb;
        self.by_kind[piece.kind().index()] |= bb;
        self.board_mailbox[square.index()] = Some(piece);
        self.zobrist_key ^= zobrist::piece(piece, square);
        self.psqt += eval::piece_square(piece, square);
        self.phase += eval::phase_weight(piece.kind());

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
        self.psqt -= eval::piece_square(piece, square);
        self.phase -= eval::phase_weight(piece.kind());

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
        self.psqt += eval::piece_square(piece, to) - eval::piece_square(piece, from);

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

    /// The Zobrist key, piece-square total and phase computed from scratch, which the kept
    /// ones must always equal, in one pass over the pieces.
    /// Read from the bitboards, while the board writes update them from what they put in the
    /// mailbox, so a check against them also compares the two.
    fn compute_incremental(&self) -> Incremental {
        let mut zobrist_key = zobrist::castling(self.castling_rights);
        let mut psqt = Tapered::ZERO;
        let mut phase = 0;
        for piece in Piece::ALL {
            for square in self.pieces(piece.color(), piece.kind()) {
                zobrist_key ^= zobrist::piece(piece, square);
                psqt += eval::piece_square(piece, square);
                phase += eval::phase_weight(piece.kind());
            }
        }
        if self.side_to_move == Color::Black {
            zobrist_key ^= zobrist::black_to_move();
        }
        zobrist_key ^= zobrist::en_passant(self.en_passant);
        Incremental { zobrist_key, psqt, phase }
    }

    #[inline]
    fn debug_check_square(&self, square: Square) {
        if cfg!(debug_assertions) {
            self.assert_square_consistent(square);
        }
    }

    #[inline]
    fn debug_check_incremental(&self) {
        if cfg!(debug_assertions) {
            self.assert_incremental_current();
        }
    }

    /// Panics if the kept Zobrist key, piece-square total or phase differs from the one
    /// computed from scratch.
    fn assert_incremental_current(&self) {
        let fresh = self.compute_incremental();
        assert_eq!(self.zobrist_key, fresh.zobrist_key, "Zobrist key is out of date");
        assert_eq!(self.psqt, fresh.psqt, "piece-square total is out of date");
        assert_eq!(self.phase, fresh.phase, "phase is out of date");
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
    /// key, piece-square total or phase is out of date.
    #[cfg_attr(not(test), expect(dead_code, reason = "only used by tests"))]
    fn assert_consistent(&self) {
        for square in Square::ALL {
            self.assert_square_consistent(square);
        }
        self.assert_incremental_current();
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

    /// Material and piece-square total from White's view, kept up to date as pieces move.
    /// See [`eval`].
    #[inline]
    pub const fn psqt(&self) -> Tapered {
        self.psqt
    }

    /// Game phase of the pieces on the board, uncapped.
    #[inline]
    #[must_use]
    pub(crate) const fn phase(&self) -> u8 {
        self.phase
    }

    /// Whether neither side has enough material to ever checkmate.
    #[inline]
    #[must_use]
    pub fn is_insufficient_material(&self) -> bool {
        let majors_and_pawns = self.pieces_of_kind(PieceKind::Pawn)
            | self.pieces_of_kind(PieceKind::Rook)
            | self.pieces_of_kind(PieceKind::Queen);
        if !majors_and_pawns.is_empty() {
            return false;
        }
        let bishops = self.pieces_of_kind(PieceKind::Bishop);
        match self.pieces_of_kind(PieceKind::Knight).popcount() {
            0 => {
                !bishops.intersects(Bitboard::DARK_SQUARES)
                    || !bishops.intersects(Bitboard::LIGHT_SQUARES)
            }
            1 => bishops.is_empty(),
            _ => false,
        }
    }
}

struct Incremental {
    zobrist_key: u64,
    psqt: Tapered,
    phase: u8,
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
        assert_eq!(size_of::<Position>(), 152);
    }

    #[test]
    fn hot_fields_are_one_contiguous_run() {
        use std::mem::offset_of;
        assert_eq!(offset_of!(Position, by_kind), 8);
        assert_eq!(offset_of!(Position, by_color), 8 + 48);
        assert_eq!(offset_of!(Position, psqt), 8 + 48 + 16);
        assert_eq!(offset_of!(Position, side_to_move), 8 + 48 + 16 + 4);
        // The phase fills the byte that would otherwise be padding before the move counters.
        assert_eq!(offset_of!(Position, phase), 8 + 48 + 16 + 4 + 3);
        assert_eq!(offset_of!(Position, halfmove_clock), 8 + 48 + 16 + 4 + 4);
        // The mailbox, read less often, comes after.
        assert!(offset_of!(Position, board_mailbox) > offset_of!(Position, side_to_move));
    }

    #[test]
    fn insufficient_material() {
        let insufficient =
            |fen| Position::from_fen(fen).expect("valid FEN").is_insufficient_material();
        assert!(insufficient("4k3/8/8/8/8/8/8/4K3 w - - 0 1"));
        assert!(insufficient("4k3/8/8/8/8/8/8/2B1K3 w - - 0 1"));
        assert!(insufficient("4k3/8/8/8/8/8/8/2N1K3 b - - 0 1"));
        assert!(!insufficient("4k3/8/8/8/8/8/8/1NN1K3 w - - 0 1"), "two minors might mate");
        assert!(!insufficient("2b1k3/8/8/8/8/8/8/2B1K3 w - - 0 1"), "opposite-color bishops");
        assert!(!insufficient("4k3/8/8/8/8/8/8/1NB1K3 w - - 0 1"), "knight and bishop");
        assert!(insufficient("2b1k3/8/8/8/8/8/8/4KB2 w - - 0 1"), "both bishops on light squares");
        assert!(insufficient("4k3/8/8/8/8/8/8/B1B1K3 w - - 0 1"), "both bishops on dark squares");
        assert!(!insufficient("4k3/8/8/8/8/8/P7/4K3 w - - 0 1"));
        assert!(!insufficient("4k3/8/8/8/8/8/8/R3K3 w - - 0 1"));
        assert!(!insufficient(crate::positions::STARTPOS));
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
