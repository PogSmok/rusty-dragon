//! Attack lookups for every piece kind.
//!
//! Every table is built at compile time by `const` evaluation.
//!
//! Sliders share one flat table, indexed with magic numbers by default, or with PEXT when
//! `build.rs` enables `cfg(pext)`. The PEXT backend is faster on CPUs that support it.
#![expect(long_running_const_eval, reason = "the lookup tables are large by design")]

#[cfg(all(pext, not(all(target_arch = "x86_64", target_feature = "bmi2"))))]
compile_error!("cfg(pext) requires an x86-64 target with BMI2 support");

use super::{Bitboard, Color, Square};

mod magics;
mod rays;
#[cfg(test)]
mod xorshift;

use magics::{BISHOP_MAGICS, ROOK_MAGICS};
use rays::{BISHOP_DIRECTIONS, Direction, ROOK_DIRECTIONS};

/// Whether slider lookups use PEXT. Set by `build.rs`.
const USE_PEXT: bool = cfg!(pext);

/// One slot per blocker subset of every square's relevant mask, for rooks and bishops.
const SLIDER_TABLE_SIZE: usize = {
    let mut size = 0;
    let mut square_idx = 0;
    while square_idx < 64 {
        let square = Square::from_index(square_idx);
        size += 1 << rays::relevant_mask(square, ROOK_DIRECTIONS).popcount();
        size += 1 << rays::relevant_mask(square, BISHOP_DIRECTIONS).popcount();
        square_idx += 1;
    }
    size
};

#[derive(Clone, Copy, Debug)]
struct SliderEntry {
    /// Squares whose occupancy can change the attacks: each ray without its last square.
    occupancy_mask: Bitboard,
    #[cfg(not(pext))]
    magic: u64,
    #[cfg(not(pext))]
    shift: u32,
    /// Start of this square's slice in `SliderTables::attacks`.
    offset: u32,
}

const _: () = assert!(size_of::<SliderEntry>() == if USE_PEXT { 16 } else { 24 });

impl SliderEntry {
    const EMPTY: Self = Self {
        occupancy_mask: Bitboard::EMPTY,
        #[cfg(not(pext))]
        magic: 0,
        #[cfg(not(pext))]
        shift: 0,
        offset: 0,
    };
}

struct SliderTables {
    rook: [SliderEntry; 64],
    bishop: [SliderEntry; 64],
    attacks: [Bitboard; SLIDER_TABLE_SIZE],
}

impl SliderTables {
    /// Only ever evaluated at compile time, to initialize `SLIDERS`.
    #[expect(clippy::large_stack_arrays, reason = "only evaluated at compile time")]
    const fn new() -> Self {
        let mut tables = Self {
            rook: [SliderEntry::EMPTY; 64],
            bishop: [SliderEntry::EMPTY; 64],
            attacks: [Bitboard::EMPTY; SLIDER_TABLE_SIZE],
        };
        let offset =
            fill_slider(&mut tables.rook, &mut tables.attacks, 0, ROOK_DIRECTIONS, &ROOK_MAGICS);
        fill_slider(
            &mut tables.bishop,
            &mut tables.attacks,
            offset,
            BISHOP_DIRECTIONS,
            &BISHOP_MAGICS,
        );
        tables
    }
}

