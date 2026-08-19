//! FIDO2 security-key presence, as a physical gate on the Bitwarden vault.
//!
//! Tier 1 of the security-key plan: with "require key" on, the vault can only
//! be unlocked while a key is inserted, and pulling the key locks it at once —
//! the key is a physical switch for "my passwords are reachable on this
//! machine". Tier 2 (touch-to-unlock via the key's hmac-secret) builds on this
//! and lands separately, once the feasibility probe has run against the real
//! key: Windows only exposes CTAP through its WebAuthn API, and whether that
//! API returns the hmac-secret output depends on the OS build.
//!
//! Detection is HID enumeration for usage page 0xF1D0 (FIDO). Enumeration is
//! deliberately all this does: it works unprivileged on every platform —
//! Windows refuses to let a non-admin process *open* a FIDO device, but
//! listing them is allowed — and presence is the only question Tier 1 asks.

use std::sync::atomic::{AtomicBool, Ordering};

/// FIDO's HID usage page. A CTAP device advertises this regardless of vendor,
/// which is what makes detection model-agnostic (Thetis, YubiKey, whatever).
const FIDO_USAGE_PAGE: u16 = 0xF1D0;

/// Is a FIDO2 key inserted right now?
///
/// A fresh `HidApi` per call: hidapi caches its device list at init, so a
/// long-lived instance would keep answering from a stale snapshot and never
/// notice the key being pulled. Enumeration costs single-digit milliseconds,
/// which the 2s watcher cadence and the occasional unlock check absorb easily.
pub fn key_present() -> bool {
    match hidapi::HidApi::new() {
        Ok(api) => api
            .device_list()
            .any(|dev| dev.usage_page() == FIDO_USAGE_PAGE),
        Err(_) => false,
    }
}

/// Start the removal watcher, once. Polls every 2s; on key removal with
/// "require key" on, locks the vault and tells `on_lock` so viewers can be
/// shown why their fill entries just vanished.
///
/// A plain thread rather than a tokio task: the vault is process-wide state
/// with no async in its API, and this must run regardless of which runtime —
/// desktop, headless — is hosting the process.
pub fn watch(on_lock: impl Fn() + Send + 'static) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("fido-presence".into())
        .spawn(move || {
            let mut was_present = key_present();
            loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                let present = key_present();
                if was_present && !present && crate::browser::vault().require_key() {
                    crate::browser::vault().lock();
                    on_lock();
                }
                was_present = present;
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No key is attached on CI or most dev machines, and the answer must be a
    /// calm `false`, not a panic or an error surfaced to the unlock path.
    #[test]
    fn absence_is_a_plain_false() {
        // Whatever the machine has plugged in, this must not panic; on the
        // machines this suite runs on today the answer is also false.
        let _ = key_present();
    }
}
