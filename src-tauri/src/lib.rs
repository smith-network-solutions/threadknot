// The MCP tool catalog is one large `json!` literal, and serde_json expands
// nested object literals recursively. The browser tool schemas now exceed the
// default 128-deep macro budget; this is a compile-time expansion limit only.
#![recursion_limit = "512"]

pub mod agents;
pub mod ai_palette;
pub mod artifacts;
pub mod browser;
pub mod browser_profiles;
pub mod bundled;
pub mod catalog;
pub mod clipboard;
pub mod connector;
pub mod device;
pub mod dictation;
pub mod dispatch;
pub mod exec;
pub mod files;
pub mod git;
pub mod claudex;
pub mod hermes;
pub mod ingress;
pub mod library;
pub mod limits;
pub mod mcp;
pub mod mcp_fleet;
pub mod mesh;
pub mod mobile;
pub mod notices;
pub mod notifications;
pub mod peernet;
pub mod peers;
pub mod ports;
pub mod protocol;
pub mod push;
pub mod personas;
pub mod recorder;
pub mod remote;
pub mod schedules;
pub mod server;
pub mod sessions;
pub mod store;
pub mod term;
pub mod themes;
pub mod update;
pub mod usage;

use std::sync::Arc;
use tokio::sync::RwLock;

const MAX_CLIPBOARD_IMAGE_BYTES: usize = 10 * 1024 * 1024;
const MAX_CLIPBOARD_IMAGES: usize = 8;

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ClipboardImage {
    name: String,
    mime_type: String,
    /// Base64 bytes without a data-URL prefix, ready for `turn.start`.
    data: String,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub port: u16,
    pub token: String,
    pub lan_url: String,
}

#[tauri::command]
fn server_info(state: tauri::State<ServerInfo>) -> ServerInfo {
    state.inner().clone()
}

/// Read an image directly from the Wayland clipboard. WebKitGTK sometimes
/// advertises GNOME Screenshot's raw `image/png` target to JavaScript without
/// materializing a `File`, leaving `clipboardData.files` empty. The native
/// fallback keeps image paste reliable in the Tauri shell while ordinary web
/// browsers continue to use their standard ClipboardEvent data.
#[tauri::command]
async fn clipboard_image() -> Result<Option<ClipboardImage>, String> {
    #[cfg(target_os = "linux")]
    {
        use base64::Engine as _;

        const IMAGE_TYPES: [(&str, &str); 4] = [
            ("image/png", "png"),
            ("image/jpeg", "jpg"),
            ("image/webp", "webp"),
            ("image/gif", "gif"),
        ];

        let listed = tokio::process::Command::new("wl-paste")
            .arg("--list-types")
            .output()
            .await
            .map_err(|e| format!("couldn't run wl-paste: {e}"))?;
        if !listed.status.success() {
            return Ok(None);
        }
        let offered = String::from_utf8_lossy(&listed.stdout);
        let Some(&(mime_type, extension)) = IMAGE_TYPES
            .iter()
            .find(|(mime, _)| offered.lines().any(|line| line.trim() == *mime))
        else {
            return Ok(None);
        };

        let pasted = tokio::process::Command::new("wl-paste")
            .args(["--no-newline", "--type", mime_type])
            .output()
            .await
            .map_err(|e| format!("couldn't read the Wayland clipboard: {e}"))?;
        if !pasted.status.success() || pasted.stdout.is_empty() {
            return Ok(None);
        }
        if pasted.stdout.len() > MAX_CLIPBOARD_IMAGE_BYTES {
            return Err("Clipboard image exceeds the 10 MB limit.".into());
        }

        Ok(Some(ClipboardImage {
            name: format!("pasted-image.{extension}"),
            mime_type: mime_type.to_string(),
            data: base64::engine::general_purpose::STANDARD.encode(pasted.stdout),
        }))
    }

    #[cfg(not(target_os = "linux"))]
    Ok(None)
}

