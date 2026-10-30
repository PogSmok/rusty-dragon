#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
#[must_use]
#[rustfmt::skip]
pub enum File {
    A, B, C, D, E, F, G, H
}

impl File {
    #[rustfmt::skip]
    pub const ALL: [Self; 8] = [
        Self::A, Self::B, Self::C, Self::D, Self::E, Self::F, Self::G, Self::H
    ];

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
            assert_eq!(File::from_index(i) as u8, i);
        }
    }

    #[test]
    #[should_panic(expected = "index out of bounds")]
    fn from_index_out_of_range_panics() {
        let _ = File::from_index(8);
    }
}
