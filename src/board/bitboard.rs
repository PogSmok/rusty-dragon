//! 64-bit bitboard representation of a chess board.
//!
//! Little-endian rank-file mapping is used: a1 = 0, h1 = 7, a8 = 56, h8 = 63.

use std::fmt;
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, BitXor, BitXorAssign, Not};

use super::{Color, File, Rank, Square};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
#[must_use]
#[repr(transparent)]
pub struct Bitboard(pub u64);

impl Bitboard {
    pub const EMPTY: Self = Self(0);
    pub const FULL: Self = Self(!0);

    pub const FILE_A: Self = Self(0x0101_0101_0101_0101);
    pub const FILE_B: Self = Self(Self::FILE_A.0 << 1);
    pub const FILE_C: Self = Self(Self::FILE_A.0 << 2);
    pub const FILE_D: Self = Self(Self::FILE_A.0 << 3);
    pub const FILE_E: Self = Self(Self::FILE_A.0 << 4);
    pub const FILE_F: Self = Self(Self::FILE_A.0 << 5);
    pub const FILE_G: Self = Self(Self::FILE_A.0 << 6);
    pub const FILE_H: Self = Self(Self::FILE_A.0 << 7);
    pub const FILES: [Self; 8] = [
        Self::FILE_A,
        Self::FILE_B,
        Self::FILE_C,
        Self::FILE_D,
        Self::FILE_E,
        Self::FILE_F,
        Self::FILE_G,
        Self::FILE_H,
    ];

    pub const RANK_1: Self = Self(0xFF);
    pub const RANK_2: Self = Self(Self::RANK_1.0 << 8);
    pub const RANK_3: Self = Self(Self::RANK_1.0 << 16);
    pub const RANK_4: Self = Self(Self::RANK_1.0 << 24);
    pub const RANK_5: Self = Self(Self::RANK_1.0 << 32);
    pub const RANK_6: Self = Self(Self::RANK_1.0 << 40);
    pub const RANK_7: Self = Self(Self::RANK_1.0 << 48);
    pub const RANK_8: Self = Self(Self::RANK_1.0 << 56);
    pub const RANKS: [Self; 8] = [
        Self::RANK_1,
        Self::RANK_2,
        Self::RANK_3,
        Self::RANK_4,
        Self::RANK_5,
        Self::RANK_6,
        Self::RANK_7,
        Self::RANK_8,
    ];

    pub const LIGHT_SQUARES: Self = Self(0x55AA_55AA_55AA_55AA);
    pub const DARK_SQUARES: Self = Self(!Self::LIGHT_SQUARES.0);

    #[inline]
    pub const fn from_square(square: Square) -> Self {
        Self(1 << square.index())
    }

    #[inline]
    pub const fn from_file(file: File) -> Self {
        Self(Self::FILE_A.0 << file.index())
    }

    #[inline]
    pub const fn from_rank(rank: Rank) -> Self {
        Self(Self::RANK_1.0 << (8 * rank.index()))
    }

