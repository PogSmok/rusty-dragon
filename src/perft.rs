//! Perft: counting the leaf nodes of the legal move tree to a fixed depth.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use crate::board::{Move, Position};

/// Number of leaf nodes `depth` plies below `pos`.
#[must_use]
pub fn perft(pos: &Position, depth: u32) -> u64 {
    match depth {
        0 => 1,
        // Bulk counting: the leaves are the legal moves, no need to make them.
        1 => pos.legal_moves().len() as u64,
        _ => pos
            .legal_moves()
            .iter()
            .map(|&mv| {
                let mut child = pos.clone();
                child.make_move(mv);
                perft(&child, depth - 1)
            })
            .sum(),
    }
}

/// Perft split by root move: for each legal move, in move generation order.
/// Counts sum to [`perft`] at the same depth. Empty when `depth` is 0.
///
/// Used for debugging incorrect perft results.
#[must_use]
pub fn divide(pos: &Position, depth: u32) -> Vec<(Move, u64)> {
    if depth == 0 {
        return Vec::new();
    }
    pos.legal_moves()
        .iter()
        .map(|&mv| {
            let mut child = pos.clone();
            child.make_move(mv);
            (mv, perft(&child, depth - 1))
        })
        .collect()
}

/// Plies below the root where [`parallel_perft`] splits the tree into jobs
const SPLIT_PLIES: u32 = 2;

/// Same count as [`perft`], with the subtrees `SPLIT_PLIES` below the root spread over all cores.
#[must_use]
pub fn parallel_perft(pos: &Position, depth: u32) -> u64 {
    // For small depths, run perft directly to avoid overhead.
    if depth <= SPLIT_PLIES + 1 {
        return perft(pos, depth);
    }

    // Expand the first `SPLIT_PLIES` plies on this thread, so workers only read a fixed job
    // list and never add to it.
    //
    // Execution time for this is negligible compared to the total time for perft.
    let mut jobs = vec![pos.clone()];
    for _ in 0..SPLIT_PLIES {
        jobs = jobs
            .iter()
            .flat_map(|parent| {
                parent
                    .legal_moves()
                    .iter()
                    .map(|&mv| {
                        let mut child = parent.clone();
                        child.make_move(mv);
                        child
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
    }

    let next = AtomicUsize::new(0);
    let workers = thread::available_parallelism().map_or(1, NonZeroUsize::get);
    thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut nodes = 0;
                    while let Some(job) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
                        nodes += perft(job, depth - SPLIT_PLIES);
                    }
                    nodes
                })
            })
            .collect();
        handles.into_iter().map(|handle| handle.join().expect("perft worker panicked")).sum()
    })
}

/// Same as [`divide`], but each root move's subtree is counted by [`parallel_perft`].
#[must_use]
pub fn parallel_divide(pos: &Position, depth: u32) -> Vec<(Move, u64)> {
    if depth == 0 {
        return Vec::new();
    }
    pos.legal_moves()
        .iter()
        .map(|&mv| {
            let mut child = pos.clone();
            child.make_move(mv);
            (mv, parallel_perft(&child, depth - 1))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_zero_is_one_node() {
        assert_eq!(perft(&Position::startpos(), 0), 1);
        assert_eq!(divide(&Position::startpos(), 0), []);
    }

    #[test]
    fn startpos_shallow() {
        let pos = Position::startpos();
        assert_eq!(perft(&pos, 1), 20);
        assert_eq!(perft(&pos, 2), 400);
        assert_eq!(perft(&pos, 3), 8_902);
    }

    #[test]
    fn divide_sums_to_perft() {
        let pos = Position::startpos();
        let split = divide(&pos, 3);
        assert_eq!(split.len(), 20);
        assert_eq!(split.iter().map(|&(_, n)| n).sum::<u64>(), perft(&pos, 3));
        let e2e4 = split.iter().find(|(mv, _)| mv.to_string() == "e2e4").expect("legal");
        assert_eq!(e2e4.1, 600);
    }

    #[test]
    fn parallel_matches_sequential() {
        // Kiwipete: castling, en passant and promotions within a few plies.
        let fen = "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1";
        let pos = Position::from_fen(fen).expect("valid FEN");
        // Below, at, and above the depth where the tree starts being split.
        for depth in 0..=SPLIT_PLIES + 2 {
            assert_eq!(parallel_perft(&pos, depth), perft(&pos, depth), "depth {depth}");
        }
        assert_eq!(parallel_divide(&pos, 4), divide(&pos, 4));
        assert_eq!(parallel_divide(&pos, 0), []);
    }
}
