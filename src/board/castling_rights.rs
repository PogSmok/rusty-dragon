use std::fmt::{self, Write};

use super::{Color, File, MoveKind, Rank, Square};

/// One of the four castling moves in standard chess
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Castle {
    pub right: CastlingRights,
    pub color: Color,
    pub kind: MoveKind,
    pub king_from: Square,
    pub king_to: Square,
    pub rook_from: Square,
    pub rook_to: Square,
}

impl Castle {
    pub const WHITE_KINGSIDE: Self = Self::new(Color::White, true);
    pub const WHITE_QUEENSIDE: Self = Self::new(Color::White, false);
    pub const BLACK_KINGSIDE: Self = Self::new(Color::Black, true);
    pub const BLACK_QUEENSIDE: Self = Self::new(Color::Black, false);

    const fn new(color: Color, kingside: bool) -> Self {
        let rank = match color {
            Color::White => Rank::R1,
            Color::Black => Rank::R8,
        };
        let king_from = Square::from_file_rank(File::E, rank);
        if kingside {
            Self {
                right: CastlingRights::kingside(color),
                color,
                kind: MoveKind::KingsideCastle,
                king_from,
                king_to: Square::from_file_rank(File::G, rank),
                rook_from: Square::from_file_rank(File::H, rank),
                rook_to: Square::from_file_rank(File::F, rank),
            }
        } else {
            Self {
                right: CastlingRights::queenside(color),
                color,
                kind: MoveKind::QueensideCastle,
                king_from,
                king_to: Square::from_file_rank(File::C, rank),
                rook_from: Square::from_file_rank(File::A, rank),
                rook_to: Square::from_file_rank(File::D, rank),
            }
        }
    }

    pub const ALL: [Self; 4] =
        [Self::WHITE_KINGSIDE, Self::WHITE_QUEENSIDE, Self::BLACK_KINGSIDE, Self::BLACK_QUEENSIDE];

    /// `color`'s kingside and queenside castles.
    #[inline]
    #[must_use]
    pub const fn of(color: Color) -> [Self; 2] {
        match color {
            Color::White => [Self::WHITE_KINGSIDE, Self::WHITE_QUEENSIDE],
            Color::Black => [Self::BLACK_KINGSIDE, Self::BLACK_QUEENSIDE],
        }
    }
}

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
                f.write_char(c)?;
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
        let mut all = CastlingRights::NONE;
        for color in Color::ALL {
            all = all.with(CastlingRights::kingside(color)).with(CastlingRights::queenside(color));
        }
        assert_eq!(all, CastlingRights::ALL);
    }

    #[test]
    fn with_and_without() {
        let rights = CastlingRights::NONE.with(CastlingRights::WHITE_KINGSIDE);
        assert!(rights.contains(CastlingRights::WHITE_KINGSIDE));
        assert!(!rights.contains(CastlingRights::WHITE_QUEENSIDE));
        assert!(rights.without(CastlingRights::WHITE_KINGSIDE).is_empty());
    }

    #[test]
    fn castles_use_the_standard_squares() {
        let white_kingside = Castle::WHITE_KINGSIDE;
        assert_eq!(white_kingside.right, CastlingRights::WHITE_KINGSIDE);
        assert_eq!(white_kingside.kind, MoveKind::KingsideCastle);
        assert_eq!([white_kingside.king_from, white_kingside.king_to], [Square::E1, Square::G1]);
        assert_eq!([white_kingside.rook_from, white_kingside.rook_to], [Square::H1, Square::F1]);
        let black_queenside = Castle::BLACK_QUEENSIDE;
        assert_eq!(black_queenside.right, CastlingRights::BLACK_QUEENSIDE);
        assert_eq!(black_queenside.kind, MoveKind::QueensideCastle);
        assert_eq!([black_queenside.king_from, black_queenside.king_to], [Square::E8, Square::C8]);
        assert_eq!([black_queenside.rook_from, black_queenside.rook_to], [Square::A8, Square::D8]);

        for castle in Castle::ALL {
            assert!(Castle::of(castle.color).contains(&castle), "{castle:?}");
        }
        let rights = Castle::ALL.iter().fold(CastlingRights::NONE, |all, c| all.with(c.right));
        assert_eq!(rights, CastlingRights::ALL);
    }

    #[test]
    fn display_as_fen() {
        assert_eq!(CastlingRights::ALL.to_string(), "KQkq");
        assert_eq!(CastlingRights::NONE.to_string(), "-");
        let rights = CastlingRights::WHITE_QUEENSIDE.with(CastlingRights::BLACK_KINGSIDE);
        assert_eq!(rights.to_string(), "Qk");
    }
}
