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
    pub const fn from_index(i: u8) -> Self {
        Self::ALL[i as usize]
    }

    #[inline]
    #[must_use]
    pub const fn index(self) -> u8 {
        self as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_index_matches_discriminant() {
        for i in 0..8 {
            assert_eq!(Rank::from_index(i) as u8, i);
        }
    }

    #[test]
    #[should_panic(expected = "index out of bounds")]
    fn from_index_out_of_range_panics() {
        let _ = Rank::from_index(8);
    }
}
