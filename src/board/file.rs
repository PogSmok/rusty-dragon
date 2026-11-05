use std::mem;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
#[must_use]
#[rustfmt::skip]
pub enum File {
    A, B, C, D, E, F, G, H
}

// `from_index` transmutes any index below `ALL.len()`, so each must be a discriminant.
const _: () = {
    let mut file_idx = 0;
    while file_idx < File::ALL.len() {
        assert!(
            File::ALL[file_idx] as usize == file_idx,
            "discriminants must be consecutive, starting at 0"
        );
        file_idx += 1;
    }
};

impl File {
    #[rustfmt::skip]
    pub const ALL: [Self; 8] = [
        Self::A, Self::B, Self::C, Self::D, Self::E, Self::F, Self::G, Self::H
    ];

    /// Panics if `i` is not below 8.
    #[inline]
    pub const fn from_index(i: usize) -> Self {
        assert!(i < 8, "file index out of range");
        #[expect(clippy::cast_possible_truncation, reason = "i < 8")]
        let discriminant = i as u8;
        // SAFETY: `File` is `repr(u8)` with the discriminants 0..8, and `i < 8`.
        unsafe { mem::transmute::<u8, Self>(discriminant) }
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
            'a' => Some(Self::A),
            'b' => Some(Self::B),
            'c' => Some(Self::C),
            'd' => Some(Self::D),
            'e' => Some(Self::E),
            'f' => Some(Self::F),
            'g' => Some(Self::G),
            'h' => Some(Self::H),
            _ => None,
        }
    }

    #[inline]
    #[must_use]
    pub fn to_char(self) -> char {
        char::from(b"abcdefgh"[self.index()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_index_matches_discriminant() {
        for i in 0..8 {
            assert_eq!(File::from_index(i).index(), i);
        }
    }

    #[test]
    #[should_panic(expected = "file index out of range")]
    fn from_index_out_of_range_panics() {
        let _ = File::from_index(8);
    }

    #[test]
    fn char_round_trip() {
        for (file, c) in File::ALL.into_iter().zip('a'..='h') {
            assert_eq!(file.to_char(), c);
            assert_eq!(File::from_char(c), Some(file));
        }
    }

    #[test]
    fn from_char_rejects_invalid() {
        for c in ['i', 'A', '1', '9', ' ', '١'] {
            assert_eq!(File::from_char(c), None, "accepted {c:?}");
        }
    }
}
