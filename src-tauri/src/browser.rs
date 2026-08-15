//! Agent-driveable browser: a headless Chrome instance driven over the Chrome
//! DevTools Protocol (chromiumoxide) and exposed two ways over ONE shared
//! session per key:
//!   * a dedicated binary WebSocket (`/browser`) — binary frames = JPEG
//!     screencast pixels, text frames = JSON control (mouse/key/navigate/…),
//!     mirroring `term.rs`. This is the human pane.
//!   * `invoke(key, op, input)` — the same actions as structured calls, used by
//!     the MCP server (`mcp.rs`) so an agent drives the *same* browser the human
//!     sees (unified).
//!
//! Sessions are keyed by a string (a thread id in practice), survive client
//! disconnects, and replay the last frame on attach. Replaces the old
//! cross-origin `<iframe>`, which could not be driven at all.

use crate::server::ServerState;
use anyhow::{anyhow, bail, Context, Result};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use base64::Engine;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::accessibility::{
    AxNode, AxNodeId, EnableParams as EnableAccessibilityParams, GetFullAxTreeParams,
};
use chromiumoxide::cdp::browser_protocol::browser::{
    EventDownloadProgress, EventDownloadWillBegin, SetDownloadBehaviorBehavior,
    SetDownloadBehaviorParams,
};
use chromiumoxide::cdp::browser_protocol::dom::{
    BackendNodeId, DescribeNodeParams, GetBoxModelParams, GetDocumentParams, GetFrameOwnerParams,
    QuerySelectorParams, ResolveNodeParams, ScrollIntoViewIfNeededParams, SetFileInputFilesParams,
};
use chromiumoxide::cdp::browser_protocol::emulation::{
    SetDeviceMetricsOverrideParams, SetPageScaleFactorParams,
};
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, EnableParams as EnableFetchParams, EventRequestPaused, FailRequestParams,
    RequestPattern, RequestStage,
};
use chromiumoxide::cdp::browser_protocol::network::{ErrorReason, ResourceType};
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, DispatchMouseEventParams, DispatchMouseEventType,
    InsertTextParams, MouseButton,
};
use chromiumoxide::cdp::browser_protocol::log::{
    EnableParams as EnableLogParams, EventEntryAdded,
};
use chromiumoxide::cdp::browser_protocol::network::{
    EnableParams as EnableNetworkParams, EventLoadingFailed, EventRequestWillBeSent,
    EventResponseReceived, GetResponseBodyParams,
};
use chromiumoxide::cdp::browser_protocol::page::{
    AddScriptToEvaluateOnNewDocumentParams, CaptureScreenshotFormat,
    EnableParams as EnablePageParams, EventFrameNavigated,
    EventJavascriptDialogOpening, EventNavigatedWithinDocument, EventScreencastFrame,
    GetFrameTreeParams, HandleJavaScriptDialogParams, ScreencastFrameAckParams,
    StartScreencastFormat, StartScreencastParams, StopScreencastParams,
};
use chromiumoxide::cdp::browser_protocol::target::{EventTargetCreated, EventTargetDestroyed};
use chromiumoxide::cdp::js_protocol::runtime::{
    CallArgument, CallFunctionOnParams, EnableParams as EnableRuntimeParams, EventConsoleApiCalled,
    EventExceptionThrown,
};
use chromiumoxide::page::ScreenshotParams;
use chromiumoxide::Page;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, Mutex as AsyncMutex};
use tokio::task::JoinHandle;
use uuid::Uuid;

const DEFAULT_WIDTH: i64 = 1280;
const DEFAULT_HEIGHT: i64 = 800;
/// Cap frames so phone/LAN clients aren't flooded; Chrome downscales to fit.
const MAX_FRAME_WIDTH: i64 = 1600;
const MAX_FRAME_HEIGHT: i64 = 1200;
const ACTIVITY_HISTORY_LIMIT: usize = 40;
const CONSOLE_HISTORY_LIMIT: usize = 200;
const NETWORK_HISTORY_LIMIT: usize = 300;
const DOWNLOAD_HISTORY_LIMIT: usize = 50;
/// Snapshot budget. Generous because the tree is de-duplicated: a page that
/// truncates here is genuinely huge, not merely verbose.
const SNAPSHOT_LINE_LIMIT: usize = 900;
const SNAPSHOT_CHAR_LIMIT: usize = 40_000;
/// A session with no attached viewer and no agent action for this long is
/// closed; its Chrome (~300 MB RSS) and profile directory go with it.
const SESSION_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);
/// Grace period before an unreferenced profile directory is swept at startup.
const PROFILE_SWEEP_MIN_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);
const PROFILE_PREFIX: &str = "threadknot-chrome-";

/// The presentation overlay (cursor, click ripples, captions, spotlight).
/// Injected into every document so a recording can draw a pointer that headless
/// Chrome would otherwise never render.
const OVERLAY_SOURCE: &str = include_str!("browser_overlay.js");

/// How often the driver dispatches a real CDP mouse move while gliding. The
/// overlay animates the visible pointer locally at display rate; these events
/// exist so the PAGE reacts (hover states, tooltips, menus), which needs far
/// fewer samples than smooth motion does.
const GLIDE_SAMPLE_MS: u64 = 40;

/// Profile directories of Chrome processes that are alive right now, so a sweep
/// never deletes a profile out from under a running browser (including one
/// owned by a second Threadknot instance). Linux-only; other platforms fall back to
/// the age check alone.
fn live_profile_dirs() -> Option<HashSet<String>> {
    let mut live = HashSet::new();
    // `None` means "cannot tell" (no /proc), which is a very different claim
    // from "nothing is running" and gets a much more conservative sweep.
    let entries = std::fs::read_dir("/proc").ok()?;
    for entry in entries.flatten() {
        let Ok(raw) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        // Chrome does not reliably NUL-separate its arguments here — the whole
        // command line can arrive as one blob — so scan for the flag rather
        // than matching argument prefixes. Getting this wrong would delete a
        // running browser's profile out from under it.
        let text = String::from_utf8_lossy(&raw).replace('\0', " ");
        for (index, _) in text.match_indices("--user-data-dir=") {
            let value = &text[index + "--user-data-dir=".len()..];
            let dir = value
                .split(|ch: char| ch.is_whitespace())
                .next()
                .unwrap_or_default();
            if !dir.is_empty() {
                live.insert(dir.to_string());
            }
        }
    }
    Some(live)
}

/// Kill headless Chromes whose Threadknot is gone. A quit or SIGKILLed app never
/// drops its `Browser` handles, so every Chrome it spawned keeps running —
/// hundreds of MB each — for as long as the machine is up. Worse, a *signed-in*
/// Chrome keeps holding its profile's SingletonLock, which makes that profile
/// unopenable in the next Threadknot until the orphan dies.
///
/// A Chrome belonging to a live Threadknot still has that Threadknot as its parent;
/// anything re-parented to init (or a subreaper like `systemd --user`) is an
/// orphan. Disposable orphans are SIGKILLed. Signed-in orphans get SIGTERM
/// first so Chrome flushes its cookie jar and removes its lock cleanly, with
/// SIGKILL only for stragglers.
///
/// Returns the *disposable* profile directories freed — safe to delete.
/// Signed-in profile directories are never returned: they ARE the stored
/// session. Linux-only; a no-op elsewhere.
fn kill_orphan_browsers() -> HashSet<String> {
    let mut freed = HashSet::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return freed;
    };
    let temp_marker = format!(
        "--user-data-dir={}",
        std::env::temp_dir().join(PROFILE_PREFIX).display()
    );
    let profiles_marker = format!(
        "--user-data-dir={}",
        crate::store::data_dir().join("browser").join("profiles").display()
    );
    let mut signed_in: Vec<i32> = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        let Ok(raw) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let cmdline = String::from_utf8_lossy(&raw).replace('\0', " ");
        // Renderer/zygote children die with their browser process; only the
        // top-level browser is targeted.
        if cmdline.contains("--type=") {
            continue;
        }
        let disposable = if cmdline.contains(&temp_marker) {
            true
        } else if cmdline.contains(&profiles_marker) {
            false
        } else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // stat is "pid (comm) state ppid …"; comm can contain spaces and
        // parens, so split on the LAST ')'.
        let ppid = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().nth(1))
            .and_then(|value| value.parse::<i32>().ok());
        let orphaned = match ppid {
            Some(1) => true,
            // Orphans don't always land on PID 1 — a session manager acting as
            // a subreaper adopts them instead. Owned means the parent is a live
            // Threadknot; any other parent means the Threadknot that spawned it died.
            Some(ppid) => {
                let parent = std::fs::read(format!("/proc/{ppid}/cmdline")).unwrap_or_default();
                !String::from_utf8_lossy(&parent).contains("threadknot")
            }
            None => false,
        };
        if !orphaned {
            continue;
        }
        if disposable {
            let index = cmdline.find(&temp_marker).unwrap_or_default();
            let dir = cmdline[index + "--user-data-dir=".len()..]
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string();
            if kill_process(pid, "KILL") && !dir.is_empty() {
                freed.insert(dir);
            }
        } else if kill_process(pid, "TERM") {
            signed_in.push(pid);
        }
    }
    // Give signed-in Chromes a moment to flush and exit on their own; only a
    // hung one is force-killed (its cookie DB is journaled, so this loses at
    // most the last unflushed writes, not the session).
    if !signed_in.is_empty() {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline
            && signed_in
                .iter()
                .any(|pid| std::path::Path::new(&format!("/proc/{pid}")).exists())
        {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        for pid in &signed_in {
            if std::path::Path::new(&format!("/proc/{pid}")).exists() {
                kill_process(*pid, "KILL");
            }
        }
        tracing::info!(
            "reclaimed {} orphaned signed-in browser(s) from a previous run",
            signed_in.len()
        );
    }
    if !freed.is_empty() {
        tracing::info!("terminated {} orphaned browser process(es)", freed.len());
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    freed
}

/// Send a signal without pulling in a libc dependency.
fn kill_process(pid: i32, signal: &str) -> bool {
    std::process::Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(pid.to_string())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Remove Chrome profiles left behind by a previous run. `Drop` handles the
/// normal path, but a SIGKILL (`scripts/restart.sh`) or a crash never runs it,
/// and each abandoned profile is ~135 MB.
fn sweep_orphan_profiles() {
    let orphaned = kill_orphan_browsers();
    for dir in &orphaned {
        let _ = std::fs::remove_dir_all(dir);
    }
    let live = live_profile_dirs();
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let (mut removed, mut bytes) = (0u32, 0u64);
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with(PROFILE_PREFIX) || !path.is_dir() {
            continue;
        }
        if live
            .as_ref()
            .is_some_and(|live| live.contains(&path.to_string_lossy().into_owned()))
        {
            continue;
        }
        let age = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok());
        // A running Chrome is always visible in /proc, so a short grace period
        // is enough there. Without /proc we cannot tell, so only clearly
        // abandoned directories are touched.
        let grace = if live.is_some() {
            std::time::Duration::from_secs(300)
        } else {
            PROFILE_SWEEP_MIN_AGE
        };
        if age.is_none_or(|age| age < grace) {
            continue;
        }
        let size = dir_size(&path);
        if std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
            bytes += size;
        }
    }
    if removed > 0 {
        tracing::info!(
            "swept {removed} orphaned browser profile(s), {} MB reclaimed",
            bytes / 1_048_576
        );
    }
}

fn dir_size(path: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(meta) if meta.is_dir() => dir_size(&entry.path()),
            Ok(meta) => meta.len(),
            Err(_) => 0,
        })
        .sum()
}