/// Read image files copied by a desktop file manager. On Linux, Files/Nautilus
/// and Dolphin publish file URIs rather than image bytes; WebKitGTK commonly
/// exposes only the URI's plain-text path to a paste event. Resolving the native
/// file list here lets the composer attach those files just like picker/drag
/// uploads. If the clipboard instead contains raw image bytes (for example a
/// screenshot), fall back to the existing image-target reader.
#[tauri::command]
async fn clipboard_images() -> Result<Vec<ClipboardImage>, String> {
    #[cfg(target_os = "linux")]
    {
        use base64::Engine as _;
        use clipboard_rs::{Clipboard, ClipboardContext};

        let images =
            tauri::async_runtime::spawn_blocking(|| -> Result<Vec<ClipboardImage>, String> {
                let context = ClipboardContext::new()
                    .map_err(|error| format!("couldn't open the system clipboard: {error}"))?;
                let mut uris = context.get_files().unwrap_or_default();

                // GNOME Files' canonical target carries `copy`/`cut` on its
                // first line followed by file URIs. Some WebKit/desktop
                // combinations omit text/uri-list, so read this target too.
                if uris.is_empty() {
                    if let Ok(bytes) = context.get_buffer("x-special/gnome-copied-files") {
                        uris = String::from_utf8_lossy(&bytes)
                            .lines()
                            .map(str::trim)
                            .filter(|line| line.starts_with("file://"))
                            .map(String::from)
                            .collect();
                    }
                }

                let mut images = Vec::new();
                for uri in uris {
                    if images.len() >= MAX_CLIPBOARD_IMAGES {
                        break;
                    }
                    let Ok(url) = url::Url::parse(&uri) else {
                        continue;
                    };
                    let Ok(path) = url.to_file_path() else {
                        continue;
                    };
                    let Some(mime_type) = clipboard_image_type(&path) else {
                        continue;
                    };
                    let bytes = std::fs::read(&path)
                        .map_err(|error| format!("Couldn't read '{}': {error}", path.display()))?;
                    if bytes.len() > MAX_CLIPBOARD_IMAGE_BYTES {
                        return Err(format!("'{}' exceeds the 10 MB limit.", path.display()));
                    }
                    let name = path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "pasted-image".into());
                    images.push(ClipboardImage {
                        name,
                        mime_type: mime_type.into(),
                        data: base64::engine::general_purpose::STANDARD.encode(bytes),
                    });
                }

                // No file URIs — the clipboard may instead hold raw image bytes,
                // as tools like ksnip, Spectacle, and Flameshot publish on X11
                // (an `image/png` target, no path). `clipboard_rs` reads that
                // target directly on both X11 and Wayland, so we don't depend on
                // `wl-paste` being present or a Wayland session being active.
                if images.is_empty() {
                    const RAW_TYPES: [(&str, &str); 4] = [
                        ("image/png", "png"),
                        ("image/webp", "webp"),
                        ("image/jpeg", "jpg"),
                        ("image/gif", "gif"),
                    ];
                    for (mime_type, extension) in RAW_TYPES {
                        let Ok(bytes) = context.get_buffer(mime_type) else {
                            continue;
                        };
                        if bytes.is_empty() {
                            continue;
                        }
                        if bytes.len() > MAX_CLIPBOARD_IMAGE_BYTES {
                            return Err("Clipboard image exceeds the 10 MB limit.".into());
                        }
                        images.push(ClipboardImage {
                            name: format!("pasted-image.{extension}"),
                            mime_type: mime_type.to_string(),
                            data: base64::engine::general_purpose::STANDARD.encode(bytes),
                        });
                        break;
                    }
                }

                Ok(images)
            })
            .await
            .map_err(|error| format!("Clipboard worker failed: {error}"))??;

        if !images.is_empty() {
            return Ok(images);
        }
        Ok(clipboard_image().await?.into_iter().collect())
    }

    #[cfg(not(target_os = "linux"))]
    Ok(Vec::new())
}

fn clipboard_image_type(path: &std::path::Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Append a line to ~/.threadknot/debug.log (diagnostics for the notification path).
pub(crate) fn debug_log(msg: &str) {
    use std::io::Write;
    if let Some(home) = std::env::var_os("HOME") {
        let path = std::path::Path::new(&home).join(".threadknot").join("debug.log");
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(f, "{msg}");
        }
    }
}

/// Frontend diagnostic breadcrumb → ~/.threadknot/debug.log.
#[tauri::command]
fn debug_note(msg: String) {
    debug_log(&format!("[note] {msg}"));
}

