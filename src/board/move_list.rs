use std::fmt;
use std::mem::MaybeUninit;
use std::ops::Deref;
use std::slice;
use std::str;

use super::{Move, PieceKind, Square};

/// Most legal moves a chess position has, 218, rounded up.
const DEFAULT_CAPACITY: usize = 256;

/// A list of up to `N` moves, stored inline so it never allocates.
///
/// Slots past `len` stay uninitialized.
#[must_use]
pub struct MoveList<const N: usize = DEFAULT_CAPACITY> {
    moves: [MaybeUninit<Move>; N],
    /// Number of initialized moves at the front of `moves`.
    len: usize,
}

impl MoveList {
    /// Room for every legal move of any position.
    pub const CAPACITY: usize = DEFAULT_CAPACITY;

    /// An empty list of the default capacity.
    #[inline]
    pub const fn new() -> Self {
        Self::empty()
    }
}

impl<const N: usize> MoveList<N> {
    /// An empty list of any capacity.
    #[inline]
    pub const fn empty() -> Self {
        Self { moves: [const { MaybeUninit::uninit() }; N], len: 0 }
    }

    /// Appends `mv`.
    ///
    /// # Panics
    ///
    /// Panics if the list is full.
    #[inline]
    pub fn push(&mut self, mv: Move) {
        self.moves[self.len].write(mv);
        self.len += 1;
    }

    /// Appends `mv` if there is room. Returns whether there was.
    #[inline]
    pub fn try_push(&mut self, mv: Move) -> bool {
        let Some(slot) = self.moves.get_mut(self.len) else { return false };
        slot.write(mv);
        self.len += 1;
        true
    }

    /// Appends every move in `other`, in order.
    ///
    /// # Panics
    ///
    /// Panics if they do not all fit.
    #[inline]
    pub fn append(&mut self, other: &Self) {
        let end = self.len + other.len;
        self.moves[self.len..end].copy_from_slice(&other.moves[..other.len]);
        self.len = end;
    }

    #[inline]
    #[must_use]
    pub fn as_slice(&self) -> &[Move] {
        // SAFETY: the first `len` slots are initialized.
        unsafe { slice::from_raw_parts(self.moves.as_ptr().cast::<Move>(), self.len) }
    }

    #[inline]
    #[must_use]
    pub fn as_mut_slice(&mut self) -> &mut [Move] {
        // SAFETY: the first `len` slots are initialized, and `self` is borrowed mutably.
        unsafe { slice::from_raw_parts_mut(self.moves.as_mut_ptr().cast::<Move>(), self.len) }
    }

    /// The move in the list written in UCI notation, e.g. `e2e4`, `e1g1` or `e7e8q`.
    #[must_use]
    pub fn find_uci(&self, uci: &str) -> Option<Move> {
        // UCI uses lowercase, but some GUIs don't follow that convention.
        let mut buf = [0; 5];
        let lower = buf.get_mut(..uci.len())?;
        lower.copy_from_slice(uci.as_bytes());
        lower.make_ascii_lowercase();
        let uci = str::from_utf8(lower).ok()?;

        let from: Square = uci.get(0..2)?.parse().ok()?;
        let to: Square = uci.get(2..4)?.parse().ok()?;
        let promotion = match uci.as_bytes()[4..] {
            [] => None,
            [c] => Some(PieceKind::from_char(char::from(c))?),
            _ => return None,
        };
        self.iter()
            .copied()
            .find(|mv| mv.from() == from && mv.to() == to && mv.promotion() == promotion)
    }
}

/// Copies only the moves, not the whole buffer.
impl<const N: usize> Clone for MoveList<N> {
    #[inline]
    fn clone(&self) -> Self {
        let mut copy = Self::empty();
        copy.append(self);
        copy
    }
}

impl<const N: usize> Default for MoveList<N> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<const N: usize> Deref for MoveList<N> {
    type Target = [Move];

    #[inline]
    fn deref(&self) -> &[Move] {
        self.as_slice()
    }
}