/// Locate a Chrome/Chromium binary; `None` lets chromiumoxide auto-detect.
fn find_chrome() -> Option<String> {
    if let Ok(p) = std::env::var("THREADKNOT_CHROME") {
        if !p.is_empty() {
            return Some(p);
        }
    }
    for name in [
        "google-chrome-stable",
        "google-chrome",
        "chromium",
        "chromium-browser",
        "chrome",
        "brave-browser",
    ] {
        if let Ok(p) = which::which(name) {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    None
}

/// A headful desktop User-Agent for `chrome_path`, or `None` if its version
/// can't be read. Chrome reports `Google Chrome 150.0.7871.128`; the UA it
/// actually sends freezes everything after the major version, so
/// `Chrome/150.0.0.0` is the genuine string minus the `Headless` token.
fn desktop_user_agent(chrome_path: &str) -> Option<String> {
    let out = std::process::Command::new(chrome_path)
        .arg("--version")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let major: String = text
        .split_whitespace()
        .find(|word| word.chars().next().is_some_and(|c| c.is_ascii_digit()))?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if major.is_empty() {
        return None;
    }
    // Chrome's UA platform token is the *build* platform, so keep it aligned
    // with the host we actually run on rather than always claiming Linux.
    let platform = if cfg!(target_os = "macos") {
        "Macintosh; Intel Mac OS X 10_15_7"
    } else if cfg!(windows) {
        "Windows NT 10.0; Win64; x64"
    } else {
        "X11; Linux x86_64"
    };
    Some(format!(
        "Mozilla/5.0 ({platform}) AppleWebKit/537.36 (KHTML, like Gecko) \
         Chrome/{major}.0.0.0 Safari/537.36"
    ))
}

/// Viewport override for a page.
///
/// `screenWidth`/`screenHeight` matter for bot checks: headless Chrome reports a
/// fixed 800x600 *screen* regardless of window size, so the default state is a
/// 1280x800 window on an 800x600 screen — a window larger than the display it
/// sits on, which no real browser can produce. Pin the screen to a common
/// desktop resolution that always contains the viewport.
fn device_metrics(width: i64, height: i64) -> SetDeviceMetricsOverrideParams {
    const COMMON_SCREENS: [(i64, i64); 4] =
        [(1920, 1080), (2560, 1440), (3840, 2160), (10_000, 10_000)];
    let (screen_w, screen_h) = COMMON_SCREENS
        .into_iter()
        .find(|(w, h)| *w >= width && *h >= height)
        .unwrap_or((width, height));
    SetDeviceMetricsOverrideParams {
        screen_width: Some(screen_w),
        screen_height: Some(screen_h),
        ..SetDeviceMetricsOverrideParams::new(width, height, 1.0, false)
    }
}

/// Fanned out to every attached client of a session.
#[derive(Clone)]
enum BrowserMsg {
    /// A JPEG screencast frame (raw bytes).
    Frame(Vec<u8>),
    /// The page navigated; carries the new URL so address bars stay in sync
    /// even when the *agent* drives the navigation.
    Nav(String),
    /// A structured agent action. Human viewers use this for the live cursor,
    /// target highlight, status HUD, and reconnect-safe activity trail.
    Activity(BrowserActivity),
    /// A page-owned JavaScript dialog. `None` means the dialog was handled.
    Dialog(Option<BrowserDialog>),
    /// Chrome or its CDP transport stopped. View sockets close so clients
    /// reconnect through the registry and receive a fresh session.
    EngineStopped,
    /// Current tab inventory. Popups, agent tab actions, and human tab actions
    /// all converge on this one shared active-page state.
    Tabs(Vec<BrowserTab>),
    /// Answer to a viewer's right-click hit-test: what sits under the point
    /// (selection text, a link, an image, an editable field), so the viewer can
    /// build a native-feeling context menu. Carries the requesting viewer's
    /// nonce so a co-viewer's menu doesn't open from someone else's click.
    Context(Value),
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserBounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserActivity {
    id: String,
    actor: &'static str,
    phase: &'static str,
    action: String,
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    x: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    y: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bounds: Option<BrowserBounds>,
    ts: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserDialog {
    kind: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_prompt: Option<String>,
    url: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserTab {
    id: String,
    url: String,
    title: String,
    active: bool,
}

#[derive(Default)]
struct ElementRefs {
    by_ref: HashMap<String, BackendNodeId>,
    by_backend: HashMap<BackendNodeId, String>,
    next: u64,
    /// True once a snapshot has been taken for the current document. Actions on
    /// an unknown ref can then say *why* it is unknown.
    fresh: bool,
}

#[derive(Clone, Debug)]
struct TargetPoint {
    x: f64,
    y: f64,
    bounds: Option<BrowserBounds>,
    /// The element this point came from, when it came from one. Carried so a
    /// recorded flow can hand the overlay the actual node its focus effects are
    /// framing, letting them clear themselves the moment it stops rendering.
    backend: Option<BackendNodeId>,
}

/// One frame's accessibility tree plus the element that hosts it.
struct FrameDoc {
    url: String,
    owner: Option<BackendNodeId>,
    nodes: Vec<AxNode>,
}

impl FrameDoc {
    fn root(&self) -> Option<&AxNode> {
        let ids: HashSet<&str> = self.nodes.iter().map(|node| node.node_id.as_ref()).collect();
        self.nodes.iter().find(|node| {
            node.parent_id
                .as_ref()
                .is_none_or(|parent| !ids.contains(parent.as_ref()))
        })
    }

    fn node(&self, id: &AxNodeId) -> Option<&AxNode> {
        self.nodes
            .iter()
            .find(|node| node.node_id.as_ref() == id.as_ref())
    }
}

/// Renders the merged accessibility tree into the indented outline agents read.
struct SnapshotRender {
    lines: Vec<String>,
    by_ref: HashMap<String, BackendNodeId>,
    by_backend: HashMap<BackendNodeId, String>,
    truncated: bool,
    /// Signed-in browsers keep Chrome's site isolation, which puts cross-origin
    /// frames out of this session's reach. Say so rather than omitting them.
    isolated: bool,
}

/// What the walk decided to do with a node.
enum NodeAction {
    /// Print it, and indent its children beneath it.
    Emit,
    /// Print nothing but keep walking children at the SAME depth. Wrapper divs
    /// and layout tables carry no meaning; dropping their subtree would hide
    /// real content, and indenting for them would be noise.
    Flatten,
    /// Print nothing and skip the subtree.
    Drop,
}

impl SnapshotRender {
    #[allow(clippy::too_many_arguments)]
    fn walk(
        &mut self,
        node: &AxNode,
        frames: &[FrameDoc],
        frame_index: usize,
        depth: usize,
        inherited_name: &str,
        frame_by_owner: &HashMap<BackendNodeId, usize>,
        refs: &mut ElementRefs,
    ) {
        if self.truncated {
            return;
        }
        let frame = &frames[frame_index];
        let role = ax_string(node.role.as_ref()).unwrap_or_else(|| "node".to_string());
        let role_lower = role.to_ascii_lowercase();
        let name = ax_string(node.name.as_ref()).unwrap_or_default();
        let value = ax_string(node.value.as_ref()).unwrap_or_default();

        let action = classify_ax_node(node.ignored, &role_lower, &name, &value, inherited_name);
        let mut child_depth = depth;
        let mut child_name = inherited_name.to_string();
        let mut empty_container_line = None;

        if matches!(action, NodeAction::Drop) {
            return;
        }
        if matches!(action, NodeAction::Emit) {
            // A cell whose text is just its own links restated ("Stolen Buttons
            // (example.com)" above links with those exact names) is pure
            // duplication; a cell that adds real information ("582 points by …")
            // keeps its text.
            let name = if matches!(role_lower.as_str(), "cell" | "gridcell")
                && name_is_restated_by_children(&name, node, frame)
            {
                String::new()
            } else {
                name
            };
            let reference = if is_interactive_role(&role_lower) {
                node.backend_dom_node_id.map(|backend| {
                    let reference = refs.by_backend.get(&backend).cloned().unwrap_or_else(|| {
                        refs.next += 1;
                        format!("e{}", refs.next)
                    });
                    self.by_ref.insert(reference.clone(), backend);
                    self.by_backend.insert(backend, reference.clone());
                    reference
                })
            } else {
                None
            };
            self.lines
                .push(render_ax_line(node, depth, &role, &name, &value, reference.clone()));
            if self.lines.len() >= SNAPSHOT_LINE_LIMIT
                || self.lines.iter().map(String::len).sum::<usize>() > SNAPSHOT_CHAR_LIMIT
            {
                self.truncated = true;
                return;
            }
            child_depth = depth + 1;
            // Children are deduplicated against BOTH the parent's name and its
            // value: a filled textbox otherwise repeats its own contents as a
            // StaticText child.
            if !name.is_empty() || !value.is_empty() {
                child_name = format!("{name} {value}");
            }
            // An unnamed container that turns out to hold nothing is noise
            // (real pages are full of empty spacer rows), so remember where its
            // line is and drop it if the subtree adds nothing.
            if reference.is_none() && name.is_empty() && value.is_empty() {
                empty_container_line = Some(self.lines.len() - 1);
            }
        }

        // An <iframe> element node owns a whole document: splice it in here so
        // its controls are addressable in the same snapshot and ref space.
        if let Some(backend) = node.backend_dom_node_id {
            if let Some(child_frame) = frame_by_owner.get(&backend).copied() {
                let child = &frames[child_frame];
                self.lines.push(format!(
                    "{}- iframe {:?}",
                    "  ".repeat(child_depth.min(12)),
                    truncate_text(&child.url, 120)
                ));
                match child.root() {
                    Some(root) => self.walk(
                        root,
                        frames,
                        child_frame,
                        child_depth + 1,
                        "",
                        frame_by_owner,
                        refs,
                    ),
                    None => self.lines.push(format!(
                        "{}- {}",
                        "  ".repeat((child_depth + 1).min(12)),
                        if self.isolated {
                            "(cross-origin frame — contents hidden because this browser is signed in and keeps site isolation on)"
                        } else {
                            "(frame content unavailable)"
                        }
                    )),
                }
            }
        }

        if let Some(child_ids) = node.child_ids.as_ref() {
            for child_id in child_ids {
                let Some(child) = frame.node(child_id) else {
                    continue;
                };
                self.walk(
                    child,
                    frames,
                    frame_index,
                    child_depth,
                    &child_name,
                    frame_by_owner,
                    refs,
                );
            }
        }

        if let Some(index) = empty_container_line {
            if self.lines.len() == index + 1 && !self.truncated {
                self.lines.remove(index);
            }
        }
    }
}

/// True when a cell's text adds nothing beyond the names of the controls it
/// contains. Removing each descendant control's name from the cell text leaves
/// only punctuation when the cell is a restatement.
fn name_is_restated_by_children(name: &str, node: &AxNode, frame: &FrameDoc) -> bool {
    if name.trim().is_empty() {
        return false;
    }
    let mut remainder = name.to_string();
    let mut pending = vec![node];
    let mut budget = 64;
    while let Some(current) = pending.pop() {
        if budget == 0 {
            break;
        }
        budget -= 1;
        if !std::ptr::eq(current, node) {
            if let Some(child_name) = ax_string(current.name.as_ref()) {
                if !child_name.trim().is_empty() {
                    remainder = remainder.replace(child_name.trim(), " ");
                }
            }
        }
        if let Some(children) = current.child_ids.as_ref() {
            for child_id in children {
                if let Some(child) = frame.node(child_id) {
                    pending.push(child);
                }
            }
        }
    }
    !remainder.chars().any(char::is_alphanumeric)
}

/// One live headless browser + its screencast fan-out. The `Browser` and the
/// handler task are kept alive for the session's lifetime.
pub struct BrowserSession {
    /// The shared active page. Page is cheaply cloneable; never hold this
    /// synchronous mutex guard across an await.
    page: Mutex<Page>,
    /// Kept so the CDP connection (and Chrome) stays alive; dropping closes it.
    _browser: Browser,
    /// Frame + nav fan-out to attached sockets.
    tx: broadcast::Sender<BrowserMsg>,
    /// Last frame, replayed to a freshly-attaching client so the pane isn't blank.
    last_frame: Mutex<Option<Vec<u8>>>,
    /// Last-navigated URL (best-effort; updated on navigate).
    url: Mutex<String>,
    /// Current emulated viewport, applied again when the active tab changes.
    viewport: Mutex<(i64, i64)>,
    /// Element references from the most recent semantic snapshot.
    refs: Mutex<ElementRefs>,
    /// Recent browser diagnostics exposed through MCP.
    console: Mutex<VecDeque<Value>>,
    network: Mutex<VecDeque<Value>>,
    dialog: Mutex<Option<BrowserDialog>>,
    /// The compact, reconnect-safe human audit trail of agent browser actions.
    activities: Mutex<VecDeque<BrowserActivity>>,
    /// MCP actions are serialized. Two model tool calls must never race the
    /// same page and leave the human watching an impossible interleaving.
    action_lock: AsyncMutex<()>,
    /// Serializes popup discovery and explicit switch/new/close operations.
    tab_lock: AsyncMutex<()>,
    /// Pages whose event streams have already been wired into this session.
    attached_pages: Mutex<HashSet<String>>,
    /// Background tasks (CDP handler pump + screencast listener) aborted on drop.
    tasks: Mutex<Vec<JoinHandle<()>>>,
    /// This session's Chrome user-data directory. Disposable sessions get a
    /// temp dir removed on drop; signed-in sessions point at the profile store.
    profile_dir: PathBuf,
    /// Files this browser downloaded, and where they landed on disk.
    downloads: Mutex<VecDeque<Value>>,
    /// Download + screenshot output directory for this session.
    work_dir: PathBuf,
    /// Attached human viewers. A session with viewers is never reaped.
    viewers: Arc<std::sync::atomic::AtomicUsize>,
    /// Last agent action or viewer attach, for idle reaping.
    last_used: Mutex<std::time::Instant>,
    /// The signed-in profile this session is attached to, if any. `None` means
    /// a disposable browser: no stored session, no origin restrictions.
    profile: Option<crate::browser_profiles::ProfileSpec>,
    /// Raw JPEG frames for the recorder. Separate from `tx` so the recorder can
    /// consume frames without decoding `BrowserMsg`, and so the extra clone per
    /// frame only happens while something is actually recording.
    frames_tx: broadcast::Sender<Vec<u8>>,
    /// The in-progress recording, if any. One per session: a second concurrent
    /// capture of the same screencast would only duplicate the first.
    recording: AsyncMutex<Option<crate::recorder::Recording>>,
    /// Virtual pointer position, in frame coordinates. Synthetic CDP input has
    /// no pointer of its own, so continuity between actions is ours to keep:
    /// without it every glide would have to start from an unknown place.
    cursor: Mutex<(f64, f64)>,
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        if let Ok(tasks) = self.tasks.lock() {
            for t in tasks.iter() {
                t.abort();
            }
        }
        // Disposable profiles are scratch space and go with the session. A
        // signed-in profile's directory IS the stored session — deleting it
        // here would silently sign the user out.
        if self.profile.is_none() {
            let _ = std::fs::remove_dir_all(&self.profile_dir);
        }
    }
}

impl BrowserSession {
    fn page(&self) -> Page {
        self.page.lock().unwrap().clone()
    }

    fn active_target_id(&self) -> String {
        self.page().target_id().as_ref().to_string()
    }

    pub fn current_url(&self) -> String {
        self.url.lock().unwrap().clone()
    }

    fn set_url(&self, url: String) {
        let mut current = self.url.lock().unwrap();
        if *current == url {
            return;
        }
        *current = url.clone();
        drop(current);
        let _ = self.tx.send(BrowserMsg::Nav(url));
    }

    /// Backend DOM ids are document-scoped. Anything that replaces the document
    /// — navigation, reload, or re-navigating to the SAME url — invalidates
    /// every ref, so a stale call fails with "take a fresh snapshot" instead of
    /// a raw CDP error or a click on a recycled node id.
    fn invalidate_refs(&self) {
        *self.refs.lock().unwrap() = ElementRefs::default();
    }

    /// A new document also resets the diagnostics rings, matching DevTools'
    /// default "clear on navigate": console/network entries always describe the
    /// page the agent is looking at now.
    /// Called when a new document commits. Console entries all belong to the
    /// old page, but the network log must KEEP the requests that produced this
    /// document — the top-level request is the first thing you look at when a
    /// page loads wrong, and clearing indiscriminately deleted it. Requests
    /// carry the loader id of the document they belong to, so scope by that.
    fn reset_for_document(&self, loader_id: &str) {
        self.invalidate_refs();
        self.console.lock().unwrap().clear();
        if loader_id.is_empty() {
            self.network.lock().unwrap().clear();
            return;
        }
        self.network
            .lock()
            .unwrap()
            .retain(|entry| entry.get("loaderId").and_then(Value::as_str) == Some(loader_id));
    }

    fn touch(&self) {
        *self.last_used.lock().unwrap() = std::time::Instant::now();
    }

    fn idle_for(&self) -> std::time::Duration {
        self.last_used.lock().unwrap().elapsed()
    }

    fn viewer_count(&self) -> usize {
        self.viewers.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn profile_id(&self) -> Option<String> {
        self.profile.as_ref().map(|profile| profile.id.clone())
    }

    pub fn profile_name(&self) -> Option<String> {
        self.profile.as_ref().map(|profile| profile.name.clone())
    }

    fn allowed_origins(&self) -> Option<&[String]> {
        self.profile.as_ref().map(|profile| profile.origins.as_slice())
    }

    /// Ask Chrome to shut down cleanly. Killing the process leaves the cookie
    /// jar unwritten, which for a signed-in profile means the login the user
    /// just performed is gone by the next session.
    async fn shutdown(&self) {
        let _ = self
            ._browser
            .execute(chromiumoxide::cdp::browser_protocol::browser::CloseParams::default())
            .await;
        // Give Chrome a moment to flush before Drop tears the connection down.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    }

    /// Refuse to leave the sites this profile is scoped to. Enforced in the
    /// browser as well (see `wire_origin_guard`); this is the fast, explicit
    /// answer the agent gets instead of a mystery blocked navigation.
    fn assert_origin_allowed(&self, url: &str) -> Result<()> {
        let Some(allowed) = self.allowed_origins() else {
            return Ok(());
        };
        let url = normalize_url(url);
        if crate::browser_profiles::origin_allowed(allowed, &url) {
            return Ok(());
        }
        let name = self.profile_name().unwrap_or_default();
        let scope = if allowed.iter().any(|entry| entry == "*") {
            "the web (http/https)".to_string()
        } else {
            allowed.join(", ")
        };
        Err(anyhow!(
            "this browser is signed in as {name:?} and may only visit {scope}. {url} is outside that scope — use a chat without a signed-in profile for general browsing"
        ))
    }

    fn activity_history(&self) -> Vec<BrowserActivity> {
        self.activities.lock().unwrap().iter().cloned().collect()
    }

    fn emit_activity(&self, mut next: BrowserActivity) {
        let mut history = self.activities.lock().unwrap();
        if let Some(existing) = history.iter_mut().find(|item| item.id == next.id) {
            if next.detail.is_none() {
                next.detail.clone_from(&existing.detail);
            }
            if next.x.is_none() {
                next.x = existing.x;
            }
            if next.y.is_none() {
                next.y = existing.y;
            }
            if next.bounds.is_none() {
                next.bounds.clone_from(&existing.bounds);
            }
            *existing = next.clone();
        } else {
            history.push_back(next.clone());
            while history.len() > ACTIVITY_HISTORY_LIMIT {
                history.pop_front();
            }
        }
        drop(history);
        let _ = self.tx.send(BrowserMsg::Activity(next));
    }

    fn activity(
        &self,
        id: &str,
        phase: &'static str,
        action: &str,
        label: &str,
        detail: Option<String>,
        target: Option<&TargetPoint>,
    ) {
        self.emit_activity(BrowserActivity {
            id: id.to_string(),
            actor: "agent",
            phase,
            action: action.to_string(),
            label: label.to_string(),
            detail,
            x: target.map(|point| point.x),
            y: target.map(|point| point.y),
            bounds: target.and_then(|point| point.bounds.clone()),
            ts: chrono::Utc::now().to_rfc3339(),
        });
    }

    fn set_dialog(&self, dialog: Option<BrowserDialog>) {
        *self.dialog.lock().unwrap() = dialog.clone();
        let _ = self.tx.send(BrowserMsg::Dialog(dialog));
    }

    async fn navigate(&self, raw: &str) -> Result<()> {
        let url = normalize_url(raw);
        let page = self.page();
        self.invalidate_refs();
        page.goto(&url).await?;
        let live_url = page.url().await?.unwrap_or(url);
        self.set_url(live_url);
        Ok(())
    }

    async fn history(&self, delta: i32) -> Result<()> {
        let js = if delta < 0 {
            "history.back()"
        } else {
            "history.forward()"
        };
        let _ = self.page().evaluate(js).await;
        self.refresh_url().await;
        Ok(())
    }

    async fn reload(&self) -> Result<()> {
        self.invalidate_refs();
        self.page().reload().await?;
        Ok(())
    }

    /// Re-read the live location after an implicit navigation (back/forward) so
    /// the broadcast URL matches what's on screen.
    async fn refresh_url(&self) {
        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
        if let Ok(res) = self.page().evaluate("location.href").await {
            if let Ok(href) = res.into_value::<String>() {
                if !href.is_empty() {
                    self.set_url(href);
                }
            }
        }
    }

    async fn mouse(
        &self,
        kind: DispatchMouseEventType,
        x: f64,
        y: f64,
        button: MouseButton,
        clicks: i64,
    ) -> Result<()> {
        let buttons = match kind {
            DispatchMouseEventType::MousePressed => mouse_button_mask(&button),
            DispatchMouseEventType::MouseMoved if clicks > 0 => mouse_button_mask(&button),
            _ => 0,
        };
        let mut ev = DispatchMouseEventParams::new(kind, x, y);
        ev.button = Some(button);
        ev.click_count = Some(clicks);
        // CDP's `buttons` is the currently held bitmask, not a click marker.
        // Releases must report zero or Chrome can leave the pointer "stuck."
        ev.buttons = Some(buttons);
        self.page().execute(ev).await?;
        Ok(())
    }

    async fn wheel(&self, x: f64, y: f64, dx: f64, dy: f64) -> Result<()> {
        let mut ev = DispatchMouseEventParams::new(DispatchMouseEventType::MouseWheel, x, y);
        ev.delta_x = Some(dx);
        ev.delta_y = Some(dy);
        self.page().execute(ev).await?;
        Ok(())
    }

    async fn key(
        &self,
        down: bool,
        key: &str,
        code: &str,
        key_code: i64,
        text: Option<&str>,
        modifiers: i64,
    ) -> Result<()> {
        let kind = if down {
            DispatchKeyEventType::KeyDown
        } else {
            DispatchKeyEventType::KeyUp
        };
        let mut ev = DispatchKeyEventParams::new(kind);
        if !key.is_empty() {
            ev.key = Some(key.to_string());
        }
        if !code.is_empty() {
            ev.code = Some(code.to_string());
        }
        if key_code != 0 {
            ev.windows_virtual_key_code = Some(key_code);
            ev.native_virtual_key_code = Some(key_code);
        }
        if modifiers != 0 {
            ev.modifiers = Some(modifiers);
        }
        if down {
            if let Some(t) = text {
                ev.text = Some(t.to_string());
            }
        }
        self.page().execute(ev).await?;
        Ok(())
    }

    async fn press_chord(&self, chord: &str) -> Result<()> {
        let parts: Vec<&str> = chord.split('+').filter(|part| !part.is_empty()).collect();
        if parts.is_empty() {
            return Err(anyhow!("key must not be empty"));
        }
        let mut modifiers = 0i64;
        let mut held = Vec::new();
        for part in &parts[..parts.len().saturating_sub(1)] {
            let normalized = normalize_modifier(part)
                .ok_or_else(|| anyhow!("unsupported key modifier: {part}"))?;
            modifiers |= modifier_mask(normalized);
            let (code, vk) = key_meta(normalized);
            self.key(true, normalized, &code, vk, None, modifiers)
                .await?;
            held.push(normalized);
        }
        let key = parts[parts.len() - 1];
        let (code, vk) = key_meta(key);
        // `text` is what makes Chrome run the key's DEFAULT action — Enter
        // submitting a form, Tab inserting a tab. Without it the page still
        // sees a keydown, so the action silently does nothing. Puppeteer and
        // Playwright both hardcode "\r" for Enter for exactly this reason.
        let printable = if modifiers == 0 {
            default_text_for_key(key)
        } else {
            None
        };
        self.key(true, key, &code, vk, printable, modifiers).await?;
        self.key(false, key, &code, vk, None, modifiers).await?;
        for modifier in held.into_iter().rev() {
            modifiers &= !modifier_mask(modifier);
            let (code, vk) = key_meta(modifier);
            self.key(false, modifier, &code, vk, None, modifiers)
                .await?;
        }
        Ok(())
    }

    /// Type like a person: a real keydown/keyup per character, so pages that
    /// listen for key events (search-as-you-type, hotkeys, tag inputs, code
    /// editors, maxlength filters) behave as they do for a human.
    /// `Input.insertText` — which fires no key events at all — stays available
    /// for long or non-typeable text where per-key dispatch is just slow.
    async fn type_text(&self, text: &str, per_key: bool) -> Result<()> {
        if !per_key || text.chars().count() > 300 || !text.chars().all(is_typeable_char) {
            self.insert_text(text).await?;
            return Ok(());
        }
        for ch in text.chars() {
            if ch == '\n' {
                self.press_chord("Enter").await?;
                continue;
            }
            let key = ch.to_string();
            let (code, vk) = key_meta(&key);
            self.key(true, &key, &code, vk, Some(&key), 0).await?;
            self.key(false, &key, &code, vk, None, 0).await?;
        }
        Ok(())
    }

    /// Type at a human cadence for a recording.
    ///
    /// [`type_text`] fires keys as fast as CDP accepts them, which on video
    /// looks like the text was pasted. Real typing is also *uneven*, so the
    /// delay is jittered around `per_key_ms` and a keystroke that ends a word
    /// rests a little longer — an exact metronome reads as synthetic too.
    async fn type_text_paced(&self, text: &str, per_key_ms: u64) -> Result<()> {
        if per_key_ms == 0 {
            return self.type_text(text, true).await;
        }
        // Cheap deterministic jitter: no RNG dependency, and a recording of the
        // same script twice stays comparable.
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        for (index, ch) in text.chars().enumerate() {
            if ch == '\n' {
                self.press_chord("Enter").await?;
            } else {
                let key = ch.to_string();
                let (code, vk) = key_meta(&key);
                self.key(true, &key, &code, vk, Some(&key), 0).await?;
                self.key(false, &key, &code, vk, None, 0).await?;
            }
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(index as u64);
            // +/-40% around the nominal interval.
            let spread = (per_key_ms * 8 / 10).max(1);
            let jitter = (seed >> 33) % spread.max(1);
            let mut delay = per_key_ms.saturating_sub(spread / 2) + jitter;
            if ch == ' ' || ch == '.' || ch == ',' {
                delay += per_key_ms;
            }
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }
        Ok(())
    }

    async fn press_key(&self, key: &str) -> Result<()> {
        self.press_chord(key).await
    }

    /// Scroll in eased increments instead of one jump, so a viewer can follow
    /// where the page went.
    async fn smooth_scroll(&self, dx: f64, dy: f64, ms: u64) -> Result<()> {
        let (x, y) = self.cursor_pos();
        // Keep the wheel inside the viewport even when the pointer is parked
        // off-canvas waiting for its first glide.
        let (vw, vh) = *self.viewport.lock().unwrap();
        let wx = x.clamp(4.0, vw as f64 - 4.0);
        let wy = y.clamp(4.0, vh as f64 - 4.0);
        let steps = (ms / 32).max(1);
        let mut sent_x = 0.0;
        let mut sent_y = 0.0;
        for step in 1..=steps {
            let t = step as f64 / steps as f64;
            let e = if t < 0.5 {
                4.0 * t * t * t
            } else {
                1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
            };
            let want_x = dx * e;
            let want_y = dy * e;
            self.wheel(wx, wy, want_x - sent_x, want_y - sent_y).await?;
            sent_x = want_x;
            sent_y = want_y;
            tokio::time::sleep(std::time::Duration::from_millis(32)).await;
        }
        Ok(())
    }

    async fn insert_text(&self, text: &str) -> Result<()> {
        self.page().execute(InsertTextParams::new(text)).await?;
        Ok(())
    }

    async fn set_viewport(&self, width: i64, height: i64) -> Result<()> {
        let w = width.clamp(200, MAX_FRAME_WIDTH);
        let h = height.clamp(200, MAX_FRAME_HEIGHT);
        *self.viewport.lock().unwrap() = (w, h);
        self.page().execute(device_metrics(w, h)).await?;
        Ok(())
    }

    async fn evaluate(&self, expr: &str) -> Result<Value> {
        let res = self.page().evaluate(expr).await?;
        Ok(res.into_value().unwrap_or(Value::Null))
    }

    /// Hit-test the active document at `(x, y)` for the viewer's right-click
    /// menu and broadcast the answer as a `context` frame. Runs only against
    /// the main document — `elementFromPoint` cannot see into cross-origin
    /// frames, and a signed-in profile keeps those isolated anyway — so a click
    /// over an unreadable frame yields the generic menu, never an error. The
    /// result is always emitted (empty on failure) so the viewer's menu opens
    /// rather than hanging on a promise that never resolves.
    async fn probe_context(&self, x: f64, y: f64, nonce: &str) -> Result<()> {
        // `getSelection().toString()` is capped so a select-all on a huge page
        // never ships the whole document back over the socket; the viewer only
        // needs enough to know a selection exists and to copy a sane amount.
        let js = format!(
            r#"(function(){{
  try {{
    var el = document.elementFromPoint({x}, {y});
    var out = {{ selection: "", editable: false }};
    var sel = window.getSelection ? String(window.getSelection()) : "";
    if (sel) out.selection = sel.slice(0, 100000);
    var node = el;
    while (node && node !== document.documentElement) {{
      var tag = (node.tagName || "").toLowerCase();
      if (!out.linkHref && tag === "a" && node.href) out.linkHref = node.href;
      if (!out.imgSrc && tag === "img" && (node.currentSrc || node.src)) out.imgSrc = node.currentSrc || node.src;
      node = node.parentElement;
    }}
    var isEditable = function(n) {{
      if (!n) return false;
      var t = (n.tagName || "").toLowerCase();
      if (n.isContentEditable) return true;
      if (t === "textarea") return true;
      if (t === "input") {{
        var ty = (n.getAttribute("type") || "text").toLowerCase();
        return ["button","checkbox","radio","submit","reset","file","image","range","color","hidden"].indexOf(ty) === -1;
      }}
      return false;
    }};
    out.editable = isEditable(el) || isEditable(document.activeElement);
    return out;
  }} catch (e) {{ return {{ selection: "", editable: false }}; }}
}})()"#
        );
        let info = self.evaluate(&js).await.unwrap_or(Value::Null);
        let mut ctx = match info {
            Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        ctx.insert("type".into(), json!("context"));
        ctx.insert("nonce".into(), json!(nonce));
        let _ = self.tx.send(BrowserMsg::Context(Value::Object(ctx)));
        Ok(())
    }

    async fn screenshot_b64(&self) -> Result<String> {
        let bytes = self.screenshot_bytes(false, None).await?;
        Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    /// Capture the viewport, the whole scrollable page, or one element's box.
    async fn screenshot_bytes(
        &self,
        full_page: bool,
        element: Option<BackendNodeId>,
    ) -> Result<Vec<u8>> {
        if let Some(backend) = element {
            let point = self.point_of_backend(backend).await?;
            let bounds = point
                .bounds
                .ok_or_else(|| anyhow!("element has no visible box to capture"))?;
            let clip = chromiumoxide::cdp::browser_protocol::page::Viewport {
                x: bounds.x,
                y: bounds.y,
                width: bounds.width,
                height: bounds.height,
                scale: 1.0,
            };
            let params = ScreenshotParams::builder()
                .format(CaptureScreenshotFormat::Png)
                .clip(clip)
                .build();
            return Ok(self.page().screenshot(params).await?);
        }
        let params = ScreenshotParams::builder()
            .format(CaptureScreenshotFormat::Png)
            .full_page(full_page)
            .build();
        Ok(self.page().screenshot(params).await?)
    }

    /// Write a screenshot to disk so it can be published as an artifact, cited
    /// as evidence, or diffed later. Agent-supplied paths are validated by the
    /// MCP layer; the default lands in this session's work directory.
    async fn screenshot_to_file(
        &self,
        path: Option<&str>,
        full_page: bool,
        element: Option<BackendNodeId>,
    ) -> Result<PathBuf> {
        let bytes = self.screenshot_bytes(full_page, element).await?;
        let path = match path {
            Some(path) => PathBuf::from(path),
            None => self.work_dir.join(format!(
                "screenshot-{}.png",
                chrono::Utc::now().format("%Y%m%d-%H%M%S%.3f")
            )),
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        std::fs::write(&path, bytes).with_context(|| format!("cannot write {}", path.display()))?;
        Ok(path)
    }

    async fn title(&self) -> String {
        self.page()
            .evaluate("document.title")
            .await
            .ok()
            .and_then(|r| r.into_value::<String>().ok())
            .unwrap_or_default()
    }

    async fn is_healthy(&self) -> bool {
        self.page().evaluate("1").await.is_ok()
    }

    async fn tabs(&self) -> Result<Vec<BrowserTab>> {
        let active = self.active_target_id();
        let mut tabs = Vec::new();
        for page in self._browser.pages().await? {
            let id = page.target_id().as_ref().to_string();
            let url = page
                .url()
                .await?
                .unwrap_or_else(|| "about:blank".to_string());
            let title = page
                .evaluate("document.title")
                .await
                .ok()
                .and_then(|result| result.into_value::<String>().ok())
                .unwrap_or_default();
            tabs.push(BrowserTab {
                active: id == active,
                id,
                url,
                title,
            });
        }
        Ok(tabs)
    }

    async fn emit_tabs(&self) {
        if let Ok(tabs) = self.tabs().await {
            let _ = self.tx.send(BrowserMsg::Tabs(tabs));
        }
    }

    async fn page_by_id(&self, id: &str) -> Result<Page> {
        self._browser
            .pages()
            .await?
            .into_iter()
            .find(|page| page.target_id().as_ref() == id)
            .ok_or_else(|| anyhow!("unknown or closed browser tab: {id}"))
    }

    async fn start_screencast(&self, page: &Page) -> Result<()> {
        page.execute(
            StartScreencastParams::builder()
                .format(StartScreencastFormat::Jpeg)
                .quality(60)
                .max_width(MAX_FRAME_WIDTH)
                .max_height(MAX_FRAME_HEIGHT)
                .every_nth_frame(1)
                .build(),
        )
        .await?;
        Ok(())
    }

    async fn activate_page(self: &Arc<Self>, page: Page) -> Result<()> {
        let _guard = self.tab_lock.lock().await;
        let next_id = page.target_id().as_ref().to_string();
        if next_id == self.active_target_id() {
            self.emit_tabs().await;
            return Ok(());
        }

        self.wire_page(page.clone()).await?;
        let previous = self.page();
        let _ = previous.execute(StopScreencastParams::default()).await;
        *self.page.lock().unwrap() = page.clone();
        *self.refs.lock().unwrap() = ElementRefs::default();
        *self.dialog.lock().unwrap() = None;
        *self.last_frame.lock().unwrap() = None;
        let (width, height) = *self.viewport.lock().unwrap();
        page.execute(device_metrics(width, height)).await?;
        self.start_screencast(&page).await?;
        let url = page
            .url()
            .await?
            .unwrap_or_else(|| "about:blank".to_string());
        *self.url.lock().unwrap() = url.clone();
        let _ = self.tx.send(BrowserMsg::Nav(url));
        let _ = self.tx.send(BrowserMsg::Dialog(None));
        self.emit_tabs().await;
        Ok(())
    }

    async fn new_tab(self: &Arc<Self>, url: &str) -> Result<BrowserTab> {
        let page = self._browser.new_page(normalize_url(url)).await?;
        self.activate_page(page.clone()).await?;
        self.tabs()
            .await?
            .into_iter()
            .find(|tab| tab.id == page.target_id().as_ref())
            .ok_or_else(|| anyhow!("new tab disappeared before it could be activated"))
    }

    async fn close_tab(self: &Arc<Self>, id: Option<&str>) -> Result<()> {
        let target_id = id
            .map(str::to_string)
            .unwrap_or_else(|| self.active_target_id());
        let pages = self._browser.pages().await?;
        let page = pages
            .iter()
            .find(|page| page.target_id().as_ref() == target_id)
            .cloned()
            .ok_or_else(|| anyhow!("unknown or closed browser tab: {target_id}"))?;
        if pages.len() == 1 {
            page.goto("about:blank").await?;
            self.set_url("about:blank".to_string());
            self.emit_tabs().await;
            return Ok(());
        }
        let was_active = target_id == self.active_target_id();
        let fallback = pages
            .into_iter()
            .rev()
            .find(|candidate| candidate.target_id().as_ref() != target_id);
        page.close().await?;
        if was_active {
            if let Some(fallback) = fallback {
                self.activate_page(fallback).await?;
            }
        } else {
            self.emit_tabs().await;
        }
        Ok(())
    }

    // ---- presentation + recording ------------------------------------------

    /// Call into the injected overlay. Failures are deliberately swallowed: a
    /// page that blocked the injection (strict CSP) must still be driveable, it
    /// just records without the annotation layer.
    async fn overlay(&self, js: &str) {
        let call = format!("(()=>{{ try {{ const o = window.__threadknotOverlay; if (o) {{ {js} }} }} catch (e) {{}} }})()");
        let _ = self.page().evaluate(call.as_str()).await;
    }

    fn cursor_pos(&self) -> (f64, f64) {
        *self.cursor.lock().unwrap()
    }

    /// Move the pointer to (x, y) along an eased path over `ms`.
    ///
    /// Two things travel together: the overlay animates the *visible* pointer
    /// locally at display rate, and we dispatch real CDP moves along the same
    /// curve every `GLIDE_SAMPLE_MS` so the page's own hover behaviour fires.
    /// Sampling the CDP side at display rate instead would mean an evaluate per
    /// frame for no visual gain.
    async fn glide(&self, x: f64, y: f64, ms: u64) {
        let (from_x, from_y) = self.cursor_pos();
        let ms = ms.max(1);
        self.overlay(&format!("o.glide({x:.1},{y:.1},{ms});")).await;

        let steps = (ms / GLIDE_SAMPLE_MS).max(1);
        for step in 1..=steps {
            let t = step as f64 / steps as f64;
            // Cubic ease-in-out: accelerate away, coast, settle — the shape a
            // hand actually makes. Linear motion is the tell of a robot.
            let e = if t < 0.5 {
                4.0 * t * t * t
            } else {
                1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
            };
            let px = from_x + (x - from_x) * e;
            let py = from_y + (y - from_y) * e;
            let _ = self
                .mouse(
                    DispatchMouseEventType::MouseMoved,
                    px,
                    py,
                    MouseButton::None,
                    0,
                )
                .await;
            tokio::time::sleep(std::time::Duration::from_millis(GLIDE_SAMPLE_MS)).await;
        }
        *self.cursor.lock().unwrap() = (x, y);
    }

    /// Snap the pointer without animating — used to seed a position before the
    /// first glide so motion never starts from a stale coordinate.
    async fn cursor_to(&self, x: f64, y: f64) {
        *self.cursor.lock().unwrap() = (x, y);
        self.overlay(&format!("o.at({x:.1},{y:.1});")).await;
    }

    async fn presentation(&self, on: bool) {
        self.overlay(if on { "o.show(true);" } else { "o.show(false);" })
            .await;
    }

    /// Re-assert the overlay after an action that may have replaced the
    /// document.
    ///
    /// A fresh document gets a fresh overlay, and it starts hidden. Captions
    /// happen to survive because every step sets its own, but the pointer would
    /// silently disappear for the rest of the recording. Navigation is not only
    /// the `navigate` step — a `type` step with `submit` also lands on a new
    /// page — so this runs after anything that settles.
    async fn reassert_presentation(&self, caption: Option<&str>) {
        let (x, y) = self.cursor_pos();
        self.presentation(true).await;
        self.cursor_to(x, y).await;
        if caption.is_some() {
            self.caption(caption).await;
        }
    }

    async fn caption(&self, text: Option<&str>) {
        match text {
            Some(t) => {
                let json = serde_json::to_string(t).unwrap_or_else(|_| "\"\"".into());
                self.overlay(&format!("o.caption({json});")).await;
            }
            None => self.overlay("o.caption(null);").await,
        }
    }

    async fn spotlight(&self, bounds: Option<&BrowserBounds>) {
        match bounds {
            Some(b) => {
                self.overlay(&format!(
                    "o.spotlight({{x:{:.1},y:{:.1},width:{:.1},height:{:.1}}});",
                    b.x, b.y, b.width, b.height
                ))
                .await
            }
            None => self.overlay("o.spotlight(null);").await,
        }
    }

    /// Hand the overlay the element its focus effects are framing, so they can
    /// clear themselves the frame it stops rendering. Without this a spotlight
    /// outlives its subject whenever an action unmounts it — clicking the submit
    /// button of a modal leaves the ring hanging over the closed dialog for the
    /// rest of the step's dwell.
    async fn watch_subject(&self, backend: BackendNodeId) {
        let _ = self
            .call_on_backend(
                backend,
                "function() { const o = window.__threadknotOverlay; if (o && o.watch) o.watch(this); }",
                Vec::new(),
            )
            .await;
    }

    /// Re-measure a target until two consecutive readings agree, so framing
    /// effects are drawn against a layout that has stopped moving. Falls back to
    /// the last reading if the page never settles inside the budget — a slightly
    /// stale rect still beats stalling the recording.
    async fn settled_target(&self, step: &Value, fallback: TargetPoint) -> TargetPoint {
        const TOLERANCE: f64 = 2.0;
        let mut previous: Option<TargetPoint> = None;
        for _ in 0..8 {
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            let Ok(current) = self.resolve_target(step).await else {
                continue;
            };
            if let Some(prev) = &previous {
                if (prev.x - current.x).abs() <= TOLERANCE
                    && (prev.y - current.y).abs() <= TOLERANCE
                {
                    return current;
                }
            }
            previous = Some(current);
        }
        previous.unwrap_or(fallback)
    }

    /// Blur everything except `bounds` — depth-of-field focus on the subject.
    async fn blur_except(&self, bounds: Option<&BrowserBounds>) {
        match bounds {
            Some(b) => {
                self.overlay(&format!(
                    "o.blur({{x:{:.1},y:{:.1},width:{:.1},height:{:.1}}});",
                    b.x, b.y, b.width, b.height
                ))
                .await
            }
            None => self.overlay("o.blur(null);").await,
        }
    }

    /// Ease the page's pinch-zoom toward `scale`, centred on `focus`.
    ///
    /// Pinch zoom is used rather than a CSS transform because it is applied by
    /// the compositor and therefore shows up in the screencast, while leaving
    /// the document's own layout (and any fixed page chrome) untouched. The
    /// overlay is told the scale so its cursor and caption keep a constant
    /// on-screen size instead of ballooning with the zoom.
    async fn zoom_to(&self, scale: f64, focus: Option<(f64, f64)>, ms: u64) {
        let scale = scale.clamp(1.0, 3.0);

        // Centre the subject BEFORE scaling, and wait for the scroll to land.
        //
        // Order matters: pinch zoom anchors at the visual viewport origin, so a
        // close-up has to be scrolled into frame. Doing that after the scale
        // (or without waiting) leaves the caller measuring the element at a
        // position the page is about to move it away from — which is exactly
        // how a blur cut-out ends up framing empty space below its subject.
        if let Some((fx, fy)) = focus {
            let js = format!(
                "window.scrollTo({{left: Math.max(0, {fx:.0} - innerWidth/(2*{scale:.3})), \
                 top: Math.max(0, {fy:.0} - innerHeight/(2*{scale:.3})), behavior:'auto'}});"
            );
            let _ = self.page().evaluate(js.as_str()).await;
            tokio::time::sleep(std::time::Duration::from_millis(140)).await;
        }

        let steps = (ms / 40).max(1);
        let start = 1.0_f64;
        for step in 1..=steps {
            let t = step as f64 / steps as f64;
            let e = 1.0 - (1.0 - t).powi(3);
            let s = start + (scale - start) * e;
            let _ = self
                .page()
                .execute(SetPageScaleFactorParams::new(s))
                .await;
            self.overlay(&format!("o.scale({s:.3});")).await;
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        }
    }

    async fn zoom_reset(&self) {
        let _ = self
            .page()
            .execute(SetPageScaleFactorParams::new(1.0))
            .await;
        self.overlay("o.scale(1);").await;
    }

    /// Begin recording the screencast to `path`.
    async fn record_start(&self, path: PathBuf) -> Result<PathBuf> {
        let mut slot = self.recording.lock().await;
        if slot.is_some() {
            return Err(anyhow!(
                "this browser is already recording; stop the current recording first"
            ));
        }
        let seed = self.last_frame.lock().unwrap().clone();
        let rec = crate::recorder::start(path.clone(), self.frames_tx.subscribe(), seed).await?;
        *slot = Some(rec);
        Ok(path)
    }

    async fn record_stop(&self) -> Result<Value> {
        let rec = self
            .recording
            .lock()
            .await
            .take()
            .ok_or_else(|| anyhow!("this browser is not recording"))?;
        // Let the tail of the last action reach the encoder before cutting.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        self.presentation(false).await;
        self.zoom_reset().await;
        let (path, duration_ms) = crate::recorder::finish(rec).await?;
        Ok(crate::recorder::describe(&path, duration_ms))
    }

    /// Give an action a short, bounded settling window, then synchronize the
    /// address bar. This covers ordinary link navigation and most SPA updates
    /// without the unbounded sleeps that make browser agents feel sluggish.
    async fn settle_after_action(&self) {
        for _ in 0..12 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let ready = self
                .page()
                .evaluate("document.readyState")
                .await
                .ok()
                .and_then(|result| result.into_value::<String>().ok())
                .unwrap_or_default();
            if ready != "loading" {
                break;
            }
        }
        self.refresh_url().await;
    }

    async fn backend_for_input(&self, input: &Value) -> Result<BackendNodeId> {
        if let Some(reference) = input.get("ref").and_then(Value::as_str) {
            let (found, fresh) = {
                let refs = self.refs.lock().unwrap();
                (refs.by_ref.get(reference).copied(), refs.fresh)
            };
            return found.ok_or_else(|| {
                if fresh {
                    anyhow!(
                        "element ref {reference:?} is not in the current snapshot; call browser_snapshot and use a ref from it"
                    )
                } else {
                    anyhow!(
                        "element ref {reference:?} is stale — the page navigated or reloaded since the last snapshot; call browser_snapshot and use a ref from it"
                    )
                }
            });
        }
        if let Some(selector) = input.get("selector").and_then(Value::as_str) {
            return self.backend_for_selector(selector).await;
        }
        Err(anyhow!("target needs ref or selector"))
    }

    /// Resolve a CSS selector against the main document and then, if it misses,
    /// against every child frame's document — otherwise a selector aimed at a
    /// payment field or an embedded widget reports "not found" while the human
    /// is looking straight at it.
    async fn backend_for_selector(&self, selector: &str) -> Result<BackendNodeId> {
        let page = self.page();
        let root = page
            .execute(GetDocumentParams::builder().depth(0).build())
            .await?
            .result
            .root
            .node_id;
        if let Ok(found) = page
            .execute(QuerySelectorParams::new(root, selector.to_string()))
            .await
        {
            if found.result.node_id.inner() != &0 {
                if let Ok(described) = page
                    .execute(
                        DescribeNodeParams::builder()
                            .node_id(found.result.node_id)
                            .build(),
                    )
                    .await
                {
                    return Ok(described.result.node.backend_node_id);
                }
            }
        }
        for frame in self.frame_docs().await.iter().skip(1) {
            let Some(owner) = frame.owner else { continue };
            let Ok(described) = page
                .execute(
                    DescribeNodeParams::builder()
                        .backend_node_id(owner)
                        .pierce(true)
                        .depth(1)
                        .build(),
                )
                .await
            else {
                continue;
            };
            let Some(document) = described.result.node.content_document else {
                continue;
            };
            let Ok(found) = page
                .execute(QuerySelectorParams::new(
                    document.node_id,
                    selector.to_string(),
                ))
                .await
            else {
                continue;
            };
            if found.result.node_id.inner() == &0 {
                continue;
            }
            if let Ok(node) = page
                .execute(
                    DescribeNodeParams::builder()
                        .node_id(found.result.node_id)
                        .build(),
                )
                .await
            {
                return Ok(node.result.node.backend_node_id);
            }
        }
        Err(anyhow!(
            "selector not found in this page or its frames: {selector}"
        ))
    }

    async fn point_of_backend(&self, backend: BackendNodeId) -> Result<TargetPoint> {
        // Scrolling through CDP is more reliable than calculating a page-space
        // box and hoping it already intersects the viewport.
        self.page()
            .execute(
                ScrollIntoViewIfNeededParams::builder()
                    .backend_node_id(backend)
                    .build(),
            )
            .await?;
        let model = self
            .page()
            .execute(
                GetBoxModelParams::builder()
                    .backend_node_id(backend)
                    .build(),
            )
            .await?
            .result
            .model;
        let quad = model.border.inner();
        if quad.len() < 8 {
            return Err(anyhow!("target has no clickable box"));
        }
        let xs = [quad[0], quad[2], quad[4], quad[6]];
        let ys = [quad[1], quad[3], quad[5], quad[7]];
        let min_x = xs.into_iter().fold(f64::INFINITY, f64::min);
        let max_x = xs.into_iter().fold(f64::NEG_INFINITY, f64::max);
        let min_y = ys.into_iter().fold(f64::INFINITY, f64::min);
        let max_y = ys.into_iter().fold(f64::NEG_INFINITY, f64::max);
        if !min_x.is_finite() || !min_y.is_finite() || max_x <= min_x || max_y <= min_y {
            return Err(anyhow!("target is not visible or has a zero-sized box"));
        }
        Ok(TargetPoint {
            x: (min_x + max_x) / 2.0,
            y: (min_y + max_y) / 2.0,
            bounds: Some(BrowserBounds {
                x: min_x,
                y: min_y,
                width: max_x - min_x,
                height: max_y - min_y,
            }),
            backend: Some(backend),
        })
    }

    /// Refuse to click something another element is covering. A cookie banner
    /// or modal backdrop over the target otherwise swallows the click while the
    /// action reports success — the same silent-success failure as a key event
    /// with no default action. Playwright calls this actionability; the check is
    /// done in the element's own document so it works inside frames too.
    async fn assert_click_reaches(&self, backend: BackendNodeId) -> Result<()> {
        let verdict = self
            .call_on_backend(
                backend,
                r##"function() {
                    const rect = this.getBoundingClientRect();
                    const x = rect.left + rect.width / 2;
                    const y = rect.top + rect.height / 2;
                    const hit = this.ownerDocument.elementFromPoint(x, y);
                    if (!hit) return { ok: false, what: "nothing (the point is outside the viewport)" };
                    if (hit === this || this.contains(hit) || hit.contains(this)) return { ok: true };
                    const id = hit.id ? "#" + hit.id : "";
                    const cls = typeof hit.className === "string" && hit.className.trim()
                        ? "." + hit.className.trim().split(/\s+/).join(".")
                        : "";
                    const text = (hit.textContent || "").trim().slice(0, 60);
                    return { ok: false, what: `<${hit.tagName.toLowerCase()}${id}${cls}>` + (text ? ` ("${text}")` : "") };
                }"##,
                Vec::new(),
            )
            .await;
        // A failure to evaluate is not evidence of interception; let the click
        // proceed rather than blocking on a diagnostic.
        let Ok(verdict) = verdict else { return Ok(()) };
        if verdict.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(());
        }
        let what = verdict
            .get("what")
            .and_then(Value::as_str)
            .unwrap_or("another element");
        Err(anyhow!(
            "the click would land on {what}, not the target — dismiss the overlay (cookie banner, modal, sticky header) first, or click by x/y coordinates to override deliberately"
        ))
    }

    async fn resolve_target(&self, input: &Value) -> Result<TargetPoint> {
        if let (Some(x), Some(y)) = (
            input.get("x").and_then(Value::as_f64),
            input.get("y").and_then(Value::as_f64),
        ) {
            return Ok(TargetPoint { x, y, bounds: None, backend: None });
        }
        let backend = self.backend_for_input(input).await?;
        self.point_of_backend(backend).await
    }

    async fn call_on_backend(
        &self,
        backend: BackendNodeId,
        function: &str,
        args: Vec<Value>,
    ) -> Result<Value> {
        let object = self
            .page()
            .execute(
                ResolveNodeParams::builder()
                    .backend_node_id(backend)
                    .build(),
            )
            .await?
            .result
            .object;
        let object_id = object
            .object_id
            .ok_or_else(|| anyhow!("target is no longer attached to the page"))?;
        let mut builder = CallFunctionOnParams::builder()
            .function_declaration(function)
            .object_id(object_id)
            .return_by_value(true)
            .user_gesture(true);
        for value in args {
            builder = builder.argument(CallArgument::builder().value(value).build());
        }
        let call = builder.build().map_err(|error| anyhow!(error))?;
        // Page::evaluate_function injects the page executionContextId when it
        // is absent, but CDP forbids combining that with objectId. Execute the
        // raw command because this function is explicitly bound to the
        // resolved element object.
        let response = self.page().execute(call).await?.result;
        if let Some(details) = response.exception_details {
            let text = details
                .exception
                .as_ref()
                .map(console_arg_text)
                .filter(|text| !text.is_empty())
                .unwrap_or(details.text);
            return Err(anyhow!("page function failed: {text}"));
        }
        Ok(response.result.value.unwrap_or(Value::Null))
    }

    async fn focus_backend(&self, backend: BackendNodeId) -> Result<()> {
        self.call_on_backend(
            backend,
            "function() { this.focus({preventScroll:false}); return document.activeElement === this; }",
            Vec::new(),
        )
        .await?;
        Ok(())
    }

    async fn fill_backend(&self, backend: BackendNodeId, value: &str) -> Result<Value> {
        self.call_on_backend(
            backend,
            r#"function(value) {
                this.focus({preventScroll:false});
                if (this instanceof HTMLSelectElement) {
                    const wanted = String(value);
                    const option = [...this.options].find(o => o.value === wanted || o.text === wanted);
                    if (!option) throw new Error(`option not found: ${wanted}`);
                    this.value = option.value;
                } else if (this.isContentEditable) {
                    this.textContent = String(value);
                } else if ("value" in this) {
                    const proto = this instanceof HTMLTextAreaElement
                        ? HTMLTextAreaElement.prototype
                        : HTMLInputElement.prototype;
                    const setter = Object.getOwnPropertyDescriptor(proto, "value")?.set;
                    if (setter) setter.call(this, String(value));
                    else this.value = String(value);
                } else {
                    throw new Error("target is not editable");
                }
                this.dispatchEvent(new InputEvent("input", {bubbles:true, inputType:"insertText", data:String(value)}));
                this.dispatchEvent(new Event("change", {bubbles:true}));
                return { value: "value" in this ? this.value : this.textContent };
            }"#,
            vec![json!(value)],
        )
        .await
    }

    async fn set_checked_backend(&self, backend: BackendNodeId, checked: bool) -> Result<Value> {
        self.call_on_backend(
            backend,
            r#"function(checked) {
                this.focus({preventScroll:false});
                if (!("checked" in this)) throw new Error("target is not checkable");
                if (Boolean(this.checked) !== Boolean(checked)) this.click();
                return { checked: Boolean(this.checked) };
            }"#,
            vec![json!(checked)],
        )
        .await
    }

    async fn upload_backend(&self, backend: BackendNodeId, paths: Vec<String>) -> Result<()> {
        self.page()
            .execute(
                SetFileInputFilesParams::builder()
                    .backend_node_id(backend)
                    .files(paths)
                    .build()
                    .map_err(|error| anyhow!(error))?,
            )
            .await?;
        Ok(())
    }

    /// Track downloads at the browser level so a file a flow produced has a
    /// real path the agent can hand to `publish_artifact` — instead of a click
    /// that appears to do nothing.
    async fn wire_downloads(self: &Arc<Self>) {
        let Ok(mut begin) = self._browser.event_listener::<EventDownloadWillBegin>().await else {
            return;
        };
        let weak = Arc::downgrade(self);
        let begin_task = tokio::spawn(async move {
            while let Some(event) = begin.next().await {
                let Some(session) = weak.upgrade() else { break };
                push_ring(
                    &session.downloads,
                    json!({
                        "guid": event.guid,
                        "url": event.url,
                        "suggestedFilename": event.suggested_filename,
                        "state": "started",
                        "ts": chrono::Utc::now().to_rfc3339(),
                    }),
                    DOWNLOAD_HISTORY_LIMIT,
                );
            }
        });
        self.tasks.lock().unwrap().push(begin_task);

        let Ok(mut progress) = self
            ._browser
            .event_listener::<EventDownloadProgress>()
            .await
        else {
            return;
        };
        let weak = Arc::downgrade(self);
        let progress_task = tokio::spawn(async move {
            while let Some(event) = progress.next().await {
                let Some(session) = weak.upgrade() else { break };
                let state = event.state.as_ref().to_string();
                // Chrome saves the file under its GUID (allowAndName); rename it
                // to the suggested filename so the artifact is recognizable.
                let mut final_path = None;
                if state == "completed" {
                    let mut downloads = session.downloads.lock().unwrap();
                    if let Some(entry) = downloads
                        .iter_mut()
                        .rev()
                        .find(|entry| entry.get("guid").and_then(Value::as_str) == Some(&event.guid))
                    {
                        let named = session.work_dir.join(&event.guid);
                        let suggested = entry
                            .get("suggestedFilename")
                            .and_then(Value::as_str)
                            .filter(|name| !name.trim().is_empty() && !name.contains('/'))
                            .unwrap_or("download")
                            .to_string();
                        let target = session.work_dir.join(&suggested);
                        let target = if std::fs::rename(&named, &target).is_ok() {
                            target
                        } else {
                            named
                        };
                        entry["path"] = json!(target.to_string_lossy());
                        entry["sizeBytes"] =
                            json!(std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0));
                        final_path = Some(target);
                    }
                }
                let mut downloads = session.downloads.lock().unwrap();
                if let Some(entry) = downloads
                    .iter_mut()
                    .rev()
                    .find(|entry| entry.get("guid").and_then(Value::as_str) == Some(&event.guid))
                {
                    entry["state"] = json!(state);
                }
                drop(downloads);
                if let Some(path) = final_path {
                    tracing::debug!("browser download saved to {}", path.display());
                }
            }
        });
        self.tasks.lock().unwrap().push(progress_task);
    }

    /// Hold a signed-in browser inside its allowed sites, in the browser itself.
    ///
    /// A tool-layer check alone would be theatre: a link, a redirect, a
    /// `window.open`, or injected page script can navigate without ever calling
    /// a tool. Interception applies to **documents** (top-level and frames), not
    /// subresources — a page's CDN, fonts and images are not the risk, and
    /// blocking them would break the very sites the user signed into. What it
    /// stops is the credentialed browser ever *being on* a page outside scope,
    /// which is the position prompt injection needs to be useful.
    async fn wire_origin_guard(self: &Arc<Self>, page: &Page) -> Result<()> {
        let Some(allowed) = self.allowed_origins().map(|origins| origins.to_vec()) else {
            return Ok(());
        };
        // CDP reports both top-level and iframe document loads as `Document`.
        let document_patterns = vec![RequestPattern::builder()
            .url_pattern("*")
            .resource_type(ResourceType::Document)
            .request_stage(RequestStage::Request)
            .build()];
        page.execute(
            EnableFetchParams::builder()
                .patterns(document_patterns)
                .build(),
        )
        .await?;

        let mut paused = page.event_listener::<EventRequestPaused>().await?;
        let guard_page = page.clone();
        let weak = Arc::downgrade(self);
        let task = tokio::spawn(async move {
            while let Some(event) = paused.next().await {
                let Some(session) = weak.upgrade() else { break };
                let request_id = event.request_id.clone();
                if crate::browser_profiles::origin_allowed(&allowed, &event.request.url) {
                    let _ = guard_page
                        .execute(ContinueRequestParams::new(request_id))
                        .await;
                    continue;
                }
                // Visible to both sides: the agent sees why the page is blank,
                // the human sees it in the pane's console.
                push_ring(
                    &session.console,
                    json!({
                        "level": "error",
                        "source": "threadknot",
                        "text": format!(
                            "blocked {} — this browser is signed in and limited to {}",
                            event.request.url,
                            allowed.join(", ")
                        ),
                        "ts": chrono::Utc::now().to_rfc3339(),
                    }),
                    CONSOLE_HISTORY_LIMIT,
                );
                let _ = guard_page
                    .execute(
                        FailRequestParams::builder()
                            .request_id(request_id)
                            .error_reason(ErrorReason::BlockedByClient)
                            .build()
                            .expect("fail request params"),
                    )
                    .await;
            }
        });
        self.tasks.lock().unwrap().push(task);
        Ok(())
    }

    /// Enable CDP domains and attach compact event streams to a page once.
    /// Streams for background tabs remain attached but only the active tab can
    /// paint or mutate the shared user-facing state.
    async fn wire_page(self: &Arc<Self>, page: Page) -> Result<()> {
        let target_id = page.target_id().as_ref().to_string();
        {
            let mut attached = self.attached_pages.lock().unwrap();
            if !attached.insert(target_id.clone()) {
                return Ok(());
            }
        }

        page.execute(EnablePageParams::default()).await?;
        page.execute(EnableRuntimeParams::default()).await?;
        page.execute(EnableNetworkParams::default()).await?;
        page.execute(EnableAccessibilityParams::default()).await?;
        // The Log domain is where "Failed to load resource: 404", CORS refusals
        // and CSP violations are reported. Runtime alone (console.* plus
        // uncaught exceptions) misses every one of them.
        page.execute(EnableLogParams::default()).await?;

        // Register the presentation overlay for every future document on this
        // target. Registration is idempotent per target and costs nothing until
        // a recording asks the overlay to show itself.
        let _ = page
            .execute(AddScriptToEvaluateOnNewDocumentParams::new(
                OVERLAY_SOURCE.to_string(),
            ))
            .await;
        // The registration above only applies to documents loaded from here on,
        // so seed the one already open.
        let _ = page.evaluate(OVERLAY_SOURCE).await;

        self.wire_origin_guard(&page).await?;

        // Decode/ack every active screencast frame. Background tabs normally
        // have their screencast stopped; the target check also rejects a late
        // frame racing with a tab switch.
        let mut frames = page.event_listener::<EventScreencastFrame>().await?;
        let cast_page = page.clone();
        let weak = Arc::downgrade(self);
        let frame_target = target_id.clone();
        let cast_task = tokio::spawn(async move {
            while let Some(frame) = frames.next().await {
                let session_id = frame.session_id;
                let b64: String = frame.data.clone().into();
                if let Some(session) = weak.upgrade() {
                    if session.active_target_id() == frame_target {
                        if let Ok(bytes) =
                            base64::engine::general_purpose::STANDARD.decode(b64.as_bytes())
                        {
                            *session.last_frame.lock().unwrap() = Some(bytes.clone());
                            // Only pay for the extra clone while a recording is
                            // actually attached to the frame stream.
                            if session.frames_tx.receiver_count() > 0 {
                                let _ = session.frames_tx.send(bytes.clone());
                            }
                            let _ = session.tx.send(BrowserMsg::Frame(bytes));
                        }
                    }
                } else {
                    break;
                }
                let _ = cast_page
                    .execute(ScreencastFrameAckParams::new(session_id))
                    .await;
            }
        });
        self.tasks.lock().unwrap().push(cast_task);

        let mut navigations = page.event_listener::<EventFrameNavigated>().await?;
        let weak = Arc::downgrade(self);
        let navigation_target = target_id.clone();
        let navigation_task = tokio::spawn(async move {
            while let Some(event) = navigations.next().await {
                if event.frame.parent_id.is_some() {
                    continue;
                }
                let Some(session) = weak.upgrade() else {
                    break;
                };
                if session.active_target_id() == navigation_target {
                    // A committed main-frame navigation is a new document: refs
                    // point at dead nodes and the old page's console entries
                    // would masquerade as this page's.
                    session.reset_for_document(event.frame.loader_id.as_ref());
                    let fragment = event.frame.url_fragment.clone().unwrap_or_default();
                    session.set_url(format!("{}{}", event.frame.url, fragment));
                    session.emit_tabs().await;
                }
            }
        });
        self.tasks.lock().unwrap().push(navigation_task);

        let mut same_document = page
            .event_listener::<EventNavigatedWithinDocument>()
            .await?;
        let weak = Arc::downgrade(self);
        let same_target = target_id.clone();
        let same_page = page.clone();
        let same_document_task = tokio::spawn(async move {
            while same_document.next().await.is_some() {
                let Some(session) = weak.upgrade() else {
                    break;
                };
                if session.active_target_id() == same_target {
                    if let Ok(Some(url)) = same_page.url().await {
                        session.set_url(url);
                        session.emit_tabs().await;
                    }
                }
            }
        });
        self.tasks.lock().unwrap().push(same_document_task);

        let mut console_events = page.event_listener::<EventConsoleApiCalled>().await?;
        let weak = Arc::downgrade(self);
        let console_target = target_id.clone();
        let console_task = tokio::spawn(async move {
            while let Some(event) = console_events.next().await {
                let Some(session) = weak.upgrade() else {
                    break;
                };
                if session.active_target_id() != console_target {
                    continue;
                }
                let text = event
                    .args
                    .iter()
                    .map(console_arg_text)
                    .collect::<Vec<_>>()
                    .join(" ");
                push_ring(
                    &session.console,
                    json!({
                        "level": event.r#type.as_ref(),
                        "text": truncate_text(&text, 2000),
                        "tabId": console_target,
                        "ts": chrono::Utc::now().to_rfc3339(),
                    }),
                    CONSOLE_HISTORY_LIMIT,
                );
            }
        });
        self.tasks.lock().unwrap().push(console_task);

        // Browser-level log entries: failed resource loads, CORS refusals, CSP
        // violations, deprecations. A developer expects these in "the console",
        // and none of them come through the Runtime domain.
        let mut log_entries = page.event_listener::<EventEntryAdded>().await?;
        let weak = Arc::downgrade(self);
        let log_target = target_id.clone();
        let log_task = tokio::spawn(async move {
            while let Some(event) = log_entries.next().await {
                let Some(session) = weak.upgrade() else {
                    break;
                };
                if session.active_target_id() != log_target {
                    continue;
                }
                let entry = &event.entry;
                push_ring(
                    &session.console,
                    json!({
                        "level": entry.level.as_ref(),
                        "source": entry.source.as_ref(),
                        "text": truncate_text(&entry.text, 2000),
                        "url": entry.url,
                        "line": entry.line_number,
                        "tabId": log_target,
                        "ts": chrono::Utc::now().to_rfc3339(),
                    }),
                    CONSOLE_HISTORY_LIMIT,
                );
            }
        });
        self.tasks.lock().unwrap().push(log_task);

        let mut exceptions = page.event_listener::<EventExceptionThrown>().await?;
        let weak = Arc::downgrade(self);
        let exception_target = target_id.clone();
        let exception_task = tokio::spawn(async move {
            while let Some(event) = exceptions.next().await {
                let Some(session) = weak.upgrade() else {
                    break;
                };
                if session.active_target_id() != exception_target {
                    continue;
                }
                let details = &event.exception_details;
                let text = details
                    .exception
                    .as_ref()
                    .map(console_arg_text)
                    .filter(|text| !text.is_empty())
                    .unwrap_or_else(|| details.text.clone());
                push_ring(
                    &session.console,
                    json!({
                        "level": "error",
                        "text": truncate_text(&text, 2000),
                        "url": details.url,
                        "line": details.line_number + 1,
                        "column": details.column_number + 1,
                        "tabId": exception_target,
                        "ts": chrono::Utc::now().to_rfc3339(),
                    }),
                    CONSOLE_HISTORY_LIMIT,
                );
            }
        });
        self.tasks.lock().unwrap().push(exception_task);

        let mut requests = page.event_listener::<EventRequestWillBeSent>().await?;
        let weak = Arc::downgrade(self);
        let request_target = target_id.clone();
        let request_task = tokio::spawn(async move {
            while let Some(event) = requests.next().await {
                let Some(session) = weak.upgrade() else {
                    break;
                };
                if session.active_target_id() != request_target {
                    continue;
                }
                push_ring(
                    &session.network,
                    json!({
                        "id": event.request_id.as_ref(),
                        "loaderId": event.loader_id.as_ref(),
                        "method": event.request.method,
                        "url": event.request.url,
                        "type": event.r#type.as_ref().map(AsRef::as_ref),
                        "status": Value::Null,
                        "tabId": request_target,
                        "ts": chrono::Utc::now().to_rfc3339(),
                    }),
                    NETWORK_HISTORY_LIMIT,
                );
            }
        });
        self.tasks.lock().unwrap().push(request_task);

        let mut responses = page.event_listener::<EventResponseReceived>().await?;
        let weak = Arc::downgrade(self);
        let response_target = target_id.clone();
        let response_task = tokio::spawn(async move {
            while let Some(event) = responses.next().await {
                let Some(session) = weak.upgrade() else {
                    break;
                };
                if session.active_target_id() != response_target {
                    continue;
                }
                let id = event.request_id.as_ref();
                let mut entries = session.network.lock().unwrap();
                if let Some(entry) = entries
                    .iter_mut()
                    .rev()
                    .find(|entry| entry.get("id").and_then(Value::as_str) == Some(id))
                {
                    entry["status"] = json!(event.response.status);
                    entry["mimeType"] = json!(event.response.mime_type);
                    entry["fromCache"] = json!(event.response.from_disk_cache);
                    // Headers answer "why did this fail" (CORS, cache, auth,
                    // content type) but would swamp a list of 80 requests, so
                    // they ride along hidden and surface via browser_network_body.
                    entry["_responseHeaders"] = json!(event.response.headers);
                    entry["_remoteAddress"] = json!(event.response.remote_ip_address);
                }
            }
        });
        self.tasks.lock().unwrap().push(response_task);

        let mut failures = page.event_listener::<EventLoadingFailed>().await?;
        let weak = Arc::downgrade(self);
        let failure_target = target_id.clone();
        let failure_task = tokio::spawn(async move {
            while let Some(event) = failures.next().await {
                let Some(session) = weak.upgrade() else {
                    break;
                };
                if session.active_target_id() != failure_target {
                    continue;
                }
                let id = event.request_id.as_ref();
                let mut entries = session.network.lock().unwrap();
                if let Some(entry) = entries
                    .iter_mut()
                    .rev()
                    .find(|entry| entry.get("id").and_then(Value::as_str) == Some(id))
                {
                    entry["failed"] = json!(true);
                    entry["error"] = json!(event.error_text);
                    entry["canceled"] = json!(event.canceled.unwrap_or(false));
                }
            }
        });
        self.tasks.lock().unwrap().push(failure_task);

        let mut dialogs = page
            .event_listener::<EventJavascriptDialogOpening>()
            .await?;
        let weak = Arc::downgrade(self);
        let dialog_target = target_id;
        let dialog_task = tokio::spawn(async move {
            while let Some(event) = dialogs.next().await {
                let Some(session) = weak.upgrade() else {
                    break;
                };
                if session.active_target_id() == dialog_target {
                    session.set_dialog(Some(BrowserDialog {
                        kind: event.r#type.as_ref().to_string(),
                        message: event.message.clone(),
                        default_prompt: event.default_prompt.clone(),
                        url: event.url.clone(),
                    }));
                }
            }
        });
        self.tasks.lock().unwrap().push(dialog_task);

        Ok(())
    }

    /// Collect the accessibility tree of every reachable frame, then render one
    /// merged, properly-nested outline.
    ///
    /// Two things this must get right, because agents act on the result:
    ///   * **Structure is meaning.** A "Delete" button is only actionable if the
    ///     agent can see which row owns it, so the tree is walked through
    ///     `childIds` (a flat pass over the node array mis-nests everything the
    ///     moment a node is skipped or arrives out of order).
    ///   * **Frames are part of the page.** Payment fields, embedded auth and
    ///     third-party widgets live in iframes; a snapshot that stops at the
    ///     main frame silently hides them.
    async fn semantic_snapshot(&self) -> Result<Value> {
        let frames = self.frame_docs().await;
        let mut trees: Vec<FrameDoc> = Vec::new();
        for frame in frames {
            trees.push(frame);
        }
        // owner element -> the frame it hosts, so the walk can descend into it.
        let mut frame_by_owner: HashMap<BackendNodeId, usize> = HashMap::new();
        for (index, frame) in trees.iter().enumerate() {
            if let Some(owner) = frame.owner {
                frame_by_owner.insert(owner, index);
            }
        }

        let (lines, element_count, truncated) = {
            let mut refs = self.refs.lock().unwrap();
            let mut render = SnapshotRender {
                lines: Vec::new(),
                by_ref: HashMap::new(),
                by_backend: HashMap::new(),
                truncated: false,
                isolated: self.profile.is_some(),
            };
            if let Some(root) = trees.first().and_then(|frame| frame.root()) {
                render.walk(root, &trees, 0, 0, "", &frame_by_owner, &mut refs);
            }
            if render.truncated {
                render.lines.push(
                    "… snapshot truncated — scroll, narrow the page state, or use selectors"
                        .to_string(),
                );
            }
            let element_count = render.by_ref.len();
            let truncated = render.truncated;
            refs.by_ref = render.by_ref;
            refs.by_backend = render.by_backend;
            refs.fresh = true;
            (render.lines, element_count, truncated)
        };

        let title = self.title().await;
        let scroll = self.scroll_state().await;

        Ok(json!({
            "url": self.current_url(),
            "title": title,
            "snapshot": lines.join("\n"),
            "elementCount": element_count,
            "frameCount": trees.len(),
            "truncated": truncated,
            "scroll": scroll,
            "note": "Use ref values from this latest snapshot for reliable interactions. Take a fresh snapshot after navigation or major page changes.",
        }))
    }

    /// Where the page is scrolled, so the agent knows whether more content
    /// exists below without screenshotting to find out.
    async fn scroll_state(&self) -> Value {
        self.evaluate(
            "({ x: Math.round(scrollX), y: Math.round(scrollY), \
              pageHeight: Math.round(document.documentElement.scrollHeight), \
              viewportHeight: Math.round(innerHeight), \
              atBottom: Math.ceil(scrollY + innerHeight) >= document.documentElement.scrollHeight - 2 })",
        )
        .await
        .unwrap_or(Value::Null)
    }

    /// Flatten the frame tree and fetch one accessibility tree per frame.
    /// A frame whose tree cannot be read (a process we are not attached to)
    /// still appears, marked, rather than vanishing from the page.
    async fn frame_docs(&self) -> Vec<FrameDoc> {
        let page = self.page();
        let mut docs = Vec::new();
        let Ok(tree) = page.execute(GetFrameTreeParams::default()).await else {
            return docs;
        };
        let mut pending = vec![(tree.result.frame_tree.clone(), true)];
        while let Some((node, is_main)) = pending.pop() {
            let frame_id = node.frame.id.clone();
            let owner = if is_main {
                None
            } else {
                page.execute(GetFrameOwnerParams::new(frame_id.clone()))
                    .await
                    .ok()
                    .map(|result| result.result.backend_node_id)
            };
            let mut builder = GetFullAxTreeParams::builder();
            if !is_main {
                builder = builder.frame_id(frame_id.clone());
            }
            let nodes = page
                .execute(builder.build())
                .await
                .map(|result| result.result.nodes.clone())
                .unwrap_or_default();
            docs.push(FrameDoc {
                url: node.frame.url.clone(),
                owner,
                nodes,
            });
            if let Some(children) = node.child_frames.clone() {
                for child in children {
                    pending.push((child, false));
                }
            }
        }
        docs
    }

    fn console_entries(&self) -> Vec<Value> {
        self.console.lock().unwrap().iter().cloned().collect()
    }

    fn network_entries(&self) -> Vec<Value> {
        self.network.lock().unwrap().iter().cloned().collect()
    }

    fn download_entries(&self) -> Vec<Value> {
        self.downloads.lock().unwrap().iter().cloned().collect()
    }

    /// Fetch a response body for a request the page already made. This is the
    /// difference between "the save failed with a 422" and knowing *which
    /// field* the API rejected.
    async fn response_body(&self, request_id: &str) -> Result<Value> {
        let response = self
            .page()
            .execute(GetResponseBodyParams::new(
                chromiumoxide::cdp::browser_protocol::network::RequestId::from(
                    request_id.to_string(),
                ),
            ))
            .await
            .with_context(|| {
                format!("no retained body for request {request_id}; bodies are only available for a short window after the response")
            })?;
        let body = if response.result.base64_encoded {
            base64::engine::general_purpose::STANDARD
                .decode(response.result.body.as_bytes())
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .unwrap_or_else(|| "<binary body>".to_string())
        } else {
            response.result.body.clone()
        };
        // Pair the body with the headers of the same request: a 403 explains
        // itself through its headers as often as through its body.
        let recorded = self
            .network
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|entry| entry.get("id").and_then(Value::as_str) == Some(request_id))
            .cloned();
        let detail = recorded.as_ref();
        Ok(json!({
            "id": request_id,
            "url": detail.and_then(|entry| entry.get("url")).cloned(),
            "status": detail.and_then(|entry| entry.get("status")).cloned(),
            "responseHeaders": detail.and_then(|entry| entry.get("_responseHeaders")).cloned(),
            "remoteAddress": detail.and_then(|entry| entry.get("_remoteAddress")).cloned(),
            "base64Encoded": response.result.base64_encoded,
            "body": truncate_text(&body, 20_000),
        }))
    }
}