/// Reduce a suggested filename to a plain basename for the save dialog's
/// default. The user still picks the final path, so this is just for a sensible
/// prompt, not a security boundary.
fn save_dialog_name(name: &str) -> String {
    let base = std::path::Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let trimmed = base.trim();
    if trimmed.is_empty() {
        "download".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Save a file this app's own server is serving to a user-chosen location.
///
/// The Files/Artifacts panes normally save by pointing a hidden `<a download>`
/// at a token-gated HTTP URL and clicking it. That works in real browsers
/// (phone/web sessions) but WebView2/wry silently drop anchor downloads (no
/// download handler is installed), so in the desktop shell the button did
/// nothing. Here we show a native save dialog and stream the bytes to the
/// chosen path instead. Returns the saved path, or the marker "cancelled" when
/// the user dismisses the dialog.
#[tauri::command]
async fn download_file(
    app: tauri::AppHandle,
    info: tauri::State<'_, ServerInfo>,
    url: String,
    suggested_name: String,
) -> Result<String, String> {
    use tauri_plugin_dialog::DialogExt;

    // Only ever fetch from this app's own HTTP endpoint. The panes always build
    // URLs against the LOCAL server (`http://127.0.0.1:<port>/...`); a peer's
    // artifact still points here and is proxied via `?machineId=`, so requiring
    // the loopback host plus our own port is both sufficient for remote
    // artifacts and enough to stop a hostile URL from turning this into an
    // arbitrary-fetch primitive.
    let expected_port = info.port;
    let parsed = url::Url::parse(&url).map_err(|_| "Invalid download URL.".to_string())?;
    let host_ok = matches!(
        parsed.host_str(),
        Some("127.0.0.1") | Some("localhost") | Some("::1") | Some("[::1]")
    );
    if parsed.scheme() != "http" || !host_ok || parsed.port() != Some(expected_port) {
        return Err("Refusing to download from a non-local URL.".into());
    }

    // Native save dialog without blocking the async runtime: the callback
    // variant dispatches the dialog to the main event loop and hands the chosen
    // path back over a channel.
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_file_name(save_dialog_name(&suggested_name))
        .save_file(move |path| {
            let _ = tx.send(path);
        });
    let chosen = rx
        .await
        .map_err(|_| "The save dialog closed unexpectedly.".to_string())?;
    let Some(target) = chosen else {
        return Ok("cancelled".into());
    };
    let target = target
        .into_path()
        .map_err(|error| format!("Couldn't resolve the save location: {error}"))?;

    // Stream to disk rather than buffering the whole response: artifacts can be
    // videos.
    use futures_util::StreamExt as _;
    use tokio::io::AsyncWriteExt as _;
    let response = reqwest::get(&url)
        .await
        .map_err(|error| format!("Download failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("Download failed: HTTP {}", response.status()));
    }
    let mut stream = response.bytes_stream();
    let mut file = tokio::fs::File::create(&target)
        .await
        .map_err(|error| format!("Couldn't create '{}': {error}", target.display()))?;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("Download interrupted: {error}"))?;
        file.write_all(&chunk)
            .await
            .map_err(|error| format!("Couldn't write '{}': {error}", target.display()))?;
    }
    file.flush()
        .await
        .map_err(|error| format!("Couldn't finish writing '{}': {error}", target.display()))?;
    Ok(target.to_string_lossy().into_owned())
}

/// Open (or focus) a dedicated window scoped to one project. The frontend
/// detects the `project-<id>` window label and renders in solo mode, so each
/// GNOME workspace can hold its own project window.
#[tauri::command]
async fn open_project_window(
    app: tauri::AppHandle,
    project_id: String,
    name: String,
) -> Result<(), String> {
    use tauri::Manager;
    // Window labels only allow [a-zA-Z0-9-/:_]; project ids are UUIDs, but
    // validate anyway so a hostile id can't smuggle label syntax.
    if !project_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err("invalid project id".into());
    }
    let label = format!("project-{project_id}");
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.unminimize();
        let _ = win.set_focus();
        return Ok(());
    }
    tauri::WebviewWindowBuilder::new(&app, &label, tauri::WebviewUrl::App("index.html".into()))
        .title(format!("{name} — Threadknot"))
        .inner_size(1100.0, 820.0)
        .min_inner_size(400.0, 500.0)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Native desktop notification (the webview has no Notification API here).
/// Fired by the frontend when a turn finishes / needs input while unfocused.
/// Returns only after the operating-system notification service accepts or
/// rejects it so failures are visible instead of disappearing in a thread.
#[tauri::command]
async fn notify(
    title: String,
    body: String,
) -> Result<notifications::NotificationReceipt, String> {
    debug_log(&format!("[notify] called title={title:?}"));
    let result = tauri::async_runtime::spawn_blocking(move || notifications::send(&title, &body))
        .await
        .map_err(|error| format!("notification worker failed: {error}"))?;
    debug_log(&format!("[notify] result -> {result:?}"));
    result
}

/// Deterministic settings/CLI probe that bypasses focus suppression and does
/// not require a live agent event.
#[tauri::command]
async fn test_notification() -> Result<notifications::NotificationReceipt, String> {
    let result = tauri::async_runtime::spawn_blocking(notifications::send_test)
        .await
        .map_err(|error| format!("notification test worker failed: {error}"))?;
    debug_log(&format!("[notify-test] result -> {result:?}"));
    result
}

