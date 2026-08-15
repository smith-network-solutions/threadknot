import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import type { ListDirData } from "../lib/protocol";
import { useStore } from "../state/store";
import { FolderIcon, FolderPlusIcon, FolderUpIcon, XIcon } from "./icons";

/**
 * Browser-side directory picker (phones can't open a native dialog),
 * driven by `fs.listDir` requests against the Threadknot server.
 *
 * Default use adds the chosen folder as a project (on `machineId`'s machine
 * when browsing a peer). Pass `onPick` to instead hand back the selected
 * path (used by the archive storage-location setting); `title` /
 * `confirmLabel` let that caller relabel the dialog. A "new folder" button
 * creates a directory inside the current one (fs.mkdir) and steps into it.
 *
 * Rendered through a portal, like every other modal here, because this one is
 * opened from *inside* other modals ("Machines & folders…" → "on <machine>").
 * The popup-surface skin animates `.modal` with `animation-fill-mode: both`, so
 * the final keyframe's `transform: none` is retained as an identity matrix
 * forever — and a retained transform makes that modal the containing block for
 * `position: fixed` descendants. Rendered inline, this picker's backdrop was
 * sized to the modal that opened it (measured: 458×149 instead of the
 * viewport), which clipped the list and killed its scrolling.
 */
export function DirPicker({
  onClose,
  onPick,
  title = "Add project folder",
  confirmLabel = "Use this folder",
  machineId,
}: {
  onClose: () => void;
  onPick?: (path: string) => void;
  title?: string;
  confirmLabel?: string;
  /** Browse a PEER machine's filesystem (proxied server-side) instead of
   *  this one — used when creating a workspace or attaching a workspace
   *  root on another machine. */
  machineId?: string;
}) {
  const { actions } = useStore();
  const [dir, setDir] = useState<ListDirData | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  /** Inline "new folder" name entry is open. */
  const [naming, setNaming] = useState(false);
  const [newName, setNewName] = useState("");

  async function load(path?: string) {
    setBusy(true);
    setError(null);
    try {
      setDir(await actions.listDir(path, machineId));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  async function choose() {
    if (!dir) return;
    if (onPick) {
      onPick(dir.path);
      onClose();
      return;
    }
    setBusy(true);
    try {
      await actions.addProject(dir.path, machineId);
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  }

  async function createFolder() {
    if (!dir || !newName.trim()) return;
    setBusy(true);
    setError(null);
    try {
      const sep = dir.path.endsWith("/") ? "" : "/";
      const created = await actions.mkdir(`${dir.path}${sep}${newName.trim()}`, machineId);
      setNaming(false);
      setNewName("");
      await load(created.path);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  }

  return createPortal(
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal dir-picker" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <span>{title}</span>
          <span className="modal-head-actions">
            <button
              className="icon-btn"
              aria-label="New folder here"
              title="Create a new folder here"
              disabled={busy}
              onClick={() => setNaming((v) => !v)}
            >
              <FolderPlusIcon size={14} />
            </button>
            <button className="icon-btn" aria-label="Close" onClick={onClose}>
              <XIcon size={14} />
            </button>
          </span>
        </div>

        <div className="dir-path">
          <code>{dir?.path ?? "…"}</code>
        </div>

        {naming && (
          <form
            className="dir-newfolder"
            onSubmit={(e) => {
              e.preventDefault();
              void createFolder();
            }}
          >
            <input
              autoFocus
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
              placeholder="new folder name"
              disabled={busy}
            />
            <button
              type="submit"
              className="btn tone-allow"
              disabled={busy || !newName.trim()}
            >
              Create
            </button>
          </form>
        )}

        <div className="dir-list">
          {dir?.parent != null && (
            <button className="dir-entry up" disabled={busy} onClick={() => void load(dir.parent!)}>
              <FolderUpIcon size={15} />
              <span>..</span>
            </button>
          )}
          {dir?.entries
            .filter((e) => e.isDir)
            .map((e) => (
              <button
                key={e.path}
                className="dir-entry"
                disabled={busy}
                onClick={() => void load(e.path)}
              >
                <FolderIcon size={15} />
                <span>{e.name}</span>
              </button>
            ))}
          {dir && dir.entries.filter((e) => e.isDir).length === 0 && (
            <div className="dir-empty">no subfolders</div>
          )}
        </div>

        {error && <div className="modal-error">{error}</div>}

        <div className="modal-actions">
          <button className="btn tone-deny" onClick={onClose}>
            Cancel
          </button>
          <button className="btn tone-allow" disabled={!dir || busy} onClick={() => void choose()}>
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