/// Live browser sessions. Constructed once in `build_server_state`.
#[derive(Default)]
pub struct BrowserRegistry {
    sessions: Mutex<HashMap<String, Arc<BrowserSession>>>,
    maintenance_started: std::sync::atomic::AtomicBool,
    /// Resolves a session key (a thread id) to the signed-in profile that
    /// thread is attached to. Installed by the server; absent in tests, where
    /// every session is disposable.
    profile_resolver: Mutex<Option<ProfileResolver>>,
}

/// Returns the profile a thread should use, or an error explaining why the
/// attached profile cannot be opened.
pub type ProfileResolver =
    Arc<dyn Fn(&str) -> Result<Option<crate::browser_profiles::ProfileSpec>> + Send + Sync>;

impl BrowserRegistry {
    /// Sweep profiles abandoned by a previous run. Cheap, synchronous, and safe
    /// to call before any session exists — running browsers are skipped.
    pub fn new() -> Self {
        sweep_orphan_profiles();
        Self::default()
    }

    pub fn set_profile_resolver(&self, resolver: ProfileResolver) {
        *self.profile_resolver.lock().unwrap() = Some(resolver);
    }

    fn resolve_profile(&self, key: &str) -> Result<Option<crate::browser_profiles::ProfileSpec>> {
        let resolver = self.profile_resolver.lock().unwrap().clone();
        match resolver {
            Some(resolver) => resolver(key),
            None => Ok(None),
        }
    }

