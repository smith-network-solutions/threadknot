//! In-process MCP server exposing Threadknot capabilities to the driven agents:
//! the unified browser (`browser.rs`, `browser_*` tools) and explicit
//! deliverable registration (`publish_artifact` → `AgentHub::publish_artifact`).
//!
//! Mirrors t3code's "preview automation": a streamable-HTTP MCP endpoint
//! (`/mcp`), authenticated by a per-thread bearer token.
//! The token identifies the thread, and the thread id is the browser session
//! key — so an agent drives the SAME Chrome the human sees in that thread's
//! Browser pane (unified). Each provider driver is launched with an MCP config
//! pointing here (see `claude.rs` / `codex.rs`).
//!
//! Transport: JSON responses (no SSE) — our tools are request/response with no
//! server-initiated messages, which streamable-HTTP explicitly permits.

use crate::server::ServerState;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

/// Per-thread MCP credentials + the endpoint agents connect back to.
pub struct McpRegistry {
    /// bearer token -> thread id (the browser session key).
    tokens: Mutex<HashMap<String, String>>,
    port: u16,
}

impl McpRegistry {
    pub fn new(port: u16) -> Self {
        Self {
            tokens: Mutex::new(HashMap::new()),
            port,
        }
    }

    /// The loopback URL the agent CLIs POST MCP requests to.
    pub fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.port)
    }

    /// Issue (or re-issue) a token for `thread_id`, dropping any prior token for
    /// the same thread so stale creds don't accumulate. Returns the new token.
    pub fn mint(&self, thread_id: &str) -> String {
        let token = crate::store::generate_token();
        let mut map = self.tokens.lock().unwrap();
        map.retain(|_, tid| tid != thread_id);
        map.insert(token.clone(), thread_id.to_string());
        token
    }

    /// Resolve a bearer token to its thread id.
    fn resolve(&self, token: &str) -> Option<String> {
        self.tokens.lock().unwrap().get(token).cloned()
    }

    /// Revoke all credentials for a thread (on teardown).
    pub fn revoke_thread(&self, thread_id: &str) {
        self.tokens
            .lock()
            .unwrap()
            .retain(|_, tid| tid != thread_id);
    }
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    let raw = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    raw.strip_prefix("Bearer ").map(|t| t.trim().to_string())
}

/// JSON-RPC 2.0 error envelope response.
fn err(id: Value, code: i64, message: &str) -> Response {
    axum::Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }))
        .into_response()
}

/// `GET|POST /mcp` — streamable-HTTP MCP endpoint (bearer-gated per thread).
pub async fn mcp_handler(
    State(state): State<ServerState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(token) = bearer(&headers) else {
        return (StatusCode::UNAUTHORIZED, "missing bearer token").into_response();
    };
    let Some(thread_id) = state.hub.mcp.resolve(&token) else {
        return (StatusCode::UNAUTHORIZED, "unknown token").into_response();
    };
    // We don't offer a server->client SSE stream; GET has nothing to deliver.
    if method == Method::GET {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }

    let Ok(req) = serde_json::from_slice::<Value>(&body) else {
        return err(Value::Null, -32700, "parse error");
    };

    // A batch is an array of messages; single is an object.
    if let Some(arr) = req.as_array() {
        let mut out = Vec::new();
        for msg in arr {
            if let Some(resp) = handle_message(&state, &thread_id, msg).await {
                out.push(resp);
            }
        }
        return if out.is_empty() {
            StatusCode::ACCEPTED.into_response()
        } else {
            axum::Json(Value::Array(out)).into_response()
        };
    }

    match handle_message(&state, &thread_id, &req).await {
        Some(resp) => axum::Json(resp).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// Handle one JSON-RPC message; `None` for notifications (no reply).
async fn handle_message(state: &ServerState, thread_id: &str, msg: &Value) -> Option<Value> {
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = msg.get("id").cloned();

    // Notifications carry no id and expect no response.
    if id.is_none() || method.starts_with("notifications/") {
        return None;
    }
    let id = id.unwrap();

    let result = match method {
        "initialize" => {
            let protocol = msg
                .get("params")
                .and_then(|p| p.get("protocolVersion"))
                .and_then(|v| v.as_str())
                .unwrap_or("2025-06-18")
                .to_string();
            json!({
                "protocolVersion": protocol,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "threadknot-browser", "version": env!("CARGO_PKG_VERSION") },
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tool_specs() }),
        "tools/call" => {
            let params = msg.get("params").cloned().unwrap_or(json!({}));
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            return Some(call_tool(&json_id(&id), state, thread_id, name, &args).await);
        }
        _ => return Some(json_err(&id, -32601, "method not found")),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn json_id(id: &Value) -> Value {
    id.clone()
}
fn json_err(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// MCP `tools/call` text-content result envelope.
fn tool_text_result(id: &Value, text: String, is_error: bool) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{ "type": "text", "text": text }],
            "isError": is_error,
        }
    })
}

