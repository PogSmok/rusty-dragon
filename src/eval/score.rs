use std::fmt;
use std::ops::Neg;

/// A score in centipawns, from the side to move's point of view.
///
/// The range is split into bands, each beating every band below it:
///
/// | Band                | Scores                                     |
/// |---------------------|--------------------------------------------|
/// | evaluations         | `-MAX_EVAL..=MAX_EVAL`                     |
/// | tablebase results   | `TB_WIN_IN_MAX_PLY..=TB_WIN`, and negated  |
/// | mates               | `MATE_IN_MAX_PLY..=MATE`, and negated      |
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Debug)]
#[repr(transparent)]
#[must_use]
pub struct Score(i32);

impl Score {
    pub const DRAW: Self = Self(0);

    /// Above every score, for the bounds of a search window.
    pub const INFINITE: Self = Self(32_001);

    /// Where mate scores count down from: mating in `n` plies is `MATE - n`,
    /// and being checkmated now is `-MATE`.
    /// No position scores `MATE` itself, since a mate takes a move.
    pub const MATE: Self = Self(32_000);

    /// Plies a mate or tablebase score can count, which also bounds the search depth.
    pub const MAX_PLY: i32 = 256;

    /// The slowest mate a score can show.
    pub const MATE_IN_MAX_PLY: Self = Self(Self::MATE.0 - Self::MAX_PLY);

    /// A tablebase win, reached `n` plies from the root as `TB_WIN - n`.
    pub const TB_WIN: Self = Self(Self::MATE_IN_MAX_PLY.0 - 1);

    /// The furthest tablebase win a score can show.
    pub const TB_WIN_IN_MAX_PLY: Self = Self(Self::TB_WIN.0 - Self::MAX_PLY);

    /// The largest evaluation, kept clear of the tablebase and mate bands.
    pub const MAX_EVAL: Self = Self(Self::TB_WIN_IN_MAX_PLY.0 - 1);

    /// An evaluation of `centipawns`, which must be within [`Score::MAX_EVAL`].
    #[inline]
    pub const fn new(centipawns: i32) -> Self {
        debug_assert!(
            -Self::MAX_EVAL.0 <= centipawns && centipawns <= Self::MAX_EVAL.0,
            "evaluation out of range"
        );
        Self(centipawns)
    }

    /// The side to move mates in `ply` plies, at least 1: it has to move to mate.
    #[inline]
    pub const fn mate_in(ply: i32) -> Self {
        debug_assert!(1 <= ply && ply <= Self::MAX_PLY, "mate distance out of range");
        Self(Self::MATE.0 - ply)
    }

    /// The side to move is mated in `ply` plies: 0 if it is checkmated now.
    #[inline]
    pub const fn mated_in(ply: i32) -> Self {
        debug_assert!(0 <= ply && ply <= Self::MAX_PLY, "mate distance out of range");
        Self(-Self::MATE.0 + ply)
    }

    /// The raw value: centipawns for an evaluation, or the band's encoding otherwise.
    #[inline]
    #[must_use]
    pub const fn value(self) -> i32 {
        self.0
    }

    /// Whether this is a forced mate, for either side.
    #[inline]
    #[must_use]
    pub const fn is_mate(self) -> bool {
        let magnitude = self.0.abs();
        Self::MATE_IN_MAX_PLY.0 <= magnitude && magnitude <= Self::MATE.0
    }

    /// Moves until mate, as UCI shows it: negative when the side to move is being mated.
    #[inline]
    #[must_use]
    pub const fn mate_moves(self) -> Option<i32> {
        if !self.is_mate() {
            return None;
        }
        // Mate in 1, 3 or 5 plies is mate in 1, 2 or 3 moves.
        Some(if self.0 > 0 {
            (Self::MATE.0 - self.0 + 1) / 2
        } else {
            -(Self::MATE.0 + self.0) / 2
        })
    }
}

impl Neg for Score {
    type Output = Self;

    #[inline]
    fn neg(self) -> Self {
        Self(-self.0)
    }
}

/// UCI notation: `cp <centipawns>`, or `mate <moves>`.
impl fmt::Display for Score {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.mate_moves() {
            Some(moves) => write!(f, "mate {moves}"),
            None => write!(f, "cp {}", self.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_are_ordered_and_fit_in_16_bits() {
        let bands = [
            Score::DRAW,
            Score::MAX_EVAL,
            Score::TB_WIN_IN_MAX_PLY,
            Score::TB_WIN,
            Score::MATE_IN_MAX_PLY,
            Score::MATE,
            Score::INFINITE,
        ];
        assert!(bands.windows(2).all(|pair| pair[0] < pair[1]), "{bands:?}");
        assert!(i16::try_from(Score::INFINITE.value()).is_ok());
        assert!(i16::try_from((-Score::INFINITE).value()).is_ok());
    }

    #[test]
    fn mate_scores_count_moves() {
        assert_eq!(Score::mate_in(1).mate_moves(), Some(1));
        assert_eq!(Score::mate_in(3).mate_moves(), Some(2));
        assert_eq!(Score::mated_in(0).mate_moves(), Some(0));
        assert_eq!(Score::mated_in(2).mate_moves(), Some(-1));
        assert_eq!(Score::mated_in(4).mate_moves(), Some(-2));
        assert_eq!(Score::new(150).mate_moves(), None);
        assert_eq!(Score::TB_WIN.mate_moves(), None);
        assert!(!Score::INFINITE.is_mate() && !(-Score::INFINITE).is_mate());
        // A faster mate scores higher.
        assert!(Score::mate_in(1) > Score::mate_in(3));
        assert!(Score::mated_in(4) > Score::mated_in(2));
    }

    #[test]
    fn display_uses_uci_notation() {
        assert_eq!(Score::new(35).to_string(), "cp 35");
        assert_eq!(Score::new(-120).to_string(), "cp -120");
        assert_eq!(Score::mate_in(5).to_string(), "mate 3");
        assert_eq!(Score::mated_in(2).to_string(), "mate -1");
    }

    #[test]
    fn negation_swaps_sides() {
        assert_eq!(-Score::new(40), Score::new(-40));
        assert_eq!(-Score::mate_in(3), Score::mated_in(3));
        assert_eq!(-Score::DRAW, Score::DRAW);
    }
}