    fn existing(&self, key: &str) -> Option<Arc<BrowserSession>> {
        self.sessions.lock().unwrap().get(key).cloned()
    }

    /// A signed-in profile is a single Chrome user-data directory, and Chrome
    /// will not open one twice. Refuse the second claim with the thread that
    /// holds it rather than letting the launch hang on a SingletonLock.
    fn assert_profile_free(&self, key: &str, profile_id: &str) -> Result<()> {
        let sessions = self.sessions.lock().unwrap();
        for (other_key, session) in sessions.iter() {
            if other_key == key {
                continue;
            }
            if session.profile_id().as_deref() == Some(profile_id) {
                bail!(
                    "that signed-in browser profile is already open in another chat (session {other_key}); close it there first"
                );
            }
        }
        Ok(())
    }

    /// Close sessions nobody is using. Each one holds a Chrome (hundreds of MB
    /// of RSS) and a profile directory; without this they accumulate for the
    /// lifetime of the app, one per thread that ever touched the browser.
    fn start_maintenance(self: &Arc<Self>) {
        if self
            .maintenance_started
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                let Some(registry) = weak.upgrade() else { break };
                let idle: Vec<String> = {
                    let sessions = registry.sessions.lock().unwrap();
                    sessions
                        .iter()
                        .filter(|(_, session)| {
                            session.viewer_count() == 0
                                && session.idle_for() > SESSION_IDLE_TIMEOUT
                        })
                        .map(|(key, _)| key.clone())
                        .collect()
                };
                for key in idle {
                    tracing::info!("closing idle browser session for {key}");
                    registry.remove(&key);
                }
            }
        });
    }

    /// Get the session for `key`, launching a headless Chrome if none exists.
    /// Async (awaits the launch); never holds the lock across an await.
    pub async fn get_or_spawn(self: &Arc<Self>, key: &str) -> Result<Arc<BrowserSession>> {
        self.start_maintenance();
        let profile = self.resolve_profile(key)?;
        if let Some(s) = self.existing(key) {
            // Attaching or detaching a profile changes which browser this
            // thread should be driving, so the old one has to go.
            let same_profile = s.profile_id() == profile.as_ref().map(|spec| spec.id.clone());
            if same_profile && s.is_healthy().await {
                return Ok(s);
            }
            // Scoped so the (non-Send) guard cannot straddle the await below.
            {
                let mut sessions = self.sessions.lock().unwrap();
                if sessions
                    .get(key)
                    .is_some_and(|current| Arc::ptr_eq(current, &s))
                {
                    sessions.remove(key);
                }
            }
            s.shutdown().await;
        }
        if let Some(spec) = &profile {
            self.assert_profile_free(key, &spec.id)?;
        }
        // The session key is a caller-supplied string and every distinct one is a
        // Chrome with its own profile directory, so without this an authenticated
        // client could ask for a thousand of them (SEC-014). Checked at the single
        // choke point rather than in `ws_handler`, so the MCP door is covered too.
        {
            let live = self.sessions.lock().unwrap().len();
            anyhow::ensure!(
                live < crate::limits::MAX_LIVE_BROWSER_SESSIONS,
                "this machine already has {live} browser sessions open, which is the limit of {} \
                 — each one is a Chrome, so close one before opening another",
                crate::limits::MAX_LIVE_BROWSER_SESSIONS
            );
        }
        let session = spawn_session(profile).await?;
        // Double-check: another attach may have spawned concurrently — keep one.
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(s) = sessions.get(key) {
            return Ok(Arc::clone(s));
        }
        sessions.insert(key.to_string(), Arc::clone(&session));
        Ok(session)
    }

    /// Drop a session (closes Chrome). Used on explicit teardown.
    ///
    /// A signed-in profile must be closed *gracefully* or Chrome never flushes
    /// its cookie jar and the session the user just established is lost, so the
    /// close runs on a background task rather than as a kill in `Drop`.
    pub fn remove(&self, key: &str) {
        let session = self.sessions.lock().unwrap().remove(key);
        if let Some(session) = session {
            tokio::spawn(async move { session.shutdown().await });
        }
    }

    /// Close every session gracefully, and wait for the closes to finish.
    /// Called on app exit: the process dying on its own never drops the
    /// `Browser` handles, which orphans every Chrome — and an orphaned
    /// signed-in Chrome keeps its profile locked and its cookie jar unflushed.
    pub async fn shutdown_all(&self) {
        let sessions: Vec<Arc<BrowserSession>> = {
            let mut map = self.sessions.lock().unwrap();
            map.drain().map(|(_, session)| session).collect()
        };
        for session in sessions {
            session.shutdown().await;
        }
    }

    /// Close every session running on a profile — used when its scope changes
    /// or it is deleted, so a live browser can't outlive the rules it was
    /// opened under.
    pub fn close_sessions_using(&self, profile_id: &str) {
        let keys: Vec<String> = {
            let sessions = self.sessions.lock().unwrap();
            sessions
                .iter()
                .filter(|(_, session)| session.profile_id().as_deref() == Some(profile_id))
                .map(|(key, _)| key.clone())
                .collect()
        };
        for key in keys {
            self.remove(&key);
        }
    }

    /// Perform a structured action against `key`'s session (MCP entry point).
    /// Returns a JSON result; `snapshot` includes a base64 PNG.
    pub async fn invoke(self: &Arc<Self>, key: &str, op: &str, input: &Value) -> Result<Value> {
        // Observation-only tools must not conjure a browser into existence:
        // asking "is anything open?" should be answerable without launching
        // Chrome and its ~300 MB of processes.
        let session = if matches!(op, "status" | "console" | "network" | "tabs" | "downloads") {
            match self.existing(key) {
                Some(session) => session,
                None => {
                    return Ok(json!({
                        "browser": "not started",
                        "note": "No browser session for this thread yet. browser_navigate (or any action) starts one.",
                    }))
                }
            }
        } else {
            self.get_or_spawn(key).await?
        };
        session.touch();
        let _action_guard = session.action_lock.lock().await;
        let action_id = Uuid::new_v4().to_string();
        let (label, detail) = activity_description(op, input);
        session.activity(&action_id, "started", op, &label, detail, None);
        let s = |k: &str| {
            input
                .get(k)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let n = |k: &str| input.get(k).and_then(|v| v.as_f64());
        let result: Result<Value> = async {
            match op {
            "navigate" => {
                session.assert_origin_allowed(&s("url"))?;
                session.navigate(&s("url")).await?;
                Ok(json!({ "url": session.current_url() }))
            }
            "back" => {
                session.history(-1).await?;
                Ok(json!({ "ok": true }))
            }
            "forward" => {
                session.history(1).await?;
                Ok(json!({ "ok": true }))
            }
            "reload" => {
                session.reload().await?;
                session.settle_after_action().await;
                Ok(json!({ "ok": true }))
            }
            "click" => {
                let target = session.resolve_target(input).await?;
                // Explicit coordinates mean the caller chose the point; only a
                // ref/selector target carries an expectation about what gets hit.
                if input.get("x").is_none() || input.get("y").is_none() {
                    let backend = session.backend_for_input(input).await?;
                    session.assert_click_reaches(backend).await?;
                }
                session.activity(&action_id, "targeting", op, &label, None, Some(&target));
                // Give the UI one frame to move the visible agent cursor before
                // the click lands. This is tiny for the agent and legible for a human.
                tokio::time::sleep(std::time::Duration::from_millis(90)).await;
                let button = mouse_button(input.get("button").and_then(Value::as_str));
                let clicks = if input.get("doubleClick").and_then(Value::as_bool) == Some(true) {
                    2
                } else {
                    1
                };
                session
                    .mouse(
                        DispatchMouseEventType::MouseMoved,
                        target.x,
                        target.y,
                        MouseButton::None,
                        0,
                    )
                    .await?;
                session
                    .mouse(
                        DispatchMouseEventType::MousePressed,
                        target.x,
                        target.y,
                        button.clone(),
                        clicks,
                    )
                    .await?;
                session
                    .mouse(
                        DispatchMouseEventType::MouseReleased,
                        target.x,
                        target.y,
                        button,
                        clicks,
                    )
                    .await?;
                session.settle_after_action().await;
                Ok(json!({
                    "clicked": { "x": target.x, "y": target.y },
                    "url": session.current_url(),
                }))
            }
            "hover" => {
                let target = session.resolve_target(input).await?;
                session.activity(&action_id, "targeting", op, &label, None, Some(&target));
                session
                    .mouse(
                        DispatchMouseEventType::MouseMoved,
                        target.x,
                        target.y,
                        MouseButton::None,
                        0,
                    )
                    .await?;
                tokio::time::sleep(std::time::Duration::from_millis(180)).await;
                Ok(json!({ "hovered": { "x": target.x, "y": target.y } }))
            }
            "drag" => {
                let from_input = prefixed_target(input, "from");
                let to_input = prefixed_target(input, "to");
                let from = session.resolve_target(&from_input).await?;
                let to = session.resolve_target(&to_input).await?;
                session.activity(&action_id, "targeting", op, &label, None, Some(&from));
                session
                    .mouse(
                        DispatchMouseEventType::MouseMoved,
                        from.x,
                        from.y,
                        MouseButton::None,
                        0,
                    )
                    .await?;
                session
                    .mouse(
                        DispatchMouseEventType::MousePressed,
                        from.x,
                        from.y,
                        MouseButton::Left,
                        1,
                    )
                    .await?;
                for step in 1..=8 {
                    let ratio = f64::from(step) / 8.0;
                    let x = from.x + (to.x - from.x) * ratio;
                    let y = from.y + (to.y - from.y) * ratio;
                    let progress = TargetPoint {
                        x,
                        y,
                        bounds: if step == 8 { to.bounds.clone() } else { None },
                        backend: to.backend,
                    };
                    session.activity(&action_id, "targeting", op, &label, None, Some(&progress));
                    session
                        .mouse(
                            DispatchMouseEventType::MouseMoved,
                            x,
                            y,
                            MouseButton::Left,
                            1,
                        )
                        .await?;
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
                session
                    .mouse(
                        DispatchMouseEventType::MouseReleased,
                        to.x,
                        to.y,
                        MouseButton::Left,
                        1,
                    )
                    .await?;
                session.settle_after_action().await;
                Ok(json!({ "dragged": { "from": { "x": from.x, "y": from.y }, "to": { "x": to.x, "y": to.y } } }))
            }
            "fill" | "select" => {
                let backend = session.backend_for_input(input).await?;
                let target = session.point_of_backend(backend).await?;
                session.activity(&action_id, "targeting", op, &label, None, Some(&target));
                let value = s("value");
                let result = session.fill_backend(backend, &value).await?;
                session.settle_after_action().await;
                Ok(json!({ "ok": true, "target": result }))
            }
            "fill_form" => {
                let fields = input
                    .get("fields")
                    .and_then(Value::as_array)
                    .ok_or_else(|| anyhow!("fill_form needs a fields array"))?;
                if fields.is_empty() {
                    return Err(anyhow!("fill_form needs at least one field"));
                }
                for field in fields {
                    let backend = session.backend_for_input(field).await?;
                    let target = session.point_of_backend(backend).await?;
                    session.activity(&action_id, "targeting", op, &label, None, Some(&target));
                    match field.get("kind").and_then(Value::as_str).unwrap_or("fill") {
                        "check" => {
                            let checked = field
                                .get("checked")
                                .and_then(Value::as_bool)
                                .unwrap_or(true);
                            session.set_checked_backend(backend, checked).await?;
                        }
                        "fill" | "select" => {
                            let value = field
                                .get("value")
                                .and_then(Value::as_str)
                                .ok_or_else(|| anyhow!("fill/select form fields need a value"))?;
                            session.fill_backend(backend, value).await?;
                        }
                        kind => return Err(anyhow!("unknown form field kind: {kind}")),
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(55)).await;
                }
                session.settle_after_action().await;
                Ok(json!({ "ok": true, "fieldCount": fields.len() }))
            }
            "check" => {
                let backend = session.backend_for_input(input).await?;
                let target = session.point_of_backend(backend).await?;
                session.activity(&action_id, "targeting", op, &label, None, Some(&target));
                let checked = input.get("checked").and_then(Value::as_bool).unwrap_or(true);
                let result = session.set_checked_backend(backend, checked).await?;
                session.settle_after_action().await;
                Ok(json!({ "ok": true, "target": result }))
            }
            "type" => {
                if input.get("ref").is_some() || input.get("selector").is_some() {
                    let backend = session.backend_for_input(input).await?;
                    let target = session.point_of_backend(backend).await?;
                    session.activity(&action_id, "targeting", op, &label, None, Some(&target));
                    session.focus_backend(backend).await?;
                }
                let per_key = input.get("fast").and_then(Value::as_bool) != Some(true);
                session.type_text(&s("text"), per_key).await?;
                if input.get("submit").and_then(Value::as_bool) == Some(true) {
                    session.press_chord("Enter").await?;
                }
                session.settle_after_action().await;
                Ok(json!({ "ok": true, "url": session.current_url() }))
            }
            "press" => {
                let key = s("key");
                session.press_chord(&key).await?;
                session.settle_after_action().await;
                Ok(json!({ "ok": true, "url": session.current_url() }))
            }
            "scroll" => {
                let (width, height) = *session.viewport.lock().unwrap();
                let x = n("x").unwrap_or(width as f64 / 2.0);
                let y = n("y").unwrap_or(height as f64 / 2.0);
                let target = TargetPoint { x, y, bounds: None, backend: None };
                session.activity(&action_id, "targeting", op, &label, None, Some(&target));
                session.wheel(x, y, n("dx").unwrap_or(0.0), n("dy").unwrap_or(400.0)).await?;
                // Scrolling blind ("did anything move? is there more?") forces a
                // screenshot round-trip, so report the new position instead.
                tokio::time::sleep(std::time::Duration::from_millis(120)).await;
                Ok(json!({ "ok": true, "scroll": session.scroll_state().await }))
            }
            "resize" => {
                let width = n("width").ok_or_else(|| anyhow!("resize needs width"))? as i64;
                let height = n("height").ok_or_else(|| anyhow!("resize needs height"))? as i64;
                session.set_viewport(width, height).await?;
                session.invalidate_refs();
                let (width, height) = *session.viewport.lock().unwrap();
                Ok(json!({ "ok": true, "width": width, "height": height }))
            }
            "record_start" => {
                let path = recording_path(&session, input.get("path").and_then(Value::as_str))?;
                session.cursor_to(-40.0, DEFAULT_HEIGHT as f64 * 0.6).await;
                session.presentation(true).await;
                let path = session.record_start(path).await?;
                Ok(json!({ "recording": true, "path": path.display().to_string() }))
            }
            "record_stop" => session.record_stop().await,
            "record_flow" => {
                let steps = input
                    .get("steps")
                    .and_then(Value::as_array)
                    .ok_or_else(|| anyhow!("record_flow needs a `steps` array"))?
                    .clone();
                if steps.is_empty() {
                    return Err(anyhow!("record_flow needs at least one step"));
                }
                let path = recording_path(&session, input.get("path").and_then(Value::as_str))?;
                let defaults = input.get("defaults").cloned().unwrap_or_else(|| json!({}));
                run_flow(&session, &action_id, op, path, &steps, &defaults).await
            }
            "screenshot" => {
                let element = if input.get("ref").is_some() || input.get("selector").is_some() {
                    let backend = session.backend_for_input(input).await?;
                    let target = session.point_of_backend(backend).await?;
                    session.activity(&action_id, "targeting", op, &label, None, Some(&target));
                    Some(backend)
                } else {
                    None
                };
                let full_page = input.get("fullPage").and_then(Value::as_bool) == Some(true);
                let path = input.get("path").and_then(Value::as_str);
                let written = session.screenshot_to_file(path, full_page, element).await?;
                let bytes = std::fs::metadata(&written).map(|meta| meta.len()).unwrap_or(0);
                Ok(json!({
                    "ok": true,
                    "path": written.to_string_lossy(),
                    "sizeBytes": bytes,
                    "fullPage": full_page,
                    "note": "Saved to disk. Call publish_artifact with this path to show it to the user as evidence.",
                }))
            }
            "downloads" => Ok(json!({
                "downloads": session.download_entries(),
                "directory": session.work_dir.to_string_lossy(),
            })),
            "network_body" => {
                let id = input
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("network_body needs the id of an entry from browser_network"))?;
                session.response_body(id).await
            }
            "evaluate" => {
                // Arbitrary JS in a signed-in browser is a way around every
                // other control here: it can read the page, post cross-origin,
                // and drive the session directly. The semantic tools cover
                // ordinary work; general browsing belongs in a chat with no
                // profile attached.
                if let Some(name) = session.profile_name() {
                    return Err(anyhow!(
                        "browser_evaluate is disabled while signed in as {name:?}; use the semantic browser tools, or a chat without a signed-in profile"
                    ));
                }
                let v = session.evaluate(&s("expression")).await?;
                Ok(json!({ "result": v }))
            }
            "wait_for" => {
                let timeout_ms = input.get("timeoutMs").and_then(|v| v.as_u64()).unwrap_or(10_000);
                wait_for_page(&session, input, timeout_ms).await?;
                Ok(json!({ "ok": true }))
            }
            "handle_dialog" => {
                let accept = input.get("accept").and_then(Value::as_bool).unwrap_or(true);
                let mut params = HandleJavaScriptDialogParams::new(accept);
                params.prompt_text = input
                    .get("promptText")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                session.page().execute(params).await?;
                session.set_dialog(None);
                Ok(json!({ "ok": true }))
            }
            "upload" => {
                let backend = session.backend_for_input(input).await?;
                let target = session.point_of_backend(backend).await?;
                session.activity(&action_id, "targeting", op, &label, None, Some(&target));
                let paths = input
                    .get("paths")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                if paths.is_empty() {
                    return Err(anyhow!("upload needs at least one path"));
                }
                session.upload_backend(backend, paths.clone()).await?;
                session.settle_after_action().await;
                Ok(json!({ "ok": true, "fileCount": paths.len() }))
            }
            "console" => {
                let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
                let mut entries = session.console_entries();
                if entries.len() > limit {
                    entries.drain(..entries.len() - limit);
                }
                Ok(json!({ "entries": entries }))
            }
            "network" => {
                let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(80) as usize;
                let strip_detail = |entry: &Value| {
                    let mut entry = entry.clone();
                    if let Some(object) = entry.as_object_mut() {
                        object.retain(|key, _| !key.starts_with('_'));
                    }
                    entry
                };
                let filter = input
                    .get("filter")
                    .and_then(Value::as_str)
                    .map(str::to_ascii_lowercase);
                let mut entries = session.network_entries();
                if let Some(filter) = filter {
                    entries.retain(|entry| entry.to_string().to_ascii_lowercase().contains(&filter));
                }
                if entries.len() > limit {
                    entries.drain(..entries.len() - limit);
                }
                let entries: Vec<Value> = entries.iter().map(strip_detail).collect();
                Ok(json!({ "entries": entries }))
            }
            "tabs" => Ok(json!({ "tabs": session.tabs().await? })),
            "new_tab" => {
                session.assert_origin_allowed(
                    input.get("url").and_then(Value::as_str).unwrap_or("about:blank"),
                )?;
                let tab = session
                    .new_tab(
                        input
                            .get("url")
                            .and_then(Value::as_str)
                            .unwrap_or("about:blank"),
                    )
                    .await?;
                Ok(json!({ "tab": tab }))
            }
            "switch_tab" => {
                let id = input
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("switch_tab needs id from browser_tabs"))?;
                let page = session.page_by_id(id).await?;
                session.activate_page(page).await?;
                Ok(json!({ "ok": true, "activeTabId": id, "url": session.current_url() }))
            }
            "close_tab" => {
                session
                    .close_tab(input.get("id").and_then(Value::as_str))
                    .await?;
                Ok(json!({ "ok": true, "tabs": session.tabs().await? }))
            }
            "status" => {
                let dialog = session.dialog.lock().unwrap().clone();
                let healthy = session.is_healthy().await;
                Ok(json!({
                    "url": session.current_url(),
                    "title": session.title().await,
                    "dialog": dialog,
                    "healthy": healthy,
                    "tabs": session.tabs().await.unwrap_or_default(),
                    "signedInProfile": session.profile_name(),
                    "allowedSites": session.allowed_origins(),
                }))
            }
            "snapshot" => {
                let mut snapshot = session.semantic_snapshot().await?;
                snapshot["screenshot"] = json!(session.screenshot_b64().await?);
                Ok(snapshot)
            }
            other => Err(anyhow!("unknown browser op: {other}")),
            }
        }
        .await;

        match result {
            Ok(mut value) => {
                if op != "snapshot"
                    && input.get("includeSnapshot").and_then(Value::as_bool) == Some(true)
                {
                    value["page"] = session.semantic_snapshot().await?;
                }
                session.emit_tabs().await;
                session.activity(&action_id, "completed", op, &label, None, None);
                Ok(value)
            }
            Err(error) => {
                let error = explain_action_error(error);
                session.activity(
                    &action_id,
                    "failed",
                    op,
                    &label,
                    Some(format!("{error:#}")),
                    None,
                );
                Err(error)
            }
        }
    }
}

