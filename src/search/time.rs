//! How long to think about a move.

use std::time::{Duration, Instant};

use super::Limits;
use crate::board::Color;

/// Moves the remaining time is spread over when the GUI does not give `movestogo`.
const DEFAULT_MOVES_TO_GO: u32 = 30;

/// Least time a search is given, however little is left on the clock.
const MIN_TIME: Duration = Duration::from_millis(1);

/// The time limits of one search, and the clock they are measured on.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TimeManager {
    /// When the clock started, which both limits count from.
    start: Instant,
    /// Past this, no new iteration starts.
    /// [`Self::soft_expired`] scales it by how settled the search is. `None` for no limit.
    soft: Option<Duration>,
    /// Past this, the search stops at once. `None` for no limit.
    hard: Option<Duration>,
}

impl TimeManager {
    /// Plans the time for `us` to move, counted from `start`, keeping `overhead` back.
    ///
    /// With a single move to search, a clock search stops after its first iteration.
    pub(crate) fn new(
        limits: &Limits,
        overhead: Duration,
        us: Color,
        start: Instant,
        single_move: bool,
    ) -> Self {
        let unlimited = Self { start, soft: None, hard: None };
        if limits.infinite {
            return unlimited;
        }
        if let Some(movetime) = limits.movetime {
            // Asked to use the time: no stopping between iterations.
            let time = movetime.saturating_sub(overhead).max(MIN_TIME);
            return Self { start, soft: None, hard: Some(time) };
        }
        let Some(time) = limits.time[us.index()] else { return unlimited };

        let left = time.saturating_sub(overhead);
        let inc = limits.inc[us.index()].unwrap_or_default();
        let moves = limits.movestogo.map_or(DEFAULT_MOVES_TO_GO, |n| n.clamp(1, 50));
        // A share of the clock, plus most of the increment, which comes back after the move.
        let optimum = (left / moves + inc * 3 / 4).min(left * 4 / 5);
        let mut hard = (optimum * 3).min(left * 4 / 5);
        // Before a time control, half of each later move's share is kept for it, so no move
        // takes nearly all the time the last ones need.
        if limits.movestogo.is_some() {
            let reserve = left / moves * (moves - 1) / 2;
            hard = hard.min(left.saturating_sub(reserve));
        }
        let hard = hard.max(MIN_TIME);
        // An iteration takes a few times longer than the last, so the next one only starts
        // early enough to likely finish near the optimum.
        let soft = if single_move { Duration::ZERO } else { optimum * 3 / 5 };
        Self { start, soft: Some(soft), hard: Some(hard) }
    }

    /// Time since the clock started.
    pub(crate) fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// Restarts the clock to `at`.
    pub(crate) fn restart(&mut self, at: Instant) {
        self.start = at;
    }

    /// Whether the soft limit, scaled by `factor`, has passed, so no new iteration should start.
    pub(crate) fn soft_expired(&self, factor: f64) -> bool {
        self.soft.is_some_and(|soft| self.elapsed() >= soft.mul_f64(factor))
    }

    /// Whether the hard limit has passed, so the search should stop at once.
    pub(crate) fn hard_expired(&self) -> bool {
        self.hard.is_some_and(|hard| self.elapsed() >= hard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(limits: &Limits) -> TimeManager {
        TimeManager::new(limits, Duration::ZERO, Color::White, Instant::now(), false)
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn no_clock_means_no_limit() {
        let tm = plan(&Limits::default());
        assert_eq!((tm.soft, tm.hard), (None, None));
        let infinite = Limits { infinite: true, movetime: Some(ms(10)), ..Limits::default() };
        assert_eq!(plan(&infinite).hard, None);
    }

    #[test]
    fn movetime_is_used_in_full() {
        let limits = Limits { movetime: Some(ms(1000)), ..Limits::default() };
        let tm = TimeManager::new(&limits, ms(30), Color::White, Instant::now(), false);
        assert_eq!((tm.soft, tm.hard), (None, Some(ms(970))));
    }

    #[test]
    fn clock_time_is_spread_over_the_moves_left() {
        let limits = Limits {
            time: [Some(ms(60_000)), Some(ms(1))],
            inc: [Some(ms(1000)), None],
            ..Limits::default()
        };
        let tm = plan(&limits);
        // 60 s over 30 moves, plus three quarters of the increment.
        assert_eq!(tm.soft, Some(ms(2750) * 3 / 5));
        assert_eq!(tm.hard, Some(ms(2750 * 3)));
        // Black's own clock is nearly empty.
        let black = TimeManager::new(&limits, Duration::ZERO, Color::Black, Instant::now(), false);
        assert_eq!(black.hard, Some(MIN_TIME));
    }

    #[test]
    fn last_move_before_the_control_keeps_a_reserve() {
        let limits =
            Limits { time: [Some(ms(10_000)); 2], movestogo: Some(1), ..Limits::default() };
        assert_eq!(plan(&limits).hard, Some(ms(8000)));
    }

    #[test]
    fn moves_before_the_control_keep_time_for_the_last_ones() {
        let limits =
            Limits { time: [Some(ms(10_000)); 2], movestogo: Some(2), ..Limits::default() };
        // Half of the 5 s share of the last move is kept for it.
        assert_eq!(plan(&limits).hard, Some(ms(7500)));
        let limits = Limits { movestogo: Some(3), ..limits };
        assert_eq!(plan(&limits).hard, Some(ms(10_000).saturating_sub(ms(10_000) / 3)));
    }

    #[test]
    fn a_single_move_stops_after_one_iteration() {
        let limits = Limits { time: [Some(ms(10_000)); 2], ..Limits::default() };
        let tm = TimeManager::new(&limits, Duration::ZERO, Color::White, Instant::now(), true);
        assert!(tm.soft_expired(1.0));
        assert!(!tm.hard_expired());
    }
}
