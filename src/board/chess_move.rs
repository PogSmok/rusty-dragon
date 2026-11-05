use std::fmt;

use super::{PieceKind, Square};

/// What a move does besides moving a piece from one square to another.
///
/// The discriminants are the 4-bit flag stored in [`Move`]:
/// - bit 2 marks captures, including en passant;
/// - bit 3 marks promotions, whose 2 least significant bits encode the piece promoted to:
///   `00` knight, `01` bishop, `10` rook, `11` queen.
///
/// Flag values 6 and 7 are unused.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
#[must_use]
pub enum MoveKind {
    Quiet = 0,
    DoublePush = 1,
    KingsideCastle = 2,
    QueensideCastle = 3,
    Capture = 4,
    EnPassant = 5,
    KnightPromotion = 8,
    BishopPromotion = 9,
    RookPromotion = 10,
    QueenPromotion = 11,
    KnightPromotionCapture = 12,
    BishopPromotionCapture = 13,
    RookPromotionCapture = 14,
    QueenPromotionCapture = 15,
}

impl MoveKind {
    const CAPTURE_BIT: u16 = 0b0100;
    const PROMOTION_BIT: u16 = 0b1000;

    /// Promotion to `piece`, which must be a knight, bishop, rook or queen.
    #[inline]
    pub const fn promotion(piece: PieceKind, capture: bool) -> Self {
        match (piece, capture) {
            (PieceKind::Knight, false) => Self::KnightPromotion,
            (PieceKind::Bishop, false) => Self::BishopPromotion,
            (PieceKind::Rook, false) => Self::RookPromotion,
            (PieceKind::Queen, false) => Self::QueenPromotion,
            (PieceKind::Knight, true) => Self::KnightPromotionCapture,
            (PieceKind::Bishop, true) => Self::BishopPromotionCapture,
            (PieceKind::Rook, true) => Self::RookPromotionCapture,
            (PieceKind::Queen, true) => Self::QueenPromotionCapture,
            (PieceKind::Pawn | PieceKind::King, _) => panic!("pawns promote to N, B, R or Q"),
        }
    }
}

/// A move packed into 16 bits:
/// - source square in bits 0-5,
/// - destination square in bits 6-11,
/// - [`MoveKind`] flag in bits 12-15.
///
/// Castling is stored as the king's move, e.g. `e1g1`, as in UCI.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
#[must_use]
pub struct Move(u16);

impl Move {
    const SQUARE_MASK: u16 = 0b11_1111;
    const TO_SHIFT: u32 = 6;
    const KIND_SHIFT: u32 = 12;

    #[inline]
    pub const fn new(from: Square, to: Square, kind: MoveKind) -> Self {
        Self(from as u16 | (to as u16) << Self::TO_SHIFT | (kind as u16) << Self::KIND_SHIFT)
    }

    #[inline]
    pub const fn from(self) -> Square {
        Square::from_index((self.0 & Self::SQUARE_MASK) as usize)
    }

    #[inline]
    pub const fn to(self) -> Square {
        Square::from_index(((self.0 >> Self::TO_SHIFT) & Self::SQUARE_MASK) as usize)
    }

    #[inline]
    const fn flag(self) -> u16 {
        self.0 >> Self::KIND_SHIFT
    }

    #[inline]
    #[must_use]
    pub const fn is_capture(self) -> bool {
        self.flag() & MoveKind::CAPTURE_BIT != 0
    }

    #[inline]
    #[must_use]
    pub const fn is_en_passant(self) -> bool {
        self.flag() == MoveKind::EnPassant as u16
    }

    #[inline]
    #[must_use]
    pub const fn is_double_push(self) -> bool {
        self.flag() == MoveKind::DoublePush as u16
    }

    #[inline]
    #[must_use]
    pub const fn is_castle(self) -> bool {
        let flag = self.flag();
        flag == MoveKind::KingsideCastle as u16 || flag == MoveKind::QueensideCastle as u16
    }