/// Chrome deletes session-only cookies at startup unless the profile is set to
/// restore the previous session ("continue where you left off",
/// `session.restore_on_startup = 1`). Plenty of sites keep their login in a
/// session cookie, so without this pref a durable profile still signs the user
/// out on every restart. Merged into the existing file, never overwritten:
/// Chrome owns everything else in it. Only called while the profile's Chrome
/// is not running.
fn enable_session_restore(profile_dir: &std::path::Path) {
    let default_dir = profile_dir.join("Default");
    let prefs_path = default_dir.join("Preferences");
    let mut prefs: Value = std::fs::read_to_string(&prefs_path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!({}));
    let Some(root) = prefs.as_object_mut() else {
        return;
    };
    let session = root
        .entry("session")
        .or_insert_with(|| json!({}));
    match session.as_object_mut() {
        Some(session) => {
            session.insert("restore_on_startup".into(), json!(1));
        }
        None => return,
    }
    if std::fs::create_dir_all(&default_dir).is_ok() {
        let _ = std::fs::write(&prefs_path, prefs.to_string());
    }
}

/// Launch a headless Chrome, open a blank page, and start screencasting.
async fn spawn_session(
    profile: Option<crate::browser_profiles::ProfileSpec>,
) -> Result<Arc<BrowserSession>> {
    // Unique profile per session — chromiumoxide otherwise points every browser
    // at one shared temp dir, so concurrent per-thread Chromes collide on the
    // profile SingletonLock.
    let profile_dir = match &profile {
        Some(spec) => spec.dir.clone(),
        None => std::env::temp_dir().join(format!("{PROFILE_PREFIX}{}", Uuid::new_v4())),
    };
    if profile.is_some() {
        enable_session_restore(&profile_dir);
    }
    let mut builder = BrowserConfig::builder()
        .new_headless_mode()
        .no_sandbox()
        .user_data_dir(&profile_dir)
        .window_size(DEFAULT_WIDTH as u32, DEFAULT_HEIGHT as u32)
        // NOTE: chromiumoxide's `.arg()` takes the switch WITHOUT leading
        // dashes — it renders `--{key}` itself. Passing "--disable-gpu" here
        // produced "----disable-gpu", which Chrome silently ignores, so every
        // flag on this builder used to be a no-op.
        .arg("disable-gpu")
        .arg("disable-dev-shm-usage")
        .arg("no-first-run")
        .arg("no-default-browser-check")
        .arg("hide-scrollbars")
        // Bot-check survival. Without these, every major site's protection
        // (Google's /sorry interstitial, Cloudflare's managed challenge) blocks
        // the agent before it reaches content, and the *user* is the one who
        // sees a CAPTCHA in their Browser pane.
        //
        // `AutomationControlled` is the blink feature behind
        // `navigator.webdriver === true`. chromiumoxide passes
        // `--enable-automation` in its DEFAULT_ARGS, which sets that flag; this
        // switches the giveaway back off (verified: webdriver true -> false).
        .arg(("disable-blink-features", "AutomationControlled"));
    // The other giveaway is the User-Agent, which in headless mode literally
    // reads `HeadlessChrome/<v>`. Rewrite it to the normal `Chrome/<v>` token.
    // Derived from the real binary rather than hardcoded so it always agrees
    // with the Sec-CH-UA client hints Chrome sends (those are NOT affected by
    // --user-agent, so a stale hardcoded version would itself be a mismatch
    // tell). If the version can't be read we leave the UA alone: an honest
    // headless UA beats an inconsistent forged one.
    let chrome_path = find_chrome();
    if let Some(ua) = chrome_path.as_deref().and_then(desktop_user_agent) {
        builder = builder.arg(("user-agent", ua.as_str()));
    }

    // Site isolation is Chrome's defence against one site reading another
    // site's data inside the browser. Turning it off buys same-process access
    // to cross-origin iframes — worth it for a throwaway profile that holds
    // nothing, and NOT worth it for a profile holding a real logged-in session.
    // So disposable browsers get iframe reach; signed-in browsers keep the
    // boundary and report cross-origin frames as hidden.
    if profile.is_none() {
        // Tuple form so these MERGE into chromiumoxide's existing
        // `disable-features=TranslateUI` rather than emitting a second,
        // conflicting `--disable-features` that would drop it.
        builder = builder.arg((
            "disable-features",
            &["IsolateOrigins", "site-per-process"][..],
        ));
    }
    if let Some(path) = chrome_path {
        builder = builder.chrome_executable(path);
    }
    let config = builder
        .build()
        .map_err(|e| anyhow!("browser config: {e}"))?;

    let (browser, mut handler) = Browser::launch(config)
        .await
        .context("launching Chrome — is a chrome/chromium binary installed?")?;
    let (tx, _rx) = broadcast::channel::<BrowserMsg>(128);

    // Pump the CDP handler; ends when the browser closes.
    let handler_tx = tx.clone();
    let handler_task = tokio::spawn(async move {
        while let Some(ev) = handler.next().await {
            if ev.is_err() {
                break;
            }
        }
        let _ = handler_tx.send(BrowserMsg::EngineStopped);
    });

    // Chrome launches with one New Tab target. Reuse it instead of creating a
    // second blank page that would leak into both the human tab strip and the
    // agent's browser_tabs result.
    let mut initial_page = None;
    for _ in 0..20 {
        if let Some(page) = browser.pages().await?.into_iter().next() {
            initial_page = Some(page);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let page = match initial_page {
        Some(page) => page,
        None => browser.new_page("about:blank").await?,
    };
    page.execute(device_metrics(DEFAULT_WIDTH, DEFAULT_HEIGHT))
        .await?;
    page.execute(EnablePageParams::default()).await?;
    page.execute(EnableRuntimeParams::default()).await?;
    page.execute(EnableNetworkParams::default()).await?;
    page.execute(EnableAccessibilityParams::default()).await?;

    // Downloads land in a per-session directory instead of vanishing: a file
    // the flow produced is usually the evidence the user asked for.
    let work_dir = dirs::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".threadknot")
        .join("browser")
        .join(Uuid::new_v4().to_string());
    let _ = std::fs::create_dir_all(&work_dir);
    let _ = browser
        .execute(
            SetDownloadBehaviorParams::builder()
                .behavior(SetDownloadBehaviorBehavior::AllowAndName)
                .download_path(work_dir.to_string_lossy().into_owned())
                .events_enabled(true)
                .build()
                .map_err(|error| anyhow!(error))?,
        )
        .await;

    let session = Arc::new(BrowserSession {
        page: Mutex::new(page.clone()),
        _browser: browser,
        tx: tx.clone(),
        last_frame: Mutex::new(None),
        url: Mutex::new(String::new()),
        viewport: Mutex::new((DEFAULT_WIDTH, DEFAULT_HEIGHT)),
        refs: Mutex::new(ElementRefs::default()),
        console: Mutex::new(VecDeque::new()),
        network: Mutex::new(VecDeque::new()),
        dialog: Mutex::new(None),
        activities: Mutex::new(VecDeque::new()),
        action_lock: AsyncMutex::new(()),
        tab_lock: AsyncMutex::new(()),
        attached_pages: Mutex::new(HashSet::new()),
        tasks: Mutex::new(vec![handler_task]),
        profile_dir,
        downloads: Mutex::new(VecDeque::new()),
        work_dir,
        viewers: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        last_used: Mutex::new(std::time::Instant::now()),
        profile,
        frames_tx: broadcast::channel::<Vec<u8>>(8).0,
        recording: AsyncMutex::new(None),
        // Start the pointer off-canvas so the first glide enters from an edge
        // instead of appearing to teleport out of the top-left corner.
        cursor: Mutex::new((-40.0, DEFAULT_HEIGHT as f64 * 0.6)),
    });
    session.wire_downloads().await;

    // All per-page event streams (screencast, navigation, console, log,
    // network, dialogs) live in one place: `wire_page`, which every tab goes
    // through. The initial tab is not special.
    session.wire_page(page.clone()).await?;

    page.execute(
        StartScreencastParams::builder()
            .format(StartScreencastFormat::Jpeg)
            .quality(60)
            .max_width(MAX_FRAME_WIDTH)
            .max_height(MAX_FRAME_HEIGHT)
            .every_nth_frame(1)
            .build(),
    )
    .await?;

    // Popups and target=_blank links become the active shared tab immediately,
    // matching a normal browser. Closing the active page selects a surviving
    // tab, so neither the agent nor the human is stranded on a dead target.
    let mut created = session
        ._browser
        .event_listener::<EventTargetCreated>()
        .await?;
    let weak = Arc::downgrade(&session);
    let created_task = tokio::spawn(async move {
        while let Some(event) = created.next().await {
            if event.target_info.r#type != "page" {
                continue;
            }
            let Some(session) = weak.upgrade() else {
                break;
            };
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            if let Ok(page) = session
                ._browser
                .get_page(event.target_info.target_id.clone())
                .await
            {
                let _ = session.activate_page(page).await;
            }
        }
    });
    session.tasks.lock().unwrap().push(created_task);

    let mut destroyed = session
        ._browser
        .event_listener::<EventTargetDestroyed>()
        .await?;
    let weak = Arc::downgrade(&session);
    let destroyed_task = tokio::spawn(async move {
        while let Some(event) = destroyed.next().await {
            let Some(session) = weak.upgrade() else {
                break;
            };
            if session.active_target_id() == event.target_id.as_ref() {
                if let Ok(pages) = session._browser.pages().await {
                    if let Some(page) = pages.into_iter().last() {
                        let _ = session.activate_page(page).await;
                    }
                }
            } else {
                session.emit_tabs().await;
            }
        }
    });
    session.tasks.lock().unwrap().push(destroyed_task);
    session.emit_tabs().await;

    Ok(session)
}

