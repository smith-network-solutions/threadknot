import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { XIcon } from "./icons";

/**
 * Type-to-delete confirmation for unpairing a machine: the red Remove button
 * stays disabled until the user types "delete" (case-insensitive, trimmed).
 * Enter submits once armed; Escape / Cancel close only this dialog.
 */
export function ConfirmRemoveMachineModal({
  name,
  onConfirm,
  onClose,
}: {
  name: string;
  onConfirm: () => Promise<unknown>;
  onClose: () => void;
}) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const armed = text.trim().toLowerCase() === "delete";

  // Escape closes only this dialog (capture phase + stopPropagation, so the
  // settings screen underneath stays open).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      onClose();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);

  async function submit() {
    if (!armed || busy) return;
    setBusy(true);
    setError(null);
    try {
      await onConfirm();
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  }

  return createPortal(
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal confirm-remove-modal"
        role="dialog"
        aria-label={`Remove ${name}`}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal-head">
          <span>Remove {name}?</span>
          <button className="icon-btn" aria-label="Cancel" onClick={onClose}>
            <XIcon size={14} />
          </button>
        </div>

        <div className="confirm-remove-body">
          This unpairs <strong>{name}</strong> from this machine. Its workspaces
          and threads vanish from your lists until you pair it again.
        </div>

        <input
          className="modal-input"
          type="text"
          value={text}
          placeholder={'type "delete" to confirm'}
          aria-label="Type delete to confirm"
          autoFocus
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void submit();
          }}
        />

        {error && <div className="modal-error">{error}</div>}

        <div className="modal-actions">
          <button className="btn tone-deny" disabled={busy} onClick={onClose}>
            Cancel
          </button>
          <button
            className="btn tone-danger"
            disabled={!armed || busy}
            onClick={() => void submit()}
          >
            {busy ? "Removing…" : "Remove"}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
