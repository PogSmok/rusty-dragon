use std::fmt;
use std::mem::MaybeUninit;
use std::ops::Deref;
use std::slice;

use super::Move;

/// Fixed-capacity move buffer, so move generation never allocates.
///
/// Slots past `len` stay uninitialized.
#[must_use]
pub struct MoveList {
    moves: [MaybeUninit<Move>; Self::CAPACITY],
    /// Number of initialized moves at the front of `moves`.
    len: usize,
}

impl MoveList {
    /// Chess positions can never have more than 218 legal moves.
    pub const CAPACITY: usize = 256;

    #[inline]
    pub const fn new() -> Self {
        Self { moves: [const { MaybeUninit::uninit() }; Self::CAPACITY], len: 0 }
    }

    /// Appends `mv`.
    /// Panics if the list is full, which no legal position can cause.
    #[inline]
    pub fn push(&mut self, mv: Move) {
        self.moves[self.len].write(mv);
        self.len += 1;
    }

    /// Appends every move in `other`, in order.
    /// Panics if they don't fit, which no legal position can cause.
    #[inline]
    pub fn append(&mut self, other: &Self) {
        let end = self.len + other.len;
        self.moves[self.len..end].copy_from_slice(&other.moves[..other.len]);
        self.len = end;
    }

    #[inline]
    #[must_use]
    pub fn as_slice(&self) -> &[Move] {
        // SAFETY: the first `len` slots are initialized
        unsafe { slice::from_raw_parts(self.moves.as_ptr().cast::<Move>(), self.len) }
    }
}

/// Copies only the moves, not the whole buffer.
impl Clone for MoveList {
    #[inline]
    fn clone(&self) -> Self {
        let mut copy = Self::new();
        copy.append(self);
        copy
    }
}

impl Default for MoveList {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for MoveList {
    type Target = [Move];

    #[inline]
    fn deref(&self) -> &[Move] {
        self.as_slice()
    }
}

impl<'a> IntoIterator for &'a MoveList {
    type Item = &'a Move;
    type IntoIter = slice::Iter<'a, Move>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

impl fmt::Debug for MoveList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.as_slice()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{MoveKind, Square};

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
