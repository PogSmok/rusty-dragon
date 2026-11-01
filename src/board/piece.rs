use super::{Color, PieceKind};

/// A colored piece, white is 0-5 and black is 8-13 to allow quick bitwise lookups
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
#[must_use]
pub enum Piece {
    WhitePawn = 0,
    WhiteKnight,
    WhiteBishop,
    WhiteRook,
    WhiteQueen,
    WhiteKing,
    BlackPawn = 8,
    BlackKnight,
    BlackBishop,
    BlackRook,
    BlackQueen,
    BlackKing,
}

const _: () = assert!(size_of::<Option<Piece>>() == 1);

impl Piece {
    pub const ALL: [Self; 12] = [
        Self::WhitePawn,
        Self::WhiteKnight,
        Self::WhiteBishop,
        Self::WhiteRook,
        Self::WhiteQueen,
        Self::WhiteKing,
        Self::BlackPawn,
        Self::BlackKnight,
        Self::BlackBishop,
        Self::BlackRook,
        Self::BlackQueen,
        Self::BlackKing,
    ];

    #[inline]
    pub const fn new(color: Color, kind: PieceKind) -> Self {
        Self::ALL[color.index() * 6 + kind.index()]
    }

    /// Number of distinct `index()` values, including the unused 6, 7.
    pub const INDEX_COUNT: usize = 14;

    #[inline]
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    #[inline]
    pub const fn color(self) -> Color {
        if self.index() & 8 == 0 { Color::White } else { Color::Black }
    }

    #[inline]
    pub const fn kind(self) -> PieceKind {
        match self {
            Self::WhitePawn | Self::BlackPawn => PieceKind::Pawn,
            Self::WhiteKnight | Self::BlackKnight => PieceKind::Knight,
            Self::WhiteBishop | Self::BlackBishop => PieceKind::Bishop,
            Self::WhiteRook | Self::BlackRook => PieceKind::Rook,
            Self::WhiteQueen | Self::BlackQueen => PieceKind::Queen,
            Self::WhiteKing | Self::BlackKing => PieceKind::King,
        }
    }

    /// Parses a FEN letter: uppercase for White (`PNBRQK`), lowercase for Black (`pnbrqk`).
    #[inline]
    #[must_use]
    pub const fn from_char(c: char) -> Option<Self> {
        let color = if c.is_ascii_uppercase() { Color::White } else { Color::Black };
        match PieceKind::from_char(c.to_ascii_lowercase()) {
            Some(kind) => Some(Self::new(color, kind)),
            None => None,
        }
    }

    #[inline]
    #[must_use]
    pub fn to_char(self) -> char {
        let c = self.kind().to_char();
        match self.color() {
            Color::White => c.to_ascii_uppercase(),
            Color::Black => c,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_round_trips_color_and_kind() {
        for color in Color::ALL {
            for kind in PieceKind::ALL {
                let piece = Piece::new(color, kind);
                assert_eq!(piece.color(), color);
                assert_eq!(piece.kind(), kind);
            }
        }
    }

    #[test]
    fn known_piece() {
        assert_eq!(Piece::new(Color::Black, PieceKind::Queen), Piece::BlackQueen);
    }

    #[test]
    fn char_round_trip() {
        for piece in Piece::ALL {
            assert_eq!(Piece::from_char(piece.to_char()), Some(piece));
        }
    }

    #[test]
    fn char_case_selects_color() {
        assert_eq!(Piece::from_char('N'), Some(Piece::WhiteKnight));
        assert_eq!(Piece::from_char('n'), Some(Piece::BlackKnight));
    }

    #[test]
    fn from_char_rejects_invalid() {
        for c in ['x', 'X', '1', ' ', '/'] {
            assert_eq!(Piece::from_char(c), None, "accepted {c:?}");
        }
    }
}