/// Fills `attacks` table for all squares and occupancy masks for given slider type.
/// Returns offset for the next slider type.
const fn fill_slider(
    entries: &mut [SliderEntry; 64],
    attacks: &mut [Bitboard; SLIDER_TABLE_SIZE],
    mut offset: usize,
    directions: [Direction; 4],
    magics: &[u64; 64],
) -> usize {
    let mut i = 0;
    while i < 64 {
        let square = Square::from_index(i);
        let mask = rays::relevant_mask(square, directions);
        let shift = 64 - mask.popcount();
        let magic = magics[i];

        // Visit every subset of `mask`; `n` counts them, which is the PEXT index.
        let mut subset = Bitboard::EMPTY;
        let mut n = 0;
        loop {
            #[expect(clippy::cast_possible_truncation, reason = "index < 2^12")]
            let index = if USE_PEXT { n } else { (subset.0.wrapping_mul(magic) >> shift) as usize };
            let expected = rays::sliding_attacks(square, subset, directions);
            let slot = &mut attacks[offset + index];
            assert!(
                slot.is_empty() || slot.0 == expected.0,
                "bad magic number, regenerate magics.rs"
            );
            *slot = expected;

            n += 1;
            subset = rays::next_subset(subset, mask);
            if subset.is_empty() {
                break;
            }
        }

        #[expect(clippy::cast_possible_truncation, reason = "offset < SLIDER_TABLE_SIZE")]
        let entry_offset = offset as u32;
        entries[i] = SliderEntry {
            occupancy_mask: mask,
            #[cfg(not(pext))]
            magic,
            #[cfg(not(pext))]
            shift,
            offset: entry_offset,
        };
        offset += 1 << mask.popcount();
        i += 1;
    }
    offset
}

static SLIDERS: SliderTables = SliderTables::new();

static PAWN_ATTACKS: [[Bitboard; 64]; 2] = {
    let mut table = [[Bitboard::EMPTY; 64]; 2];
    let mut square_idx = 0;
    while square_idx < 64 {
        let bb = Bitboard::from_square(Square::from_index(square_idx));
        table[Color::White.index()][square_idx] = pawn_attacks_from(Color::White, bb);
        table[Color::Black.index()][square_idx] = pawn_attacks_from(Color::Black, bb);
        square_idx += 1;
    }
    table
};

static KNIGHT_ATTACKS: [Bitboard; 64] = {
    let mut table = [Bitboard::EMPTY; 64];
    let mut square_idx = 0;
    while square_idx < 64 {
        table[square_idx] =
            knight_attacks_from(Bitboard::from_square(Square::from_index(square_idx)));
        square_idx += 1;
    }
    table
};

static KING_ATTACKS: [Bitboard; 64] = {
    let mut table = [Bitboard::EMPTY; 64];
    let mut square_idx = 0;
    while square_idx < 64 {
        table[square_idx] =
            king_attacks_from(Bitboard::from_square(Square::from_index(square_idx)));
        square_idx += 1;
    }
    table
};

/// Squares attacked by all `pawns` of `color` at once.
const fn pawn_attacks_from(color: Color, pawns: Bitboard) -> Bitboard {
    match color {
        Color::White => pawns.north_west().union(pawns.north_east()),
        Color::Black => pawns.south_west().union(pawns.south_east()),
    }
}

/// Squares attacked by all `knights` at once.
const fn knight_attacks_from(knights: Bitboard) -> Bitboard {
    let n = knights.north();
    let s = knights.south();
    let e = knights.east();
    let w = knights.west();
    // Clockwise from 2 north, 1 east.
    n.north_east() // 2 north, 1 east
        .union(e.north_east()) // 1 north, 2 east
        .union(e.south_east()) // 1 south, 2 east
        .union(s.south_east()) // 2 south, 1 east
        .union(s.south_west()) // 2 south, 1 west
        .union(w.south_west()) // 1 south, 2 west
        .union(w.north_west()) // 1 north, 2 west
        .union(n.north_west()) // 2 north, 1 west
}

/// Squares attacked by all `kings` at once.
const fn king_attacks_from(kings: Bitboard) -> Bitboard {
    // Clockwise from north.
    kings
        .north()
        .union(kings.north_east())
        .union(kings.east())
        .union(kings.south_east())
        .union(kings.south())
        .union(kings.south_west())
        .union(kings.west())
        .union(kings.north_west())
}

