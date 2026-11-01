#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
#[must_use]
pub enum Rank {
    R1,
    R2,
    R3,
    R4,
    R5,
    R6,
    R7,
    R8,
}

impl Rank {
    pub const ALL: [Self; 8] =
        [Self::R1, Self::R2, Self::R3, Self::R4, Self::R5, Self::R6, Self::R7, Self::R8];

    #[inline]
    pub const fn from_index(i: usize) -> Self {
        Self::ALL[i]
    }

    #[inline]
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    #[inline]
    #[must_use]
    pub const fn from_char(c: char) -> Option<Self> {
        match c {
            '1' => Some(Self::R1),
            '2' => Some(Self::R2),
            '3' => Some(Self::R3),
            '4' => Some(Self::R4),
            '5' => Some(Self::R5),
            '6' => Some(Self::R6),
            '7' => Some(Self::R7),
            '8' => Some(Self::R8),
            _ => None,
        }
    }

    #[inline]
    #[must_use]
    pub fn to_char(self) -> char {
        char::from(b"12345678"[self.index()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_index_matches_discriminant() {
        for i in 0..8 {
            assert_eq!(Rank::from_index(i).index(), i);
        }
    }

    #[test]
    #[should_panic(expected = "index out of bounds")]
    fn from_index_out_of_range_panics() {
        let _ = Rank::from_index(8);
    }

    #[test]
    fn char_round_trip() {
        for (rank, c) in Rank::ALL.into_iter().zip('1'..='8') {
            assert_eq!(rank.to_char(), c);
            assert_eq!(Rank::from_char(c), Some(rank));
        }
    }

    #[test]
    fn from_char_rejects_invalid() {
        for c in ['0', '9', 'a', 'R', ' ', '١'] {
            assert_eq!(Rank::from_char(c), None, "accepted {c:?}");
        }
    }
}
