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
