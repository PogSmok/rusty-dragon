use std::error::Error;
use std::fmt;
use std::mem;
use std::str::FromStr;

use super::{File, Rank};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
#[must_use]
#[rustfmt::skip]
pub enum Square {
    A1, B1, C1, D1, E1, F1, G1, H1,
    A2, B2, C2, D2, E2, F2, G2, H2,
    A3, B3, C3, D3, E3, F3, G3, H3,
    A4, B4, C4, D4, E4, F4, G4, H4,
    A5, B5, C5, D5, E5, F5, G5, H5,
    A6, B6, C6, D6, E6, F6, G6, H6,
    A7, B7, C7, D7, E7, F7, G7, H7,
    A8, B8, C8, D8, E8, F8, G8, H8,
}

// `from_index` transmutes any index below `ALL.len()`, so each must be a discriminant.
const _: () = {
    let mut square_idx = 0;
    while square_idx < Square::ALL.len() {
        assert!(
            Square::ALL[square_idx] as usize == square_idx,
            "discriminants must be consecutive, starting at 0"
        );
        square_idx += 1;
    }
};

impl Square {
    #[rustfmt::skip]
    pub const ALL: [Self; 64] = [
        Self::A1, Self::B1, Self::C1, Self::D1, Self::E1, Self::F1, Self::G1, Self::H1,
        Self::A2, Self::B2, Self::C2, Self::D2, Self::E2, Self::F2, Self::G2, Self::H2,
        Self::A3, Self::B3, Self::C3, Self::D3, Self::E3, Self::F3, Self::G3, Self::H3,
        Self::A4, Self::B4, Self::C4, Self::D4, Self::E4, Self::F4, Self::G4, Self::H4,
        Self::A5, Self::B5, Self::C5, Self::D5, Self::E5, Self::F5, Self::G5, Self::H5,
        Self::A6, Self::B6, Self::C6, Self::D6, Self::E6, Self::F6, Self::G6, Self::H6,
        Self::A7, Self::B7, Self::C7, Self::D7, Self::E7, Self::F7, Self::G7, Self::H7,
        Self::A8, Self::B8, Self::C8, Self::D8, Self::E8, Self::F8, Self::G8, Self::H8,
    ];

    /// Panics if `i` is not below 64.
    #[inline]
    pub const fn from_index(i: usize) -> Self {
        assert!(i < 64, "square index out of range");
        #[expect(clippy::cast_possible_truncation, reason = "i < 64")]
        let discriminant = i as u8;
        // SAFETY: `Square` is `repr(u8)` with the discriminants 0..64, and `i < 64`.
        unsafe { mem::transmute::<u8, Self>(discriminant) }
    }

    /// The square `delta` indices away, e.g. `8` for one rank up. Indices wrap modulo 64.
    #[inline]
    pub fn wrapping_offset(self, delta: i8) -> Self {
        Self::from_index(self.index().wrapping_add_signed(isize::from(delta)) % 64)
    }

    #[inline]
    pub const fn from_file_rank(file: File, rank: Rank) -> Self {
        Self::from_index(rank.index() * 8 + file.index())
    }

    #[inline]
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    #[inline]
    pub const fn file(self) -> File {
        File::from_index(self.index() % 8)
    }

    #[inline]
    pub const fn rank(self) -> Rank {
        Rank::from_index(self.index() / 8)
    }
}

impl fmt::Display for Square {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.file().to_char(), self.rank().to_char())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParseSquareError {
    Length(usize),
    File(char),
    Rank(char),
}

impl fmt::Display for ParseSquareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Length(n) => write!(f, "expected 2 characters, got {n}"),
            Self::File(c) => write!(f, "invalid file {c:?}, expected a-h"),
            Self::Rank(c) => write!(f, "invalid rank {c:?}, expected 1-8"),
        }
    }
}

impl Error for ParseSquareError {}

impl FromStr for Square {
    type Err = ParseSquareError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut chars = s.chars();
        let (Some(f), Some(r), None) = (chars.next(), chars.next(), chars.next()) else {
            return Err(ParseSquareError::Length(s.chars().count()));
        };
        let file = File::from_char(f).ok_or(ParseSquareError::File(f))?;
        let rank = Rank::from_char(r).ok_or(ParseSquareError::Rank(r))?;
        Ok(Self::from_file_rank(file, rank))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_index_matches_discriminant() {
        for i in 0..64 {
            assert_eq!(Square::from_index(i).index(), i);
        }
    }

    #[test]
    fn from_index_matches_all() {
        for (i, &square) in Square::ALL.iter().enumerate() {
            assert_eq!(Square::from_index(i), square);
        }
    }

    #[test]
    #[should_panic(expected = "square index out of range")]
    fn from_index_out_of_range_panics() {
        let _ = Square::from_index(64);
    }

    #[test]
    fn wrapping_offset_moves_by_index() {
        assert_eq!(Square::E2.wrapping_offset(8), Square::E3);
        assert_eq!(Square::E2.wrapping_offset(16), Square::E4);
        assert_eq!(Square::E7.wrapping_offset(-9), Square::D6);
        assert_eq!(Square::H1.wrapping_offset(1), Square::A2);

        // Modulo 64 wrapping
        assert_eq!(Square::A1.wrapping_offset(-1), Square::H8);
        assert_eq!(Square::H8.wrapping_offset(1), Square::A1);
    }

    #[test]
    fn file_rank_round_trip() {
        for file in File::ALL {
            for rank in Rank::ALL {
                let sq = Square::from_file_rank(file, rank);
                assert_eq!(sq.file(), file);
                assert_eq!(sq.rank(), rank);
            }
        }
    }

    #[test]
    fn file_and_rank_of_known_square() {
        assert_eq!(Square::E4.file(), File::E);
        assert_eq!(Square::E4.rank(), Rank::R4);
    }

    #[test]
    fn display_and_parse_round_trip() {
        for sq in Square::ALL {
            assert_eq!(sq.to_string().parse::<Square>(), Ok(sq));
        }
    }

    #[test]
    fn parse_rejects_bad_input() {
        assert_eq!("".parse::<Square>(), Err(ParseSquareError::Length(0)));
        assert_eq!("e".parse::<Square>(), Err(ParseSquareError::Length(1)));
        assert_eq!("e44".parse::<Square>(), Err(ParseSquareError::Length(3)));
        assert_eq!("ę4".parse::<Square>(), Err(ParseSquareError::File('ę')));
        assert_eq!("i4".parse::<Square>(), Err(ParseSquareError::File('i')));
        assert_eq!("A4".parse::<Square>(), Err(ParseSquareError::File('A')));
        assert_eq!("4e".parse::<Square>(), Err(ParseSquareError::File('4')));
        assert_eq!("e9".parse::<Square>(), Err(ParseSquareError::Rank('9')));
    }
}