impl<'a, const N: usize> IntoIterator for &'a MoveList<N> {
    type Item = &'a Move;
    type IntoIter = slice::Iter<'a, Move>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

impl<const N: usize> fmt::Debug for MoveList<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.as_slice()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::MoveKind;

    #[test]
    fn push_and_iterate() {
        let mut list = MoveList::new();
        assert!(list.is_empty());
        let a = Move::new(Square::E2, Square::E4, MoveKind::DoublePush);
        let b = Move::new(Square::G1, Square::F3, MoveKind::Quiet);
        list.push(a);
        list.push(b);
        assert_eq!(list.len(), 2);
        assert_eq!(list.as_slice(), [a, b]);
        assert_eq!((&list).into_iter().copied().collect::<Vec<_>>(), [a, b]);
        assert_eq!(format!("{list:?}"), "[Move(e2e4), Move(g1f3)]");
    }

    #[test]
    fn find_uci_matches_squares_and_promotion_in_any_case() {
        let mut list = MoveList::new();
        let push = Move::new(Square::E2, Square::E4, MoveKind::DoublePush);
        let promotion = Move::new(Square::A7, Square::A8, MoveKind::QueenPromotion);
        list.push(push);
        list.push(promotion);
        assert_eq!(list.find_uci("e2e4"), Some(push));
        assert_eq!(list.find_uci("E2E4"), Some(push));
        assert_eq!(list.find_uci("a7a8q"), Some(promotion));
        assert_eq!(list.find_uci("A7A8Q"), Some(promotion));
        for bad in ["", "e2", "e2e5", "a7a8", "a7a8n", "a7a8x", "e2e4 ", "é2e4", "e2e4qq"] {
            assert_eq!(list.find_uci(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn clone_copies_the_moves() {
        let mut list = MoveList::new();
        list.push(Move::new(Square::E2, Square::E4, MoveKind::DoublePush));
        let copy = list.clone();
        list.push(Move::new(Square::G1, Square::F3, MoveKind::Quiet));
        assert_eq!(copy.as_slice(), [Move::new(Square::E2, Square::E4, MoveKind::DoublePush)]);
    }

    #[test]
    fn append_keeps_order() {
        let a = Move::new(Square::E2, Square::E4, MoveKind::DoublePush);
        let b = Move::new(Square::G1, Square::F3, MoveKind::Quiet);
        let c = Move::new(Square::B1, Square::C3, MoveKind::Quiet);
        let mut list = MoveList::new();
        list.push(a);
        let mut other = MoveList::new();
        other.push(b);
        other.push(c);
        list.append(&other);
        assert_eq!(list.as_slice(), [a, b, c]);
        list.append(&MoveList::new());
        assert_eq!(list.len(), 3);
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn append_past_capacity_panics() {
        let mut full = MoveList::new();
        for _ in 0..MoveList::CAPACITY {
            full.push(Move::new(Square::E2, Square::E4, MoveKind::Quiet));
        }
        let mut one = MoveList::new();
        one.push(Move::new(Square::E2, Square::E4, MoveKind::Quiet));
        full.append(&one);
    }

    #[test]
    fn try_push_stops_at_capacity() {
        let mut small = MoveList::<2>::empty();
        let mv = Move::new(Square::E2, Square::E4, MoveKind::Quiet);
        assert!(small.try_push(mv) && small.try_push(mv));
        assert!(!small.try_push(mv));
        assert_eq!(small.len(), 2);
    }

    #[test]
    fn fills_to_capacity() {
        let mut list = MoveList::new();
        for _ in 0..MoveList::CAPACITY {
            list.push(Move::new(Square::E2, Square::E4, MoveKind::Quiet));
        }
        assert_eq!(list.len(), MoveList::CAPACITY);
    }

    #[test]
    #[should_panic(expected = "index out of bounds")]
    fn push_past_capacity_panics() {
        let mut list = MoveList::new();
        for _ in 0..=MoveList::CAPACITY {
            list.push(Move::new(Square::E2, Square::E4, MoveKind::Quiet));
        }
    }
}