/// Client -> server control frames (WS Text, JSON). Field names are camelCase.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Control {
    #[serde(rename_all = "camelCase")]
    Mouse {
        x: f64,
        y: f64,
        /// "moved" | "pressed" | "released"
        event: String,
        #[serde(default)]
        button: Option<String>,
        #[serde(default)]
        click_count: Option<i64>,
    },
    #[serde(rename_all = "camelCase")]
    Wheel {
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
    },
    #[serde(rename_all = "camelCase")]
    Key {
        /// "down" | "up"
        event: String,
        #[serde(default)]
        key: String,
        #[serde(default)]
        code: String,
        #[serde(default)]
        key_code: i64,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        modifiers: i64,
    },
    /// Text captured from the viewer's clipboard. Chrome runs in a separate
    /// process and cannot read that clipboard from a forwarded Ctrl/Cmd+V.
    Paste {
        text: String,
    },
    Navigate {
        url: String,
    },
    Back,
    Forward,
    Reload,
    Resize {
        width: i64,
        height: i64,
    },
    #[serde(rename_all = "camelCase")]
    Dialog {
        accept: bool,
        #[serde(default)]
        prompt_text: Option<String>,
    },
    #[serde(rename = "newTab")]
    #[serde(rename_all = "camelCase")]
    NewTab {
        #[serde(default)]
        url: Option<String>,
    },
    #[serde(rename = "switchTab")]
    SwitchTab {
        id: String,
    },
    #[serde(rename = "closeTab")]
    #[serde(rename_all = "camelCase")]
    CloseTab {
        #[serde(default)]
        id: Option<String>,
    },
    /// Right-click hit-test at a viewport point. Chrome is headless and its
    /// context menu is browser chrome that never reaches the screencast, so the
    /// viewer draws its own menu — but only the page's own document knows what
    /// is under the point. The reply comes back as a `context` frame tagged
    /// with `nonce`.
    Probe {
        x: f64,
        y: f64,
        nonce: String,
    },
    Reset,
}

fn mouse_button(name: Option<&str>) -> MouseButton {
    match name.unwrap_or("left") {
        "right" => MouseButton::Right,
        "middle" => MouseButton::Middle,
        "none" => MouseButton::None,
        _ => MouseButton::Left,
    }
}

fn mouse_button_mask(button: &MouseButton) -> i64 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Right => 2,
        MouseButton::Middle => 4,
        _ => 0,
    }
}

fn param<'a>(params: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    params.get(key).map(String::as_str)
}

/// `GET /browser?token=…&session=…` — attach (or spawn) the browser session and
/// bridge it to the socket. `session` is a thread id (or any stable key).
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<HashMap<String, String>>,
    headers: axum::http::HeaderMap,
    State(state): State<ServerState>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use crate::mobile::Capability;
    if !crate::server::ws_origin_allowed(&headers) {
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    let principal = match state.authenticate_ingress(&headers, &params) {
        Ok(authenticated) => authenticated.principal,
        Err(rejection) => return rejection.into_response(),
    };
    if let Err(e) = principal.require(Capability::Browser) {
        return crate::server::forbidden(e);
    }
    let key = param(&params, "session").unwrap_or("default").to_string();

    // A thread that lives on another machine drives THAT machine's browser:
    // its Chrome, its signed-in profiles, its network. Splicing is what makes
    // "sign a remote machine's browser in from the one you're sitting at"
    // possible at all — the alternative is walking over to it.
    //
    // Owner-only, deliberately stricter than `browser` + `mesh`.
    //
    // The original reason was that the splice carried the PEER's master token,
    // so the signed-in check would run over there as that machine's owner and
    // pass. SEC-012 removed that: the splice now carries the caller's own grants
    // and the far side enforces them, so `browser` + `mesh` would in fact be
    // sound. It stays owner-only anyway, because relaxing it is a *widening* of
    // authority over stored browser logins and that is a product decision, not a
    // consequence of a transport change. The check to relax, when someone
    // decides to, is this one — and `mesh` + `browser` is then sufficient.
    if let Some(mid) = param(&params, "machineId") {
        if mid != state.device.machine_id {
            if !principal.is_owner() {
                return (
                    StatusCode::FORBIDDEN,
                    "another machine's browser can only be opened from that machine's owner",
                )
                    .into_response();
            }
            let mid = mid.to_string();
            let grants = principal.mesh_assertion();
            // A spliced screencast costs this machine a socket pair and the
            // bandwidth either way, so it counts against the same budget.
            let guard = match admit_browser(&state, &principal) {
                Ok(guard) => guard,
                Err(resp) => return *resp,
            };
            return crate::limits::control_frame_caps(ws).on_upgrade(move |socket| async move {
                tokio::select! {
                    _ = crate::peernet::splice_browser(socket, state, mid, params, grants) => {}
                    _ = guard.closed() => {}
                }
            });
        }
    }

    // A signed-in browser shows a logged-in account and can act as its owner,
    // so it takes the separate `signedBrowser` grant — which is never part of
    // the default set, on any pairing.
    if !principal.can(crate::mobile::Capability::SignedBrowser) {
        let signed_in = state
            .hub
            .store
            .thread(&key)
            .and_then(|thread| thread.settings.browser_profile_id.clone())
            .is_some_and(|id| !id.is_empty());
        if signed_in {
            return (
                StatusCode::FORBIDDEN,
                "this chat's browser is signed in; open it on the machine itself",
            )
                .into_response();
        }
    }

    let guard = match admit_browser(&state, &principal) {
        Ok(guard) => guard,
        Err(resp) => return *resp,
    };
    let registry = Arc::clone(&state.browsers);
    let session = match registry.get_or_spawn(&key).await {
        Ok(s) => s,
        Err(e) => {
            // Includes the live-session limit, whose message names it.
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("browser spawn failed: {e:#}"),
            )
                .into_response();
        }
    };
    // Navigate to an initial URL if provided and the session is fresh.
    if let Some(url) = param(&params, "url") {
        if !url.is_empty() && session.current_url().is_empty() {
            let _ = session.navigate(url).await;
        }
    }
    crate::limits::control_frame_caps(ws).on_upgrade(move |socket| async move {
        tokio::select! {
            _ = bridge(socket, registry, key, session) => {}
            _ = guard.closed() => {}
        }
    })
}

/// Take a slot in this principal's browser budget, before the upgrade — after it
/// the only way to refuse is a close frame with nothing readable in it.
fn admit_browser(
    state: &ServerState,
    principal: &crate::mobile::Principal,
) -> Result<crate::sessions::SessionGuard, Box<axum::response::Response>> {
    state
        .sessions
        .try_register(principal, state.policy, crate::sessions::SessionKind::Browser)
        .map_err(|e| {
            Box::new(
                (axum::http::StatusCode::TOO_MANY_REQUESTS, format!("{e:#}")).into_response(),
            )
        })
}

/// Counts attached human viewers for the lifetime of one socket.
struct ViewerGuard(Arc<std::sync::atomic::AtomicUsize>);

impl ViewerGuard {
    fn new(counter: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self(counter)
    }
}

impl Drop for ViewerGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Bridge one WebSocket client to a session: replay the last frame, fan out new
/// frames as binary, forward control frames. The session outlives the socket.
async fn bridge(
    socket: WebSocket,
    registry: Arc<BrowserRegistry>,
    key: String,
    session: Arc<BrowserSession>,
) {
    let (mut sink, mut stream) = socket.split();

    // A watched session is never reaped, and the guard survives every early
    // return below so a failed attach can't leak a phantom viewer.
    session.touch();
    let _viewer = ViewerGuard::new(Arc::clone(&session.viewers));

    let (replay, mut rx) = {
        let last = session.last_frame.lock().unwrap().clone();
        (last, session.tx.subscribe())
    };
    // The frontend may update before this machine process does (dev hot reload,
    // or a newer peer viewing an older machine). Unknown controls are ignored
    // for forward compatibility, so advertise the efficient paste primitive;
    // viewers without this message fall back to the long-standing key control.
    let capabilities = json!({ "type": "capabilities", "paste": true }).to_string();
    if sink.send(Message::Text(capabilities.into())).await.is_err() {
        return;
    }
    // Sync the client's address bar to the session's current URL, then replay
    // the last frame so the pane isn't blank on attach.
    let cur_url = session.current_url();
    if !cur_url.is_empty() {
        let _ = sink
            .send(Message::Text(
                json!({ "type": "nav", "url": cur_url }).to_string().into(),
            ))
            .await;
    }
    let history = session.activity_history();
    if !history.is_empty() {
        let frame = json!({ "type": "activityHistory", "activities": history }).to_string();
        if sink.send(Message::Text(frame.into())).await.is_err() {
            return;
        }
    }
    let dialog = session.dialog.lock().unwrap().clone();
    if dialog.is_some() {
        let frame = json!({ "type": "dialog", "dialog": dialog }).to_string();
        if sink.send(Message::Text(frame.into())).await.is_err() {
            return;
        }
    }
    if let Ok(tabs) = session.tabs().await {
        let frame = json!({ "type": "tabs", "tabs": tabs }).to_string();
        if sink.send(Message::Text(frame.into())).await.is_err() {
            return;
        }
    }
    if let Some(frame) = replay {
        if sink.send(Message::Binary(frame.into())).await.is_err() {
            return;
        }
    }

    // Output task: broadcast frames/nav -> WS.
    let out = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(BrowserMsg::Frame(bytes)) => {
                    if sink.send(Message::Binary(bytes.into())).await.is_err() {
                        break;
                    }
                }
                Ok(BrowserMsg::Nav(url)) => {
                    let frame = json!({ "type": "nav", "url": url }).to_string();
                    if sink.send(Message::Text(frame.into())).await.is_err() {
                        break;
                    }
                }
                Ok(BrowserMsg::Activity(activity)) => {
                    let frame = json!({ "type": "activity", "activity": activity }).to_string();
                    if sink.send(Message::Text(frame.into())).await.is_err() {
                        break;
                    }
                }
                Ok(BrowserMsg::Dialog(dialog)) => {
                    let frame = json!({ "type": "dialog", "dialog": dialog }).to_string();
                    if sink.send(Message::Text(frame.into())).await.is_err() {
                        break;
                    }
                }
                Ok(BrowserMsg::EngineStopped) => {
                    let frame = json!({ "type": "engine", "state": "stopped" }).to_string();
                    let _ = sink.send(Message::Text(frame.into())).await;
                    break;
                }
                Ok(BrowserMsg::Tabs(tabs)) => {
                    let frame = json!({ "type": "tabs", "tabs": tabs }).to_string();
                    if sink.send(Message::Text(frame.into())).await.is_err() {
                        break;
                    }
                }
                Ok(BrowserMsg::Context(ctx)) => {
                    if sink.send(Message::Text(ctx.to_string().into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
        let _ = sink.send(Message::Close(None)).await;
    });

    // Input loop: control frames -> CDP. Ends on disconnect; session survives.
    while let Some(Ok(msg)) = stream.next().await {
        let Message::Text(text) = msg else { continue };
        let Ok(ctrl) = serde_json::from_str::<Control>(&text) else {
            continue;
        };
        if matches!(&ctrl, Control::Reset) {
            registry.remove(&key);
            break;
        }
        session.touch();
        let _ = dispatch_control(&session, ctrl).await;
    }

    out.abort();
}

async fn dispatch_control(session: &Arc<BrowserSession>, ctrl: Control) -> Result<()> {
    match ctrl {
        Control::Mouse {
            x,
            y,
            event,
            button,
            click_count,
        } => {
            let kind = match event.as_str() {
                "pressed" => DispatchMouseEventType::MousePressed,
                "released" => DispatchMouseEventType::MouseReleased,
                _ => DispatchMouseEventType::MouseMoved,
            };
            let btn = if matches!(kind, DispatchMouseEventType::MouseMoved) {
                MouseButton::None
            } else {
                mouse_button(button.as_deref())
            };
            session
                .mouse(kind.clone(), x, y, btn, click_count.unwrap_or(1))
                .await?;
            if matches!(kind, DispatchMouseEventType::MouseReleased) {
                session.settle_after_action().await;
            }
            Ok(())
        }
        Control::Wheel {
            x,
            y,
            delta_x,
            delta_y,
        } => session.wheel(x, y, delta_x, delta_y).await,
        Control::Key {
            event,
            key,
            code,
            key_code,
            text,
            modifiers,
        } => {
            session
                .key(
                    event == "down",
                    &key,
                    &code,
                    key_code,
                    text.as_deref(),
                    modifiers,
                )
                .await
        }
        Control::Paste { text } => session.insert_text(&text).await,
        Control::Navigate { url } => session.navigate(&url).await,
        Control::Back => session.history(-1).await,
        Control::Forward => session.history(1).await,
        Control::Reload => {
            session.reload().await?;
            session.settle_after_action().await;
            Ok(())
        }
        Control::Resize { width, height } => session.set_viewport(width, height).await,
        Control::Dialog {
            accept,
            prompt_text,
        } => {
            let mut params = HandleJavaScriptDialogParams::new(accept);
            params.prompt_text = prompt_text;
            session.page().execute(params).await?;
            session.set_dialog(None);
            Ok(())
        }
        Control::NewTab { url } => {
            session
                .new_tab(url.as_deref().unwrap_or("about:blank"))
                .await?;
            Ok(())
        }
        Control::SwitchTab { id } => {
            let page = session.page_by_id(&id).await?;
            session.activate_page(page).await
        }
        Control::CloseTab { id } => session.close_tab(id.as_deref()).await,
        Control::Probe { x, y, nonce } => session.probe_context(x, y, &nonce).await,
        Control::Reset => Ok(()),
    }
}

// ---- helpers shared by the MCP invoke() path ----

/// Turn raw CDP node failures into the instruction that actually resolves them.
/// "Node does not have a layout object" tells an agent nothing; it retries the
/// same dead ref or falls back to guessing coordinates.
fn explain_action_error(error: anyhow::Error) -> anyhow::Error {
    let text = format!("{error:#}");
    let stale = [
        "Could not find node with given id",
        "does not have a layout object",
        "No node with given id found",
        "is no longer attached",
        "Node with given id does not belong to the document",
    ];
    if stale.iter().any(|needle| text.contains(needle)) {
        return anyhow!(
            "that element is no longer in the page (it was removed, re-rendered, or the page navigated); call browser_snapshot and use a ref from the fresh snapshot — original error: {text}"
        );
    }
    error
}

/// Prefix a bare host/path with http:// so navigation gets a real URL.
fn normalize_url(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return "about:blank".into();
    }
    if s.contains("://")
        || ["about:", "data:", "file:", "chrome:", "view-source:"]
            .iter()
            .any(|prefix| s.starts_with(prefix))
    {
        return s.into();
    }
    format!("http://{s}")
}

