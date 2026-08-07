// Saving a server file in the desktop shell.
//
// The Files/Artifacts panes save by clicking a hidden `<a download>` pointed at
// a token-gated HTTP URL. That works in real browsers (phone/web sessions) but
// WebView2/wry silently drop anchor downloads, so in the Tauri shell the button
// did nothing. There we route through a native save dialog instead, via the
// `download_file` backend command.

/**
 * Ask the desktop shell to save `url` (an app-server URL) through a native save
 * dialog, defaulting the filename to `suggestedName`.
 *
 * Returns true when the shell handled it — including when the user cancelled the
 * save dialog — so the caller does nothing further. Returns false when there is
 * no Tauri shell, so the caller falls back to the ordinary anchor click. Throws
 * on a real download/IO error so the pane can surface it.
 */
export async function downloadViaShell(url: string, suggestedName: string): Promise<boolean> {
  if (!("__TAURI_INTERNALS__" in window)) return false;
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke<string>("download_file", { url, suggestedName });
  return true;
}
