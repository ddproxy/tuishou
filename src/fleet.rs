//! Local fleet controls shared by `update` and `auto-update`.
//!
//! Every node updates itself. Start jitter spreads cycles across a fleet, and
//! the fleet limit stops a node from draining while too many other nodes are
//! already unavailable. Both controls are injectable so their bounds and
//! fail-closed behaviour can be tested without sleeping or reading a real
//! controller.

use crate::config::UpdateConfig;
use crate::duration::parse_duration;
use crate::{lu, process};
use std::fs::File;
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SLEEP_SLICE: Duration = Duration::from_millis(200);

/// Minimal clock used by the jitter loop.
pub trait Clock {
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Randomness source for the jitter fraction.
pub trait RandomSource {
    fn next_u64(&self) -> u64;
}

pub struct SystemRandom;

impl RandomSource for SystemRandom {
    fn next_u64(&self) -> u64 {
        let mut bytes = [0_u8; 8];
        if File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut bytes))
            .is_ok()
        {
            return u64::from_ne_bytes(bytes);
        }
        // Fallback for platforms without /dev/urandom: still varies between
        // calls and processes. Jitter is a courtesy, not a security primitive.
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        nanos
            ^ std::process::id() as u64
            ^ COUNTER.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed)
    }
}

/// Wait for a uniformly sampled duration in `0..=max`, returning the elapsed
/// time. A zero maximum returns immediately without consulting the clock.
pub fn apply_start_delay<C: Clock, R: RandomSource>(max: Duration, clock: &C, rng: &R) -> Duration {
    if max.is_zero() {
        return Duration::ZERO;
    }
    let start = clock.now();
    let span = max.as_nanos().min(u128::from(u64::MAX)) as u64;
    let fraction = (rng.next_u64() as u128 * u128::from(span) / u128::from(u64::MAX)) as u64;
    let target = start + Duration::from_nanos(fraction);
    loop {
        let now = clock.now();
        let remaining = target.saturating_duration_since(now);
        if remaining.is_zero() {
            break;
        }
        clock.sleep(remaining.min(SLEEP_SLICE));
    }
    clock.now().saturating_duration_since(start)
}

/// Ask the controller for `lu nodes --json`. Any failure is reported to the
/// caller, which fails closed when a fleet limit is configured.
pub fn read_controller_nodes(timeout: Duration) -> Result<String, String> {
    let output = process::checked("lu", &["nodes", "--json"], timeout)
        .map_err(|e| format!("lu nodes --json failed: {e}"))?;
    if !output.stdout.trim_start().starts_with('[') {
        return Err("lu nodes --json did not return a node list".to_string());
    }
    Ok(output.stdout)
}

/// Return a skip reason when this cycle must not start, or `None` to proceed.
/// With no limit configured the controller is never consulted. A controller
/// read or parse failure fails closed.
pub fn fleet_limit_skip(
    max: Option<usize>,
    read_nodes: impl FnOnce() -> Result<String, String>,
) -> Option<String> {
    let max = max?;
    let json = match read_nodes() {
        Ok(json) => json,
        Err(reason) => {
            return Some(format!(
                "fleet limit (controller state unavailable: {reason})"
            ))
        }
    };
    let count = match lu::count_unavailable_nodes(&json) {
        Ok(count) => count,
        Err(reason) => {
            return Some(format!(
                "fleet limit (controller state unreadable: {reason})"
            ))
        }
    };
    if count >= max {
        Some(format!("fleet limit ({count} of {max} nodes unavailable)"))
    } else {
        None
    }
}

/// Run the shared pre-cycle controls for both local update paths. The config
/// is validated on load, so a malformed duration cannot reach this point.
pub fn before_cycle(cfg: &UpdateConfig, command_timeout: Duration) -> Option<String> {
    let delay = parse_duration(&cfg.start_delay_max).unwrap_or(Duration::ZERO);
    apply_start_delay(delay, &SystemClock, &SystemRandom);
    fleet_limit_skip(cfg.max_unavailable, || {
        read_controller_nodes(command_timeout)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct TestClock {
        now: Mutex<Instant>,
    }

    impl TestClock {
        fn new() -> Self {
            Self {
                now: Mutex::new(Instant::now()),
            }
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> Instant {
            *self.now.lock().unwrap()
        }

        fn sleep(&self, duration: Duration) {
            let mut now = self.now.lock().unwrap();
            *now += duration;
        }
    }

    struct FixedRandom(u64);

    impl RandomSource for FixedRandom {
        fn next_u64(&self) -> u64 {
            self.0
        }
    }

    #[test]
    fn zero_delay_never_sleeps() {
        let clock = TestClock::new();
        let start = clock.now();
        assert_eq!(
            apply_start_delay(Duration::ZERO, &clock, &FixedRandom(u64::MAX)),
            Duration::ZERO
        );
        assert_eq!(clock.now(), start);
    }

    #[test]
    fn start_delay_stays_within_bounds() {
        let max = Duration::from_secs(600);
        for (value, expected) in [
            (0_u64, Duration::ZERO),
            (u64::MAX, max),
            (u64::MAX / 2, max / 2),
        ] {
            let clock = TestClock::new();
            let elapsed = apply_start_delay(max, &clock, &FixedRandom(value));
            assert!(elapsed <= max, "elapsed={elapsed:?}");
            let drift = elapsed.abs_diff(expected);
            assert!(
                drift <= Duration::from_nanos(1),
                "value={value} elapsed={elapsed:?} expected={expected:?}"
            );
        }
    }

    #[test]
    fn start_delay_advances_in_bounded_slices() {
        let clock = TestClock::new();
        let max = Duration::from_millis(450);
        // A large sample exercises more than one 200ms sleep slice.
        let elapsed = apply_start_delay(max, &clock, &FixedRandom(u64::MAX / 4 + 1));
        assert!(elapsed > Duration::from_millis(100));
        assert!(elapsed <= max);
    }

    const NODES: &str = r#"[
      {"name":"a","draining":false,"online":true},
      {"name":"b","draining":false,"online":true}
    ]"#;

    #[test]
    fn fleet_limit_allows_when_under_limit() {
        assert_eq!(
            fleet_limit_skip(None, || panic!("no controller read")),
            None
        );
        assert_eq!(fleet_limit_skip(Some(2), || Ok(NODES.to_string())), None);
        assert_eq!(fleet_limit_skip(Some(1), || Ok(NODES.to_string())), None);
    }

    #[test]
    fn fleet_limit_refuses_when_at_limit() {
        let json = r#"[
          {"name":"a","draining":true,"online":true},
          {"name":"b","draining":false,"online":false},
          {"name":"c","draining":false,"online":true}
        ]"#;
        let reason = fleet_limit_skip(Some(2), || Ok(json.to_string())).unwrap();
        assert!(reason.contains("fleet limit"), "{reason}");
        let reason = fleet_limit_skip(Some(1), || Ok(json.to_string())).unwrap();
        assert!(reason.contains("fleet limit"), "{reason}");
    }

    #[test]
    fn fleet_limit_fails_closed_on_controller_failure() {
        for reason in [
            fleet_limit_skip(Some(1), || Err("connection refused".to_string())),
            fleet_limit_skip(Some(1), || Ok("{}".to_string())),
        ] {
            let reason = reason.unwrap();
            assert!(reason.contains("fleet limit"), "{reason}");
        }
    }
}
