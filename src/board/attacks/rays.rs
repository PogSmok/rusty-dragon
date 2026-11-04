//! Slider ray geometry.
//!
//! Only used at compile time to build the slider tables, and in tests as the reference to check
//! them against.

use super::{Bitboard, Square};

/// A direction a slider can move in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Direction {
    North,
    NorthEast,
    East,
    SouthEast,
    South,
    SouthWest,
    West,
    NorthWest,
}

impl Direction {
    const ALL: [Self; 8] = [
        Self::North,
        Self::NorthEast,
        Self::East,
        Self::SouthEast,
        Self::South,
        Self::SouthWest,
        Self::West,
        Self::NorthWest,
    ];

    const DIRECTION_COUNT: usize = Self::ALL.len();

    /// (file step, rank step) for one move in this direction.
    #[inline]
    #[must_use]
    const fn step(self) -> (i8, i8) {
        match self {
            Self::North => (0, 1),
            Self::NorthEast => (1, 1),
            Self::East => (1, 0),
            Self::SouthEast => (1, -1),
            Self::South => (0, -1),
            Self::SouthWest => (-1, -1),
            Self::West => (-1, 0),
            Self::NorthWest => (-1, 1),
        }
    }

    /// Whether moving this way goes towards higher square indices
    #[inline]
    #[must_use]
    const fn is_positive(self) -> bool {
        matches!(self, Self::North | Self::NorthEast | Self::East | Self::NorthWest)
    }

    #[inline]
    #[must_use]
    const fn index(self) -> usize {
        self as usize
    }
}

pub(super) const ROOK_DIRECTIONS: [Direction; 4] =
    [Direction::North, Direction::East, Direction::South, Direction::West];
pub(super) const BISHOP_DIRECTIONS: [Direction; 4] =
    [Direction::NorthEast, Direction::SouthEast, Direction::SouthWest, Direction::NorthWest];

/// Squares along a ray from `square` in `direction`, excluding `square` itself.
const fn generate_ray(square: Square, direction: Direction) -> Bitboard {
    let (file_step, rank_step) = direction.step();
    #[expect(clippy::cast_possible_truncation, reason = "file and rank < 8")]
    let (mut file, mut rank) = (square.file().index() as i8, square.rank().index() as i8);
    let mut ray = Bitboard::EMPTY;
    loop {
        file += file_step;
        rank += rank_step;
        if file < 0 || file >= 8 || rank < 0 || rank >= 8 {
            return ray;
        }
        #[expect(clippy::cast_sign_loss, reason = "file and rank are in 0..8 here")]
        let index = (rank * 8 + file) as usize;
        ray = ray.with_square(Square::from_index(index));
    }
}

/// `RAYS[square][direction]`: every square from `square` to the edge in `direction`.
const RAYS: [[Bitboard; Direction::DIRECTION_COUNT]; 64] = {
    let mut rays = [[Bitboard::EMPTY; Direction::DIRECTION_COUNT]; 64];
    let mut square_idx = 0;

    while square_idx < 64 {
        let mut direction_idx = 0;
        while direction_idx < Direction::DIRECTION_COUNT {
            let direction = Direction::ALL[direction_idx];
            rays[square_idx][direction.index()] =
                generate_ray(Square::from_index(square_idx), direction);
            direction_idx += 1;
        }
        square_idx += 1;
    }

    rays
};

/// Nearest square of the nonempty `squares` when looking along `direction`.
const fn first_blocker(squares: Bitboard, direction: Direction) -> Square {
    debug_assert!(!squares.is_empty());
    let index =
        if direction.is_positive() { squares.0.trailing_zeros() } else { squares.0.ilog2() };
    Square::from_index(index as usize)
}

/// Farthest square of the nonempty `ray` in `direction`, i.e. the one on the board edge.
const fn ray_end(ray: Bitboard, direction: Direction) -> Square {
    debug_assert!(!ray.is_empty());
    let index = if direction.is_positive() { ray.0.ilog2() } else { ray.0.trailing_zeros() };
    Square::from_index(index as usize)
}

