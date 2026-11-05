use std::error::Error;
use std::fmt;
use std::str::FromStr;

use super::Position;
use crate::board::{
    Bitboard, Castle, CastlingRights, Color, File, ParseSquareError, Piece, PieceKind, Rank, Square,
};

/// Error returned when parsing a FEN string.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FenError {
    /// Expected 6 space-separated fields, or 4 with the move counters left out.
    FieldCount(usize),
    /// Expected 8 ranks separated by '/'.
    RankCount(usize),
    /// A rank doesn't describe exactly 8 squares.
    RankLength(usize),
    /// Unknown piece letter.
    Piece(char),
    /// Side to move is not "w" or "b".
    SideToMove(String),
    /// Castling field is not "-" or a combination of `KQkq` without repeats.
    Castling(String),
    /// These castling rights lack their king or rook on the standard home square.
    /// Chess960 is not supported.
    IllegalCastling(CastlingRights),
    /// En passant field is not "-" or a square.
    EnPassant(ParseSquareError),
    /// En passant square doesn't follow a double pawn push by the side not to move.
    IllegalEnPassant(Square),
    /// Halfmove clock or fullmove number is not a valid number.
    Clock(String),
    /// Not exactly one king per side.
    Kings,
    /// A pawn on rank 1 or 8.
    PawnRank,
    /// The side not to move is in check, so the last move left its own king attacked.
    OpponentInCheck,
}

impl fmt::Display for FenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FieldCount(n) => write!(f, "expected 4 or 6 fields, got {n}"),
            Self::RankCount(n) => write!(f, "expected 8 ranks, got {n}"),
            Self::RankLength(n) => write!(f, "rank describes {n} squares, expected 8"),
            Self::Piece(c) => write!(f, "invalid piece {c:?}, expected one of \"PNBRQKpnbrqk\""),
            Self::SideToMove(s) => write!(f, "invalid side to move {s:?}, expected \"w\" or \"b\""),
            Self::Castling(s) => {
                write!(f, "invalid castling rights {s:?}, expected \"-\" or subset of \"KQkq\"")
            }
            Self::IllegalCastling(rights) => {
                write!(f, "castling rights {rights} need the king and rook on their home squares")
            }
            Self::EnPassant(_) => write!(f, "invalid en passant square"),
            Self::IllegalEnPassant(sq) => {
                write!(f, "en passant square {sq} does not follow a double pawn push")
            }
            Self::Clock(s) => write!(f, "invalid move counter {s:?}"),
            Self::Kings => write!(f, "each side must have exactly one king"),
            Self::PawnRank => write!(f, "pawns cannot be on rank 1 or 8"),
            Self::OpponentInCheck => write!(f, "the side not to move is in check"),
        }
    }
}

impl Error for FenError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::EnPassant(e) => Some(e),
            _ => None,
        }
    }
}

impl Position {
    pub const STARTPOS_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

    pub fn startpos() -> Self {
        Self::from_fen(Self::STARTPOS_FEN).expect("STARTPOS_FEN is valid")
    }

    /// Parses a FEN string. The halfmove clock and fullmove number may be left out, in which
    /// case they default to `0` and `1`. EPD operations after the first four fields (`bm`,
    /// `id`, ...) are not accepted and must be stripped by the caller.
    pub fn from_fen(fen: &str) -> Result<Self, FenError> {
        let fields: Vec<&str> = fen.split_whitespace().collect();
        let (placement, side, castling, en_passant, halfmove, fullmove) = match *fields {
            [placement, side, castling, en_passant] => {
                (placement, side, castling, en_passant, "0", "1")
            }
            [placement, side, castling, en_passant, halfmove, fullmove] => {
                (placement, side, castling, en_passant, halfmove, fullmove)
            }
            _ => return Err(FenError::FieldCount(fields.len())),
        };

        // Parse every field first, so a malformed field is reported before a problem with the
        // position itself.
        let mut pos = Self::EMPTY;
        pos.parse_placement(placement)?;
        pos.side_to_move = match side {
            "w" => Color::White,
            "b" => Color::Black,
            _ => return Err(FenError::SideToMove(side.to_string())),
        };
        let castling_rights = parse_castling(castling)?;
        let en_passant = match en_passant {
            "-" => None,
            square => Some(square.parse().map_err(FenError::EnPassant)?),
        };
        pos.halfmove_clock = parse_clock(halfmove)?;
        pos.fullmove_number = parse_fullmove(fullmove)?;

        pos.validate_pieces()?;
        pos.castling_rights = pos.validate_castling(castling_rights)?;
        pos.en_passant = match en_passant {
            None => None,
            Some(square) => pos.validate_en_passant(square)?,
        };
        pos.validate_opponent_not_in_check()?;
        Ok(pos)
    }

