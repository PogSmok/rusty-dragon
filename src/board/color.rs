use super::Bitboard;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
#[must_use]
pub enum Color {
    White,
    Black,
}

impl Color {
    pub const ALL: [Self; 2] = [Self::White, Self::Black];

    #[inline]
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Returns the opposite color
    #[inline]
    pub const fn opposite(self) -> Self {
        match self {
            Self::White => Self::Black,
            Self::Black => Self::White,
        }
    }

    /// Square index delta of a single pawn push towards the opponent's side.
    #[inline]
    #[must_use]
    pub const fn pawn_push(self) -> i8 {
        match self {
            Self::White => 8,
            Self::Black => -8,
        }
    }

    /// Rank of the en passant square after this color double-pushes a pawn.
    #[inline]
    pub const fn en_passant_rank(self) -> Bitboard {
        match self {
            Self::White => Bitboard::RANK_3,
            Self::Black => Bitboard::RANK_6,
        }
    }

    /// Where this color's pawns promote.
    #[inline]
    pub const fn promotion_rank(self) -> Bitboard {
        match self {
            Self::White => Bitboard::RANK_8,
            Self::Black => Bitboard::RANK_1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opposite_swaps_colors() {
        assert_eq!(Color::White.opposite(), Color::Black);
        assert_eq!(Color::Black.opposite(), Color::White);
    }
}
