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
#[cfg(target_os = "macos")]
const FIDO_USAGE_PAGE: u16 = 0xF1D0;

/// Is a FIDO2 key inserted right now?
///
/// A fresh `HidApi` per call: hidapi caches its device list at init, so a
/// long-lived instance would keep answering from a stale snapshot and never
/// notice the key being pulled. Enumeration costs single-digit milliseconds,
/// which the 2s watcher cadence and the occasional unlock check absorb easily.
#[cfg(target_os = "macos")]
pub fn key_present() -> bool {
    match hidapi::HidApi::new() {
        Ok(api) => api
            .device_list()
            .any(|dev| dev.usage_page() == FIDO_USAGE_PAGE),
        Err(_) => false,
    }
}

/// Windows: hidapi cannot see FIDO keys at all. Windows blocks unprivileged
/// processes from opening FIDO HID devices, and hidapi needs that open to read
/// a device's capabilities — so the key simply never appears in its list
/// (measured with a Thetis inserted: thirty HID devices enumerated, none of
/// them the key). The device's PnP metadata is readable without touching the
/// device, and a FIDO collection's hardware IDs carry the vendor-independent
/// `HID_DEVICE_UP:F1D0_U:0001`. So: list PRESENT devices on the HID enumerator
/// via cfgmgr32 and match the usage-page hardware id. The PRESENT filter is
/// what makes pulling the key visible — an unplugged device drops out of the
/// list, not merely into a "disabled" state.
#[cfg(windows)]
pub fn key_present() -> bool {
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_DevNode_Registry_PropertyW, CM_Get_Device_ID_ListW,
        CM_Get_Device_ID_List_SizeW, CM_Locate_DevNodeW, CM_GETIDLIST_FILTER_ENUMERATOR,
        CM_GETIDLIST_FILTER_PRESENT, CM_LOCATE_DEVNODE_NORMAL, CR_SUCCESS,
        CM_DRP_HARDWAREID,
    };
    let filter: Vec<u16> = "HID\0".encode_utf16().collect();
    let flags = CM_GETIDLIST_FILTER_ENUMERATOR | CM_GETIDLIST_FILTER_PRESENT;
    unsafe {
        let mut len: u32 = 0;
        if CM_Get_Device_ID_List_SizeW(&mut len, filter.as_ptr(), flags) != CR_SUCCESS
            || len == 0
        {
            return false;
        }
        let mut ids = vec![0u16; len as usize];
        if CM_Get_Device_ID_ListW(filter.as_ptr(), ids.as_mut_ptr(), len, flags) != CR_SUCCESS {
            return false;
        }
        for id in ids.split(|c| *c == 0).filter(|s| !s.is_empty()) {
            let mut id_z: Vec<u16> = id.iter().copied().chain(std::iter::once(0)).collect();
            let mut devinst = 0;
            if CM_Locate_DevNodeW(&mut devinst, id_z.as_mut_ptr(), CM_LOCATE_DEVNODE_NORMAL)
                != CR_SUCCESS
            {
                continue;
            }
            // Two-call pattern: sizes first (this call "fails" by design), then
            // the REG_MULTI_SZ hardware-id list itself.
            let mut bytes: u32 = 0;
            let mut regtype: u32 = 0;
            let _ = CM_Get_DevNode_Registry_PropertyW(
                devinst,
                CM_DRP_HARDWAREID,
                &mut regtype,
                std::ptr::null_mut(),
                &mut bytes,
                0,
            );
            if bytes == 0 {
                continue;
            }
            let mut prop = vec![0u16; bytes as usize / 2 + 1];
            if CM_Get_DevNode_Registry_PropertyW(
                devinst,
                CM_DRP_HARDWAREID,
                &mut regtype,
                prop.as_mut_ptr().cast(),
                &mut bytes,
                0,
            ) != CR_SUCCESS
            {
                continue;
            }
            if String::from_utf16_lossy(&prop)
                .to_ascii_uppercase()
                .contains("UP:F1D0")
            {
                return true;
            }
        }
    }
    false
}

/// Linux: no hidapi — its build compiles C against libudev headers, a system
/// package this feature is too small to demand. The kernel already publishes
/// every hidraw device's report descriptor world-readable in sysfs, and a FIDO
/// device's descriptor opens with the FIDO usage page item: `06 D0 F1`
/// (Usage Page, 2-byte value 0xF1D0). That three-byte signature is how fido2
/// tooling recognises CTAP-HID devices generally, not a heuristic invented
/// here.
#[cfg(target_os = "linux")]
pub fn key_present() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/class/hidraw") else {
        return false;
    };
    entries.flatten().any(|dev| {
        std::fs::read(dev.path().join("device/report_descriptor"))
            .map(|desc| descriptor_is_fido(&desc))
            .unwrap_or(false)
    })
}

/// Does a HID report descriptor declare the FIDO usage page? Searched, not
/// just prefix-matched: a compound device may declare other collections first.
#[cfg(any(target_os = "linux", test))]
fn descriptor_is_fido(desc: &[u8]) -> bool {
    desc.windows(3).any(|w| w == [0x06, 0xD0, 0xF1])
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

    /// The Linux detector keys on the FIDO usage-page item. A real Thetis/
    /// YubiKey descriptor opens `06 D0 F1 09 01 …`; a keyboard's opens with
    /// the generic-desktop page `05 01 …` and must not match — nor may the
    /// bytes matching accidentally across an item boundary be missed, hence
    /// the windowed search rather than a prefix check.
    #[test]
    fn fido_descriptors_are_recognised_and_keyboards_are_not() {
        assert!(descriptor_is_fido(&[0x06, 0xD0, 0xF1, 0x09, 0x01]));
        // FIDO collection declared after a vendor collection still counts.
        assert!(descriptor_is_fido(&[0x05, 0x01, 0xA1, 0x01, 0xC0, 0x06, 0xD0, 0xF1]));
        // Ordinary keyboard / mouse descriptors.
        assert!(!descriptor_is_fido(&[0x05, 0x01, 0x09, 0x06, 0xA1, 0x01]));
        assert!(!descriptor_is_fido(&[]));
    }
}