#[cfg(pext)]
#[inline]
fn slider_index(entry: &SliderEntry, occupied: Bitboard) -> usize {
    use std::arch::x86_64::_pext_u64;
    // SAFETY: the `compile_error!` at the top of this module rejects `cfg(pext)` unless
    // `target_feature = "bmi2"` is enabled, so the PEXT instruction is available.
    let index = unsafe { _pext_u64(occupied.0, entry.occupancy_mask.0) };
    #[expect(clippy::cast_possible_truncation, reason = "index < 2^12")]
    let index = index as usize;
    entry.offset as usize + index
}

#[cfg(not(pext))]
#[inline]
fn slider_index(entry: &SliderEntry, occupied: Bitboard) -> usize {
    let index = (occupied.0 & entry.occupancy_mask.0).wrapping_mul(entry.magic) >> entry.shift;
    #[expect(clippy::cast_possible_truncation, reason = "index < 2^12")]
    let index = index as usize;
    entry.offset as usize + index
}

#[inline]
pub fn pawn_attacks(color: Color, square: Square) -> Bitboard {
    PAWN_ATTACKS[color.index()][square.index()]
}

#[inline]
pub fn knight_attacks(square: Square) -> Bitboard {
    KNIGHT_ATTACKS[square.index()]
}

#[inline]
pub fn king_attacks(square: Square) -> Bitboard {
    KING_ATTACKS[square.index()]
}

/// Attacks of the slider `entry` describes, given the `occupied` squares.
#[inline]
fn slider_attacks(entry: &SliderEntry, occupied: Bitboard) -> Bitboard {
    let index = slider_index(entry, occupied);
    debug_assert!(index < SLIDER_TABLE_SIZE);
    // SAFETY: It is guaranteed that `slider_index` returns a valid index into the `attacks` table
    unsafe { *SLIDERS.attacks.get_unchecked(index) }
}

#[inline]
pub fn bishop_attacks(square: Square, occupied: Bitboard) -> Bitboard {
    slider_attacks(&SLIDERS.bishop[square.index()], occupied)
}

#[inline]
pub fn rook_attacks(square: Square, occupied: Bitboard) -> Bitboard {
    slider_attacks(&SLIDERS.rook[square.index()], occupied)
}

#[inline]
pub fn queen_attacks(square: Square, occupied: Bitboard) -> Bitboard {
    bishop_attacks(square, occupied) | rook_attacks(square, occupied)
}

static BETWEEN: rays::PairTable = rays::between_table();
static LINE: rays::PairTable = rays::line_table();

/// Squares strictly between `a` and `b` if they share a rank, file or diagonal, otherwise empty.
#[inline]
pub fn between(a: Square, b: Square) -> Bitboard {
    BETWEEN[a.index()][b.index()]
}

/// The whole rank, file or diagonal through `a` and `b`, edge to edge, if they share one,
/// otherwise empty. Also empty when `a == b`.
#[inline]
pub fn line(a: Square, b: Square) -> Bitboard {
    LINE[a.index()][b.index()]
}

#[cfg(test)]
mod tests {
    use super::xorshift::Xorshift64;
    use super::*;

    /// Every subset of `mask`, in the same order the tables are filled.
    fn subsets(mask: Bitboard) -> Vec<Bitboard> {
        let mut all = vec![Bitboard::EMPTY];
        let mut subset = rays::next_subset(Bitboard::EMPTY, mask);
        while !subset.is_empty() {
            all.push(subset);
            subset = rays::next_subset(subset, mask);
        }
        all
    }

    #[test]
    fn pawn_attacks_known_squares() {
        use Square::*;
        assert_eq!(pawn_attacks(Color::White, E4), Bitboard::from_iter([D5, F5]));
        assert_eq!(pawn_attacks(Color::White, A2), Bitboard::from_iter([B3]));
        assert_eq!(pawn_attacks(Color::White, H2), Bitboard::from_iter([G3]));
        assert_eq!(pawn_attacks(Color::Black, E5), Bitboard::from_iter([D4, F4]));
        assert_eq!(pawn_attacks(Color::Black, A7), Bitboard::from_iter([B6]));
        assert_eq!(pawn_attacks(Color::White, D8), Bitboard::EMPTY);
        assert_eq!(pawn_attacks(Color::Black, D1), Bitboard::EMPTY);
    }

