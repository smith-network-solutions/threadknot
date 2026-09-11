//! Query idle duration, never keyboard contents, pointer positions or app names.
//! Unknown/locked sessions return None, which cannot suppress phone pushes.
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

static IDLE_SECONDS: AtomicU64 = AtomicU64::new(0);
static SAMPLE: Mutex<Option<(Instant, Option<u64>)>> = Mutex::new(None);

#[tauri::command]
pub fn configure_desktop_activity(idle_seconds: u64) -> Result<(), String> {
    if ![0, 30, 60, 120, 300].contains(&idle_seconds) {
        return Err("invalid idle timeout".into());
    }
    IDLE_SECONDS.store(idle_seconds, Ordering::Relaxed);
    Ok(())
}

// Native timer: minimized/background WebViews throttle JavaScript timers.
// Only Tauri starts this; a headless agent server never claims a human is here.
pub fn start(state: crate::server::ServerState) {
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let idle = query_idle().await;
            let now = Instant::now();
            *SAMPLE.lock().unwrap() = Some((now, idle));
            let threshold = IDLE_SECONDS.load(Ordering::Relaxed) * 1000;
            let active_for = idle
                .map(|idle| threshold.saturating_sub(idle))
                .unwrap_or(0)
                .min(12_000);
            if let Some(push) = state.hub.push() {
                push.presence.lock().unwrap().report(
                    crate::people::OWNER_ID,
                    "native",
                    active_for,
                    now,
                );
            }
            let reports = state.peernet.registry.list().into_iter()
                .filter(|peer| state.peernet.is_online(&peer.machine_id))
                .map(|peer| {
                    let net = state.peernet.clone();
                    async move {
                        let _ = tokio::time::timeout(Duration::from_secs(2), net.request(
                            &peer.machine_id, "desktop.presence",
                            serde_json::json!({"clientId": "native", "activeForMs": active_for}), None
                        )).await;
                    }
                });
            futures::future::join_all(reports).await;
        }
    });
}

#[tauri::command]
pub async fn desktop_idle() -> Option<u64> {
    let sample = SAMPLE.lock().ok()?;
    let (at, idle) = (*sample)?;
    if at.elapsed() > Duration::from_secs(8) {
        return None;
    }
    idle.map(|ms| ms.saturating_add(at.elapsed().as_millis() as u64))
}

async fn query_idle() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        tokio::time::timeout(std::time::Duration::from_secs(2), linux_idle())
            .await
            .ok()
            .flatten()
    }
    #[cfg(not(target_os = "linux"))]
    {
        native_idle()
    }
}

#[cfg(target_os = "windows")]
fn native_idle() -> Option<u64> {
    use windows_sys::Win32::{
        System::{StationsAndDesktops::*, SystemInformation::GetTickCount},
        UI::Input::KeyboardAndMouse::*,
    };
    unsafe {
        let desktop = OpenInputDesktop(0, 0, DESKTOP_READOBJECTS);
        if desktop.is_null() {
            return None;
        }
        let mut name = [0u16; 64];
        let mut needed = 0;
        let ok = GetUserObjectInformationW(
            desktop,
            UOI_NAME,
            name.as_mut_ptr().cast(),
            std::mem::size_of_val(&name) as u32,
            &mut needed,
        );
        CloseDesktop(desktop);
        let len = name.iter().position(|v| *v == 0).unwrap_or(name.len());
        if ok == 0 || String::from_utf16_lossy(&name[..len]) != "Default" {
            return None;
        }
        let mut input = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        if GetLastInputInfo(&mut input) == 0 {
            return None;
        }
        Some(GetTickCount().wrapping_sub(input.dwTime) as u64)
    }
}

