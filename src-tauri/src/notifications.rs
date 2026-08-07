//! Native desktop notifications with an explicit OS application identity.
//!
//! The identity is not cosmetic: GNOME uses the `desktop-entry` hint to attach
//! banner policy to Threadknot's desktop file, and Windows requires the installed
//! AppUserModelID registered by the Tauri installer.

const APP_ID: &str = "com.smithnetwork.threadknot";
const LINUX_DESKTOP_ENTRY: &str = "threadknot";

// GNOME 50 destroys an application's notification source when the D-Bus
// sender vanishes. `notify-rust::NotificationHandle` intentionally owns that
// connection, so keep one handle and update it for the lifetime of Threadknot.
// This also gives the notification center one clean, latest-status entry.
#[cfg(all(unix, not(target_os = "macos")))]
static LIVE_NOTIFICATION: std::sync::OnceLock<
    std::sync::Mutex<Option<notify_rust::NotificationHandle>>,
> = std::sync::OnceLock::new();

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationReceipt {
    pub platform: &'static str,
    pub identity: &'static str,
    /// The freedesktop notification-server id returned by GNOME/KDE.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notification_id: Option<u32>,
    pub connection_reused: bool,
}

/// Send a normal Threadknot desktop notification and wait for the OS API to accept
/// it. A successful return means the notification daemon accepted it; unlike
/// the old detached thread, transport failures reach the frontend.
pub fn send(title: &str, body: &str) -> Result<NotificationReceipt, String> {
    let mut notification = notify_rust::Notification::new();
    notification
        .appname("Threadknot")
        .summary(title)
        .body(body)
        .timeout(notify_rust::Timeout::Milliseconds(10_000));

    #[cfg(not(target_os = "macos"))]
    notification.urgency(notify_rust::Urgency::Normal);

    #[cfg(all(unix, not(target_os = "macos")))]
    notification
        .icon("threadknot")
        .hint(notify_rust::Hint::DesktopEntry(
            LINUX_DESKTOP_ENTRY.to_string(),
        ));

    // Tauri's NSIS/MSI bundle registers this identifier with Windows. Raw
    // portable executables do not have a registered toast identity, which is
    // why CI also publishes an installer.
    #[cfg(target_os = "windows")]
    notification.app_id(APP_ID);

    #[cfg(target_os = "macos")]
    notification.icon("dialog-information");

    #[cfg(all(unix, not(target_os = "macos")))]
    let (notification_id, connection_reused) = {
        let slot = LIVE_NOTIFICATION.get_or_init(|| std::sync::Mutex::new(None));
        let mut guard = slot
            .lock()
            .map_err(|_| "native notification state is poisoned".to_string())?;

        if let Some(handle) = guard.as_mut() {
            // `update` uses the handle's existing D-Bus connection and updates
            // its server id if GNOME had already dismissed the previous entry.
            *std::ops::DerefMut::deref_mut(handle) = notification;
            handle.update().map_err(|error| error.to_string())?;
            (Some(handle.id()), true)
        } else {
            let handle = notification.show().map_err(|error| error.to_string())?;
            let id = handle.id();
            *guard = Some(handle);
            (Some(id), false)
        }
    };

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let (notification_id, connection_reused) = {
        let _handle = notification.show().map_err(|error| error.to_string())?;
        (None, false)
    };

    Ok(NotificationReceipt {
        platform: std::env::consts::OS,
        identity: if cfg!(target_os = "linux") {
            LINUX_DESKTOP_ENTRY
        } else {
            APP_ID
        },
        notification_id,
        connection_reused,
    })
}

pub fn send_test() -> Result<NotificationReceipt, String> {
    send(
        "Threadknot system notification test",
        "Native desktop notifications are connected.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_identity_matches_packaging() {
        assert_eq!(APP_ID, "com.smithnetwork.threadknot");
        assert_eq!(LINUX_DESKTOP_ENTRY, "threadknot");
    }
}