    #[must_use]
    pub fn to_fen(&self) -> String {
        let mut placement = String::with_capacity(64);
        for rank in Rank::ALL.into_iter().rev() {
            let mut empty = 0u8;
            for file in File::ALL {
                match self.piece_at(Square::from_file_rank(file, rank)) {
                    Some(piece) => {
                        if empty > 0 {
                            placement.push(char::from(b'0' + empty));
                            empty = 0;
                        }
                        placement.push(piece.to_char());
                    }
                    None => empty += 1,
                }
            }
            if empty > 0 {
                placement.push(char::from(b'0' + empty));
            }
            if rank != Rank::R1 {
                placement.push('/');
            }
        }

        let side = match self.side_to_move {
            Color::White => 'w',
            Color::Black => 'b',
        };
        let en_passant = self.en_passant.map_or_else(|| "-".to_string(), |sq| sq.to_string());
        format!(
            "{placement} {side} {} {en_passant} {} {}",
            self.castling_rights, self.halfmove_clock, self.fullmove_number
        )
    }

    /// Fills the board from the first FEN field, rank 8 first.
    fn parse_placement(&mut self, placement: &str) -> Result<(), FenError> {
        let ranks: Vec<&str> = placement.split('/').collect();
        if ranks.len() != 8 {
            return Err(FenError::RankCount(ranks.len()));
        }

        for (row, rank) in ranks.into_iter().zip(Rank::ALL.into_iter().rev()) {
            let mut file = 0;
            for c in row.chars() {
                if let Some(skip @ 1..=8) = c.to_digit(10) {
                    file += skip as usize;
                } else {
                    let piece = Piece::from_char(c).ok_or(FenError::Piece(c))?;
                    if file < 8 {
                        self.put_piece(Square::from_file_rank(File::from_index(file), rank), piece);
                    }
                    file += 1;
                }
            }
            if file != 8 {
                return Err(FenError::RankLength(file));
            }
        }
        Ok(())
    }

    /// Rejects piece placements that can't occur in a game.
    fn validate_pieces(&self) -> Result<(), FenError> {
        for color in Color::ALL {
            if self.pieces(color, PieceKind::King).popcount() != 1 {
                return Err(FenError::Kings);
            }
        }
        let back_ranks = Bitboard::RANK_1 | Bitboard::RANK_8;
        if self.pieces_of_kind(PieceKind::Pawn).intersects(back_ranks) {
            return Err(FenError::PawnRank);
        }
        Ok(())
    }

    /// Rejects positions where the side to move could capture the opponent's king.
    /// Needs `validate_pieces` first, so each side has exactly one king.
    fn validate_opponent_not_in_check(&self) -> Result<(), FenError> {
        let us = self.side_to_move;
        if self.is_attacked(self.king_square(us.opposite()), us) {
            Err(FenError::OpponentInCheck)
        } else {
            Ok(())
        }
    }

    /// Checks that every right in `rights` has its king and rook on the standard home squares.
    fn validate_castling(&self, rights: CastlingRights) -> Result<CastlingRights, FenError> {
        let mut illegal = CastlingRights::NONE;
        for castle in Castle::ALL {
            if !rights.contains(castle.right) {
                continue;
            }
            let color = castle.color;
            let in_place = self.piece_at(castle.king_from)
                == Some(Piece::new(color, PieceKind::King))
                && self.piece_at(castle.rook_from) == Some(Piece::new(color, PieceKind::Rook));
            if !in_place {
                illegal = illegal.with(castle.right);
            }
        }
        if illegal.is_empty() { Ok(rights) } else { Err(FenError::IllegalCastling(illegal)) }
    }

    /// Checks that `square` is one the opponent's pawn just skipped with a double push. Returns
    /// it only if the side to move has a legal en passant capture there, as `make_move` would
    /// record it, so equal positions compare equal however they were set up.
    /// Needs `validate_pieces` first, so each side has exactly one king.
    fn validate_en_passant(&self, square: Square) -> Result<Option<Square>, FenError> {
        let us = self.side_to_move;
        let them = us.opposite();
        let target = Bitboard::from_square(square);

        let legal = them.en_passant_rank().contains(square)
            && !self.occupied().contains(square)
            && self.pieces(them, PieceKind::Pawn).intersects(target.forward(them))
            && !self.occupied().intersects(target.forward(us));
        if !legal {
            return Err(FenError::IllegalEnPassant(square));
        }
        Ok(self.has_legal_en_passant(square).then_some(square))
    }
}