/// Squares whose occupancy can change a slider's attacks from `square`.
/// A piece on the end of a ray blocks nothing behind it, so ray ends are excluded.
pub(super) const fn relevant_mask(square: Square, directions: [Direction; 4]) -> Bitboard {
    let mut mask = Bitboard::EMPTY;
    let mut direction_idx = 0;
    while direction_idx < directions.len() {
        let direction = directions[direction_idx];
        let ray = RAYS[square.index()][direction.index()];
        if !ray.is_empty() {
            mask = mask.union(ray.without_square(ray_end(ray, direction)));
        }
        direction_idx += 1;
    }
    mask
}

/// Squares a slider on `square` attacks given the `occupied` squares.
pub(super) const fn sliding_attacks(
    square: Square,
    occupied: Bitboard,
    directions: [Direction; 4],
) -> Bitboard {
    let square = square.index();
    let mut attacks = Bitboard::EMPTY;
    let mut direction_idx = 0;
    while direction_idx < directions.len() {
        let direction = directions[direction_idx];
        let ray = RAYS[square][direction.index()];
        let blockers = ray.intersection(occupied);

        let attacked = if blockers.is_empty() {
            ray
        } else {
            // Ray of the first blocker in this direction contains all blocked squares
            // so removing it from the ray leaves only the attacked squares.
            ray.difference(RAYS[first_blocker(blockers, direction).index()][direction.index()])
        };
        attacks = attacks.union(attacked);
        direction_idx += 1;
    }
    attacks
}

/// The subset of `mask` after `subset`, in increasing numeric order.
/// Starting from empty, it visits every subset and wraps back to empty after `mask` itself.
///
/// Increasing order allows direct PEXT mapping, nth subset of `mask` is the nth PEXT of `mask`.
///
/// e.g. for mask = 0b1010, the subsets are visited in this order:
///
/// 0b0000 → 0b0010 → 0b1000 → 0b1010 → 0b0000
///
pub(super) const fn next_subset(subset: Bitboard, mask: Bitboard) -> Bitboard {
    debug_assert!(subset.difference(mask).is_empty(), "subset has bits outside mask");
    Bitboard(subset.0.wrapping_sub(mask.0) & mask.0)
}

#[cfg(test)]
mod tests {
    use super::super::xorshift::Xorshift64;
    use super::*;

    #[test]
    fn is_positive_matches_square_offset() {
        for direction in Direction::ALL {
            let (file_step, rank_step) = direction.step();
            let offset = i32::from(rank_step) * 8 + i32::from(file_step);
            assert_eq!(direction.is_positive(), offset > 0, "{direction:?}");
        }
    }

    #[test]
    fn all_lists_each_direction_at_its_index() {
        for (i, direction) in Direction::ALL.into_iter().enumerate() {
            assert_eq!(direction.index(), i, "{direction:?}");
        }
    }

    /// Naive implementation of sliding attacks, independent of the precomputed rays,
    /// used to verify the correctness of the optimized implementation.
    fn walk_attacks(square: Square, occupied: Bitboard, directions: [Direction; 4]) -> Bitboard {
        let mut attacks = Bitboard::EMPTY;
        for direction in directions {
            let (file_step, rank_step) = direction.step();
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_possible_wrap,
                reason = "file and rank < 8"
            )]
            let (mut file, mut rank) = (square.file().index() as i32, square.rank().index() as i32);
            loop {
                file += i32::from(file_step);
                rank += i32::from(rank_step);
                if !(0..8).contains(&file) || !(0..8).contains(&rank) {
                    break;
                }
                #[expect(clippy::cast_sign_loss, reason = "file and rank are in 0..8")]
                let target = Square::from_index((rank * 8 + file) as usize);
                attacks = attacks.with_square(target);
                if occupied.contains(target) {
                    break;
                }
            }
        }
        attacks
    }

    #[test]
    fn sliding_attacks_match_walk() {
        let mut rng = Xorshift64::new(Xorshift64::DEFAULT_SEED);
        for _ in 0..1000 {
            let occupied = Bitboard(rng.next());
            for square in Square::ALL {
                for directions in [ROOK_DIRECTIONS, BISHOP_DIRECTIONS] {
                    assert_eq!(
                        sliding_attacks(square, occupied, directions),
                        walk_attacks(square, occupied, directions),
                        "square {square}, occupied {occupied:?}"
                    );
                }
            }
        }
    }
}