    /// The piece a pawn promotes to, if self is a promotion.
    #[inline]
    #[must_use]
    pub const fn promotion(self) -> Option<PieceKind> {
        let flag = self.flag();
        if flag & MoveKind::PROMOTION_BIT == 0 {
            return None;
        }
        Some(PieceKind::from_index(PieceKind::Knight.index() + (flag & 0b11) as usize))
    }
}

/// UCI notation: source, destination and a lowercase promotion letter, e.g. `e7e8q`.
impl fmt::Display for Move {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.from(), self.to())?;
        match self.promotion() {
            Some(piece) => write!(f, "{}", piece.to_char()),
            None => Ok(()),
        }
    }
}

impl fmt::Debug for Move {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Move({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_KINDS: [MoveKind; 14] = [
        MoveKind::Quiet,
        MoveKind::DoublePush,
        MoveKind::KingsideCastle,
        MoveKind::QueensideCastle,
        MoveKind::Capture,
        MoveKind::EnPassant,
        MoveKind::KnightPromotion,
        MoveKind::BishopPromotion,
        MoveKind::RookPromotion,
        MoveKind::QueenPromotion,
        MoveKind::KnightPromotionCapture,
        MoveKind::BishopPromotionCapture,
        MoveKind::RookPromotionCapture,
        MoveKind::QueenPromotionCapture,
    ];

    #[test]
    fn is_two_bytes() {
        assert_eq!(size_of::<Move>(), 2);
    }

    #[test]
    fn squares_round_trip() {
        for from in Square::ALL {
            for to in Square::ALL {
                for kind in ALL_KINDS {
                    let mv = Move::new(from, to, kind);
                    assert_eq!((mv.from(), mv.to()), (from, to), "{kind:?}");
                }
            }
        }
    }

    #[test]
    fn flags_match_kind() {
        use MoveKind::*;
        for kind in ALL_KINDS {
            let mv = Move::new(Square::E2, Square::E4, kind);
            let capture = matches!(
                kind,
                Capture
                    | EnPassant
                    | KnightPromotionCapture
                    | BishopPromotionCapture
                    | RookPromotionCapture
                    | QueenPromotionCapture
            );
            assert_eq!(mv.is_capture(), capture, "{kind:?}");
            assert_eq!(mv.is_en_passant(), kind == EnPassant, "{kind:?}");
            assert_eq!(mv.is_double_push(), kind == DoublePush, "{kind:?}");
            assert_eq!(
                mv.is_castle(),
                matches!(kind, KingsideCastle | QueensideCastle),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn promotion_round_trips() {
        for piece in [PieceKind::Knight, PieceKind::Bishop, PieceKind::Rook, PieceKind::Queen] {
            for capture in [false, true] {
                let mv = Move::new(Square::B7, Square::A8, MoveKind::promotion(piece, capture));
                assert_eq!(mv.promotion(), Some(piece));
                assert_eq!(mv.is_capture(), capture);
            }
        }
        assert_eq!(Move::new(Square::E2, Square::E4, MoveKind::DoublePush).promotion(), None);
    }

    #[test]
    #[should_panic(expected = "pawns promote to N, B, R or Q")]
    fn promotion_to_king_panics() {
        let _ = MoveKind::promotion(PieceKind::King, false);
    }

    #[test]
    fn display_as_uci() {
        assert_eq!(Move::new(Square::E2, Square::E4, MoveKind::DoublePush).to_string(), "e2e4");
        assert_eq!(Move::new(Square::E1, Square::G1, MoveKind::KingsideCastle).to_string(), "e1g1");
        let promotion = MoveKind::promotion(PieceKind::Knight, true);
        assert_eq!(Move::new(Square::B7, Square::A8, promotion).to_string(), "b7a8n");
        assert_eq!(
            format!("{:?}", Move::new(Square::G1, Square::F3, MoveKind::Quiet)),
            "Move(g1f3)"
        );
    }
}