/// `publish_artifact` — the agent explicitly registers a file it produced as a
/// deliverable for the user. This is the primary artifacts channel; the
/// turn-diff detector is only a fallback (see `artifacts.rs`).
fn publish_artifact(state: &ServerState, thread_id: &str, args: &Value) -> Result<String, String> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| "missing required argument: path".to_string())?;
    let title = args.get("title").and_then(|v| v.as_str());
    let description = args.get("description").and_then(|v| v.as_str());
    let record = state
        .hub
        .publish_artifact(thread_id, path, title, description)
        .map_err(|e| format!("{e:#}"))?;
    Ok(json!({
        "ok": true,
        "artifactId": record.id,
        "name": record.name,
        "sizeBytes": record.size_bytes,
        "note": "Published — the file now appears as an artifact card in the chat and in the project's Artifacts tab.",
    })
    .to_string())
}

/// Map a tool call to its handler, returning an MCP `tools/call` result
/// (content blocks; browser snapshot carries a PNG image block).
async fn call_tool(
    id: &Value,
    state: &ServerState,
    thread_id: &str,
    name: &str,
    args: &Value,
) -> Value {
    if name == "publish_artifact" {
        return match publish_artifact(state, thread_id, args) {
            Ok(text) => tool_text_result(id, text, false),
            Err(e) => tool_text_result(id, format!("error: {e}"), true),
        };
    }
    let op = name.strip_prefix("browser_").unwrap_or(name);
    let args = match normalized_browser_args(state, thread_id, op, args) {
        Ok(args) => args,
        Err(error) => return tool_text_result(id, format!("error: {error}"), true),
    };
    match state.browsers.invoke(thread_id, op, &args).await {
        Ok(mut result) => {
            let content = if op == "snapshot" {
                let shot = result
                    .as_object_mut()
                    .and_then(|o| o.remove("screenshot"))
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default();
                let snapshot_text = semantic_snapshot_text(&mut result);
                json!([
                    { "type": "text", "text": snapshot_text },
                    { "type": "image", "data": shot, "mimeType": "image/png" },
                ])
            } else {
                let page = result
                    .as_object_mut()
                    .and_then(|object| object.remove("page"));
                let text = if let Some(mut page) = page {
                    format!(
                        "{}\n\nPage after action:\n{}",
                        result,
                        semantic_snapshot_text(&mut page)
                    )
                } else {
                    result.to_string()
                };
                json!([{ "type": "text", "text": text }])
            };
            json!({ "jsonrpc": "2.0", "id": id, "result": { "content": content, "isError": false } })
        }
        Err(e) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{ "type": "text", "text": format!("error: {e:#}") }],
                "isError": true,
            }
        }),
    }
}