    #[inline]
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == Self::EMPTY.0
    }

    #[inline]
    #[must_use]
    pub const fn popcount(self) -> u32 {
        self.0.count_ones()
    }

    #[inline]
    #[must_use]
    pub const fn contains(self, square: Square) -> bool {
        (self.0 & Self::from_square(square).0) != 0
    }

    /// Whether `self` and `other` share at least one square.
    #[inline]
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }

    #[inline]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[inline]
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Squares in `self` that are not in `other`.
    #[inline]
    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    #[inline]
    pub const fn with_square(self, square: Square) -> Self {
        Self(self.0 | Self::from_square(square).0)
    }

    #[inline]
    pub const fn without_square(self, square: Square) -> Self {
        Self(self.0 & !Self::from_square(square).0)
    }

    /// Lowest set square or `None` if self is empty
    #[inline]
    #[must_use]
    pub const fn lsb(self) -> Option<Square> {
        if self.is_empty() {
            None
        } else {
            Some(Square::from_index(self.0.trailing_zeros() as usize))
        }
    }

    /// Removes and returns the lowest set square
    #[inline]
    pub const fn pop_lsb(&mut self) -> Option<Square> {
        match self.lsb() {
            None => None,
            Some(square) => {
                self.clear_lsb();
                Some(square)
            }
        }
    }

    /// Removes the lowest set square
    #[inline]
    pub const fn clear_lsb(&mut self) {
        self.0 &= self.0.wrapping_sub(1);
    }

    // Single-step shifts towards each side of the board from white's perspective

    #[inline]
    pub const fn north(self) -> Self {
        Self(self.0 << 8)
    }

    #[inline]
    pub const fn north_east(self) -> Self {
        Self((self.0 & !Self::FILE_H.0) << 9)
    }

    #[inline]
    pub const fn east(self) -> Self {
        Self((self.0 & !Self::FILE_H.0) << 1)
    }

    #[inline]
    pub const fn south_east(self) -> Self {
        Self((self.0 & !Self::FILE_H.0) >> 7)
    }

    #[inline]
    pub const fn south(self) -> Self {
        Self(self.0 >> 8)
    }

    #[inline]
    pub const fn south_west(self) -> Self {
        Self((self.0 & !Self::FILE_A.0) >> 9)
    }

    #[inline]
    pub const fn west(self) -> Self {
        Self((self.0 & !Self::FILE_A.0) >> 1)
    }

    #[inline]
    pub const fn north_west(self) -> Self {
        Self((self.0 & !Self::FILE_A.0) << 7)
    }

    /// One square towards the opponent's side for `color`'s pawns.
    #[inline]
    pub const fn forward(self, color: Color) -> Self {
        match color {
            Color::White => self.north(),
            Color::Black => self.south(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct BitboardIter(Bitboard);

impl Iterator for BitboardIter {
    type Item = Square;

    #[inline]
    fn next(&mut self) -> Option<Square> {
        self.0.pop_lsb()
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.0.popcount() as usize;
        (n, Some(n))
    }
}

impl ExactSizeIterator for BitboardIter {}

impl IntoIterator for Bitboard {
    type Item = Square;
    type IntoIter = BitboardIter;

    #[inline]
    fn into_iter(self) -> BitboardIter {
        BitboardIter(self)
    }
}

impl FromIterator<Square> for Bitboard {
    fn from_iter<I: IntoIterator<Item = Square>>(squares: I) -> Self {
        squares.into_iter().fold(Self::EMPTY, Self::with_square)
    }
}

macro_rules! impl_bit_op {
    ($trait:ident, $fn:ident, $assign_trait:ident, $assign_fn:ident, $op:tt) => {
        impl $trait for Bitboard {
            type Output = Self;

            #[inline]
            fn $fn(self, rhs: Self) -> Self {
                Self(self.0 $op rhs.0)
            }
        }

        impl $assign_trait for Bitboard {
            #[inline]
            fn $assign_fn(&mut self, rhs: Self) {
                self.0 = self.0 $op rhs.0;
            }
        }
    };
}

impl_bit_op!(BitAnd, bitand, BitAndAssign, bitand_assign, &);
impl_bit_op!(BitOr, bitor, BitOrAssign, bitor_assign, |);
impl_bit_op!(BitXor, bitxor, BitXorAssign, bitxor_assign, ^);

impl Not for Bitboard {
    type Output = Self;

    #[inline]
    fn not(self) -> Self {
        Self(!self.0)
    }
}

impl fmt::Debug for Bitboard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Bitboard({:#018x})", self.0)
    }
}

/// Prints an 8x8 grid with rank 8 at the top, `X` for set squares.
impl fmt::Display for Bitboard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for rank in Rank::ALL.into_iter().rev() {
            write!(f, "{} ", rank.to_char())?;
            for file in File::ALL {
                let c = if self.contains(Square::from_file_rank(file, rank)) { 'X' } else { '.' };
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

    #[test]
    fn square_colors_alternate_like_the_board() {
        for square in Square::ALL {
            // A square is dark when its file and rank are both even or both odd.
            let dark = (square.file().index() + square.rank().index()) % 2 == 0;
            assert_eq!(Bitboard::DARK_SQUARES.contains(square), dark, "{square}");
            assert_eq!(Bitboard::LIGHT_SQUARES.contains(square), !dark, "{square}");
        }
    }

    fn bb(square: Square) -> Bitboard {
        Bitboard::from_square(square)
    }

    #[test]
    fn constants() {
        assert_eq!(Bitboard::FILE_A.popcount(), 8);
        assert_eq!(Bitboard::RANK_1.popcount(), 8);
        assert_eq!(Bitboard::FULL.popcount(), 64);
        assert_eq!(Bitboard::EMPTY.popcount(), 0);
        assert!(Bitboard::FILE_A.contains(Square::A1) && Bitboard::FILE_A.contains(Square::A8));
        assert!(Bitboard::RANK_1.contains(Square::A1) && Bitboard::RANK_1.contains(Square::H1));
        for (i, (&file, &rank)) in Bitboard::FILES.iter().zip(&Bitboard::RANKS).enumerate() {
            assert_eq!(file, Bitboard::from_file(File::ALL[i]));
            assert_eq!(rank, Bitboard::from_rank(Rank::ALL[i]));
        }
    }

    #[test]
    fn set_and_clear() {
        let b = Bitboard::EMPTY.with_square(Square::E4);
        assert!(b.contains(Square::E4));
        assert!(!b.contains(Square::E5));
        assert_eq!(b.popcount(), 1);
        assert!(b.without_square(Square::E4).is_empty());
    }

    #[test]
    fn intersects() {
        assert!(Bitboard::FILE_A.intersects(Bitboard::RANK_1));
        assert!(!Bitboard::FILE_A.intersects(Bitboard::FILE_H));
        assert!(!Bitboard::FULL.intersects(Bitboard::EMPTY));
    }

    #[test]
    fn lsb_and_pop_lsb() {
        let mut b = bb(Square::H8) | bb(Square::C3);
        assert_eq!(b.lsb(), Some(Square::C3));
        assert_eq!(b.pop_lsb(), Some(Square::C3));
        assert_eq!(b.pop_lsb(), Some(Square::H8));
        assert_eq!(b.pop_lsb(), None);
        assert_eq!(b.lsb(), None);
    }

    #[test]
    fn clear_lsb_on_empty() {
        let mut b = Bitboard::EMPTY;
        b.clear_lsb();
        assert!(b.is_empty());
    }

    #[test]
    fn iterates_low_to_high() {
        let b = bb(Square::H8) | bb(Square::A1) | bb(Square::E4);
        assert_eq!(b.into_iter().len(), 3);
        assert_eq!(b.into_iter().collect::<Vec<_>>(), [Square::A1, Square::E4, Square::H8]);
    }

    #[test]
    fn collects_from_squares() {
        let b: Bitboard = [Square::E4, Square::A1, Square::E4].into_iter().collect();
        assert_eq!(b, bb(Square::A1) | bb(Square::E4));
        assert_eq!(std::iter::empty().collect::<Bitboard>(), Bitboard::EMPTY);
        assert_eq!(b.into_iter().collect::<Bitboard>(), b);
    }

    #[test]
    fn shifts_from_center() {
        let e4 = bb(Square::E4);
        assert_eq!(e4.north(), bb(Square::E5));
        assert_eq!(e4.north_east(), bb(Square::F5));
        assert_eq!(e4.east(), bb(Square::F4));
        assert_eq!(e4.south_east(), bb(Square::F3));
        assert_eq!(e4.south(), bb(Square::E3));
        assert_eq!(e4.south_west(), bb(Square::D3));
        assert_eq!(e4.west(), bb(Square::D4));
        assert_eq!(e4.north_west(), bb(Square::D5));
    }

    #[test]
    fn forward_follows_pawn_direction() {
        assert_eq!(bb(Square::E4).forward(Color::White), bb(Square::E5));
        assert_eq!(bb(Square::E4).forward(Color::Black), bb(Square::E3));
    }

    #[test]
    fn shifts_do_not_wrap() {
        assert!(Bitboard::RANK_8.north().is_empty());
        assert!(Bitboard::RANK_1.south().is_empty());
        assert!(Bitboard::FILE_H.east().is_empty());
        assert!(Bitboard::FILE_A.west().is_empty());
        assert!(Bitboard::FILE_H.north_east().is_empty());
        assert!(Bitboard::FILE_H.south_east().is_empty());
        assert!(Bitboard::FILE_A.north_west().is_empty());
        assert!(Bitboard::FILE_A.south_west().is_empty());
    }

    #[test]
    fn operators() {
        let a = Bitboard::FILE_A;
        let r = Bitboard::RANK_1;
        assert_eq!(a & r, bb(Square::A1));
        assert_eq!((a | r).popcount(), 15);
        assert_eq!((a ^ r).popcount(), 14);
        assert_eq!(!Bitboard::EMPTY, Bitboard::FULL);
        assert_eq!(a.union(r), a | r);
        assert_eq!(a.intersection(r), a & r);
        assert_eq!(a.difference(r), a & !r);

        let mut m = a;
        m &= r;
        assert_eq!(m, bb(Square::A1));
        m |= bb(Square::H8);
        assert_eq!(m.popcount(), 2);
        m ^= bb(Square::H8);
        assert_eq!(m, bb(Square::A1));
    }

    #[test]
    fn display() {
        let expected = concat!(
            "8  . . . . . . . .\n",
            "7  . . . . . . . .\n",
            "6  . . . . . . . .\n",
            "5  . . . . . . . .\n",
            "4  . . . . X . . .\n",
            "3  . . . . . . . .\n",
            "2  . . . . . . . .\n",
            "1  X . . . . . . X\n",
            "   a b c d e f g h",
        );
        let b = bb(Square::A1) | bb(Square::H1) | bb(Square::E4);
        assert_eq!(b.to_string(), expected);
    }
}
