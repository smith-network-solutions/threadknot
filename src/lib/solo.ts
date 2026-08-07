// Solo mode: a window dedicated to a single project (dragged out of the
// sidebar, or opened as a separate browser tab). In the Tauri shell the
// window label carries the project id (`project-<id>`); in a plain browser
// it's the `?project=` query param.

const LABEL_PREFIX = "project-";

function isTauriEnv(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** Project id this window is dedicated to, or null for the full fleet view. */
export async function detectSoloProject(): Promise<string | null> {
  if (isTauriEnv()) {
    try {
      const { getCurrentWebviewWindow } = await import("@tauri-apps/api/webviewWindow");
      const label = getCurrentWebviewWindow().label;
      if (label.startsWith(LABEL_PREFIX)) return label.slice(LABEL_PREFIX.length);
    } catch {
      // fall through to URL detection
    }
    return null;
  }
  return new URLSearchParams(window.location.search).get("project");
}

/**
 * Open (or focus) a dedicated window for a project. Tauri: native webview
 * window via the `open_project_window` command. Browser: a new tab whose
 * URL pins the project.
 */
export async function openProjectWindow(project: { id: string; name: string }): Promise<void> {
  if (isTauriEnv()) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("open_project_window", { projectId: project.id, name: project.name });
    return;
  }
  const url = new URL(window.location.href);
  url.searchParams.set("project", project.id);
  window.open(url.toString(), `threadknot-project-${project.id}`);
}

// ---- cross-window notification dedupe ------------------------------------
// Solo windows advertise themselves in localStorage (shared across windows of
// the same app) so the fleet window can stay quiet about projects that have
// their own window watching them.

const REG_PREFIX = "threadknot.soloWindow.";
const HEARTBEAT_MS = 15_000;
const FRESH_MS = 45_000;

/** Start advertising this solo window; returns a cleanup function. */
export function advertiseSoloWindow(projectId: string): () => void {
  const key = REG_PREFIX + projectId;
  const beat = () => localStorage.setItem(key, String(Date.now()));
  beat();
  const timer = setInterval(beat, HEARTBEAT_MS);
  const onUnload = () => localStorage.removeItem(key);
  window.addEventListener("beforeunload", onUnload);
  return () => {
    clearInterval(timer);
    window.removeEventListener("beforeunload", onUnload);
    localStorage.removeItem(key);
  };
}

/** True if some other (solo) window is live-watching this project. */
export function hasSoloWindow(projectId: string): boolean {
  const raw = localStorage.getItem(REG_PREFIX + projectId);
  if (!raw) return false;
  const ts = Number(raw);
  return Number.isFinite(ts) && Date.now() - ts < FRESH_MS;
}