fn semantic_snapshot_text(snapshot: &mut Value) -> String {
    let tree = snapshot
        .as_object_mut()
        .and_then(|object| object.remove("snapshot"))
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default();
    let mut header = format!(
        "URL: {}\nTitle: {}\nInteractive elements: {}",
        snapshot.get("url").and_then(Value::as_str).unwrap_or(""),
        snapshot.get("title").and_then(Value::as_str).unwrap_or(""),
        snapshot
            .get("elementCount")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    );
    if let Some(frames) = snapshot.get("frameCount").and_then(Value::as_u64) {
        if frames > 1 {
            header.push_str(&format!("\nFrames: {frames} (iframe contents included)"));
        }
    }
    // Whether more page exists below the fold, without a screenshot round-trip.
    if let Some(scroll) = snapshot.get("scroll").filter(|value| value.is_object()) {
        let number = |key: &str| scroll.get(key).and_then(Value::as_f64).unwrap_or(0.0) as i64;
        header.push_str(&format!(
            "\nScroll: y={} of {} (viewport {}px){}",
            number("y"),
            number("pageHeight"),
            number("viewportHeight"),
            if scroll.get("atBottom").and_then(Value::as_bool) == Some(true) {
                " — at bottom"
            } else {
                ""
            }
        ));
    }
    format!("{header}\n\n{tree}")
}

/// File upload is the one browser action that can move bytes from the host
/// filesystem into an untrusted page. Restrict it to the current project root;
/// this preserves full project workflows without turning a web prompt
/// injection into an arbitrary home-directory exfiltration primitive.
fn normalized_browser_args(
    state: &ServerState,
    thread_id: &str,
    op: &str,
    args: &Value,
) -> Result<Value, String> {
    if op == "screenshot" {
        return normalized_screenshot_args(state, thread_id, args);
    }
    if op != "upload" {
        return Ok(args.clone());
    }
    let thread = state
        .hub
        .store
        .thread(thread_id)
        .ok_or_else(|| "unknown thread".to_string())?;
    let project = state
        .hub
        .store
        .project(&thread.project_id)
        .ok_or_else(|| "unknown project".to_string())?;
    let root = std::fs::canonicalize(&project.path)
        .map_err(|error| format!("project root is unavailable: {error}"))?;
    let paths = args
        .get("paths")
        .and_then(Value::as_array)
        .ok_or_else(|| "upload needs paths".to_string())?;
    let mut resolved = Vec::new();
    for value in paths {
        let raw = value
            .as_str()
            .ok_or_else(|| "every upload path must be a string".to_string())?;
        let candidate = {
            let path = std::path::Path::new(raw);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                root.join(path)
            }
        };
        let canonical = std::fs::canonicalize(&candidate)
            .map_err(|error| format!("upload file {raw:?} is unavailable: {error}"))?;
        if !canonical.starts_with(&root) || !canonical.is_file() {
            return Err(format!(
                "upload file {raw:?} must be an existing file inside the current project"
            ));
        }
        resolved.push(canonical.to_string_lossy().into_owned());
    }
    if resolved.is_empty() {
        return Err("upload needs at least one file".to_string());
    }
    let mut normalized = args.clone();
    normalized["paths"] = json!(resolved);
    Ok(normalized)
}

/// Screenshots write to disk, so the destination gets the same treatment as
/// upload's source: an explicit path must stay inside the project. Omitting the
/// path is the safe default — the session's own directory.
fn normalized_screenshot_args(
    state: &ServerState,
    thread_id: &str,
    args: &Value,
) -> Result<Value, String> {
    let Some(raw) = args.get("path").and_then(Value::as_str) else {
        return Ok(args.clone());
    };
    let thread = state
        .hub
        .store
        .thread(thread_id)
        .ok_or_else(|| "unknown thread".to_string())?;
    let project = state
        .hub
        .store
        .project(&thread.project_id)
        .ok_or_else(|| "unknown project".to_string())?;
    let root = std::fs::canonicalize(&project.path)
        .map_err(|error| format!("project root is unavailable: {error}"))?;
    let candidate = {
        let path = std::path::Path::new(raw);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        }
    };
    // The file does not exist yet, so canonicalize its parent instead.
    let parent = candidate
        .parent()
        .ok_or_else(|| format!("screenshot path {raw:?} has no directory"))?;
    let parent = std::fs::canonicalize(parent)
        .map_err(|error| format!("screenshot directory for {raw:?} is unavailable: {error}"))?;
    if !parent.starts_with(&root) {
        return Err(format!(
            "screenshot path {raw:?} must be inside the current project (omit path to use the browser session directory)"
        ));
    }
    let name = candidate
        .file_name()
        .ok_or_else(|| format!("screenshot path {raw:?} needs a file name"))?;
    let mut normalized = args.clone();
    normalized["path"] = json!(parent.join(name).to_string_lossy());
    Ok(normalized)
}