fn truncate_text(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

fn ax_string(
    value: Option<&chromiumoxide::cdp::browser_protocol::accessibility::AxValue>,
) -> Option<String> {
    let value = value?.value.as_ref()?;
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

fn is_interactive_role(role: &str) -> bool {
    matches!(
        role,
        "button"
            | "checkbox"
            | "combobox"
            | "gridcell"
            | "link"
            | "listbox"
            | "menuitem"
            | "menuitemcheckbox"
            | "menuitemradio"
            | "option"
            | "radio"
            | "scrollbar"
            | "searchbox"
            | "slider"
            | "spinbutton"
            | "switch"
            | "tab"
            | "textbox"
            | "treeitem"
    )
}

/// Container roles worth a line even when they have no accessible name: they
/// are what lets an agent say "the Delete button *in the INV-002 row*".
fn is_structural_role(role: &str) -> bool {
    matches!(
        role,
        "alert"
            | "alertdialog"
            | "article"
            | "banner"
            | "cell"
            | "columnheader"
            | "complementary"
            | "contentinfo"
            | "dialog"
            | "form"
            | "grid"
            | "list"
            | "listitem"
            | "main"
            | "navigation"
            | "region"
            | "row"
            | "rowheader"
            | "search"
            | "status"
            | "table"
            | "tablist"
            | "toolbar"
            // Chrome demotes a table with no headers or caption to a *layout*
            // table, which is what most application data grids are. The row
            // grouping is exactly what disambiguates "the Delete button in the
            // INV-002 row", so keep it even in demoted form.
            | "layouttablerow"
    )
}

/// Chrome's internal role names for demoted tables read like implementation
/// details; present them as what they are.
fn display_role(role: &str) -> &str {
    match role {
        "LayoutTableRow" => "row",
        "LayoutTable" => "table",
        "LayoutTableCell" => "cell",
        other => other,
    }
}

/// Wrappers that exist for layout, not meaning. Their children still matter, so
/// the walk descends without emitting a line or an indent level.
fn is_layout_wrapper_role(role: &str) -> bool {
    matches!(
        role,
        "none"
            | "generic"
            | "genericcontainer"
            | "presentation"
            | "layouttable"
            | "layouttablecell"
            | "paragraph"
            | "section"
            | "labeltext"
    )
}

fn classify_ax_node(
    ignored: bool,
    role: &str,
    name: &str,
    value: &str,
    inherited_name: &str,
) -> NodeAction {
    if ignored {
        return NodeAction::Flatten;
    }
    // Per-character leaf text. Its parent StaticText already carries the words;
    // emitting these turned "Skip to content" into thirteen separate lines.
    if role == "inlinetextbox" || role == "linebreak" {
        return NodeAction::Drop;
    }
    if role == "statictext" {
        let trimmed = name.trim();
        // Text already spoken by the nearest emitted ancestor, or pure
        // separators like " | ", carry no information for the agent.
        if trimmed.is_empty()
            || inherited_name.contains(trimmed)
            || !trimmed.chars().any(char::is_alphanumeric)
        {
            return NodeAction::Drop;
        }
        return NodeAction::Emit;
    }
    if is_layout_wrapper_role(role) {
        return NodeAction::Flatten;
    }
    if is_interactive_role(role) || is_structural_role(role) {
        return NodeAction::Emit;
    }
    if name.is_empty() && value.is_empty() {
        return NodeAction::Flatten;
    }
    NodeAction::Emit
}

fn render_ax_line(
    node: &AxNode,
    depth: usize,
    role: &str,
    name: &str,
    value: &str,
    reference: Option<String>,
) -> String {
    let mut state = Vec::new();
    if let Some(properties) = &node.properties {
        for property in properties {
            let key = property.name.as_ref();
            if matches!(
                key,
                "checked" | "disabled" | "expanded" | "focused" | "pressed" | "selected" | "required"
            ) {
                if let Some(value) = ax_string(Some(&property.value)) {
                    if value != "false" {
                        state.push(format!("{key}={value}"));
                    }
                }
            }
        }
    }
    let ref_text = reference
        .map(|reference| format!("[ref={reference}] "))
        .unwrap_or_default();
    let mut text = format!(
        "{}- {ref_text}{}",
        "  ".repeat(depth.min(12)),
        display_role(role)
    );
    if !name.is_empty() {
        text.push_str(&format!(" {:?}", truncate_text(name, 180)));
    }
    if !value.is_empty() && value != name {
        text.push_str(&format!(" value={:?}", truncate_text(value, 120)));
    }
    if !state.is_empty() {
        text.push_str(&format!(" ({})", state.join(", ")));
    }
    text
}

/// Keys whose DEFAULT action Chrome only performs when the keydown carries
/// `text`. Enter is the one that matters most: without it, "type a query and
/// press Enter" fires a keydown and submits nothing.
fn default_text_for_key(key: &str) -> Option<&str> {
    match key {
        "Enter" | "NumpadEnter" => Some("\r"),
        "Tab" => Some("\t"),
        " " | "Space" => Some(" "),
        single if single.chars().count() == 1 => Some(single),
        _ => None,
    }
}

/// Characters we can dispatch as real key events. Anything else (emoji, IME
/// output, pasted blobs) goes through `Input.insertText`.
fn is_typeable_char(ch: char) -> bool {
    ch == '\n' || (ch.is_ascii() && !ch.is_control())
}

fn prefixed_target(input: &Value, prefix: &str) -> Value {
    let key = |suffix: &str| {
        let mut name = prefix.to_string();
        name.push_str(suffix);
        name
    };
    json!({
        "ref": input.get(key("Ref")).cloned().unwrap_or(Value::Null),
        "selector": input.get(key("Selector")).cloned().unwrap_or(Value::Null),
        "x": input.get(key("X")).cloned().unwrap_or(Value::Null),
        "y": input.get(key("Y")).cloned().unwrap_or(Value::Null),
    })
}

fn target_description(input: &Value) -> String {
    if let Some(reference) = input.get("ref").and_then(Value::as_str) {
        return format!("element {reference}");
    }
    if let Some(selector) = input.get("selector").and_then(Value::as_str) {
        return format!("selector {}", truncate_text(selector, 90));
    }
    if let (Some(x), Some(y)) = (
        input.get("x").and_then(Value::as_f64),
        input.get("y").and_then(Value::as_f64),
    ) {
        return format!("point {x:.0}, {y:.0}");
    }
    "focused element".to_string()
}

/// Resolve where a recording should be written, defaulting into the session's
/// work directory. A caller-supplied relative path is taken as relative to that
/// same directory so a flow cannot scatter files across the filesystem.
fn recording_path(session: &BrowserSession, requested: Option<&str>) -> Result<PathBuf> {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let Some(raw) = requested.map(str::trim).filter(|p| !p.is_empty()) else {
        return Ok(crate::recorder::default_output(&session.work_dir, &stamp));
    };
    let candidate = PathBuf::from(raw);
    let path = if candidate.is_absolute() {
        candidate
    } else {
        session.work_dir.join(candidate)
    };
    if path.extension().is_none() {
        return Err(anyhow!("recording path needs a .mp4 extension: {raw}"));
    }
    Ok(path)
}

/// Pacing for a recorded flow. These are the knobs that decide whether a
/// walkthrough reads as a demonstration or as a machine hammering a page.
struct FlowPacing {
    /// Time the pointer takes to travel to a target.
    move_ms: u64,
    /// Pause after an action completes, so a viewer can register what changed.
    dwell_ms: u64,
    /// Typing speed. Real typing is uneven, so this is a mean, not a metronome.
    key_ms: u64,
    /// Beat between a caption appearing and the action it describes, so the
    /// viewer reads the instruction before watching it happen.
    read_ms: u64,
}

impl FlowPacing {
    fn from(defaults: &Value) -> Self {
        let u = |k: &str, fallback: u64| {
            defaults
                .get(k)
                .and_then(Value::as_u64)
                .unwrap_or(fallback)
                .clamp(0, 20_000)
        };
        Self {
            move_ms: u("moveMs", 620),
            dwell_ms: u("dwellMs", 900),
            key_ms: u("keyMs", 55),
            read_ms: u("readMs", 700),
        }
    }

    fn step(&self, step: &Value) -> Self {
        let u = |k: &str, fallback: u64| {
            step.get(k)
                .and_then(Value::as_u64)
                .unwrap_or(fallback)
                .clamp(0, 20_000)
        };
        Self {
            move_ms: u("moveMs", self.move_ms),
            dwell_ms: u("dwellMs", self.dwell_ms),
            key_ms: u("keyMs", self.key_ms),
            read_ms: u("readMs", self.read_ms),
        }
    }
}

async fn pause(ms: u64) {
    if ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    }
}

/// Execute a storyboard end to end while recording.
///
/// This exists because the alternative — the agent issuing act/screenshot/act
/// over MCP — produces neither a usable video nor a natural one: every step
/// pays a model round trip, so the pacing is whatever the model's latency
/// happened to be. Here the whole flow is described up front and played back on
/// timings chosen for a viewer.
async fn run_flow(
    session: &Arc<BrowserSession>,
    action_id: &str,
    op: &str,
    path: PathBuf,
    steps: &[Value],
    defaults: &Value,
) -> Result<Value> {
    let base = FlowPacing::from(defaults);
    let (w, h) = *session.viewport.lock().unwrap();

    session.cursor_to(-40.0, h as f64 * 0.6).await;
    session.presentation(true).await;
    session.record_start(path).await?;

    // From here on a failure must still finalize the file: a half-recorded
    // walkthrough that plays is far more useful for diagnosis than an orphaned
    // ffmpeg process and no output.
    let outcome = async {
        let mut done: Vec<Value> = Vec::new();
        for (index, step) in steps.iter().enumerate() {
            let pacing = base.step(step);
            let action = step
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("wait")
                .to_string();
            let caption = step.get("caption").and_then(Value::as_str);
            let label = caption.unwrap_or(action.as_str()).to_string();
            session.activity(
                action_id,
                "running",
                op,
                &format!("Recording step {}/{}: {label}", index + 1, steps.len()),
                None,
                None,
            );

            if caption.is_some() {
                session.caption(caption).await;
                pause(pacing.read_ms).await;
            }

            // Anything that addresses an element gets the same treatment:
            // resolve it, optionally spotlight and zoom, then travel there.
            let addressed = step.get("ref").is_some() || step.get("selector").is_some();
            let mut target: Option<TargetPoint> = None;
            if addressed && matches!(action.as_str(), "click" | "type" | "hover" | "fill" | "focus")
            {
                // Measuring once is not enough on a page that is still settling:
                // ad slots and lazy modules on a heavy quote page reflow for a
                // second or two after it looks loaded, and a rect captured
                // mid-reflow ends up framing whatever has since slid into that
                // space. Wait for two consecutive agreeing readings first.
                let first = session.resolve_target(step).await?;
                let mut point = session.settled_target(step, first).await;
                if let Some(zoom) = step.get("zoom").and_then(Value::as_f64) {
                    session.zoom_to(zoom, Some((point.x, point.y)), 420).await;
                    // The zoom's centring scroll moves the subject again, so the
                    // rect that located it a moment ago describes where it used
                    // to be.
                    point = session.settled_target(step, point).await;
                }
                let spotlit = step.get("spotlight").and_then(Value::as_bool) == Some(true);
                let blurred = step.get("blur").and_then(Value::as_bool) == Some(true);
                if spotlit {
                    session.spotlight(point.bounds.as_ref()).await;
                }
                if blurred {
                    session.blur_except(point.bounds.as_ref()).await;
                }
                // Bind after setting, so the effects being watched are the ones
                // just painted for this element.
                if spotlit || blurred {
                    if let Some(backend) = point.backend {
                        session.watch_subject(backend).await;
                    }
                }
                if action == "focus" {
                    // `focus` features a detail rather than interacting with it,
                    // so there is nothing for a pointer to explain. Hiding it
                    // also avoids parking an arrow at coordinates the zoom's
                    // centring scroll has since moved out from under.
                    session.overlay("o.cursorVisible(false);").await;
                } else {
                    session.glide(point.x, point.y, pacing.move_ms).await;
                }
                target = Some(point);
            }

            match action.as_str() {
                "navigate" => {
                    let url = step.get("url").and_then(Value::as_str).unwrap_or_default();
                    session.assert_origin_allowed(url)?;
                    session.zoom_reset().await;
                    session.spotlight(None).await;
                    session.blur_except(None).await;
                    session.navigate(url).await?;
                    session.settle_after_action().await;
                    session.reassert_presentation(caption).await;
                }
                "click" => {
                    let point = target.ok_or_else(|| anyhow!("click step needs ref or selector"))?;
                    session.overlay(&format!("o.click({:.1},{:.1});", point.x, point.y)).await;
                    session
                        .mouse(DispatchMouseEventType::MousePressed, point.x, point.y, MouseButton::Left, 1)
                        .await?;
                    // A real click has a dwell between press and release.
                    pause(90).await;
                    session
                        .mouse(DispatchMouseEventType::MouseReleased, point.x, point.y, MouseButton::Left, 1)
                        .await?;
                    session.settle_after_action().await;
                    // A click can navigate (a link, a submit button) just as
                    // readily as an explicit navigate step.
                    session.reassert_presentation(caption).await;
                }
                "type" | "fill" => {
                    let text = step.get("text").and_then(Value::as_str).unwrap_or_default();
                    if let Some(point) = &target {
                        session.overlay(&format!("o.click({:.1},{:.1});", point.x, point.y)).await;
                        session
                            .mouse(DispatchMouseEventType::MousePressed, point.x, point.y, MouseButton::Left, 1)
                            .await?;
                        pause(80).await;
                        session
                            .mouse(DispatchMouseEventType::MouseReleased, point.x, point.y, MouseButton::Left, 1)
                            .await?;
                    }
                    session.type_text_paced(text, pacing.key_ms).await?;
                    if step.get("submit").and_then(Value::as_bool) == Some(true) {
                        pause(260).await;
                        session.press_key("Enter").await?;
                        session.settle_after_action().await;
                        // Submitting is the most common way a recorded flow
                        // changes document without a `navigate` step.
                        session.reassert_presentation(caption).await;
                    }
                }
                "hover" | "focus" => {
                    // Travelling there — with whatever spotlight, blur and zoom
                    // the step asked for — was the whole action. `focus` exists
                    // so a walkthrough can draw attention to something it never
                    // interacts with: a price, a chart, a status badge.
                }
                "scroll" => {
                    let dy = step.get("dy").and_then(Value::as_f64).unwrap_or(400.0);
                    let dx = step.get("dx").and_then(Value::as_f64).unwrap_or(0.0);
                    session.smooth_scroll(dx, dy, pacing.move_ms.max(400)).await?;
                }
                "wait" => {
                    let ms = step.get("ms").and_then(Value::as_u64).unwrap_or(800);
                    pause(ms.min(20_000)).await;
                }
                other => return Err(anyhow!("unknown flow action: {other}")),
            }

            pause(pacing.dwell_ms).await;
            if step.get("zoom").is_some() {
                session.zoom_reset().await;
            }
            if step.get("spotlight").and_then(Value::as_bool) == Some(true) {
                session.spotlight(None).await;
            }
            if step.get("blur").and_then(Value::as_bool) == Some(true) {
                session.blur_except(None).await;
            }
            if action == "focus" {
                session.overlay("o.cursorVisible(true);").await;
            }
            done.push(json!({ "index": index, "action": action, "caption": caption }));
        }
        // Let the final frame breathe rather than cutting on the last click.
        session.caption(None).await;
        pause(600).await;
        Ok::<Vec<Value>, anyhow::Error>(done)
    }
    .await;

    let stopped = session.record_stop().await;
    let _ = (w, h);
    match (outcome, stopped) {
        (Ok(done), Ok(mut video)) => {
            if let Some(obj) = video.as_object_mut() {
                obj.insert("steps".into(), Value::Array(done));
            }
            Ok(video)
        }
        // The flow failed but the partial video finalized: hand back both, so
        // the caller can see how far it got instead of guessing.
        (Err(e), Ok(video)) => Err(anyhow!(
            "flow failed: {e:#} (partial recording at {})",
            video.get("path").and_then(Value::as_str).unwrap_or("unknown")
        )),
        (Err(e), Err(_)) => Err(e),
        (Ok(_), Err(e)) => Err(e),
    }
}

fn activity_description(op: &str, input: &Value) -> (String, Option<String>) {
    let target = target_description(input);
    match op {
        "navigate" => (
            "Navigating".to_string(),
            input
                .get("url")
                .and_then(Value::as_str)
                .map(|value| truncate_text(value, 180)),
        ),
        "back" => ("Going back".to_string(), None),
        "forward" => ("Going forward".to_string(), None),
        "reload" => ("Reloading page".to_string(), None),
        "click" => (format!("Clicking {target}"), None),
        "hover" => (format!("Hovering over {target}"), None),
        "drag" => ("Dragging between page elements".to_string(), None),
        "fill" => (
            format!("Filling {target}"),
            Some("Input value hidden".to_string()),
        ),
        "fill_form" => (
            "Filling form".to_string(),
            Some("Form values hidden".to_string()),
        ),
        "select" => (format!("Selecting in {target}"), None),
        "check" => (format!("Changing {target}"), None),
        "type" => (
            format!("Typing into {target}"),
            Some("Input text hidden".to_string()),
        ),
        "press" => (
            format!(
                "Pressing {}",
                input.get("key").and_then(Value::as_str).unwrap_or("key")
            ),
            None,
        ),
        "scroll" => ("Scrolling page".to_string(), None),
        "record_start" => ("Starting screen recording".to_string(), None),
        "record_stop" => ("Finishing screen recording".to_string(), None),
        "record_flow" => (
            "Recording a guided walkthrough".to_string(),
            input
                .get("steps")
                .and_then(Value::as_array)
                .map(|steps| format!("{} steps", steps.len())),
        ),
        "evaluate" => (
            "Running page JavaScript".to_string(),
            input
                .get("expression")
                .and_then(Value::as_str)
                .map(|value| truncate_text(value, 180)),
        ),
        "wait_for" => ("Waiting for page state".to_string(), None),
        "handle_dialog" => ("Handling page dialog".to_string(), None),
        "upload" => {
            let names = input
                .get("paths")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter_map(|path| std::path::Path::new(path).file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            (format!("Uploading to {target}"), Some(names.join(", ")))
        }
        "console" => ("Reading browser console".to_string(), None),
        "network" => ("Inspecting network traffic".to_string(), None),
        "tabs" => ("Inspecting browser tabs".to_string(), None),
        "new_tab" => (
            "Opening a new tab".to_string(),
            input
                .get("url")
                .and_then(Value::as_str)
                .map(|value| truncate_text(value, 180)),
        ),
        "switch_tab" => ("Switching browser tabs".to_string(), None),
        "close_tab" => ("Closing a browser tab".to_string(), None),
        "snapshot" => ("Inspecting page structure".to_string(), None),
        "status" => ("Checking browser status".to_string(), None),
        other => (format!("Running browser action: {other}"), None),
    }
}

fn push_ring(ring: &Mutex<VecDeque<Value>>, value: Value, limit: usize) {
    let mut ring = ring.lock().unwrap();
    ring.push_back(value);
    while ring.len() > limit {
        ring.pop_front();
    }
}

fn console_arg_text(object: &chromiumoxide::cdp::js_protocol::runtime::RemoteObject) -> String {
    let raw = serde_json::to_value(object).unwrap_or(Value::Null);
    if let Some(value) = raw.get("value") {
        return match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
    }
    raw.get("description")
        .or_else(|| raw.get("unserializableValue"))
        .and_then(Value::as_str)
        .unwrap_or("[object]")
        .to_string()
}

async fn wait_for_page(session: &BrowserSession, input: &Value, timeout_ms: u64) -> Result<()> {
    let selector = input.get("selector").and_then(Value::as_str);
    let text = input.get("text").and_then(Value::as_str);
    let text_gone = input.get("textGone").and_then(Value::as_str);
    if selector.is_none() && text.is_none() && text_gone.is_none() {
        return Err(anyhow!("wait_for needs selector, text, or textGone"));
    }
    let expression = format!(
        r#"(() => {{
            const body = document.body?.innerText || "";
            return ({selector_check}) && ({text_check}) && ({gone_check});
        }})()"#,
        selector_check = selector
            .map(|selector| {
                format!(
                    r#"(() => {{
                        const element = document.querySelector({});
                        if (!element) return false;
                        const style = getComputedStyle(element);
                        const rect = element.getBoundingClientRect();
                        return style.display !== "none" && style.visibility !== "hidden"
                            && Number(style.opacity) !== 0 && rect.width > 0 && rect.height > 0;
                    }})()"#,
                    json!(selector)
                )
            })
            .unwrap_or_else(|| "true".to_string()),
        text_check = text
            .map(|text| format!("body.includes({})", json!(text)))
            .unwrap_or_else(|| "true".to_string()),
        gone_check = text_gone
            .map(|text| format!("!body.includes({})", json!(text)))
            .unwrap_or_else(|| "true".to_string()),
    );
    let deadline = timeout_ms.max(100);
    let mut waited = 0u64;
    loop {
        let ready = session
            .evaluate(&expression)
            .await?
            .as_bool()
            .unwrap_or(false);
        if ready {
            return Ok(());
        }
        if waited >= deadline {
            return Err(anyhow!(
                "timed out after {deadline}ms waiting for the requested page state"
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        waited += 150;
    }
}

/// Map a named key to its DOM `code` and Windows virtual-key code (common keys).
fn key_meta(key: &str) -> (String, i64) {
    let known = match key {
        "Enter" => Some(("Enter", 13)),
        "Tab" => Some(("Tab", 9)),
        "Backspace" => Some(("Backspace", 8)),
        "Delete" => Some(("Delete", 46)),
        "Escape" => Some(("Escape", 27)),
        " " | "Space" => Some(("Space", 32)),
        "ArrowUp" => Some(("ArrowUp", 38)),
        "ArrowDown" => Some(("ArrowDown", 40)),
        "ArrowLeft" => Some(("ArrowLeft", 37)),
        "ArrowRight" => Some(("ArrowRight", 39)),
        "Home" => Some(("Home", 36)),
        "End" => Some(("End", 35)),
        "PageUp" => Some(("PageUp", 33)),
        "PageDown" => Some(("PageDown", 34)),
        "Control" => Some(("ControlLeft", 17)),
        "Shift" => Some(("ShiftLeft", 16)),
        "Alt" => Some(("AltLeft", 18)),
        "Meta" => Some(("MetaLeft", 91)),
        _ => None,
    };
    if let Some((code, vk)) = known {
        return (code.to_string(), vk);
    }
    if key.chars().count() == 1 {
        let ch = key.chars().next().unwrap();
        if ch.is_ascii_alphabetic() {
            let upper = ch.to_ascii_uppercase();
            return (format!("Key{upper}"), upper as i64);
        }
        if ch.is_ascii_digit() {
            return (format!("Digit{ch}"), ch as i64);
        }
    }
    (String::new(), 0)
}

fn normalize_modifier(value: &str) -> Option<&'static str> {
    match value.to_ascii_lowercase().as_str() {
        "control" | "ctrl" => Some("Control"),
        "shift" => Some("Shift"),
        "alt" | "option" => Some("Alt"),
        "meta" | "cmd" | "command" => Some("Meta"),
        _ => None,
    }
}

fn modifier_mask(value: &str) -> i64 {
    match value {
        "Alt" => 1,
        "Control" => 2,
        "Meta" => 4,
        "Shift" => 8,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_web_and_internal_urls() {
        assert_eq!(normalize_url("localhost:5173"), "http://localhost:5173");
        assert_eq!(normalize_url("example.com/path"), "http://example.com/path");
        assert_eq!(normalize_url("https://example.com"), "https://example.com");
        assert_eq!(normalize_url("about:blank"), "about:blank");
        assert_eq!(
            normalize_url("data:text/plain,hello"),
            "data:text/plain,hello"
        );
        assert_eq!(normalize_url(""), "about:blank");
    }

    #[test]
    fn session_restore_pref_merges_without_clobbering_chromes_own_state() {
        let dir = std::env::temp_dir().join(format!("threadknot-prefs-{}", Uuid::new_v4()));
        let default_dir = dir.join("Default");
        std::fs::create_dir_all(&default_dir).unwrap();
        std::fs::write(
            default_dir.join("Preferences"),
            r#"{"profile":{"name":"keep me"},"session":{"startup_urls":["https://x"]}}"#,
        )
        .unwrap();
        enable_session_restore(&dir);
        let prefs: Value = serde_json::from_str(
            &std::fs::read_to_string(default_dir.join("Preferences")).unwrap(),
        )
        .unwrap();
        assert_eq!(prefs["session"]["restore_on_startup"], 1);
        assert_eq!(prefs["profile"]["name"], "keep me");
        assert_eq!(prefs["session"]["startup_urls"][0], "https://x");

        // A brand-new profile has no Preferences yet; the file is created so
        // the very first restart already keeps session cookies.
        let fresh = std::env::temp_dir().join(format!("threadknot-prefs-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&fresh).unwrap();
        enable_session_restore(&fresh);
        let prefs: Value = serde_json::from_str(
            &std::fs::read_to_string(fresh.join("Default").join("Preferences")).unwrap(),
        )
        .unwrap();
        assert_eq!(prefs["session"]["restore_on_startup"], 1);
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&fresh).unwrap();
    }

    #[test]
    fn truncates_on_character_boundaries() {
        assert_eq!(truncate_text("abc", 3), "abc");
        assert_eq!(truncate_text("a🛟bc", 3), "a🛟b…");
    }

    #[test]
    fn maps_keyboard_chords_and_mouse_buttons() {
        assert_eq!(key_meta("Enter"), ("Enter".to_string(), 13));
        assert_eq!(key_meta("a"), ("KeyA".to_string(), 65));
        assert_eq!(normalize_modifier("cmd"), Some("Meta"));
        assert_eq!(modifier_mask("Control"), 2);
        assert_eq!(mouse_button_mask(&MouseButton::Left), 1);
        assert_eq!(mouse_button_mask(&MouseButton::Right), 2);
        assert_eq!(mouse_button_mask(&MouseButton::Middle), 4);

        let Control::Paste { text } = serde_json::from_value(json!({
            "type": "paste",
            "text": "first line\nsecond line 🧶",
        }))
        .expect("paste control should deserialize") else {
            panic!("paste control decoded as another variant");
        };
        assert_eq!(text, "first line\nsecond line 🧶");
    }

    /// The right-click hit-test the viewer sends before opening its context
    /// menu: point plus a nonce that ties the `context` reply back to the
    /// window that asked.
    #[test]
    fn probe_control_carries_point_and_nonce() {
        let Control::Probe { x, y, nonce } = serde_json::from_value(json!({
            "type": "probe",
            "x": 12.5,
            "y": 340.0,
            "nonce": "p7",
        }))
        .expect("probe control should deserialize") else {
            panic!("probe control decoded as another variant");
        };
        assert_eq!((x, y), (12.5, 340.0));
        assert_eq!(nonce, "p7");
    }

    /// Old machine processes understand `Control::Key` but not the dedicated
    /// paste control. Key events carrying individual characters are the
    /// compatibility path when a viewer updates before its machine restarts.
    #[tokio::test]
    #[ignore = "requires an installed Chrome/Chromium binary"]
    async fn live_chrome_key_text_accepts_a_paste_payload() {
        let registry = Arc::new(BrowserRegistry::default());
        let key = "browser-live-key-text-paste";
        let html = "<textarea id=q autofocus></textarea>";
        registry
            .invoke(
                key,
                "navigate",
                &json!({ "url": format!("data:text/html,{}", urlencoding::encode(html)) }),
            )
            .await
            .unwrap();
        let session = registry.get_or_spawn(key).await.unwrap();
        for character in "first line\nsecond line 🧶".chars() {
            let text = if character == '\n' {
                "\r".to_string()
            } else {
                character.to_string()
            };
            session
                .key(true, "", "", 0, Some(&text), 0)
                .await
                .unwrap();
        }
        let value = session
            .evaluate("document.querySelector('#q').value")
            .await
            .unwrap();
        assert_eq!(value, "first line\nsecond line 🧶");
    }

    #[test]
    fn target_prefixes_are_mapped_without_values() {
        let input = json!({
            "fromRef": "e3",
            "toSelector": "#drop",
            "value": "never-copy-me",
        });
        assert_eq!(prefixed_target(&input, "from")["ref"], "e3");
        assert_eq!(prefixed_target(&input, "to")["selector"], "#drop");
        assert!(prefixed_target(&input, "from").get("value").is_none());
    }

    #[test]
    fn activity_feed_hides_entered_values() {
        let secret = "this-must-not-reach-the-human-feed";
        for (op, input) in [
            ("fill", json!({ "ref": "e4", "value": secret })),
            ("type", json!({ "selector": "#password", "text": secret })),
            (
                "fill_form",
                json!({ "fields": [{ "ref": "e4", "value": secret }] }),
            ),
        ] {
            let (label, detail) = activity_description(op, &input);
            assert!(!label.contains(secret));
            assert!(!detail.unwrap_or_default().contains(secret));
        }
    }

    #[test]
    fn semantic_snapshot_keeps_useful_structure() {
        assert!(is_interactive_role("button"));
        assert!(is_interactive_role("textbox"));
        assert!(!is_interactive_role("statictext"));
        // Rows and cells stay: they are what makes repeated buttons tellable apart.
        assert!(is_structural_role("row"));
        assert!(is_structural_role("listitem"));
        assert!(matches!(
            classify_ax_node(false, "heading", "Checkout", "", ""),
            NodeAction::Emit
        ));
        assert!(matches!(
            classify_ax_node(false, "dialog", "", "", ""),
            NodeAction::Emit
        ));
    }

    #[test]
    fn snapshot_drops_duplicated_and_per_character_text() {
        // InlineTextBox turned "Skip to content" into one line per character.
        assert!(matches!(
            classify_ax_node(false, "inlinetextbox", "S", "", ""),
            NodeAction::Drop
        ));
        // Text already spoken by the enclosing button/link is pure duplication.
        assert!(matches!(
            classify_ax_node(false, "statictext", "Save", "", "Save changes"),
            NodeAction::Drop
        ));
        // Separators like " | " carry nothing.
        assert!(matches!(
            classify_ax_node(false, "statictext", " | ", "", ""),
            NodeAction::Drop
        ));
        // Real text survives.
        assert!(matches!(
            classify_ax_node(false, "statictext", "Total: $42", "", "Cart"),
            NodeAction::Emit
        ));
    }

    #[test]
    fn layout_wrappers_flatten_instead_of_hiding_their_children() {
        // Flatten, not Drop: the wrapper is meaningless but its contents are not.
        // Dropping these once hid every table cell on the page.
        for role in ["generic", "none", "layouttablecell", "section"] {
            assert!(matches!(
                classify_ax_node(false, role, "", "", ""),
                NodeAction::Flatten
            ));
        }
        assert!(matches!(
            classify_ax_node(true, "button", "Ignored", "", ""),
            NodeAction::Flatten
        ));
    }

    #[test]
    fn enter_and_tab_carry_the_text_that_triggers_their_default_action() {
        // Without `text`, Chrome fires keydown but never submits the form.
        assert_eq!(default_text_for_key("Enter"), Some("\r"));
        assert_eq!(default_text_for_key("Tab"), Some("\t"));
        assert_eq!(default_text_for_key("a"), Some("a"));
        assert_eq!(default_text_for_key("Escape"), None);
        assert_eq!(default_text_for_key("ArrowDown"), None);
    }

    #[test]
    fn stale_node_errors_become_an_actionable_instruction() {
        let raw = anyhow!("Error -32000: Node does not have a layout object");
        let explained = format!("{:#}", explain_action_error(raw));
        assert!(explained.contains("browser_snapshot"));
        let unrelated = anyhow!("navigation timed out");
        assert_eq!(
            format!("{:#}", explain_action_error(unrelated)),
            "navigation timed out"
        );
    }

    #[test]
    fn only_ascii_text_is_typed_key_by_key() {
        assert!(is_typeable_char('a'));
        assert!(is_typeable_char('\n'));
        assert!(!is_typeable_char('🛟'));
    }

    #[tokio::test]
    #[ignore = "requires an installed Chrome/Chromium binary"]
    async fn live_chrome_semantic_action_round_trip() {
        let registry = Arc::new(BrowserRegistry::default());
        let page = concat!(
            "data:text/html,",
            "<label>Name<input id=name></label>",
            "<label>Ready<input id=ready type=checkbox></label>",
            "<button id=save onclick=\"document.title=document.querySelector('input').value\">Save</button>"
        );
        registry
            .invoke("browser-live-test", "navigate", &json!({ "url": page }))
            .await
            .unwrap();

        let snapshot = registry
            .invoke("browser-live-test", "snapshot", &json!({}))
            .await
            .unwrap();
        let tree = snapshot["snapshot"].as_str().unwrap();
        assert!(tree.contains("[ref=e"));
        assert!(tree.contains("Name"));
        assert!(tree.contains("Save"));
        assert!(snapshot["screenshot"]
            .as_str()
            .is_some_and(|png| png.len() > 100));
        let element_ref = |name: &str| {
            tree.lines()
                .find(|line| line.contains(name) && line.contains("[ref="))
                .and_then(|line| line.split("[ref=").nth(1))
                .and_then(|rest| rest.split(']').next())
                .unwrap()
                .to_string()
        };
        let name_ref = element_ref("Name");
        let ready_ref = element_ref("Ready");
        let save_ref = element_ref("Save");

        registry
            .invoke(
                "browser-live-test",
                "fill",
                &json!({ "ref": name_ref, "value": "Ada" }),
            )
            .await
            .unwrap();
        registry
            .invoke(
                "browser-live-test",
                "check",
                &json!({ "ref": ready_ref, "checked": true }),
            )
            .await
            .unwrap();
        registry
            .invoke("browser-live-test", "click", &json!({ "ref": save_ref }))
            .await
            .unwrap();
        let state = registry
            .invoke(
                "browser-live-test",
                "evaluate",
                &json!({
                    "expression": "({title:document.title, ready:document.querySelector('#ready').checked})"
                }),
            )
            .await
            .unwrap();
        assert_eq!(state["result"]["title"], "Ada");
        assert_eq!(state["result"]["ready"], true);

        let before = registry
            .invoke("browser-live-test", "tabs", &json!({}))
            .await
            .unwrap();
        assert_eq!(before["tabs"].as_array().unwrap().len(), 1);
        let original = before["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tab| tab["active"] == true)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let opened = registry
            .invoke(
                "browser-live-test",
                "new_tab",
                &json!({ "url": "data:text/html,<title>Second</title><h1>Second tab</h1>" }),
            )
            .await
            .unwrap();
        let second = opened["tab"]["id"].as_str().unwrap().to_string();
        assert_eq!(opened["tab"]["title"], "Second");
        registry
            .invoke(
                "browser-live-test",
                "switch_tab",
                &json!({ "id": original }),
            )
            .await
            .unwrap();
        assert_eq!(
            registry
                .invoke("browser-live-test", "status", &json!({}))
                .await
                .unwrap()["title"],
            "Ada"
        );
        registry
            .invoke("browser-live-test", "close_tab", &json!({ "id": second }))
            .await
            .unwrap();
        let after = registry
            .invoke("browser-live-test", "tabs", &json!({}))
            .await
            .unwrap();
        assert_eq!(
            after["tabs"].as_array().unwrap().len(),
            before["tabs"].as_array().unwrap().len()
        );

        registry
            .invoke(
                "browser-live-test",
                "evaluate",
                &json!({
                    "expression": "(() => { const a=document.createElement('a'); a.id='popup'; a.target='_blank'; a.href='data:text/html;base64,PHRpdGxlPlBvcHVwPC90aXRsZT48aDE+UG9wdXA8L2gxPg=='; a.textContent='Popup'; document.body.append(a); return true; })()"
                }),
            )
            .await
            .unwrap();
        registry
            .invoke(
                "browser-live-test",
                "click",
                &json!({ "selector": "#popup" }),
            )
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let popup = registry
            .invoke("browser-live-test", "status", &json!({}))
            .await
            .unwrap();
        assert_eq!(
            popup["tabs"].as_array().unwrap().len(),
            before["tabs"].as_array().unwrap().len() + 1
        );
        let popup_active = popup["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tab| tab["active"] == true)
            .unwrap()["id"]
            .as_str()
            .unwrap();
        assert_ne!(popup_active, original);
    }

    /// The interactions that a browser tool has to get right to drive a real
    /// user flow, each of which silently failed before: pressing Enter to
    /// submit, reaching into an iframe, telling repeated buttons apart by their
    /// row, and saving visual evidence to disk.
    #[tokio::test]
    #[ignore = "requires an installed Chrome/Chromium binary"]
    async fn live_chrome_real_flow_capabilities() {
        let registry = Arc::new(BrowserRegistry::default());
        let key = "browser-live-flow";
        // Percent-encoded: a bare `#` in a data: URL would truncate the
        // document at the fragment.
        let frame = format!(
            "data:text/html,{}",
            urlencoding::encode(
                "<button id=inner>Inner button</button><input id=innerbox placeholder=inner>"
            )
        );
        let html = format!(
            concat!(
                "<form id=f onsubmit=\"document.title='submitted:'+document.getElementById('q').value;return false\">",
                "<input id=q name=q><button type=submit>Go</button></form>",
                "<table><tr><td>INV-001</td><td><button onclick=\"document.title='deleted 1'\">Delete</button></td></tr>",
                "<tr><td>INV-002</td><td><button onclick=\"document.title='deleted 2'\">Delete</button></td></tr></table>",
                "<iframe id=kid src=\"{frame}\"></iframe>",
            ),
            frame = frame
        );
        let page = format!("data:text/html,{}", urlencoding::encode(&html));
        registry
            .invoke(key, "navigate", &json!({ "url": page }))
            .await
            .unwrap();

        // 1. Typing and pressing Enter must actually submit the form. Chrome
        //    only performs the default action when the keydown carries text.
        registry
            .invoke(
                key,
                "type",
                &json!({ "selector": "#q", "text": "hello", "submit": true }),
            )
            .await
            .unwrap();
        let title = registry
            .invoke(key, "evaluate", &json!({ "expression": "document.title" }))
            .await
            .unwrap();
        assert_eq!(title["result"], "submitted:hello");

        // 2. Per-key typing must fire real key events, not just set a value.
        registry
            .invoke(
                key,
                "evaluate",
                &json!({ "expression": "(() => { window.__keys = 0; document.querySelector('#q').addEventListener('keydown', () => window.__keys++); return true; })()" }),
            )
            .await
            .unwrap();
        registry
            .invoke(key, "fill", &json!({ "selector": "#q", "value": "" }))
            .await
            .unwrap();
        registry
            .invoke(key, "type", &json!({ "selector": "#q", "text": "abc" }))
            .await
            .unwrap();
        let keys = registry
            .invoke(key, "evaluate", &json!({ "expression": "window.__keys" }))
            .await
            .unwrap();
        assert_eq!(keys["result"], 3);

        let snapshot = registry.invoke(key, "snapshot", &json!({})).await.unwrap();
        let tree = snapshot["snapshot"].as_str().unwrap();

        // 3. Iframe contents are part of the page: visible in the snapshot,
        //    addressable by ref, and reachable by selector.
        assert!(tree.contains("iframe"), "iframe missing from tree:\n{tree}");
        assert!(
            tree.contains("Inner button"),
            "iframe contents missing:\n{tree}"
        );
        assert!(snapshot["frameCount"].as_u64().unwrap() >= 2);
        registry
            .invoke(key, "click", &json!({ "selector": "#inner" }))
            .await
            .expect("selector must resolve inside a child frame");

        // 4. Structure is real, and it is what makes repeated controls safe to
        //    act on: pick the Delete button that follows INV-002 in the tree and
        //    confirm the page agrees it deleted invoice 2. A flat snapshot can
        //    only guess by ordinal, which is how agents delete the wrong row.
        assert!(
            tree.lines().any(|line| line.trim_start().starts_with("- row")),
            "table rows were flattened away:\n{tree}"
        );
        let indent_of = |needle: &str| {
            tree.lines()
                .find(|line| line.contains(needle))
                .map(|line| line.len() - line.trim_start().len())
                .unwrap()
        };
        assert!(
            indent_of("Delete") > indent_of("- row"),
            "buttons must nest under their row:\n{tree}"
        );
        let second_delete = tree
            .lines()
            .skip_while(|line| !line.contains("INV-002"))
            .find(|line| line.contains("[ref=") && line.contains("Delete"))
            .and_then(|line| line.split("[ref=").nth(1))
            .and_then(|rest| rest.split(']').next())
            .expect("a Delete button under the INV-002 row")
            .to_string();
        registry
            .invoke(key, "click", &json!({ "ref": second_delete }))
            .await
            .unwrap();
        let deleted = registry
            .invoke(key, "evaluate", &json!({ "expression": "document.title" }))
            .await
            .unwrap();
        assert_eq!(
            deleted["result"], "deleted 2",
            "row structure did not identify the right button:\n{tree}"
        );

        // 5. No per-character text spam.
        assert!(
            !tree.contains("InlineTextBox"),
            "snapshot still contains per-character nodes:\n{tree}"
        );

        // 6. A screenshot has to become a file before it can be evidence.
        let shot = registry
            .invoke(key, "screenshot", &json!({ "fullPage": true }))
            .await
            .unwrap();
        let path = std::path::PathBuf::from(shot["path"].as_str().unwrap());
        assert!(path.is_file());
        assert!(shot["sizeBytes"].as_u64().unwrap() > 1000);
        let _ = std::fs::remove_file(&path);

        // 7. Failed resource loads reach the console, not just the network log.
        registry
            .invoke(
                key,
                "evaluate",
                &json!({ "expression": "(() => { const i = document.createElement('img'); i.src = 'http://127.0.0.1:9/none.png'; document.body.append(i); return true; })()" }),
            )
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        let console = registry
            .invoke(key, "console", &json!({ "limit": 20 }))
            .await
            .unwrap();
        assert!(
            console["entries"].as_array().unwrap().iter().any(|entry| {
                entry["text"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("Failed to load resource")
                    && entry["url"].as_str().unwrap_or_default().contains("none.png")
            }),
            "resource failures must surface in the console: {console}"
        );

        // 8. Reloading invalidates refs, and the failure says what to do.
        let stale = tree
            .lines()
            .find(|line| line.contains("[ref="))
            .and_then(|line| line.split("[ref=").nth(1))
            .and_then(|rest| rest.split(']').next())
            .unwrap()
            .to_string();
        registry.invoke(key, "reload", &json!({})).await.unwrap();
        let error = registry
            .invoke(key, "click", &json!({ "ref": stale }))
            .await
            .unwrap_err();
        let error = format!("{error:#}");
        assert!(
            error.contains("browser_snapshot") && error.contains("stale"),
            "unhelpful stale-ref error: {error}"
        );
    }

    #[test]
    fn filled_field_value_is_not_repeated_as_child_text() {
        // A textbox carries its contents as `value`; Chrome also exposes them as
        // a StaticText child. Dedup considers the parent's name AND value.
        assert!(matches!(
            classify_ax_node(false, "statictext", "4242424242424242", "", "card number 4242424242424242"),
            NodeAction::Drop
        ));
    }

    /// The top-level document request is the first thing anyone looks at when a
    /// page loads wrong. Clearing the network log on navigation once deleted it.
    #[tokio::test]
    #[ignore = "requires an installed Chrome/Chromium binary"]
    async fn live_chrome_keeps_the_document_request_after_navigation() {
        let registry = Arc::new(BrowserRegistry::default());
        let key = "browser-live-network";
        registry
            .invoke(key, "navigate", &json!({ "url": "https://example.com" }))
            .await
            .unwrap();
        // A second navigation must not carry the first page's requests over.
        registry
            .invoke(key, "navigate", &json!({ "url": "https://example.org" }))
            .await
            .unwrap();
        let network = registry
            .invoke(key, "network", &json!({ "limit": 50 }))
            .await
            .unwrap();
        let entries = network["entries"].as_array().unwrap();
        assert!(
            entries.iter().any(|entry| {
                entry["type"] == "Document"
                    && entry["url"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("example.org")
            }),
            "the current document request must survive its own navigation: {network}"
        );
        assert!(
            !entries.iter().any(|entry| entry["url"]
                .as_str()
                .unwrap_or_default()
                .contains("example.com")),
            "the previous page's requests must not linger: {network}"
        );
    }

    /// A click that cannot reach its target must fail loudly. Before this
    /// check, a cookie banner over the button swallowed the click and the tool
    /// still reported success.
    #[tokio::test]
    #[ignore = "requires an installed Chrome/Chromium binary"]
    async fn live_chrome_refuses_clicks_an_overlay_would_swallow() {
        let registry = Arc::new(BrowserRegistry::default());
        let key = "browser-live-overlay";
        let covered = "<button id=real onclick=\"document.title='REAL BUTTON'\">Confirm</button>\
            <div id=cover style=\"position:fixed;inset:0;background:rgba(0,0,0,.5)\" \
            onclick=\"document.title='COOKIE BANNER ATE IT'\">cookie banner</div>";
        registry
            .invoke(key, "navigate", &json!({ "url": format!("data:text/html,{}", urlencoding::encode(covered)) }))
            .await
            .unwrap();
        let error = registry
            .invoke(key, "click", &json!({ "selector": "#real" }))
            .await
            .unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("cover"), "error should name the blocker: {error}");
        assert_eq!(
            registry
                .invoke(key, "evaluate", &json!({ "expression": "document.title" }))
                .await
                .unwrap()["result"],
            "",
            "the swallowed click must not have been dispatched at all"
        );
        // Coordinates are an explicit override, not a mistake.
        registry
            .invoke(key, "click", &json!({ "x": 40, "y": 18 }))
            .await
            .unwrap();

        // And the check must not fire on ordinary markup: a button whose centre
        // is its own child span, and a label wrapping its input.
        let normal = "<button id=b><span>Save <b>now</b></span></button>\
            <label id=l>Agree <input id=cb type=checkbox></label>\
            <script>document.getElementById('b').onclick=()=>document.title='saved'</script>";
        registry
            .invoke(key, "navigate", &json!({ "url": format!("data:text/html,{}", urlencoding::encode(normal)) }))
            .await
            .unwrap();
        registry
            .invoke(key, "click", &json!({ "selector": "#b" }))
            .await
            .expect("a button with child markup must still be clickable");
        registry
            .invoke(key, "click", &json!({ "selector": "#cb" }))
            .await
            .expect("an input inside its label must still be clickable");
        assert_eq!(
            registry
                .invoke(key, "evaluate", &json!({ "expression": "document.title" }))
                .await
                .unwrap()["result"],
            "saved"
        );
    }

    /// The registry is driven from axum handlers, which require Send futures.
    /// A std::sync guard accidentally held across an await breaks that with an
    /// error that points at the route, not the cause — so assert it here.
    #[allow(dead_code)]
    fn registry_futures_stay_send(registry: Arc<BrowserRegistry>) {
        fn is_send<T: Send>(_: T) {}
        is_send(async move {
            let _ = registry.get_or_spawn("k").await;
        });
        is_send(async move {});
    }

    /// The point of a signed-in profile: a cookie set in one session is still
    /// there in the next one, after Chrome has fully exited and relaunched.
    #[tokio::test]
    #[ignore = "requires an installed Chrome/Chromium binary"]
    async fn live_chrome_signed_in_profile_survives_a_restart() {
        let root = std::env::temp_dir().join(format!("threadknot-profile-live-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let profiles =
            crate::browser_profiles::BrowserProfileStore::open(&root, "machine-live").unwrap();
        let created = profiles
            .create("Fixture", &["http://127.0.0.1:8799".into()])
            .unwrap();
        let spec = profiles.spec(&created.id).unwrap();

        let registry = Arc::new(BrowserRegistry::default());
        {
            let profiles_for_resolver = spec.clone();
            registry.set_profile_resolver(Arc::new(move |_key: &str| {
                Ok(Some(profiles_for_resolver.clone()))
            }));
        }

        let server = tiny_http_fixture(8799);
        let key = "profile-persistence";
        registry
            .invoke(key, "navigate", &json!({ "url": "http://127.0.0.1:8799/" }))
            .await
            .unwrap();
        registry
            .invoke(
                key,
                "click",
                &json!({ "selector": "#setcookie" }),
            )
            .await
            .unwrap();

        // Close the browser the way the app does, then reopen the same profile.
        registry.remove(key);
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

        registry
            .invoke(key, "navigate", &json!({ "url": "http://127.0.0.1:8799/" }))
            .await
            .unwrap();
        let snapshot = registry.invoke(key, "snapshot", &json!({})).await.unwrap();
        let tree = snapshot["snapshot"].as_str().unwrap();
        assert!(
            tree.contains("signed in as tester"),
            "the stored session did not survive the restart:\n{tree}"
        );

        registry.remove(key);
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        drop(server);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A signed-in browser must not be able to leave its sites — not by tool
    /// call, and not by following a link on the page.
    #[tokio::test]
    #[ignore = "requires an installed Chrome/Chromium binary"]
    async fn live_chrome_signed_in_profile_cannot_leave_its_sites() {
        let root = std::env::temp_dir().join(format!("threadknot-profile-scope-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let profiles =
            crate::browser_profiles::BrowserProfileStore::open(&root, "machine-live").unwrap();
        let created = profiles
            .create("Fixture", &["http://127.0.0.1:8798".into()])
            .unwrap();
        let spec = profiles.spec(&created.id).unwrap();
        let registry = Arc::new(BrowserRegistry::default());
        {
            let spec = spec.clone();
            registry.set_profile_resolver(Arc::new(move |_key: &str| Ok(Some(spec.clone()))));
        }

        let server = tiny_http_fixture(8798);
        let key = "profile-scope";
        registry
            .invoke(key, "navigate", &json!({ "url": "http://127.0.0.1:8798/" }))
            .await
            .unwrap();

        // 1. The tool refuses outright, with a reason.
        let refused = registry
            .invoke(key, "navigate", &json!({ "url": "http://example.com/" }))
            .await
            .unwrap_err()
            .to_string();
        assert!(refused.contains("signed in"), "{refused}");

        // 2. Following a link on the page cannot escape either — the browser
        //    itself blocks the load, so the URL never becomes the current page.
        registry
            .invoke(key, "click", &json!({ "selector": "#offsite" }))
            .await
            .ok();
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        let status = registry.invoke(key, "status", &json!({})).await.unwrap();
        let url = status["url"].as_str().unwrap_or_default();
        assert!(
            !url.contains("example.com"),
            "a signed-in browser followed a link off its sites: {url}"
        );
        assert_eq!(status["signedInProfile"], "Fixture");

        // 3. The escape hatch is closed while signed in.
        let evaluated = registry
            .invoke(key, "evaluate", &json!({ "expression": "1" }))
            .await
            .unwrap_err()
            .to_string();
        assert!(evaluated.contains("disabled"), "{evaluated}");

        registry.remove(key);
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        drop(server);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// One Chrome per profile directory: the second chat asking for the same
    /// signed-in profile is told who holds it instead of hanging on a lock.
    #[tokio::test]
    #[ignore = "requires an installed Chrome/Chromium binary"]
    async fn live_chrome_one_chat_at_a_time_per_signed_in_profile() {
        let root = std::env::temp_dir().join(format!("threadknot-profile-lease-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let profiles =
            crate::browser_profiles::BrowserProfileStore::open(&root, "machine-live").unwrap();
        let created = profiles.create("Fixture", &["example.com".into()]).unwrap();
        let spec = profiles.spec(&created.id).unwrap();
        let registry = Arc::new(BrowserRegistry::default());
        {
            let spec = spec.clone();
            registry.set_profile_resolver(Arc::new(move |_key: &str| Ok(Some(spec.clone()))));
        }

        registry
            .invoke("chat-one", "status", &json!({}))
            .await
            .unwrap();
        registry
            .invoke("chat-one", "navigate", &json!({ "url": "about:blank" }))
            .await
            .unwrap();
        let conflict = registry
            .invoke("chat-two", "navigate", &json!({ "url": "about:blank" }))
            .await
            .unwrap_err()
            .to_string();
        assert!(conflict.contains("already open in another chat"), "{conflict}");

        registry.remove("chat-one");
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Minimal fixture server: sets a cookie on click, renders whether that
    /// cookie came back, and offers an off-site link. Killed on drop.
    struct FixtureServer(std::process::Child);

    impl Drop for FixtureServer {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn tiny_http_fixture(port: u16) -> FixtureServer {
        let script = format!(
            r#"
import http.server
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        cookie = self.headers.get('Cookie') or ''
        who = 'signed in as tester' if 'who=tester' in cookie else 'signed out'
        body = ('<!doctype html><meta charset=utf-8><title>Fixture</title>'
                '<h1>' + who + '</h1>'
                '<button id=setcookie onclick="document.cookie=\'who=tester;max-age=86400;path=/\';location.reload()">Sign in</button>'
                '<a id=offsite href="http://example.com/">Go off-site</a>').encode()
        self.send_response(200)
        self.send_header('Content-Type','text/html')
        self.send_header('Content-Length',str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a): pass
http.server.HTTPServer(('127.0.0.1', {port}), H).serve_forever()
"#
        );
        let child = std::process::Command::new("python3")
            .arg("-c")
            .arg(script)
            .spawn()
            .expect("python3 for the browser fixture");
        std::thread::sleep(std::time::Duration::from_millis(700));
        FixtureServer(child)
    }


    #[test]
    fn observation_tools_do_not_launch_a_browser() {
        let registry = Arc::new(BrowserRegistry::default());
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(registry.invoke("never-started", "status", &json!({})))
            .unwrap();
        assert_eq!(result["browser"], "not started");
        assert!(registry.sessions.lock().unwrap().is_empty());
    }

    #[test]
    fn orphan_detection_reads_the_parent_pid_after_the_last_paren() {
        // comm can itself contain spaces and parens, so the split must be on the
        // LAST ')' — getting this wrong would misread the ppid and kill a
        // browser belonging to a running Threadknot.
        let stat = "4242 (chrome (headless)) S 1 4242 4242 0 -1 4194304";
        let ppid = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().nth(1))
            .and_then(|value| value.parse::<i32>().ok());
        assert_eq!(ppid, Some(1));

        let child_of_threadknot = "4243 (chrome) S 99001 4243 4243 0 -1 4194304";
        let ppid = child_of_threadknot
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().nth(1))
            .and_then(|value| value.parse::<i32>().ok());
        assert_eq!(ppid, Some(99001));
    }

    #[test]
    fn profile_sweep_skips_directories_a_live_chrome_is_using() {
        let root = std::env::temp_dir().join(format!("{PROFILE_PREFIX}sweep-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("marker"), b"x").unwrap();
        assert!(dir_size(&root) > 0);

        // This test process' own command line contains the profile path, which
        // is exactly the shape the sweep must recognize as "in use". Chrome does
        // not always NUL-separate argv in /proc, so detection scans for the flag
        // anywhere in the command line rather than matching an argument prefix.
        let cmdline = format!(
            "/opt/google/chrome/chrome --headless=new --user-data-dir={} --hide-scrollbars",
            root.display()
        );
        let mut found = None;
        for (index, _) in cmdline.match_indices("--user-data-dir=") {
            found = cmdline[index + "--user-data-dir=".len()..]
                .split(char::is_whitespace)
                .next()
                .map(str::to_string);
        }
        assert_eq!(found.as_deref(), Some(root.to_string_lossy().as_ref()));

        // Freshly created, so even without /proc evidence the age guard holds.
        sweep_orphan_profiles();
        std::fs::remove_dir_all(&root).unwrap();
    }
}
