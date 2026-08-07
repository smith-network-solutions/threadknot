import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { AVATAR_CROP_SPEC } from "../lib/sidebarImage";
import {
  AvatarCropStage,
  AvatarDropzone,
  type AvatarCropStageHandle,
} from "./AvatarCropStage";
import { ColorPicker } from "./ColorPicker";
import { MachineAvatar } from "./MachineAvatar";
import { XIcon } from "./icons";

/** What happened to the avatar image while the popup was open. */
type ImageState =
  | { kind: "unchanged" }
  | { kind: "clear" }
  | { kind: "new"; src: string };

/**
 * "Customize Profile" popup for one machine (local or peer): big live
 * preview, upload-dropzone → embedded crop stage for the image, and the full
 * color picker. Nothing is applied until Save, which emits a single patch
 * (absent field = unchanged, null = clear) matching setDeviceAppearance /
 * setPeerAppearance.
 */
export function CustomizeProfileModal({
  name,
  subtitle,
  image,
  color,
  onSave,
  onClose,
}: {
  name: string;
  /** Optional line under the title clarifying the edit scope (real profile vs
   *  local-only override). */
  subtitle?: string;
  image?: string;
  color?: string;
  onSave: (patch: { image?: string | null; color?: string | null }) => Promise<unknown>;
  onClose: () => void;
}) {
  const [imageState, setImageState] = useState<ImageState>({ kind: "unchanged" });
  /** Debounced snapshot of the crop circle, feeding the live preview. */
  const [cropPreview, setCropPreview] = useState<string | null>(null);
  const [pendingColor, setPendingColor] = useState<string | null>(color ?? null);
  const [colorTouched, setColorTouched] = useState(false);
  const [cropReady, setCropReady] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const stageRef = useRef<AvatarCropStageHandle>(null);
  /** Object URL for a freshly-chosen file: ours to revoke. */
  const ownedUrlRef = useRef<string | null>(null);

  useEffect(
    () => () => {
      if (ownedUrlRef.current) URL.revokeObjectURL(ownedUrlRef.current);
    },
    [],
  );

  // Escape closes only this popup. Capture phase + stopPropagation so the
  // settings screen underneath doesn't close with us (AvatarCropModal's
  // pattern).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      onClose();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);

  function chooseFile(file: File) {
    if (ownedUrlRef.current) URL.revokeObjectURL(ownedUrlRef.current);
    const url = URL.createObjectURL(file);
    ownedUrlRef.current = url;
    setError(null);
    setCropReady(false);
    setCropPreview(null);
    setImageState({ kind: "new", src: url });
  }

  function discardNewImage() {
    if (ownedUrlRef.current) {
      URL.revokeObjectURL(ownedUrlRef.current);
      ownedUrlRef.current = null;
    }
    setCropPreview(null);
    setCropReady(false);
    setImageState({ kind: "unchanged" });
  }

  async function save() {
    if (busy) return;
    setError(null);
    const patch: { image?: string | null; color?: string | null } = {};
    if (imageState.kind === "new") {
      const stage = stageRef.current;
      if (!stage || !cropReady) return;
      try {
        patch.image = stage.save();
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
        return;
      }
    } else if (imageState.kind === "clear") {
      patch.image = null;
    }
    if (colorTouched && pendingColor !== (color ?? null)) {
      patch.color = pendingColor;
    }
    if (Object.keys(patch).length === 0) {
      onClose();
      return;
    }
    setBusy(true);
    try {
      await onSave(patch);
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  }

  // What the big preview shows right now, given pending edits.
  const previewImage =
    imageState.kind === "new"
      ? (cropPreview ?? undefined)
      : imageState.kind === "clear"
        ? undefined
        : image;
  const previewColor = (colorTouched ? pendingColor : (color ?? null)) ?? undefined;
  const saveBlocked = busy || (imageState.kind === "new" && !cropReady);

  return createPortal(
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal profile-modal"
        role="dialog"
        aria-label={`Customize profile for ${name}`}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal-head">
          <span>
            Customize Profile
            <span className="profile-modal-machine"> · {name}</span>
          </span>
          <button className="icon-btn" aria-label="Close" onClick={onClose}>
            <XIcon size={14} />
          </button>
        </div>

        <div className="profile-modal-body">
          {subtitle && <div className="profile-modal-scope">{subtitle}</div>}
          <div className="profile-preview">
            <MachineAvatar image={previewImage} color={previewColor} name={name} size={96} />
          </div>

          <div className="profile-section-label">image</div>
          {imageState.kind === "new" ? (
            <>
              <AvatarCropStage
                key={imageState.src}
                ref={stageRef}
                src={imageState.src}
                spec={AVATAR_CROP_SPEC}
                onReadyChange={setCropReady}
                onError={setError}
                onPreview={setCropPreview}
              />
              <div className="profile-image-links">
                <button type="button" className="link-btn" onClick={discardNewImage}>
                  discard this image
                </button>
              </div>
            </>
          ) : (
            <>
              <AvatarDropzone onFile={chooseFile} onError={setError} />
              <div className="profile-image-links">
                {imageState.kind === "unchanged" && image && (
                  <button
                    type="button"
                    className="link-btn"
                    onClick={() => setImageState({ kind: "clear" })}
                  >
                    remove image
                  </button>
                )}
                {imageState.kind === "clear" && (
                  <button
                    type="button"
                    className="link-btn"
                    onClick={() => setImageState({ kind: "unchanged" })}
                  >
                    image will be removed · undo
                  </button>
                )}
              </div>
            </>
          )}

          <div className="profile-section-label">color</div>
          <ColorPicker
            value={colorTouched ? pendingColor : (color ?? null)}
            onChange={(c) => {
              setColorTouched(true);
              setPendingColor(c);
            }}
          />
        </div>

        {error && <div className="modal-error">{error}</div>}

        <div className="modal-actions">
          <button className="btn tone-deny" disabled={busy} onClick={onClose}>
            Cancel
          </button>
          <button className="btn tone-allow" disabled={saveBlocked} onClick={() => void save()}>
            {busy ? "Saving…" : "Save"}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
