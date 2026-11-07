//! Perft: counting the leaf nodes of the legal move tree to a fixed depth.

use std::error::Error;
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;

use crate::board::{Move, Position};

/// Deepest perft [`parallel_divide`] accepts. Prevents stack overflow.
pub const MAX_DEPTH: u32 = 32;

/// Perft split by root move. Fields are private, so the total always matches the moves.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Divide {
    moves: Vec<(Move, u64)>,
    nodes: u64,
}

impl Divide {
    /// Depth 0: the root itself is the only leaf.
    const ROOT: Self = Self { moves: Vec::new(), nodes: 1 };

    /// Totals the counts of every root move, for a depth above 0.
    fn new(moves: Vec<(Move, u64)>) -> Self {
        let nodes = moves.iter().map(|&(_, n)| n).sum();
        Self { moves, nodes }
    }

    /// Count below each legal root move, in move generation order. Empty at depth 0.
    #[must_use]
    pub fn moves(&self) -> &[(Move, u64)] {
        &self.moves
    }

    /// Same as [`perft`]: the sum of [`Self::moves`], or 1 at depth 0.
    #[must_use]
    pub fn nodes(&self) -> u64 {
        self.nodes
    }
}

/// `<move>: <count>` for each root move, then a blank line and `Nodes searched: <total>`.
impl fmt::Display for Divide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (mv, nodes) in &self.moves {
            writeln!(f, "{mv}: {nodes}")?;
        }
        write!(f, "\nNodes searched: {}", self.nodes)
    }
}

/// Why [`parallel_divide`] gave no count.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PerftError {
    /// The depth is above [`MAX_DEPTH`].
    TooDeep,
    /// `stop` was set before the count finished.
    Stopped,
}

impl fmt::Display for PerftError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooDeep => write!(f, "perft depth must be at most {MAX_DEPTH}"),
            Self::Stopped => write!(f, "perft stopped"),
        }
    }
}

impl Error for PerftError {}

/// Number of leaf nodes `depth` plies below `pos`.
#[must_use]
pub fn perft(pos: &Position, depth: u32) -> u64 {
    match depth {
        0 => 1,
        // Bulk counting: the leaves are the legal moves, no need to make them.
        1 => pos.legal_moves().len() as u64,
        _ => pos.legal_moves().iter().map(|&mv| perft(&child(pos, mv), depth - 1)).sum(),
    }
}

/// Perft split by root move. Used for debugging incorrect perft results.
#[must_use]
pub fn divide(pos: &Position, depth: u32) -> Divide {
    if depth == 0 {
        return Divide::ROOT;
    }
    Divide::new(
        pos.legal_moves().iter().map(|&mv| (mv, perft(&child(pos, mv), depth - 1))).collect(),
    )
}

/// Plies below the root where [`parallel_divide`] splits the tree into jobs.
const SPLIT_PLIES: u32 = 2;

/// [`perft_until`] hands subtrees shallower than this to [`perft`], without checking `stop`.
const STOP_CHECK_DEPTH: u32 = 3;

/// Same as [`divide`], with the subtrees `SPLIT_PLIES` below the root spread over all cores.
/// Gives up once `stop` is set, though a count small enough may finish first.
pub fn parallel_divide(
    pos: &Position,
    depth: u32,
    stop: &AtomicBool,
) -> Result<Divide, PerftError> {
    if depth > MAX_DEPTH {
        return Err(PerftError::TooDeep);
    }
    if stop.load(Ordering::Relaxed) {
        return Err(PerftError::Stopped);
    }
    if depth <= SPLIT_PLIES + 1 {
        return Ok(divide(pos, depth));
    }

    // Expand the first `SPLIT_PLIES` plies on this thread, so workers only read a fixed job
    // list. Each job keeps the index of its root move, so every root move shares one pool of
    // workers and no core idles until the whole count is done.
    let roots = pos.legal_moves();
    let mut jobs: Vec<(usize, Position)> =
        roots.iter().enumerate().map(|(root, &mv)| (root, child(pos, mv))).collect();
    for _ in 1..SPLIT_PLIES {
        let mut deeper = Vec::new();
        for (root, parent) in &jobs {
            deeper.extend(parent.legal_moves().iter().map(|&mv| (*root, child(parent, mv))));
        }
        jobs = deeper;
    }

    let next = AtomicUsize::new(0);
    let workers = thread::available_parallelism().map_or(1, NonZeroUsize::get);
    let per_worker: Vec<Vec<u64>> = thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut counts = vec![0; roots.len()];
                    while let Some((root, job)) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
                        counts[*root] += perft_until(job, depth - SPLIT_PLIES, stop)?;
                    }
                    Ok(counts)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("perft worker panicked"))
            .collect::<Result<_, _>>()
    })?;

    let mut counts = vec![0; roots.len()];
    for worker in per_worker {
        for (total, n) in counts.iter_mut().zip(worker) {
            *total += n;
        }
    }
    Ok(Divide::new(roots.iter().copied().zip(counts).collect()))
}