/// Input schema for `browser_record_flow`.
///
/// Built here rather than inline in [`tool_specs`]: the storyboard is nested
/// deeply enough that folding it into that one big `json!` blows serde_json's
/// macro recursion limit for the whole catalog.
fn record_flow_schema() -> Value {
    let step = json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["navigate", "click", "type", "fill", "hover", "focus", "scroll", "wait"],
                "description": "What this step does. `focus` draws attention to an element without interacting with it — pair it with zoom/blur to feature a price, chart or status."
            },
            "caption": { "type": "string", "description": "Scribe-style instruction shown on screen for this step, e.g. \"Click Sign in\". Omit for steps needing no narration." },
            "url": { "type": "string", "description": "navigate: destination" },
            "ref": { "type": "string", "description": "Element ref from the latest snapshot (click/type/fill/hover)" },
            "selector": { "type": "string", "description": "CSS selector, as an alternative to ref" },
            "text": { "type": "string", "description": "type/fill: the text to enter" },
            "submit": { "type": "boolean", "description": "type/fill: press Enter afterwards" },
            "dx": { "type": "number", "description": "scroll: horizontal distance" },
            "dy": { "type": "number", "description": "scroll: vertical distance" },
            "ms": { "type": "number", "description": "wait: how long to hold" },
            "spotlight": { "type": "boolean", "description": "Dim the page and ring this element while the step runs" },
            "blur": { "type": "boolean", "description": "Blur everything except this element — depth-of-field focus. Combines well with zoom to feature a detail." },
            "zoom": { "type": "number", "description": "Ease in to this page scale (1.0-3.0) centred on the target, then back out. Good for small UI; overuse is disorienting." },
            "moveMs": { "type": "number", "description": "Override pointer travel time for this step" },
            "dwellMs": { "type": "number", "description": "Override the pause after this step" },
            "keyMs": { "type": "number", "description": "Override typing speed for this step" },
            "readMs": { "type": "number", "description": "Override the caption read beat for this step" }
        },
        "required": ["action"]
    });
    let defaults = json!({
        "type": "object",
        "description": "Pacing applied to every step unless the step overrides it.",
        "properties": {
            "moveMs": { "type": "number", "description": "Pointer travel time to a target (default 620)" },
            "dwellMs": { "type": "number", "description": "Pause after each action so the change registers (default 900)" },
            "keyMs": { "type": "number", "description": "Mean per-keystroke delay when typing; jittered automatically (default 55)" },
            "readMs": { "type": "number", "description": "Beat between a caption appearing and its action running (default 700)" }
        }
    });
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "Destination .mp4 (absolute, or relative to this session's browser directory). Defaults to a timestamped file." },
            "defaults": defaults,
            "steps": {
                "type": "array",
                "description": "The storyboard, played in order.",
                "items": step
            }
        },
        "required": ["steps"]
    })
}

