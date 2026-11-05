use std::mem;

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

// `from_index` transmutes any index below `ALL.len()`, so each must be a discriminant.
const _: () = {
    let mut piece_kind_idx = 0;
    while piece_kind_idx < PieceKind::ALL.len() {
        assert!(
            PieceKind::ALL[piece_kind_idx] as usize == piece_kind_idx,
            "discriminants must be consecutive, starting at 0"
        );
        piece_kind_idx += 1;
    }
};

impl PieceKind {
    pub const ALL: [Self; 6] =
        [Self::Pawn, Self::Knight, Self::Bishop, Self::Rook, Self::Queen, Self::King];

    /// Panics if `i` is not below 6.
    #[inline]
    pub const fn from_index(i: usize) -> Self {
        assert!(i < 6, "piece kind index out of range");
        #[expect(clippy::cast_possible_truncation, reason = "i < 6")]
        let discriminant = i as u8;
        // SAFETY: `PieceKind` is `repr(u8)` with the discriminants 0..6, and `i < 6`.
        unsafe { mem::transmute::<u8, Self>(discriminant) }
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
    #[should_panic(expected = "piece kind index out of range")]
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