    #[test]
    fn knight_attacks_known_squares() {
        use Square::*;
        assert_eq!(knight_attacks(A1), Bitboard::from_iter([B3, C2]));
        assert_eq!(knight_attacks(H8), Bitboard::from_iter([F7, G6]));
        assert_eq!(knight_attacks(B1), Bitboard::from_iter([A3, C3, D2]));
        assert_eq!(knight_attacks(E4), Bitboard::from_iter([D6, F6, G5, G3, F2, D2, C3, C5]));
    }

    #[test]
    fn king_attacks_known_squares() {
        use Square::*;
        assert_eq!(king_attacks(A1), Bitboard::from_iter([A2, B1, B2]));
        assert_eq!(king_attacks(H8), Bitboard::from_iter([G8, G7, H7]));
        assert_eq!(king_attacks(E4), Bitboard::from_iter([D3, D4, D5, E3, E5, F3, F4, F5]));
    }

    #[test]
    fn relevant_masks_exclude_edges() {
        use Square::*;
        let rook = |sq: Square| rays::relevant_mask(sq, ROOK_DIRECTIONS);
        let bishop = |sq: Square| rays::relevant_mask(sq, BISHOP_DIRECTIONS);
        assert_eq!(rook(A1), Bitboard::from_iter([B1, C1, D1, E1, F1, G1, A2, A3, A4, A5, A6, A7]));
        assert_eq!(rook(E4).popcount(), 10);
        assert_eq!(bishop(D4).popcount(), 9);
        assert_eq!(bishop(A1), Bitboard::from_iter([B2, C3, D4, E5, F6, G7]));
    }

    /// CI sets `RUSTY_DRAGON_EXPECTED_SLIDER_BACKEND` so a misconfigured matrix row can't
    /// silently test the wrong slider backend. No-op on local builds.
    #[test]
    fn slider_backend_matches_ci_expectation() {
        let active = if cfg!(pext) { "pext" } else { "magic" };
        if let Some(expected) = option_env!("RUSTY_DRAGON_EXPECTED_SLIDER_BACKEND") {
            assert_eq!(active, expected, "built with the wrong slider backend");
        }
    }

    #[test]
    fn table_size_is_the_known_total() {
        // 102_400 rook slots + 5_248 bishop slots.
        assert_eq!(SLIDER_TABLE_SIZE, 107_648);
    }

    #[test]
    fn subsets_enumerate_in_pext_order() {
        let expected = [
            0b0000_0000,
            0b0001_0000,
            0b0010_0000,
            0b0011_0000,
            0b1000_0000,
            0b1001_0000,
            0b1010_0000,
            0b1011_0000,
        ];
        assert_eq!(subsets(Bitboard(0b1011_0000)), expected.map(Bitboard));
    }

    #[test]
    fn rook_attacks_known_positions() {
        use Square::*;
        let a1_empty = (Bitboard::FILE_A | Bitboard::RANK_1).without_square(A1);
        assert_eq!(rook_attacks(A1, Bitboard::EMPTY), a1_empty);
        // Blockers on e6 and c4 are included, squares behind them are not.
        let occupied = Bitboard::from_iter([E4, E6, C4, H1]);
        assert_eq!(
            rook_attacks(E4, occupied),
            Bitboard::from_iter([E5, E6, D4, C4, F4, G4, H4, E3, E2, E1])
        );
    }

