#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
#[must_use]
pub enum PieceKind {
    Pawn,
    Knight,
    Bishop,
    Rook,
    Queen,
    King,
}

impl PieceKind {
    pub const ALL: [Self; 6] =
        [Self::Pawn, Self::Knight, Self::Bishop, Self::Rook, Self::Queen, Self::King];

    #[inline]
    pub const fn from_index(i: usize) -> Self {
        Self::ALL[i]
    }

    #[inline]
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Parses a lowercase FEN letter: 'p', 'n', 'b', 'r', 'q' or 'k'.
    #[inline]
    #[must_use]
    pub const fn from_char(c: char) -> Option<Self> {
        match c {
            'p' => Some(Self::Pawn),
            'n' => Some(Self::Knight),
            'b' => Some(Self::Bishop),
            'r' => Some(Self::Rook),
            'q' => Some(Self::Queen),
            'k' => Some(Self::King),
            _ => None,
        }
    }

    /// Lowercase FEN letter.
    #[inline]
    #[must_use]
    pub fn to_char(self) -> char {
        char::from(b"pnbrqk"[self.index()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_index_matches_discriminant() {
        for i in 0..6 {
            assert_eq!(PieceKind::from_index(i).index(), i);
        }
    }

    #[test]
    #[should_panic(expected = "index out of bounds")]
    fn from_index_out_of_range_panics() {
        let _ = PieceKind::from_index(6);
    }

    #[test]
    fn char_round_trip() {
        for (kind, c) in PieceKind::ALL.into_iter().zip(['p', 'n', 'b', 'r', 'q', 'k']) {
            assert_eq!(kind.to_char(), c);
            assert_eq!(PieceKind::from_char(c), Some(kind));
        }
    }

    #[test]
    fn from_char_rejects_invalid() {
        for c in ['P', 'x', '1', ' '] {
            assert_eq!(PieceKind::from_char(c), None, "accepted {c:?}");
        }
    }
}
