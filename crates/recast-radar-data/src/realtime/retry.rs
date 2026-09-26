//! Retry policy: exponential backoff with jitter.
//!
//! The policy only computes delays. Nothing here sleeps, spawns or reads a
//! clock, so the same schedule drives the blocking
//! [`ChunkIterator`](super::iterator::ChunkIterator), the async `ChunkStream`
//! (feature `async`) and callers' own loops, and it builds for
//! `wasm32-unknown-unknown`.
//!
//! The delay before retry `n` (1-based) has the ceiling
//! `min(max_delay, initial_delay * multiplier^(n - 1))`. [`Jitter`] then
//! picks the actual delay from that ceiling ("Exponential Backoff and
//! Jitter", AWS Architecture Blog, 2015):
//!
//! | jitter | delay |
//! |---|---|
//! | [`Jitter::None`] | `ceiling` |
//! | [`Jitter::Full`] | uniform in `[0, ceiling]` |
//! | [`Jitter::Equal`] | uniform in `[ceiling / 2, ceiling]` |
//!
//! (Delays are whole nanoseconds, so a draw just below the ceiling can round
//! up to it.)

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::time::Duration;

/// How the delay is drawn below the exponential ceiling.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Jitter {
    /// Always the ceiling: a fixed exponential schedule.
    None,
    /// Uniform in `[0, ceiling]`. Spreads synchronized clients the most.
    #[default]
    Full,
    /// Uniform in `[ceiling / 2, ceiling]`. Never retries sooner than half
    /// the ceiling.
    Equal,
}

/// Exponential backoff with jitter and an attempt budget.
///
/// `max_attempts` counts every attempt, the first one included, so
/// `max_attempts = 1` never retries and `max_attempts = 4` allows three
/// retries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RetryPolicy {
    /// Total attempts including the first. `0` is treated as `1`.
    pub max_attempts: u32,
    /// Ceiling of the delay before the first retry.
    pub initial_delay: Duration,
    /// Upper bound of every delay.
    pub max_delay: Duration,
    /// Growth of the ceiling per retry. Values below `1.0` (or NaN) are
    /// treated as `1.0`.
    pub multiplier: f64,
    /// How the delay is drawn below the ceiling.
    pub jitter: Jitter,
}

impl Default for RetryPolicy {
    /// Four attempts; ceilings 0.5 s, 1 s, 2 s (capped at 20 s); full jitter.
    fn default() -> Self {
        Self {
            max_attempts: 4,
            initial_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(20),
            multiplier: 2.0,
            jitter: Jitter::Full,
        }
    }
}

impl RetryPolicy {
    /// A policy that never retries.
    pub fn no_retry() -> Self {
        Self {
            max_attempts: 1,
            ..Self::default()
        }
    }

    /// Number of retries the policy allows after the first attempt.
    pub fn max_retries(&self) -> u32 {
        self.max_attempts.max(1) - 1
    }

    /// Ceiling of the delay before retry `retry` (1-based; `0` is treated as
    /// `1`): `min(max_delay, initial_delay * multiplier^(retry - 1))`.
    pub fn ceiling(&self, retry: u32) -> Duration {
        let exponent = retry.max(1) - 1;
        let multiplier = if self.multiplier.is_nan() || self.multiplier < 1.0 {
            1.0
        } else {
            self.multiplier
        };
        let exponent = i32::try_from(exponent).unwrap_or(i32::MAX);
        let seconds = self.initial_delay.as_secs_f64() * multiplier.powi(exponent);
        match Duration::try_from_secs_f64(seconds) {
            Ok(delay) => delay.min(self.max_delay),
            // Overflow (or an infinite product) only happens far above any
            // sensible cap.
            Err(_) => self.max_delay,
        }
    }

    /// Delay before retry `retry` (1-based) given `unit`, a uniform sample
    /// in `[0, 1)`. Out-of-range samples are clamped into `[0, 1)`.
    pub fn delay(&self, retry: u32, unit: f64) -> Duration {
        let ceiling = self.ceiling(retry);
        let unit = if unit.is_nan() {
            0.0
        } else {
            unit.clamp(0.0, 1.0 - f64::EPSILON)
        };
        match self.jitter {
            Jitter::None => ceiling,
            Jitter::Full => scale(ceiling, unit),
            Jitter::Equal => {
                let half = ceiling / 2;
                half.saturating_add(scale(ceiling - half, unit))
            }
        }
    }

    /// The delay to wait after `failed_attempts` consecutive failures, or
    /// `None` when the attempt budget is spent.
    pub fn delay_after(&self, failed_attempts: u32, unit: f64) -> Option<Duration> {
        if failed_attempts == 0 || failed_attempts > self.max_retries() {
            return None;
        }
        Some(self.delay(failed_attempts, unit))
    }

    /// A stateful schedule for one operation, jittered by a SplitMix64
    /// generator seeded with `seed` (equal seeds give equal schedules).
    pub fn backoff(&self, seed: u64) -> Backoff {
        Backoff {
            policy: *self,
            rng: SplitMix64(seed),
            failures: 0,
        }
    }
}

/// The retry state of one operation under a [`RetryPolicy`].
///
/// Call [`Backoff::next_delay`] after each failure; wait the returned delay
/// and try again, or give up on `None`. As an [`Iterator`] it yields the
/// whole schedule.
#[derive(Clone, Debug)]
pub struct Backoff {
    policy: RetryPolicy,
    rng: SplitMix64,
    failures: u32,
}

