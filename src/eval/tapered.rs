use std::fmt;
use std::ops::{Add, AddAssign, Neg, Sub, SubAssign};

/// A middlegame and an endgame value packed into one `i32`, so one add updates both.
///
/// The endgame value sits in the high 16 bits and the middlegame value in the low 16.
/// Each half must stay within `i16`, or it spills into the other; debug builds check it.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
#[must_use]
pub struct Tapered(i32);

impl Tapered {
    pub const ZERO: Self = Self(0);

    #[inline]
    pub const fn new(mg: i16, eg: i16) -> Self {
        Self(((eg as i32) << 16).wrapping_add(mg as i32))
    }

    /// The middlegame value.
    #[inline]
    #[must_use]
    pub const fn mg(self) -> i16 {
        #[expect(clippy::cast_possible_truncation, reason = "the low 16 bits are the value")]
        let mg = self.0 as i16;
        mg
    }

    /// The endgame value.
    #[inline]
    #[must_use]
    pub const fn eg(self) -> i16 {
        // Adding 0x8000 undoes the borrow a negative middlegame value took from this half.
        // The shift leaves a value that fits in 16 bits.
        (self.0.wrapping_add(0x8000) >> 16) as i16
    }

    /// Panics in debug builds unless both halves of a result fit in `i16`.
    /// A half that didn't would have spilled into the other.
    #[inline]
    fn debug_check_halves(mg: i32, eg: i32) {
        debug_assert!(
            i16::try_from(mg).is_ok() && i16::try_from(eg).is_ok(),
            "a half of a Tapered result is outside i16: mg {mg}, eg {eg}"
        );
    }
}

impl Add for Tapered {
    type Output = Self;

    #[inline]
    fn add(self, rhs: Self) -> Self {
        Self::debug_check_halves(
            i32::from(self.mg()) + i32::from(rhs.mg()),
            i32::from(self.eg()) + i32::from(rhs.eg()),
        );
        Self(self.0.wrapping_add(rhs.0))
    }
}

impl Sub for Tapered {
    type Output = Self;

    #[inline]
    fn sub(self, rhs: Self) -> Self {
        Self::debug_check_halves(
            i32::from(self.mg()) - i32::from(rhs.mg()),
            i32::from(self.eg()) - i32::from(rhs.eg()),
        );
        Self(self.0.wrapping_sub(rhs.0))
    }
}

impl Neg for Tapered {
    type Output = Self;

    #[inline]
    fn neg(self) -> Self {
        Self::debug_check_halves(-i32::from(self.mg()), -i32::from(self.eg()));
        Self(self.0.wrapping_neg())
    }
}

impl AddAssign for Tapered {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl SubAssign for Tapered {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}

/// Shows the two halves, not the packed number.
impl fmt::Debug for Tapered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tapered").field("mg", &self.mg()).field("eg", &self.eg()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EDGES: [i16; 7] = [i16::MIN, -32_767, -1, 0, 1, 32_766, i16::MAX];

    #[test]
    fn halves_round_trip_through_the_packing() {
        for mg in EDGES {
            for eg in EDGES {
                let t = Tapered::new(mg, eg);
                assert_eq!((t.mg(), t.eg()), (mg, eg), "mg {mg}, eg {eg}");
            }
        }
    }

    #[test]
    fn arithmetic_works_on_each_half() {
        let a = Tapered::new(-300, 250);
        let b = Tapered::new(120, -400);
        assert_eq!(a + b, Tapered::new(-180, -150));
        assert_eq!(a - b, Tapered::new(-420, 650));
        assert_eq!(-a, Tapered::new(300, -250));
        let mut c = a;
        c += b;
        c -= b;
        assert_eq!(c, a);
        assert_eq!(Tapered::ZERO, Tapered::new(0, 0));
    }

    #[test]
    fn sums_stay_exact_while_halves_fit() {
        // A borrow from a negative middlegame value carries through the sign bit.
        let mut sum = Tapered::ZERO;
        for _ in 0..30 {
            sum += Tapered::new(-1_000, -1_000);
        }
        assert_eq!((sum.mg(), sum.eg()), (-30_000, -30_000));
    }

    #[test]
    #[cfg_attr(debug_assertions, should_panic(expected = "outside i16"))]
    fn debug_builds_catch_a_half_spilling_over() {
        // Release builds wrap without checking, so the test only expects a panic in debug.
        let near_max = Tapered::new(i16::MAX, 0);
        let _ = near_max + Tapered::new(1, 0);
    }

    #[test]
    fn debug_shows_the_halves() {
        assert_eq!(format!("{:?}", Tapered::new(-5, 7)), "Tapered { mg: -5, eg: 7 }");
    }
}