#[cfg(target_os = "macos")]
fn native_idle() -> Option<u64> {
    use std::ffi::{c_char, c_void};
    type CFRef = *const c_void;
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceSecondsSinceLastEventType(state: i32, event: u32) -> f64;
        fn CGSessionCopyCurrentDictionary() -> CFRef;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithCString(
            allocator: CFRef,
            value: *const c_char,
            encoding: u32,
        ) -> CFRef;
        fn CFDictionaryGetValue(dict: CFRef, key: CFRef) -> CFRef;
        fn CFBooleanGetValue(value: CFRef) -> u8;
        fn CFRelease(value: CFRef);
    }
    unsafe {
        let session = CGSessionCopyCurrentDictionary();
        if session.is_null() {
            return None;
        }
        let read_bool = |name: &[u8]| {
            let key = CFStringCreateWithCString(std::ptr::null(), name.as_ptr().cast(), 0x08000100);
            let value = CFDictionaryGetValue(session, key);
            let result = !value.is_null() && CFBooleanGetValue(value) != 0;
            CFRelease(key);
            result
        };
        let locked = read_bool(b"CGSSessionScreenIsLocked\0");
        let console = read_bool(b"kCGSessionOnConsoleKey\0");
        CFRelease(session);
        if locked || !console {
            return None;
        }
        // Combined session state, any input event. Querying elapsed time does
        // not install an event tap or collect the underlying input events.
        let seconds = CGEventSourceSecondsSinceLastEventType(0, u32::MAX);
        (seconds.is_finite() && seconds >= 0.0).then_some((seconds * 1000.0) as u64)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn native_idle() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
async fn linux_idle() -> Option<u64> {
    // Screen saver activation/locking takes precedence over recent input (a
    // lock shortcut is itself input). Never use GetActiveTime as idle time.
    if let Ok(bus) = zbus::Connection::system().await {
        if let Ok(session) = zbus::Proxy::new(
            &bus,
            "org.freedesktop.login1",
            "/org/freedesktop/login1/session/auto",
            "org.freedesktop.login1.Session",
        )
        .await
        {
            if session
                .get_property::<bool>("LockedHint")
                .await
                .unwrap_or(false)
                || !session.get_property::<bool>("Active").await.unwrap_or(true)
            {
                return None;
            }
        }
    }
    if let Ok(bus) = zbus::Connection::session().await {
        // Probe running services only; querying a different desktop's screen
        // saver must not auto-start it alongside the user's real screen saver.
        let names = zbus::fdo::DBusProxy::new(&bus).await.ok();
        let names = match names {
            Some(proxy) => proxy.list_names().await.unwrap_or_default(),
            None => vec![],
        };
        for (service, path) in [
            ("org.cinnamon.ScreenSaver", "/org/cinnamon/ScreenSaver"),
            ("org.mate.ScreenSaver", "/org/mate/ScreenSaver"),
            ("org.gnome.ScreenSaver", "/org/gnome/ScreenSaver"),
            (
                "org.freedesktop.ScreenSaver",
                "/org/freedesktop/ScreenSaver",
            ),
        ] {
            if !names.iter().any(|name| name.as_str() == service) {
                continue;
            }
            if let Ok(proxy) = zbus::Proxy::new(&bus, service, path, service).await {
                if proxy
                    .call::<_, _, bool>("GetActive", &())
                    .await
                    .unwrap_or(false)
                {
                    return None;
                }
            }
        }
        if let Ok(proxy) = zbus::Proxy::new(
            &bus,
            "org.gnome.Mutter.IdleMonitor",
            "/org/gnome/Mutter/IdleMonitor/Core",
            "org.gnome.Mutter.IdleMonitor",
        )
        .await
        {
            if let Ok(ms) = proxy.call::<_, _, u64>("GetIdletime", &()).await {
                return Some(ms);
            }
        }
    }
    if wayland_session(
        std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
    ) {
        // Xwayland cannot measure input going to native Wayland windows.
        return wayland::idle();
    }
    static X11_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if X11_BUSY.swap(true, Ordering::Relaxed) {
        return None;
    }
    tokio::task::spawn_blocking(|| {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                X11_BUSY.store(false, Ordering::Relaxed);
            }
        }
        let _reset = Reset;
        use x11rb::{
            connection::Connection,
            protocol::screensaver::{ConnectionExt, State},
        };
        let (conn, screen) = x11rb::connect(None).ok()?;
        let info = conn
            .screensaver_query_info(conn.setup().roots[screen].root)
            .ok()?
            .reply()
            .ok()?;
        // Disabling the screen saver doesn't disable the input idle counter.
        (info.state == u8::from(State::OFF) || info.state == u8::from(State::DISABLED))
            .then_some(info.ms_since_user_input as u64)
    })
    .await
    .ok()
    .flatten()
}