/// Same as [`perft`], but gives up once `stop` is set.
fn perft_until(pos: &Position, depth: u32, stop: &AtomicBool) -> Result<u64, PerftError> {
    if stop.load(Ordering::Relaxed) {
        return Err(PerftError::Stopped);
    }
    if depth < STOP_CHECK_DEPTH {
        return Ok(perft(pos, depth));
    }
    pos.legal_moves().iter().map(|&mv| perft_until(&child(pos, mv), depth - 1, stop)).sum()
}

/// The position after `mv`.
fn child(pos: &Position, mv: Move) -> Position {
    let mut child = pos.clone();
    child.make_move(mv);
    child
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::positions::KIWIPETE;

    fn never() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn depth_zero_is_one_node() {
        let pos = Position::startpos();
        let root = Divide { moves: Vec::new(), nodes: 1 };
        assert_eq!(perft(&pos, 0), 1);
        assert_eq!(divide(&pos, 0), root);
        assert_eq!(parallel_divide(&pos, 0, &never()), Ok(root));
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
        assert_eq!(split.moves.len(), 20);
        assert_eq!(split.nodes, 8_902);
        let e2e4 = split.moves.iter().find(|(mv, _)| mv.to_string() == "e2e4").expect("legal");
        assert_eq!(e2e4.1, 600);
    }

    #[test]
    fn parallel_matches_sequential() {
        let pos = Position::from_fen(KIWIPETE).expect("valid FEN");
        // Below, at, and above the depth where the tree starts being split.
        for depth in 0..=SPLIT_PLIES + 2 {
            assert_eq!(parallel_divide(&pos, depth, &never()), Ok(divide(&pos, depth)), "{depth}");
        }
    }

    #[test]
    fn parallel_divide_gives_up_when_stopped() {
        let stop = AtomicBool::new(true);
        // No jobs, then jobs both shallower and deeper than `STOP_CHECK_DEPTH`.
        for depth in [1, SPLIT_PLIES + 2, SPLIT_PLIES + STOP_CHECK_DEPTH + 1] {
            let result = parallel_divide(&Position::startpos(), depth, &stop);
            assert_eq!(result, Err(PerftError::Stopped), "{depth}");
        }
    }

    #[test]
    fn perft_until_checks_stop_inside_a_job() {
        let pos = Position::from_fen(KIWIPETE).expect("valid FEN");
        let depth = STOP_CHECK_DEPTH + 1;
        assert_eq!(perft_until(&pos, depth, &AtomicBool::new(true)), Err(PerftError::Stopped));
        assert_eq!(perft_until(&pos, depth, &never()), Ok(perft(&pos, depth)));
    }

    #[test]
    fn parallel_divide_rejects_absurd_depths() {
        let result = parallel_divide(&Position::startpos(), MAX_DEPTH + 1, &never());
        assert_eq!(result, Err(PerftError::TooDeep));
    }

    #[test]
    fn display_lists_root_moves_then_the_total() {
        let pos = Position::from_fen("4k3/8/8/8/8/8/8/R3K3 w - - 0 1").expect("valid FEN");
        let text = divide(&pos, 1).to_string();
        // 10 rook moves and 5 king moves, one line each, in move generation order.
        assert_eq!(text.lines().count(), 15 + 2, "{text}");
        assert!(text.contains("a1a8: 1\n"), "{text}");
        assert!(text.ends_with("\n\nNodes searched: 15"), "{text}");
        assert_eq!(Divide::ROOT.to_string(), "\nNodes searched: 1");
    }
}
