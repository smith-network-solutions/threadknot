//! The numbers that keep one installation from taking the relay down.
//!
//! They live in the protocol rather than in the relay's config because the
//! connector needs the same values: a limit the desktop does not know about is a
//! limit it cannot explain to the person hitting it, and "it just stopped
//! working" is the failure mode this crate exists to prevent.

use serde::{Deserialize, Serialize};

/// Per-installation ceilings, sent in [`crate::SessionReady`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionLimits {
    /// Concurrent proxied streams. A browser screencast plus a terminal plus a
    /// thread socket is ~4; a phone and a laptop together maybe 10. 64 leaves
    /// generous headroom and still bounds the connector's loopback fan-out.
    pub max_concurrent_streams: u32,
    /// Bytes per second, both directions summed. `None` is unlimited. Set when
    /// fair use is exceeded — the plan throttles rather than billing or
    /// disconnecting, because an unexpected invoice becomes a refund and a
    /// severed terminal becomes lost work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throttle_bytes_per_sec: Option<u64>,
    /// Fair use for the calendar month.
    pub month_quota_bytes: u64,
    /// Paired devices the installation may hold.
    pub max_devices: u32,
}

/// 500 GB/month, the fair-use figure. ~15x a heavy legitimate user, so no real
/// customer sees it; low enough that the link cannot be resold through us.
pub const DEFAULT_MONTH_QUOTA_BYTES: u64 = 500 * 1024 * 1024 * 1024;

/// The throttle applied *after* fair use is exceeded. 2 MB/s still carries a
/// terminal and a thread comfortably and makes bulk transfer unattractive.
pub const THROTTLED_BYTES_PER_SEC: u64 = 2 * 1024 * 1024;

/// Devices on the one flat plan.
pub const DEFAULT_MAX_DEVICES: u32 = 25;

impl Default for SessionLimits {
    fn default() -> Self {
        Self {
            max_concurrent_streams: 64,
            throttle_bytes_per_sec: None,
            month_quota_bytes: DEFAULT_MONTH_QUOTA_BYTES,
            max_devices: DEFAULT_MAX_DEVICES,
        }
    }
}

impl SessionLimits {
    /// Apply the fair-use rule for an installation that has used `month_bytes`.
    pub fn with_fair_use(mut self, month_bytes: u64) -> Self {
        if month_bytes >= self.month_quota_bytes {
            self.throttle_bytes_per_sec = Some(THROTTLED_BYTES_PER_SEC);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fair_use_throttles_rather_than_cutting_off() {
        let under = SessionLimits::default().with_fair_use(1);
        assert_eq!(under.throttle_bytes_per_sec, None);
        let over = SessionLimits::default().with_fair_use(DEFAULT_MONTH_QUOTA_BYTES + 1);
        assert_eq!(over.throttle_bytes_per_sec, Some(THROTTLED_BYTES_PER_SEC));
        // Crossing the line must never zero the allowance — that would be a
        // disconnect wearing a throttle's clothes.
        assert!(over.throttle_bytes_per_sec.unwrap() > 0);
        assert_eq!(over.max_concurrent_streams, under.max_concurrent_streams);
    }
}
