import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  registerCropHost,
  type CropRequest,
  type CropSpec,
} from "../lib/sidebarImage";
import {
  AvatarCropStage,
  AvatarDropzone,
  type AvatarCropStageHandle,
} from "./AvatarCropStage";
import { XIcon } from "./icons";

/**
 * Profile-photo dialog. Two stages sharing one modal:
 *  - upload: when opened without a file (src = null), a "click here to
 *    upload" dropzone; choosing/dropping a file moves to the crop stage.
 *  - crop: the shared AvatarCropStage (drag/zoom over a circular mask);
 *    Save renders the circle to `spec.size` and returns the data URL via
 *    onDone (null on cancel).
 * Portaled to document.body, so it renders unzoomed regardless of the
 * work-pane zoom.
 */
export function AvatarCropModal({
  src,
  spec,
  onDone,
}: {
  /** Object URL of a pre-chosen file, or null to start on the upload panel. */
  src: string | null;
  spec: CropSpec;
  onDone: (dataUrl: string | null) => void;
}) {
  const [stageSrc, setStageSrc] = useState<string | null>(src);
  const [ready, setReady] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const stageRef = useRef<AvatarCropStageHandle>(null);
  /** Object URL created here (upload path): ours to revoke. */
  const ownedUrlRef = useRef<string | null>(null);

  useEffect(
    () => () => {
      if (ownedUrlRef.current) URL.revokeObjectURL(ownedUrlRef.current);
    },
    [],
  );

  // Escape cancels. Capture phase + stopPropagation so an underlying
  // Escape-to-close surface (the settings screen) doesn't close with us.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      onDone(null);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onDone]);

  function chooseFile(file: File) {
    if (ownedUrlRef.current) URL.revokeObjectURL(ownedUrlRef.current);
    const url = URL.createObjectURL(file);
    ownedUrlRef.current = url;
    setError(null);
    setReady(false);
    setStageSrc(url);
  }

  function save() {
    const stage = stageRef.current;
    if (!stage) return;
    try {
      onDone(stage.save());
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  return createPortal(
    <div className="modal-backdrop" onClick={() => onDone(null)}>
      <div
        className="modal crop-modal"
        role="dialog"
        aria-label={stageSrc ? "Crop profile photo" : "Upload profile photo"}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal-head">
          <span>{stageSrc ? "Crop profile photo" : "Profile photo"}</span>
          <button className="icon-btn" aria-label="Cancel" onClick={() => onDone(null)}>
            <XIcon size={14} />
          </button>
        </div>

        {stageSrc ? (
          <AvatarCropStage
            key={stageSrc}
            ref={stageRef}
            src={stageSrc}
            spec={spec}
            onReadyChange={setReady}
            onError={setError}
          />
        ) : (
          <AvatarDropzone onFile={chooseFile} onError={setError} />
        )}

        {error && <div className="modal-error">{error}</div>}

        <div className="modal-actions">
          <button className="btn tone-deny" onClick={() => onDone(null)}>
            Cancel
          </button>
          {stageSrc && (
            <button className="btn tone-allow" disabled={!ready} onClick={save}>
              Save
            </button>
          )}
        </div>
      </div>
    </div>,
    document.body,
  );
}

/**
 * Mounted once (in App). Registers itself as the crop-flow UI so every
 * existing picker call site gains the upload + interactive crop steps
 * without changing its call shape.
 */
export function AvatarCropHost() {
  const [req, setReq] = useState<CropRequest | null>(null);
  useEffect(() => {
    registerCropHost((next) => {
      setReq((prev) => {
        // A second pick while one is open preempts the first (cancels it).
        prev?.resolve(null);
        return next;
      });
    });
    return () => registerCropHost(null);
  }, []);
  if (!req) return null;
  return (
    <AvatarCropModal
      key={req.src ?? "upload"}
      src={req.src}
      spec={req.spec}
      onDone={(url) => {
        setReq(null);
        req.resolve(url);
      }}
    />
  );
}
