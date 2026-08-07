import { useEffect, useState } from "react";
import type { ListDirData } from "../lib/protocol";
import { useStore } from "../state/store";
import { FolderIcon, FolderUpIcon, XIcon } from "./icons";

/**
 * Browser-side directory picker (phones can't open a native dialog),
 * driven by `fs.listDir` requests against the Threadknot server.
 *
 * Default use adds the chosen folder as a project. Pass `onPick` to instead
 * hand back the selected path (used by the archive storage-location setting);
 * `title` / `confirmLabel` let that caller relabel the dialog.
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
   *  this one — used when attaching a workspace root on another machine. */
  machineId?: string;
}) {
  const { actions } = useStore();
  const [dir, setDir] = useState<ListDirData | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

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
      await actions.addProject(dir.path);
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  }

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal dir-picker" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <span>{title}</span>
          <button className="icon-btn" aria-label="Close" onClick={onClose}>
            <XIcon size={14} />
          </button>
        </div>

        <div className="dir-path">
          <code>{dir?.path ?? "…"}</code>
        </div>

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
    </div>
  );
}
