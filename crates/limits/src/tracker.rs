use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::WindowKind;
use crate::{LimitError, RequestLimits};

const SECS_PER_DAY: u64 = 86_400;
const SECS_PER_HOUR: u64 = 3_600;
const SECS_PER_MONTH: u64 = 30 * SECS_PER_DAY;

pub struct Tracker {
    state: Mutex<HashMap<String, UserUsage>>,
}

impl Default for Tracker {
    fn default() -> Self {
        Self::new()
    }
}

impl Tracker {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(HashMap::new()),
        }
    }

    pub fn check(&self, user: &str, limits: RequestLimits) -> Result<(), LimitError> {
        if limits.is_empty() {
            return Ok(());
        }
        let now = Self::now_secs();
        let mut state = self.state.lock().expect("tracker mutex poisoned");
        let usage = state.entry(user.to_string()).or_default();
        usage.refresh(now);
        usage.check(limits, now)
    }

    pub fn record(&self, user: &str, tokens: u64) {
        if tokens == 0 {
            return;
        }
        let now = Self::now_secs();
        let mut state = self.state.lock().expect("tracker mutex poisoned");
        let usage = state.entry(user.to_string()).or_default();
        usage.refresh(now);
        usage.add(tokens);
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

struct UserUsage {
    day: Window,
    hour: Window,
    month: Window,
}

impl Default for UserUsage {
    fn default() -> Self {
        Self {
            day: Window::new(WindowKind::Day),
            hour: Window::new(WindowKind::Hour),
            month: Window::new(WindowKind::Month),
        }
    }
}

impl UserUsage {
    fn refresh(&mut self, now: u64) {
        self.day.refresh(now);
        self.hour.refresh(now);
        self.month.refresh(now);
    }

    fn check(&self, limits: RequestLimits, now: u64) -> Result<(), LimitError> {
        if let Some(cap) = limits.tokens_per_hour {
            self.hour.check(cap, now)?;
        }
        if let Some(cap) = limits.tokens_per_day {
            self.day.check(cap, now)?;
        }
        if let Some(cap) = limits.tokens_per_month {
            self.month.check(cap, now)?;
        }
        Ok(())
    }

    fn add(&mut self, tokens: u64) {
        self.day.count = self.day.count.saturating_add(tokens);
        self.hour.count = self.hour.count.saturating_add(tokens);
        self.month.count = self.month.count.saturating_add(tokens);
    }
}

struct Window {
    count: u64,
    kind: WindowKind,
    size: u64,
    start: u64,
}

impl Window {
    fn new(kind: WindowKind) -> Self {
        let size = match kind {
            WindowKind::Day => SECS_PER_DAY,
            WindowKind::Hour => SECS_PER_HOUR,
            WindowKind::Month => SECS_PER_MONTH,
        };
        Self {
            count: 0,
            kind,
            size,
            start: 0,
        }
    }

    fn refresh(&mut self, now: u64) {
        let current = now - (now % self.size);
        if current != self.start {
            self.start = current;
            self.count = 0;
        }
    }

    fn check(&self, cap: u64, now: u64) -> Result<(), LimitError> {
        if self.count >= cap {
            return Err(LimitError::Exceeded {
                limit: cap,
                retry_after: (self.start + self.size).saturating_sub(now),
                used: self.count,
                window: self.kind,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_when_over_limit() {
        let tracker = Tracker::new();
        let limits = RequestLimits {
            tokens_per_hour: Some(100),
            ..Default::default()
        };
        tracker.record("alice", 100);
        let err = tracker.check("alice", limits).unwrap_err();
        match err {
            LimitError::Exceeded {
                window,
                used,
                limit,
                ..
            } => {
                assert_eq!(window, WindowKind::Hour);
                assert_eq!(used, 100);
                assert_eq!(limit, 100);
            }
            _ => panic!("expected Exceeded, got {err:?}"),
        }
    }

    #[test]
    fn allows_under_limit() {
        let tracker = Tracker::new();
        let limits = RequestLimits {
            tokens_per_hour: Some(100),
            ..Default::default()
        };
        tracker.record("alice", 50);
        tracker.check("alice", limits).unwrap();
    }

    #[test]
    fn no_limits_always_passes() {
        let tracker = Tracker::new();
        tracker.record("alice", 1_000_000_000);
        tracker.check("alice", RequestLimits::default()).unwrap();
    }

    #[test]
    fn isolated_users() {
        let tracker = Tracker::new();
        let limits = RequestLimits {
            tokens_per_hour: Some(10),
            ..Default::default()
        };
        tracker.record("alice", 10);
        tracker.check("alice", limits).unwrap_err();
        tracker.check("bob", limits).unwrap();
    }
}