fn parse_castling(field: &str) -> Result<CastlingRights, FenError> {
    if field == "-" {
        return Ok(CastlingRights::NONE);
    }

    let mut rights = CastlingRights::NONE;
    for c in field.chars() {
        let right = match c {
            'K' => CastlingRights::WHITE_KINGSIDE,
            'Q' => CastlingRights::WHITE_QUEENSIDE,
            'k' => CastlingRights::BLACK_KINGSIDE,
            'q' => CastlingRights::BLACK_QUEENSIDE,
            _ => return Err(FenError::Castling(field.to_string())),
        };
        if rights.contains(right) {
            return Err(FenError::Castling(field.to_string()));
        }
        rights = rights.with(right);
    }
    Ok(rights)
}

/// Parses a move counter
fn parse_clock(field: &str) -> Result<u16, FenError> {
    let error = || FenError::Clock(field.to_string());
    if !field.bytes().all(|b| b.is_ascii_digit()) {
        return Err(error());
    }
    field.parse().map_err(|_| error())
}

/// Parses the fullmove number, which starts at 1.
fn parse_fullmove(field: &str) -> Result<u16, FenError> {
    match parse_clock(field)? {
        0 => Err(FenError::Clock(field.to_string())),
        n => Ok(n),
    }
}

impl FromStr for Position {
    type Err = FenError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_fen(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIWIPETE: &str = "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1";
    const EN_PASSANT: &str = "rnbqkbnr/ppp1p1pp/8/3pPp2/8/8/PPPP1PPP/RNBQKBNR w KQkq f6 0 3";
    const BLACK_EN_PASSANT: &str = "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1";
    const ENDGAME: &str = "8/8/8/8/8/8/8/K6k b - - 12 60";

    fn fen_error(fen: &str) -> FenError {
        Position::from_fen(fen).expect_err("FEN should be rejected")
    }

    #[test]
    fn startpos_fields() {
        let pos = Position::startpos();
        assert_eq!(pos.occupied().popcount(), 32);
        assert_eq!(pos.piece_at(Square::E1), Some(Piece::new(Color::White, PieceKind::King)));
        assert_eq!(pos.piece_at(Square::D8), Some(Piece::new(Color::Black, PieceKind::Queen)));
        assert_eq!(pos.pieces(Color::White, PieceKind::Pawn), Bitboard::RANK_2);
        assert_eq!(pos.side_to_move(), Color::White);
        assert_eq!(pos.castling_rights(), CastlingRights::ALL);
        assert_eq!(pos.en_passant(), None);
        assert_eq!(pos.halfmove_clock(), 0);
        assert_eq!(pos.fullmove_number(), 1);
    }

    #[test]
    fn round_trip() {
        for fen in [Position::STARTPOS_FEN, KIWIPETE, EN_PASSANT, ENDGAME] {
            assert_eq!(Position::from_fen(fen).map(|pos| pos.to_fen()), Ok(fen.to_string()));
        }
    }

    #[test]
    fn en_passant_square_is_kept_only_if_the_capture_is_legal() {
        // After 1.e4 no black pawn can capture on e3, so the square is dropped.
        let pos: Position = BLACK_EN_PASSANT.parse().expect("valid FEN");
        assert_eq!(pos.en_passant(), None);
        assert_eq!(pos.to_fen(), BLACK_EN_PASSANT.replace(" e3 ", " - "));
        // With a black pawn on d4 it's kept.
        let fen = "rnbqkbnr/ppp1pppp/8/8/3pP3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 3";
        assert_eq!(Position::from_fen(fen).map(|pos| pos.en_passant()), Ok(Some(Square::E3)));
        // A pawn is next to it, but capturing would expose the king to the rook on h5.
        let pos: Position = "8/8/8/K2pP2r/8/8/8/7k w - d6 0 1".parse().expect("valid FEN");
        assert_eq!(pos.en_passant(), None);
        // The pawn that double-pushed gives check and capturing it is legal.
        let pos: Position = "8/8/8/3pP3/4K3/8/8/7k w - d6 0 1".parse().expect("valid FEN");
        assert_eq!(pos.en_passant(), Some(Square::D6));
    }

    #[test]
    fn parses_en_passant_and_clocks() {
        let pos: Position = EN_PASSANT.parse().expect("valid FEN");
        assert_eq!(pos.en_passant(), Some(Square::F6));
        assert_eq!(pos.fullmove_number(), 3);

        let pos: Position = ENDGAME.parse().expect("valid FEN");
        assert_eq!(pos.side_to_move(), Color::Black);
        assert_eq!(pos.castling_rights(), CastlingRights::NONE);
        assert_eq!(pos.halfmove_clock(), 12);
        assert_eq!(pos.fullmove_number(), 60);
    }

    #[test]
    fn move_counters_are_optional() {
        let pos = Position::from_fen("4k3/8/8/8/8/8/8/4K3 b - -").expect("valid 4-field FEN");
        assert_eq!(pos.side_to_move(), Color::Black);
        assert_eq!(pos.halfmove_clock(), 0);
        assert_eq!(pos.fullmove_number(), 1);
        assert_eq!(pos.to_fen(), "4k3/8/8/8/8/8/8/4K3 b - - 0 1");
    }

    #[test]
    fn rejects_bad_structure() {
        assert_eq!(fen_error("4k3/8/8/8/8/8/8/4K3 w -"), FenError::FieldCount(3));
        assert_eq!(fen_error("4k3/8/8/8/8/8/8/4K3 w - - 0"), FenError::FieldCount(5));
        assert_eq!(fen_error("4k3/8/8/8/8/8/8 w - - 0 1"), FenError::RankCount(7));
        assert_eq!(fen_error("4k3/8/8/8/8/8/8/4K2 w - - 0 1"), FenError::RankLength(7));
        assert_eq!(fen_error("4k3/8/8/8/8/8/8/4K4 w - - 0 1"), FenError::RankLength(9));
        assert_eq!(fen_error("4k3/8/8/8/8/8/8/4X3 w - - 0 1"), FenError::Piece('X'));
        assert_eq!(fen_error("4k3/8/8/8/8/8/8/4K3 w - - 0 1 extra"), FenError::FieldCount(7));
    }

    #[test]
    fn rejects_bad_fields() {
        assert_eq!(fen_error("4k3/8/8/8/8/8/8/4K3 x - - 0 1"), FenError::SideToMove("x".into()));
        assert_eq!(fen_error("4k3/8/8/8/8/8/8/4K3 w X - 0 1"), FenError::Castling("X".into()));
        assert_eq!(
            fen_error("4k3/8/8/8/8/8/8/4K3 w - e9 0 1"),
            FenError::EnPassant(ParseSquareError::Rank('9'))
        );
        assert_eq!(fen_error("4k3/8/8/8/8/8/8/4K3 w - - x 1"), FenError::Clock("x".into()));
        assert_eq!(fen_error("4k3/8/8/8/8/8/8/4K3 w - - 0 -1"), FenError::Clock("-1".into()));
        assert_eq!(fen_error("4k3/8/8/8/8/8/8/4K3 w - - +5 1"), FenError::Clock("+5".into()));
        assert_eq!(fen_error("4k3/8/8/8/8/8/8/4K3 w - - 0 +1"), FenError::Clock("+1".into()));
        assert_eq!(fen_error("4k3/8/8/8/8/8/8/4K3 w - - 0 0"), FenError::Clock("0".into()));
        assert_eq!(fen_error("4k3/8/8/8/8/8/8/4K3 w - - 65536 1"), FenError::Clock("65536".into()));
    }

    #[test]
    fn castling_rights_need_king_and_rook_at_home() {
        // The king is not on e1.
        assert_eq!(
            fen_error("4k3/8/8/8/8/8/8/1K5R w K - 0 1"),
            FenError::IllegalCastling(CastlingRights::WHITE_KINGSIDE)
        );
        // No rook on h1.
        assert_eq!(
            fen_error("4k3/8/8/8/8/8/8/4K3 w K - 0 1"),
            FenError::IllegalCastling(CastlingRights::WHITE_KINGSIDE)
        );
        // A black rook on a1 doesn't count for White.
        assert_eq!(
            fen_error("4k3/8/8/8/8/8/8/r3K3 w Q - 0 1"),
            FenError::IllegalCastling(CastlingRights::WHITE_QUEENSIDE)
        );
        // Every missing right is reported, not just the first.
        assert_eq!(
            fen_error("r3k3/8/8/8/8/8/8/4K2R w KQkq - 0 1"),
            FenError::IllegalCastling(
                CastlingRights::WHITE_QUEENSIDE.with(CastlingRights::BLACK_KINGSIDE)
            )
        );
        // Rights whose pieces are home are kept; others may be absent.
        let pos = Position::from_fen("r3k3/8/8/8/8/8/8/4K2R w Kq - 0 1").expect("valid FEN");
        assert_eq!(
            pos.castling_rights(),
            CastlingRights::WHITE_KINGSIDE.with(CastlingRights::BLACK_QUEENSIDE)
        );
    }

    #[test]
    fn rejects_repeated_castling_right() {
        assert_eq!(
            fen_error(&KIWIPETE.replace("KQkq", "KKQkq")),
            FenError::Castling("KKQkq".into())
        );
    }

    #[test]
    fn rejects_illegal_en_passant() {
        // White to move, but the square is on rank 3.
        assert_eq!(
            fen_error(&EN_PASSANT.replace("f6", "f3")),
            FenError::IllegalEnPassant(Square::F3)
        );
        // No black pawn on c5.
        assert_eq!(
            fen_error(&EN_PASSANT.replace("f6", "c6")),
            FenError::IllegalEnPassant(Square::C6)
        );
        // The pawn's starting square g7 is occupied, so it can't have just pushed.
        assert_eq!(
            fen_error("4k3/6p1/8/6p1/8/8/8/4K3 w - g6 0 1"),
            FenError::IllegalEnPassant(Square::G6)
        );
        // The en passant square itself is occupied.
        assert_eq!(
            fen_error("4k3/8/6n1/6p1/8/8/8/4K3 w - g6 0 1"),
            FenError::IllegalEnPassant(Square::G6)
        );
        // Black to move needs a white pawn in front of the square.
        assert_eq!(
            fen_error(&BLACK_EN_PASSANT.replace("e3", "d3")),
            FenError::IllegalEnPassant(Square::D3)
        );
    }

    #[test]
    fn rejects_impossible_positions() {
        assert_eq!(fen_error("8/8/8/8/8/8/8/4K3 w - - 0 1"), FenError::Kings);
        assert_eq!(fen_error("4k3/8/8/8/8/8/8/4KK2 w - - 0 1"), FenError::Kings);
        assert_eq!(fen_error("4k3/8/8/8/8/8/8/P3K3 w - - 0 1"), FenError::PawnRank);
        assert_eq!(fen_error("p3k3/8/8/8/8/8/8/4K3 w - - 0 1"), FenError::PawnRank);
    }

    #[test]
    fn rejects_side_not_to_move_in_check() {
        // White to move with the black king attacked by the rook on e1.
        assert_eq!(fen_error("4k3/8/8/8/8/8/8/4R1K1 w - - 0 1"), FenError::OpponentInCheck);
        // Pawn attacks are checked in the right direction for each color.
        assert_eq!(fen_error("4k3/3P4/8/8/8/8/8/4K3 w - - 0 1"), FenError::OpponentInCheck);
        assert_eq!(fen_error("4k3/8/8/8/8/8/3p4/4K3 b - - 0 1"), FenError::OpponentInCheck);
        // The side to move being in check is a normal position.
        assert!(Position::from_fen("4k3/8/8/8/8/8/8/4R1K1 b - - 0 1").is_ok());
        assert!(Position::from_fen("4k3/3P4/8/8/8/8/8/4K3 b - - 0 1").is_ok());
    }

    #[test]
    fn malformed_fields_are_reported_before_check() {
        assert_eq!(
            fen_error("4k3/8/8/8/8/8/8/4R1K1 w XYZ - 0 1"),
            FenError::Castling("XYZ".into())
        );
        assert_eq!(fen_error("4k3/8/8/8/8/8/8/4R1K1 w - - 0 x"), FenError::Clock("x".into()));
        assert_eq!(fen_error("4k3/8/8/8/8/8/8/4K3 w K - abc 1"), FenError::Clock("abc".into()));
        assert_eq!(fen_error("4k3/8/8/8/8/8/8/4K3 w - e6 abc 1"), FenError::Clock("abc".into()));
        assert_eq!(fen_error("8/8/8/8/8/8/8/8 x - - 0 1"), FenError::SideToMove("x".into()));
    }
}