#[cfg(target_os = "linux")]
fn wayland_session(session_type: Option<&str>, has_wayland_display: bool) -> bool {
    match session_type {
        Some("x11") => false, // An inherited/nested WAYLAND_DISPLAY is not the desktop session.
        Some("wayland") => true,
        _ => has_wayland_display,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(target_os = "linux")]
    fn session_type_wins_over_inherited_display_variables() {
        assert!(!super::wayland_session(Some("x11"), true));
        assert!(super::wayland_session(Some("wayland"), false));
        assert!(super::wayland_session(None, true));
        assert!(!super::wayland_session(None, false));
    }
    #[tokio::test]
    #[ignore = "requires an unlocked graphical desktop session"]
    async fn native_idle_probe() {
        let idle = super::query_idle().await;
        assert!(idle.is_some(), "no supported, unlocked desktop idle source");
        eprintln!("Native desktop idle source returned {} ms", idle.unwrap());
    }
}

#[cfg(target_os = "linux")]
mod wayland {
    use std::{
        sync::{Arc, Mutex, OnceLock},
        time::{Duration, Instant},
    };
    use wayland_client::{
        globals::{registry_queue_init, GlobalListContents},
        protocol::{wl_registry, wl_seat},
        Connection, Dispatch, QueueHandle,
    };
    use wayland_protocols::ext::idle_notify::v1::client::{
        ext_idle_notification_v1::{self, ExtIdleNotificationV1},
        ext_idle_notifier_v1::ExtIdleNotifierV1,
    };

    #[derive(Default)]
    struct Sample {
        input: Option<Instant>,
        checked: Option<Instant>,
        active: bool,
    }
    struct Monitor(Arc<Mutex<Sample>>);
    static SAMPLE: OnceLock<Arc<Mutex<Sample>>> = OnceLock::new();

    pub fn idle() -> Option<u64> {
        let sample = SAMPLE
            .get_or_init(|| {
                let sample = Arc::new(Mutex::new(Sample::default()));
                let worker = sample.clone();
                std::thread::spawn(move || loop {
                    let _ = run(worker.clone());
                    *worker.lock().unwrap() = Sample::default();
                    std::thread::sleep(Duration::from_secs(10));
                });
                sample
            })
            .lock()
            .ok()?;
        if sample.checked?.elapsed() > Duration::from_secs(3) {
            return None;
        }
        if sample.active {
            Some(0)
        } else {
            Some(sample.input?.elapsed().as_millis() as u64)
        }
    }

    fn run(sample: Arc<Mutex<Sample>>) -> Result<(), Box<dyn std::error::Error>> {
        let conn = Connection::connect_to_env()?;
        let (globals, mut queue) = registry_queue_init::<Monitor>(&conn)?;
        let qh = queue.handle();
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=1, ())?;
        // v2's input-idle watch ignores video/screensaver inhibitors. Treating
        // an inhibited screen saver as user activity could mute pushes forever.
        let notifier: ExtIdleNotifierV1 = globals.bind(&qh, 2..=2, ())?;
        let _watch = notifier.get_input_idle_notification(1000, &seat, &qh, ());
        let mut state = Monitor(sample.clone());
        loop {
            queue.roundtrip(&mut state)?;
            sample.lock().unwrap().checked = Some(Instant::now());
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Monitor {
        fn event(
            _: &mut Self,
            _: &wl_registry::WlRegistry,
            _: wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }
    wayland_client::delegate_noop!(Monitor: ignore wl_seat::WlSeat);
    wayland_client::delegate_noop!(Monitor: ignore ExtIdleNotifierV1);
    impl Dispatch<ExtIdleNotificationV1, ()> for Monitor {
        fn event(
            state: &mut Self,
            _: &ExtIdleNotificationV1,
            event: ext_idle_notification_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            let mut sample = state.0.lock().unwrap();
            match event {
                ext_idle_notification_v1::Event::Idled => {
                    sample.active = false;
                    // The first idled event only says "at least one second";
                    // the user may already have been away for hours. Estimate
                    // elapsed idle time only after observing actual activity.
                    if sample.input.is_some() {
                        sample.input = Some(Instant::now() - Duration::from_secs(1));
                    }
                }
                ext_idle_notification_v1::Event::Resumed => {
                    sample.active = true;
                    sample.input = Some(Instant::now());
                }
                _ => {}
            }
        }
    }
}