    #[test]
    fn bishop_attacks_known_positions() {
        use Square::*;
        assert_eq!(bishop_attacks(D4, Bitboard::EMPTY).popcount(), 13);
        let occupied = Bitboard::from_iter([D4, F6, B2]);
        assert_eq!(
            bishop_attacks(D4, occupied),
            Bitboard::from_iter([E5, F6, C3, B2, C5, B6, A7, E3, F2, G1])
        );
    }

    #[test]
    fn slider_tables_match_reference() {
        for sq in Square::ALL {
            for occupied in subsets(rays::relevant_mask(sq, ROOK_DIRECTIONS)) {
                let expected = rays::sliding_attacks(sq, occupied, ROOK_DIRECTIONS);
                assert_eq!(rook_attacks(sq, occupied), expected, "rook on {sq}");
            }
            for occupied in subsets(rays::relevant_mask(sq, BISHOP_DIRECTIONS)) {
                let expected = rays::sliding_attacks(sq, occupied, BISHOP_DIRECTIONS);
                assert_eq!(bishop_attacks(sq, occupied), expected, "bishop on {sq}");
            }
        }
    }

    #[test]
    fn between_known_pairs() {
        use Square::*;
        assert_eq!(between(A1, H8), Bitboard::from_iter([B2, C3, D4, E5, F6, G7]));
        assert_eq!(between(E1, H1), Bitboard::from_iter([F1, G1]));
        assert_eq!(between(E4, E1), Bitboard::from_iter([E3, E2]));
        assert_eq!(between(E4, E5), Bitboard::EMPTY);
        assert_eq!(between(A1, B3), Bitboard::EMPTY, "not aligned");
        assert_eq!(between(E4, E4), Bitboard::EMPTY);
    }

    #[test]
    fn line_known_pairs() {
        use Square::*;
        assert_eq!(line(C3, E5), between(A1, H8) | Bitboard::from_iter([A1, H8]));
        assert_eq!(line(E1, E4), Bitboard::FILE_E);
        assert_eq!(line(H3, B3), Bitboard::RANK_3);
        assert_eq!(line(A1, B3), Bitboard::EMPTY, "not aligned");
        assert_eq!(line(E4, E4), Bitboard::EMPTY);
    }

    #[test]
    fn pair_tables_are_symmetric_and_consistent() {
        for a in Square::ALL {
            for b in Square::ALL {
                assert_eq!(between(a, b), between(b, a), "{a} {b}");
                assert_eq!(line(a, b), line(b, a), "{a} {b}");
                let aligned = !line(a, b).is_empty();
                // Aligned squares are exactly those a queen on `a` sees on an empty board.
                assert_eq!(aligned, queen_attacks(a, Bitboard::EMPTY).contains(b), "{a} {b}");
                if aligned {
                    let segment = between(a, b).with_square(a).with_square(b);
                    assert_eq!(segment & line(a, b), segment, "{a} {b}");
                    let blockers = Bitboard::from_square(a) | Bitboard::from_square(b);
                    let seen = queen_attacks(a, blockers) & queen_attacks(b, blockers) & line(a, b);
                    assert_eq!(seen, between(a, b), "{a} {b}");
                }
            }
        }
    }

    #[test]
    fn slider_lookups_ignore_squares_outside_the_mask() {
        let mut rng = Xorshift64::new(Xorshift64::DEFAULT_SEED);
        for occupied in (0..500).flat_map(|_| [rng.next(), rng.sparse()]).map(Bitboard) {
            for sq in Square::ALL {
                let rook = rays::sliding_attacks(sq, occupied, ROOK_DIRECTIONS);
                let bishop = rays::sliding_attacks(sq, occupied, BISHOP_DIRECTIONS);
                assert_eq!(rook_attacks(sq, occupied), rook, "rook on {sq}, {occupied:?}");
                assert_eq!(bishop_attacks(sq, occupied), bishop, "bishop on {sq}, {occupied:?}");
                assert_eq!(queen_attacks(sq, occupied), rook | bishop, "queen on {sq}");
            }
        }
    }
}
