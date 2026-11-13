//! Portable per-output lifecycle. Each output owns its own state machine so one
//! failing endpoint never stops healthy outputs.
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Retry delays double from one second and are capped at 30 seconds.
pub const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
/// After this many consecutive failed attempts the output needs explicit user action.
pub const MAX_RETRY_ATTEMPTS: u32 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputLifecycleState {
    Disabled,
    Starting,
    Running,
    Disconnected,
    Retrying,
    Failed,
}

pub fn retry_delay(attempt: u32) -> Duration {
    let seconds = 1_u64
        .checked_shl(attempt.saturating_sub(1).min(16))
        .unwrap_or(u64::MAX);
    Duration::from_secs(seconds).min(MAX_RETRY_DELAY)
}

#[derive(Clone, Debug)]
pub struct OutputLifecycle {
    state: OutputLifecycleState,
    attempts: u32,
    retry_at: Option<Instant>,
    last_error: Option<String>,
}

impl Default for OutputLifecycle {
    fn default() -> Self {
        Self {
            state: OutputLifecycleState::Disabled,
            attempts: 0,
            retry_at: None,
            last_error: None,
        }
    }
}

impl OutputLifecycle {
    pub fn state(&self) -> OutputLifecycleState {
        self.state
    }
    pub fn attempts(&self) -> u32 {
        self.attempts
    }
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }
    pub fn retry_at(&self) -> Option<Instant> {
        self.retry_at
    }

    /// User enabled the output (or explicitly retried a failed output).
    pub fn enable(&mut self) {
        self.state = OutputLifecycleState::Starting;
        self.attempts = 0;
        self.retry_at = None;
        self.last_error = None;
    }

    pub fn disable(&mut self) {
        *self = Self::default();
    }

    pub fn started(&mut self) {
        if matches!(
            self.state,
            OutputLifecycleState::Starting | OutputLifecycleState::Retrying
        ) {
            self.state = OutputLifecycleState::Running;
            self.attempts = 0;
            self.retry_at = None;
        }
    }

    /// The worker failed to open or lost its device. Schedules a bounded retry.
    pub fn failed(&mut self, now: Instant, error: impl Into<String>) {
        if self.state == OutputLifecycleState::Disabled {
            return;
        }
        self.last_error = Some(error.into());
        self.attempts = self.attempts.saturating_add(1);
        if self.attempts >= MAX_RETRY_ATTEMPTS {
            self.state = OutputLifecycleState::Failed;
            self.retry_at = None;
        } else {
            self.state = OutputLifecycleState::Disconnected;
            self.retry_at = Some(now + retry_delay(self.attempts));
        }
    }

    /// Returns true when the caller should attempt to reopen the endpoint now.
    pub fn poll_retry(&mut self, now: Instant) -> bool {
        match (self.state, self.retry_at) {
            (OutputLifecycleState::Disconnected, Some(at)) if now >= at => {
                self.state = OutputLifecycleState::Retrying;
                self.retry_at = None;
                true
            }
            _ => false,
        }
    }

    /// Device reappeared (hotplug notification): retry immediately instead of waiting.
    pub fn device_returned(&mut self, now: Instant) {
        if self.state == OutputLifecycleState::Disconnected {
            self.retry_at = Some(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps_at_thirty_seconds() {
        let delays: Vec<u64> = (1..=8).map(|n| retry_delay(n).as_secs()).collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(retry_delay(u32::MAX), MAX_RETRY_DELAY);
    }

    #[test]
    fn running_output_recovers_after_disconnect() {
        let now = Instant::now();
        let mut output = OutputLifecycle::default();
        output.enable();
        output.started();
        assert_eq!(output.state(), OutputLifecycleState::Running);
        output.failed(now, "device removed");
        assert_eq!(output.state(), OutputLifecycleState::Disconnected);
        assert!(!output.poll_retry(now));
        assert!(output.poll_retry(now + Duration::from_secs(1)));
        assert_eq!(output.state(), OutputLifecycleState::Retrying);
        output.started();
        assert_eq!(output.state(), OutputLifecycleState::Running);
        assert_eq!(output.attempts(), 0);
    }

    #[test]
    fn repeated_failures_require_explicit_user_action() {
        let mut now = Instant::now();
        let mut output = OutputLifecycle::default();
        output.enable();
        for _ in 0..MAX_RETRY_ATTEMPTS - 1 {
            output.failed(now, "open failed");
            now += MAX_RETRY_DELAY;
            assert!(output.poll_retry(now));
        }
        output.failed(now, "open failed");
        assert_eq!(output.state(), OutputLifecycleState::Failed);
        assert!(!output.poll_retry(now + Duration::from_secs(3600)));
        output.enable();
        assert_eq!(output.state(), OutputLifecycleState::Starting);
    }

    #[test]
    fn independent_outputs_do_not_share_failure_state() {
        let now = Instant::now();
        let mut a = OutputLifecycle::default();
        let mut b = OutputLifecycle::default();
        a.enable();
        b.enable();
        a.started();
        b.started();
        a.failed(now, "unplugged");
        assert_eq!(a.state(), OutputLifecycleState::Disconnected);
        assert_eq!(b.state(), OutputLifecycleState::Running);
    }

    #[test]
    fn disabled_output_ignores_late_failures_and_hotplug_shortens_wait() {
        let now = Instant::now();
        let mut output = OutputLifecycle::default();
        output.failed(now, "late");
        assert_eq!(output.state(), OutputLifecycleState::Disabled);
        output.enable();
        for _ in 0..4 {
            output.failed(now, "gone");
            output.poll_retry(now + MAX_RETRY_DELAY);
        }
        output.failed(now, "gone");
        assert!(!output.poll_retry(now));
        output.device_returned(now);
        assert!(output.poll_retry(now));
    }
}
