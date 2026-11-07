//! Zobrist hashing: a 64-bit key per position, updated as moves are made.

use super::{CastlingRights, Piece, Square};

struct Keys {
    /// Indexed by `Piece::index()`, then `Square::index()`. The unused piece indices stay 0.
    pieces: [[u64; 64]; Piece::INDEX_COUNT],
    /// Indexed by `CastlingRights::index()`. No rights has value 0.
    castling: [u64; 16],
    /// Indexed by the en passant square's file. If there is no en passant square value is 0.
    en_passant: [u64; 9],
    black_to_move: u64,
}

static KEYS: Keys = Keys::new();

struct SplitMix64(u64);

impl SplitMix64 {
    /// Arbitrary: any value works, 0 included.
    const DEFAULT_SEED: u64 = 0x5EED;

    const fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

impl Keys {
    const fn new() -> Self {
        let mut rng = SplitMix64(SplitMix64::DEFAULT_SEED);

        let mut pieces = [[0; 64]; Piece::INDEX_COUNT];
        let mut p = 0;
        while p < Piece::ALL.len() {
            let piece = Piece::ALL[p].index();
            let mut square = 0;
            while square < 64 {
                pieces[piece][square] = rng.next();
                square += 1;
            }
            p += 1;
        }

        let rights = [rng.next(), rng.next(), rng.next(), rng.next()];
        let mut castling = [0; 16];
        let mut set = 0;
        while set < castling.len() {
            let mut bit = 0;
            while bit < rights.len() {
                if set & (1 << bit) != 0 {
                    castling[set] ^= rights[bit];
                }
                bit += 1;
            }
            set += 1;
        }

        let mut en_passant = [0; 9];
        let mut file = 0;
        while file < 8 {
            en_passant[file] = rng.next();
            file += 1;
        }

        Self { pieces, castling, en_passant, black_to_move: rng.next() }
    }
}

#[inline]
pub(super) fn piece(piece: Piece, square: Square) -> u64 {
    KEYS.pieces[piece.index()][square.index()]
}

#[inline]
pub(super) fn castling(rights: CastlingRights) -> u64 {
    KEYS.castling[rights.index()]
}

/// Keyed by file: the rank always follows from the side to move. 0 for no square.
#[inline]
pub(super) fn en_passant(square: Option<Square>) -> u64 {
    KEYS.en_passant[square.map_or(8, |square| square.file().index())]
}

#[inline]
pub(super) fn black_to_move() -> u64 {
    KEYS.black_to_move
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn keys_are_distinct_and_nonzero() {
        let mut keys: Vec<u64> = Piece::ALL
            .iter()
            .flat_map(|&p| Square::ALL.iter().map(move |&sq| piece(p, sq)))
            .collect();
        keys.extend((0..8).map(|file| KEYS.en_passant[file]));
        keys.extend((0..4).map(|bit| KEYS.castling[1 << bit]));
        keys.push(black_to_move());
        assert!(keys.iter().all(|&key| key != 0));
        assert_eq!(keys.iter().collect::<HashSet<_>>().len(), keys.len());
        assert_eq!(en_passant(None), 0);
    }

    #[test]
    fn castling_keys_combine_the_rights() {
        // All 16 sets, the empty one included, get distinct keys.
        assert_eq!(KEYS.castling.iter().collect::<HashSet<_>>().len(), KEYS.castling.len());
        assert_eq!(castling(CastlingRights::NONE), 0);
        let both = CastlingRights::WHITE_KINGSIDE.with(CastlingRights::BLACK_QUEENSIDE);
        assert_eq!(
            castling(both),
            castling(CastlingRights::WHITE_KINGSIDE) ^ castling(CastlingRights::BLACK_QUEENSIDE)
        );
    }
}
