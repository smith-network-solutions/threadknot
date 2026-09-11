import { invoke } from "@tauri-apps/api/core";
import { isWindowFocused } from "./focus";
import { getNotifyPrefs, subscribeNotifyPrefs } from "./notify";
import { findThread, remoteMachineId, type AppState } from "../state/store";
import type { ThreadknotClient } from "./ws";

/** Native code owns activity sampling and peer heartbeats. The view only
 * configures its timeout and acknowledges a transcript actually on screen. */
export function startDesktopActivity(client: ThreadknotClient, getState: () => AppState): () => void {
  let stopped = false;
  let checking = false;
  let lastRead = "";
  const configure = () => {
    void invoke("configure_desktop_activity", { idleSeconds: getNotifyPrefs().desktopIdleSeconds }).catch(() => undefined);
  };
  configure();
  const unsubscribe = subscribeNotifyPrefs(configure);

  const check = async () => {
    if (checking || stopped) return;
    const state = getState();
    if (state.conn !== "online") { lastRead = ""; return; }
    if (!isWindowFocused() || document.hidden || state.feedLoading
      || !state.activeThreadId || state.feedThreadId !== state.activeThreadId || state.feed.length === 0) return;
    const thread = findThread(state, state.activeThreadId);
    const machineId = remoteMachineId(state, thread?.machineId);
    const key = `${machineId ?? "local"}:${state.activeThreadId}:${state.lastSeq}`;
    if (key === lastRead) return;
    checking = true;
    try {
      const idle = await invoke<number | null>("desktop_idle");
      const current = getState();
      if (stopped || idle === null || idle >= (getNotifyPrefs().desktopIdleSeconds || 30) * 1000
        || document.hidden || !isWindowFocused() || current.activeThreadId !== state.activeThreadId
        || current.feedLoading || current.feedThreadId !== state.feedThreadId) return;
      await client.request("thread.read", { threadId: state.activeThreadId, seq: state.lastSeq, machineId });
      lastRead = key;
    } catch {
      // Old/offline peers do not support acknowledgments. Retry while visible;
      // activity leases always expire independently of this view.
    } finally { checking = false; }
  };
  const timer = window.setInterval(() => void check(), 1000);
  return () => { stopped = true; window.clearInterval(timer); unsubscribe(); };
}
