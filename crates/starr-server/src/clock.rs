//! Source of "now" for time-dependent rules (session lifetimes, throttling),
//! replaceable in tests so they never sleep.

use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, TimeDelta, Utc};

/// Wall clock, or a manually driven clock for tests. Cheap to clone; clones
/// of a manual clock share the same time.
#[derive(Debug, Clone, Default)]
pub struct Clock {
    manual: Option<Arc<Mutex<DateTime<Utc>>>>,
}

impl Clock {
    /// The system clock.
    #[must_use]
    pub fn system() -> Self {
        Self::default()
    }

    /// A clock that stands still at `start` until [`Clock::advance`] is called.
    #[must_use]
    pub fn manual(start: DateTime<Utc>) -> Self {
        Self {
            manual: Some(Arc::new(Mutex::new(start))),
        }
    }

    #[must_use]
    pub fn now(&self) -> DateTime<Utc> {
        match &self.manual {
            Some(time) => *time.lock().unwrap_or_else(PoisonError::into_inner),
            None => Utc::now(),
        }
    }

    /// Move a manual clock forward. Has no effect on the system clock.
    pub fn advance(&self, by: TimeDelta) {
        if let Some(time) = &self.manual {
            *time.lock().unwrap_or_else(PoisonError::into_inner) += by;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_is_shared_between_clones() {
        let clock = Clock::manual(DateTime::UNIX_EPOCH);
        let other = clock.clone();
        other.advance(TimeDelta::minutes(3));
        assert_eq!(clock.now(), DateTime::UNIX_EPOCH + TimeDelta::minutes(3));
    }
}