/// Tool catalog advertised via `tools/list`. Names are `browser_*` so agents see
/// them as `mcp__threadknot-browser__browser_*`.
fn tool_specs() -> Value {
    let obj = |props: Value, required: Value| json!({ "type": "object", "properties": props, "required": required });
    json!([
        {
            "name": "publish_artifact",
            "description": "Publish a file you produced as a deliverable for the user. Call this once per file, right after creating it, for every file the user asked you to produce or that constitutes your output (reports, exported data, generated documents, images, archives). The file appears as an openable/downloadable artifact card in the chat. Do NOT publish source-code edits, config changes, intermediate/scratch files, or files the user provided to you.",
            "inputSchema": obj(json!({
                "path": { "type": "string", "description": "Path to the file (absolute, or relative to the working directory)" },
                "title": { "type": "string", "description": "Optional short display title (defaults to the file name)" },
                "description": { "type": "string", "description": "Optional one-line summary of what the deliverable contains" }
            }), json!(["path"])),
        },
        {
            "name": "browser_navigate",
            "description": "Navigate the shared browser to a URL and wait briefly for it to settle. The user sees this page and your actions live in Threadknot.",
            "inputSchema": obj(json!({
                "url": { "type": "string", "description": "URL to open; bare localhost and domain names are accepted" },
                "includeSnapshot": { "type": "boolean", "description": "Also return a fresh semantic page snapshot" }
            }), json!(["url"])),
        },
        {
            "name": "browser_back",
            "description": "Go back one entry in the browser history.",
            "inputSchema": obj(json!({
                "includeSnapshot": { "type": "boolean", "description": "Also return a fresh semantic page snapshot" }
            }), json!([])),
        },
        {
            "name": "browser_forward",
            "description": "Go forward one entry in the browser history.",
            "inputSchema": obj(json!({
                "includeSnapshot": { "type": "boolean", "description": "Also return a fresh semantic page snapshot" }
            }), json!([])),
        },
        {
            "name": "browser_reload",
            "description": "Reload the current page.",
            "inputSchema": obj(json!({
                "includeSnapshot": { "type": "boolean", "description": "Also return a fresh semantic page snapshot" }
            }), json!([])),
        },
        {
            "name": "browser_click",
            "description": "Click a page element. Prefer a ref from the latest browser_snapshot; use a CSS selector or viewport coordinates only as fallbacks.",
            "inputSchema": obj(json!({
                "ref": { "type": "string", "description": "Stable element ref such as e12 from the latest snapshot" },
                "selector": { "type": "string", "description": "CSS selector of the element to click" },
                "x": { "type": "number" },
                "y": { "type": "number" },
                "button": { "type": "string", "enum": ["left", "right", "middle"], "description": "Mouse button (default left)" },
                "doubleClick": { "type": "boolean" },
                "includeSnapshot": { "type": "boolean", "description": "Also return a fresh semantic page snapshot" }
            }), json!([])),
        },
        {
            "name": "browser_hover",
            "description": "Move the visible agent cursor over an element without clicking. Prefer a ref from the latest snapshot.",
            "inputSchema": obj(json!({
                "ref": { "type": "string", "description": "Stable element ref from the latest snapshot" },
                "selector": { "type": "string" },
                "x": { "type": "number" },
                "y": { "type": "number" },
                "includeSnapshot": { "type": "boolean", "description": "Also return a fresh semantic page snapshot" }
            }), json!([])),
        },
        {
            "name": "browser_drag",
            "description": "Drag between two elements or viewport points. Prefer refs from the latest snapshot; the user sees the complete pointer path.",
            "inputSchema": obj(json!({
                "fromRef": { "type": "string" },
                "fromSelector": { "type": "string" },
                "fromX": { "type": "number" },
                "fromY": { "type": "number" },
                "toRef": { "type": "string" },
                "toSelector": { "type": "string" },
                "toX": { "type": "number" },
                "toY": { "type": "number" },
                "includeSnapshot": { "type": "boolean", "description": "Also return a fresh semantic page snapshot" }
            }), json!([])),
        },
        {
            "name": "browser_fill",
            "description": "Clear and replace the value of an input or textarea. Prefer a ref from the latest snapshot. The entered value is never shown in the live activity feed.",
            "inputSchema": obj(json!({
                "ref": { "type": "string", "description": "Stable element ref from the latest snapshot" },
                "selector": { "type": "string" },
                "value": { "type": "string" },
                "includeSnapshot": { "type": "boolean", "description": "Also return a fresh semantic page snapshot" }
            }), json!(["value"])),
        },
        {
            "name": "browser_select",
            "description": "Choose an option in a select element by option value or visible label. Prefer a ref from the latest snapshot.",
            "inputSchema": obj(json!({
                "ref": { "type": "string", "description": "Stable element ref from the latest snapshot" },
                "selector": { "type": "string" },
                "value": { "type": "string", "description": "Option value or visible label" },
                "includeSnapshot": { "type": "boolean", "description": "Also return a fresh semantic page snapshot" }
            }), json!(["value"])),
        },
        {
            "name": "browser_fill_form",
            "description": "Fill several form controls in one ordered action. Values stay hidden from the live activity feed. Prefer refs from the latest snapshot.",
            "inputSchema": obj(json!({
                "fields": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "ref": { "type": "string", "description": "Stable element ref from the latest snapshot" },
                            "selector": { "type": "string" },
                            "kind": { "type": "string", "enum": ["fill", "select", "check"], "description": "Control operation (default fill)" },
                            "value": { "type": "string", "description": "Required for fill and select" },
                            "checked": { "type": "boolean", "description": "Desired state for check" }
                        }
                    }
                },
                "includeSnapshot": { "type": "boolean", "description": "Also return a fresh semantic page snapshot" }
            }), json!(["fields"])),
        },
        {
            "name": "browser_check",
            "description": "Set a checkbox or radio control to an exact checked state. Prefer a ref from the latest snapshot.",
            "inputSchema": obj(json!({
                "ref": { "type": "string", "description": "Stable element ref from the latest snapshot" },
                "selector": { "type": "string" },
                "checked": { "type": "boolean", "description": "Desired state (default true)" },
                "includeSnapshot": { "type": "boolean", "description": "Also return a fresh semantic page snapshot" }
            }), json!([])),
        },
        {
            "name": "browser_type",
            "description": "Type text with real key events, one key at a time, so search-as-you-type, hotkeys, and editors react exactly as they do for a human. Use browser_fill when replacing an existing value. The typed text is never shown in the live activity feed.",
            "inputSchema": obj(json!({
                "text": { "type": "string" },
                "ref": { "type": "string", "description": "Optional stable element ref from the latest snapshot" },
                "selector": { "type": "string", "description": "Optional element to focus first" },
                "submit": { "type": "boolean", "description": "Press Enter after typing" },
                "fast": { "type": "boolean", "description": "Insert the whole string at once without per-key events (faster, but pages listening for keydown will not react)" },
                "includeSnapshot": { "type": "boolean", "description": "Also return a fresh semantic page snapshot" }
            }), json!(["text"])),
        },
        {
            "name": "browser_press",
            "description": "Press a key or chord such as Enter, Tab, Escape, ArrowDown, Control+A, or Meta+L.",
            "inputSchema": obj(json!({
                "key": { "type": "string" },
                "includeSnapshot": { "type": "boolean", "description": "Also return a fresh semantic page snapshot" }
            }), json!(["key"])),
        },
        {
            "name": "browser_scroll",
            "description": "Scroll the page by a wheel delta at an optional point. Returns the new scroll position, page height, and whether the bottom is reached.",
            "inputSchema": obj(json!({
                "x": { "type": "number" }, "y": { "type": "number" },
                "dx": { "type": "number" }, "dy": { "type": "number", "description": "Vertical scroll amount (default 400)" },
                "includeSnapshot": { "type": "boolean", "description": "Also return a fresh semantic page snapshot" }
            }), json!([])),
        },
        {
            "name": "browser_resize",
            "description": "Resize the shared browser viewport — use it to check responsive layouts or reproduce a mobile-only bug. The user's pane shows the same size.",
            "inputSchema": obj(json!({
                "width": { "type": "number", "description": "Viewport width in CSS pixels" },
                "height": { "type": "number", "description": "Viewport height in CSS pixels" },
                "includeSnapshot": { "type": "boolean", "description": "Also return a fresh semantic page snapshot" }
            }), json!(["width", "height"])),
        },
        {
            "name": "browser_screenshot",
            "description": "Save a PNG of the page to disk and return its path — the way to capture visual evidence. Defaults to the viewport; set fullPage for the whole scrollable page, or pass ref/selector to capture one element. Follow with publish_artifact to show it to the user.",
            "inputSchema": obj(json!({
                "path": { "type": "string", "description": "Optional destination inside the current project (absolute or project-relative). Defaults to a file in this session's browser directory." },
                "fullPage": { "type": "boolean", "description": "Capture the entire scrollable page instead of the viewport" },
                "ref": { "type": "string", "description": "Capture just this element, by ref from the latest snapshot" },
                "selector": { "type": "string", "description": "Capture just the element matching this CSS selector" }
            }), json!([])),
        },
        {
            "name": "browser_record_flow",
            "description": "Record a narrated, Scribe-style walkthrough video (MP4) of a browser flow in ONE call. Prefer this over click/screenshot/click loops for anything a person will watch: those pace the video at model latency and produce a slideshow, not a demo. Here you describe the whole storyboard up front and it is played back on timings chosen for a viewer — the pointer glides along eased paths, clicks ripple, typing runs at human speed, and captions appear before the action they describe. Returns the video path; follow with publish_artifact to deliver it.",
            "inputSchema": record_flow_schema(),
        },
        {
            "name": "browser_record_start",
            "description": "Start recording the browser to an MP4 and return immediately. For ad-hoc capture of a flow you drive step by step; for anything a person will watch, prefer browser_record_flow, which paces the result for a viewer instead of at model latency. Shows the presentation cursor for the duration. Stop with browser_record_stop.",
            "inputSchema": obj(json!({
                "path": { "type": "string", "description": "Destination .mp4 (absolute, or relative to this session's browser directory). Defaults to a timestamped file." }
            }), json!([])),
        },
        {
            "name": "browser_record_stop",
            "description": "Stop the in-progress recording, finalize the MP4, and return its path and duration. Follow with publish_artifact to deliver it.",
            "inputSchema": obj(json!({}), json!([])),
        },
        {
            "name": "browser_downloads",
            "description": "List files this browser downloaded, with their on-disk paths. Pass a completed download's path to publish_artifact to deliver it to the user.",
            "inputSchema": obj(json!({}), json!([])),
        },
        {
            "name": "browser_evaluate",
            "description": "Evaluate a JavaScript expression in the page and return its JSON-serializable result. Prefer semantic browser tools for ordinary interaction.",
            "inputSchema": obj(json!({
                "expression": { "type": "string" },
                "includeSnapshot": { "type": "boolean", "description": "Also return a fresh semantic page snapshot" }
            }), json!(["expression"])),
        },
        {
            "name": "browser_wait_for",
            "description": "Wait until a selector is visibly rendered, text appears, or text disappears. Conditions may be combined.",
            "inputSchema": obj(json!({
                "selector": { "type": "string" },
                "text": { "type": "string" },
                "textGone": { "type": "string" },
                "timeoutMs": { "type": "number", "description": "Maximum wait in milliseconds (default 10000)" },
                "includeSnapshot": { "type": "boolean", "description": "Also return a fresh semantic page snapshot" }
            }), json!([])),
        },
        {
            "name": "browser_handle_dialog",
            "description": "Accept or dismiss the currently open JavaScript alert, confirm, or prompt.",
            "inputSchema": obj(json!({
                "accept": { "type": "boolean", "description": "Accept when true (default), dismiss when false" },
                "promptText": { "type": "string", "description": "Optional response for a JavaScript prompt" },
                "includeSnapshot": { "type": "boolean", "description": "Also return a fresh semantic page snapshot" }
            }), json!([])),
        },
        {
            "name": "browser_upload",
            "description": "Set files on a file input. Paths are restricted to existing files inside the current project.",
            "inputSchema": obj(json!({
                "ref": { "type": "string", "description": "Stable file-input ref from the latest snapshot" },
                "selector": { "type": "string", "description": "CSS selector for the file input" },
                "paths": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "Absolute paths or project-relative paths" },
                "includeSnapshot": { "type": "boolean", "description": "Also return a fresh semantic page snapshot" }
            }), json!(["paths"])),
        },
        {
            "name": "browser_console",
            "description": "Return console messages, uncaught exceptions, failed resource loads, CORS refusals, and CSP violations for the CURRENT page. Cleared automatically on navigation, so entries always describe the page you are on.",
            "inputSchema": obj(json!({
                "limit": { "type": "number", "description": "Maximum recent entries (default 50)" }
            }), json!([])),
        },
        {
            "name": "browser_network",
            "description": "Return recent network requests with methods, URLs, resource types, status, and failures. Use the returned id with browser_network_body to read a response.",
            "inputSchema": obj(json!({
                "limit": { "type": "number", "description": "Maximum recent entries (default 80)" },
                "filter": { "type": "string", "description": "Optional case-insensitive substring filter" }
            }), json!([])),
        },
        {
            "name": "browser_network_body",
            "description": "Read the response body and headers of a request listed by browser_network — how you find out WHICH field an API rejected, or which CORS/cache/auth header caused a failure, rather than just seeing a status code. Bodies are only retained for a short window after the response.",
            "inputSchema": obj(json!({
                "id": { "type": "string", "description": "Request id from a browser_network entry" }
            }), json!(["id"])),
        },
        {
            "name": "browser_tabs",
            "description": "List all open browser tabs, including their ids, URLs, titles, and which tab is active.",
            "inputSchema": obj(json!({}), json!([])),
        },
        {
            "name": "browser_new_tab",
            "description": "Open and activate a new browser tab. Popups and target=_blank links also become visible automatically.",
            "inputSchema": obj(json!({
                "url": { "type": "string", "description": "URL to open (default about:blank)" },
                "includeSnapshot": { "type": "boolean", "description": "Also return a fresh semantic snapshot of the new tab" }
            }), json!([])),
        },
        {
            "name": "browser_switch_tab",
            "description": "Switch the shared browser to a tab id returned by browser_tabs. The user sees the same tab switch.",
            "inputSchema": obj(json!({
                "id": { "type": "string", "description": "Opaque tab id from browser_tabs" },
                "includeSnapshot": { "type": "boolean", "description": "Also return a fresh semantic snapshot of the selected tab" }
            }), json!(["id"])),
        },
        {
            "name": "browser_close_tab",
            "description": "Close a tab by id, or the active tab when id is omitted. Closing the final tab resets it to about:blank.",
            "inputSchema": obj(json!({
                "id": { "type": "string", "description": "Optional opaque tab id from browser_tabs" },
                "includeSnapshot": { "type": "boolean", "description": "Also return a fresh semantic snapshot of the remaining active tab" }
            }), json!([])),
        },
        {
            "name": "browser_status",
            "description": "Return browser health, current URL and title, and any open JavaScript dialog.",
            "inputSchema": obj(json!({}), json!([])),
        },
        {
            "name": "browser_snapshot",
            "description": "Return a nested semantic accessibility snapshot with stable element refs — including the contents of iframes — plus a PNG screenshot. Nesting is meaningful: use the enclosing row/list/dialog to tell repeated controls apart. Take a fresh snapshot after navigation or major page changes, then interact by ref.",
            "inputSchema": obj(json!({}), json!([])),
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_tool_names_are_unique_and_complete() {
        let specs = tool_specs();
        let tools = specs.as_array().unwrap();
        let names = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(names.len(), tools.len());
        for required in [
            "browser_snapshot",
            "browser_click",
            "browser_fill_form",
            "browser_handle_dialog",
            "browser_upload",
            "browser_console",
            "browser_network",
            "browser_network_body",
            "browser_screenshot",
            "browser_downloads",
            "browser_resize",
            "browser_tabs",
            "browser_new_tab",
            "browser_switch_tab",
            "browser_close_tab",
        ] {
            assert!(names.contains(required), "missing {required}");
        }
    }

    #[test]
    fn semantic_snapshot_renders_as_readable_text() {
        let mut snapshot = json!({
            "url": "https://example.test",
            "title": "Example",
            "elementCount": 1,
            "snapshot": "- [ref=e1] button \"Continue\"",
        });
        let text = semantic_snapshot_text(&mut snapshot);
        assert!(text.contains("URL: https://example.test"));
        assert!(text.contains("Interactive elements: 1"));
        assert!(text.contains("[ref=e1] button"));
        assert!(snapshot.get("snapshot").is_none());
    }
}