impl Backoff {
    /// Record a failure. Returns the delay before the next attempt, or `None`
    /// when the policy's attempt budget is spent.
    pub fn next_delay(&mut self) -> Option<Duration> {
        self.failures = self.failures.saturating_add(1);
        let unit = self.rng.next_unit();
        self.policy.delay_after(self.failures, unit)
    }

    /// Failures recorded since creation or the last [`Backoff::reset`].
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// Start over after a success. The jitter sequence continues, so
    /// successive operations do not repeat the same delays.
    pub fn reset(&mut self) {
        self.failures = 0;
    }

    /// The policy this schedule follows.
    pub fn policy(&self) -> &RetryPolicy {
        &self.policy
    }
}

impl Iterator for Backoff {
    type Item = Duration;

    fn next(&mut self) -> Option<Duration> {
        if self.failures >= self.policy.max_retries() {
            return None;
        }
        self.next_delay()
    }
}

/// `duration * factor` for `factor` in `[0, 1)`, without the float-rounding
/// panic `Duration::mul_f64` has near `Duration::MAX`.
fn scale(duration: Duration, factor: f64) -> Duration {
    Duration::try_from_secs_f64(duration.as_secs_f64() * factor)
        .map_or(duration, |scaled| scaled.min(duration))
}

/// A seed for [`RetryPolicy::backoff`] from the random keys the standard
/// library draws for `HashMap`. Reads no clock, so it also works on
/// `wasm32-unknown-unknown`, where those keys (and so the seed) are fixed.
pub fn random_seed() -> u64 {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(0x9E37_79B9_7F4A_7C15);
    hasher.finish()
}

/// SplitMix64 (Steele, Lea and Flood 2014): a tiny, well-mixed generator,
/// ample for jitter.
#[derive(Clone, Debug)]
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)` with 53 bits of resolution.
    fn next_unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 6,
            initial_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(3),
            multiplier: 2.0,
            jitter: Jitter::None,
        }
    }

    #[test]
    fn ceilings_grow_exponentially_and_cap() {
        let policy = fixed();
        let ceilings: Vec<_> = (1..=6).map(|retry| policy.ceiling(retry)).collect();
        assert_eq!(
            ceilings,
            [250, 500, 1000, 2000, 3000, 3000].map(Duration::from_millis)
        );
        assert_eq!(policy.ceiling(0), policy.ceiling(1));
        assert_eq!(policy.ceiling(u32::MAX), Duration::from_secs(3));
    }

    #[test]
    fn schedule_stops_at_the_attempt_budget() {
        let delays: Vec<_> = fixed().backoff(7).collect();
        assert_eq!(delays.len(), 5, "6 attempts allow 5 retries");
        assert_eq!(RetryPolicy::no_retry().backoff(7).count(), 0);

        let mut backoff = fixed().backoff(7);
        for _ in 0..5 {
            assert!(backoff.next_delay().is_some());
        }
        assert_eq!(backoff.next_delay(), None);
        assert_eq!(backoff.failures(), 6);
        backoff.reset();
        assert_eq!(backoff.next_delay(), Some(Duration::from_millis(250)));
    }

    #[test]
    fn jitter_stays_within_bounds() {
        for jitter in [Jitter::Full, Jitter::Equal] {
            let policy = RetryPolicy { jitter, ..fixed() };
            let mut backoff = policy.backoff(0xDEAD_BEEF);
            for retry in 1..=5 {
                let delay = backoff.next_delay().unwrap_or_default();
                let ceiling = policy.ceiling(retry);
                assert!(delay <= ceiling, "{jitter:?} retry {retry}: {delay:?}");
                if jitter == Jitter::Equal {
                    assert!(delay >= ceiling / 2, "retry {retry}: {delay:?}");
                }
            }
        }
    }

    #[test]
    fn equal_seeds_give_equal_schedules() {
        let policy = RetryPolicy::default();
        let a: Vec<_> = policy.backoff(42).collect();
        let b: Vec<_> = policy.backoff(42).collect();
        let c: Vec<_> = policy.backoff(43).collect();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        let policy = RetryPolicy {
            max_attempts: 0,
            multiplier: f64::NAN,
            ..RetryPolicy::default()
        };
        assert_eq!(policy.max_retries(), 0);
        assert_eq!(policy.ceiling(9), policy.initial_delay);
        let huge = RetryPolicy {
            multiplier: 1e300,
            max_delay: Duration::MAX,
            ..fixed()
        };
        assert_eq!(huge.ceiling(40), Duration::MAX);
        assert_eq!(fixed().delay(1, f64::NAN), Duration::from_millis(250));
        let full = RetryPolicy {
            jitter: Jitter::Full,
            ..fixed()
        };
        let clamped = full.delay(1, 7.0);
        assert!(clamped <= Duration::from_millis(250), "{clamped:?}");
        assert!(clamped > Duration::from_millis(249), "{clamped:?}");
        assert_eq!(full.delay(1, -1.0), Duration::ZERO);
        let unbounded = RetryPolicy {
            max_delay: Duration::MAX,
            multiplier: 1e300,
            jitter: Jitter::Equal,
            ..fixed()
        };
        assert!(unbounded.delay(40, 0.999_999) >= Duration::MAX / 2);
    }
}