pub fn build_server_state() -> anyhow::Result<(server::ServerState, ServerInfo)> {
    let store = Arc::new(store::Store::open()?);
    let config = store.server_config()?;
    // Mesh identity + one-time migration (thread stamping, workspace
    // wrapping) before anything reads the store.
    store.migrate_mesh(&config.server_id)?;
    let device = Arc::new(device::Device::load(store.dir(), &config.server_id)?);
    let hub = agents::Hub::new(store, config.port);
    let peer_registry = Arc::new(peers::PeerRegistry::open(hub.store.dir())?);
    let peernet = peernet::PeerNet::new(
        peer_registry,
        Arc::clone(&hub),
        Arc::clone(&device),
        config.port,
    );
    let mobile = Arc::new(mobile::MobileStore::open(&store::data_dir())?);
    let browser_profiles = Arc::new(browser_profiles::BrowserProfileStore::open(
        &store::data_dir(),
        &config.server_id,
    )?);
    let remote = Arc::new(remote::RemoteStore::open(&store::data_dir(), config.port)?);
    // This machine's mesh identity. Minted on first run and then stable for the
    // life of the install: regenerating it would silently unpair every peer,
    // since each one pinned the certificate authority at pairing.
    // The connector forwards to the strict ingress and nowhere else, so it is
    // handed that port at construction and has no way to be told another one.
    let connector = connector::Connector::open(&store::data_dir(), remote.port())?;
    let mesh_identity = Arc::new(mesh::MeshIdentity::load_or_create(
        &store::data_dir(),
        &config.server_id,
    )?);
    let browser_sessions = Arc::new(ingress::BrowserSessions::open(&store::data_dir())?);
    let lan_url = server::lan_url(config.port, &config.token);
    let info = ServerInfo {
        port: config.port,
        token: config.token.clone(),
        lan_url: lan_url.clone(),
    };
    // A thread's browser attaches to whichever signed-in profile its settings
    // name; everything else gets a disposable browser. Resolving lazily (rather
    // than passing a profile at every call site) keeps the browser layer free
    // of thread/store knowledge.
    let browsers = Arc::new(browser::BrowserRegistry::new());
    {
        let profiles = Arc::clone(&browser_profiles);
        let threads = Arc::clone(&hub.store);
        browsers.set_profile_resolver(Arc::new(move |key: &str| {
            let Some(thread) = threads.thread(key) else {
                return Ok(None);
            };
            match thread.settings.browser_profile_id.as_deref() {
                Some(id) if !id.is_empty() => {
                    let spec = profiles.spec(id)?;
                    profiles.touch(id);
                    Ok(Some(spec))
                }
                _ => Ok(None),
            }
        }));
    }

    let state = server::ServerState {
        hub,
        config,
        device,
        peernet,
        mesh: mesh_identity,
        pairing_challenges: Arc::new(mesh::PairingChallenges::new()),
        exec: Arc::new(exec::ExecRegistry::new()),
        connector,
        lan_url,
        agents_cache: Arc::new(RwLock::new(None)),
        terms: Arc::new(term::TermRegistry::new(store::data_dir().join("terminals"))),
        browsers,
        browser_profiles,
        mobile,
        dictation: Arc::new(dictation::Dictation::open(&store::data_dir())?),
        sessions: sessions::SessionRegistry::new(),
        browser_sessions,
        remote,
        // The LAN/Tauri listener. `server::run` clones this state with
        // `IngressPolicy::Remote` for the loopback listener.
        policy: ingress::IngressPolicy::Compat,
    };
    Ok((state, info))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let (state, info) = build_server_state().expect("failed to init threadknot state");
    let clipboard_state = clipboard::ClipboardState::new(state.hub.store.clone());
    // Held past the server task so the exit handler can close browsers cleanly:
    // a signed-in Chrome that dies with the process keeps its profile locked
    // and its last cookie writes unflushed.
    let browsers = Arc::clone(&state.browsers);

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(info)
        .manage(clipboard_state)
        .setup(move |_app| {
            tauri::async_runtime::spawn(async move {
                server::run(state).await;
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            server_info,
            clipboard_image,
            clipboard_images,
            clipboard::copy_project_file,
            clipboard::copy_artifact_file,
            notify,
            test_notification,
            debug_note,
            download_file,
            open_project_window
        ])
        .build(tauri::generate_context!())
        .expect("error while running threadknot")
        .run(move |_app, event| {
            if let tauri::RunEvent::Exit = event {
                let browsers = Arc::clone(&browsers);
                tauri::async_runtime::block_on(async move {
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        browsers.shutdown_all(),
                    )
                    .await;
                });
            }
        });
}
