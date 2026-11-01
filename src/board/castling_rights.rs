use std::fmt;

use super::Color;

/// Set of the four castling rights, one bit each.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
#[must_use]
#[repr(transparent)]
pub struct CastlingRights(u8);

impl CastlingRights {
    pub const NONE: Self = Self(0);
    pub const WHITE_KINGSIDE: Self = Self(1 << 0);
    pub const WHITE_QUEENSIDE: Self = Self(1 << 1);
    pub const BLACK_KINGSIDE: Self = Self(1 << 2);
    pub const BLACK_QUEENSIDE: Self = Self(1 << 3);
    pub const ALL: Self = Self(0b1111);

    /// Kingside right for `color`.
    #[inline]
    pub const fn kingside(color: Color) -> Self {
        match color {
            Color::White => Self::WHITE_KINGSIDE,
            Color::Black => Self::BLACK_KINGSIDE,
        }
    }

    /// Queenside right for `color`.
    #[inline]
    pub const fn queenside(color: Color) -> Self {
        match color {
            Color::White => Self::WHITE_QUEENSIDE,
            Color::Black => Self::BLACK_QUEENSIDE,
        }
    }

    /// Both rights for `color`.
    #[inline]
    pub const fn both(color: Color) -> Self {
        Self(Self::kingside(color).0 | Self::queenside(color).0)
    }

    #[inline]
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == Self::NONE.0
    }

    /// True if every right in `rights` is present.
    #[inline]
    #[must_use]
    pub const fn contains(self, rights: Self) -> bool {
        self.0 & rights.0 == rights.0
    }

    #[inline]
    pub const fn with(self, rights: Self) -> Self {
        Self(self.0 | rights.0)
    }

    #[inline]
    pub const fn without(self, rights: Self) -> Self {
        Self(self.0 & !rights.0)
    }
}

/// FEN notation: `KQkq`, a subset of it, or `-` when no rights remain.
impl fmt::Display for CastlingRights {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return f.write_str("-");
        }
        for (right, c) in [
            (Self::WHITE_KINGSIDE, 'K'),
            (Self::WHITE_QUEENSIDE, 'Q'),
            (Self::BLACK_KINGSIDE, 'k'),
            (Self::BLACK_QUEENSIDE, 'q'),
        ] {
            if self.contains(right) {
                write!(f, "{c}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_contains_every_right() {
        for color in Color::ALL {
            assert!(CastlingRights::ALL.contains(CastlingRights::kingside(color)));
            assert!(CastlingRights::ALL.contains(CastlingRights::queenside(color)));
        }
        assert_eq!(
            CastlingRights::both(Color::White).with(CastlingRights::both(Color::Black)),
            CastlingRights::ALL
        );
    }

    #[test]
    fn with_and_without() {
        let rights = CastlingRights::NONE.with(CastlingRights::WHITE_KINGSIDE);
        assert!(rights.contains(CastlingRights::WHITE_KINGSIDE));
        assert!(!rights.contains(CastlingRights::WHITE_QUEENSIDE));
        assert!(rights.without(CastlingRights::WHITE_KINGSIDE).is_empty());
    }

    #[test]
    fn king_move_removes_both_rights() {
        let rights = CastlingRights::ALL.without(CastlingRights::both(Color::White));
        assert_eq!(rights, CastlingRights::both(Color::Black));
    }

    #[test]
    fn display_as_fen() {
        assert_eq!(CastlingRights::ALL.to_string(), "KQkq");
        assert_eq!(CastlingRights::NONE.to_string(), "-");
        let rights = CastlingRights::WHITE_QUEENSIDE.with(CastlingRights::BLACK_KINGSIDE);
        assert_eq!(rights.to_string(), "Qk");
    }
}
