import { useEffect, useRef, useState } from "react";
import { copyText } from "../../lib/format";
import { useStore } from "../../state/store";
import { CheckIcon, CopyIcon, FileIcon, ImageIcon, XIcon } from "../icons";

type FileTarget =
  | { kind: "project"; projectId: string; path: string }
  | { kind: "artifact"; artifactId: string };

type CopyState = "idle" | "copied" | "error";
type CopyAction = "file" | "path" | "image";

const IDLE_LABELS: Record<CopyAction, string> = {
  file: "Copy file",
  path: "Copy path",
  image: "Copy image",
};
const IDLE_ICONS = { file: FileIcon, path: CopyIcon, image: ImageIcon };

/** Fetch an image and return it as PNG — the one image type every desktop app
 * and browser accepts from the clipboard. Other formats (JPEG, WebP, SVG, …)
 * are rasterized through a canvas at their natural size. */
async function imageAsPng(url: string): Promise<Blob> {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`Couldn't load the image (HTTP ${response.status}).`);
  const blob = await response.blob();
  if (blob.type === "image/png") return blob;

  const objectUrl = URL.createObjectURL(blob);
  try {
    const image = new Image();
    image.src = objectUrl;
    await image.decode();
    // An SVG without width/height has no natural size; give it a usable one.
    const canvas = document.createElement("canvas");
    canvas.width = image.naturalWidth || 1024;
    canvas.height = image.naturalHeight || 1024;
    const context = canvas.getContext("2d");
    if (!context) throw new Error("Couldn't convert the image.");
    context.drawImage(image, 0, 0, canvas.width, canvas.height);
    return await new Promise<Blob>((resolve, reject) =>
      canvas.toBlob(
        (png) => (png ? resolve(png) : reject(new Error("Couldn't convert the image."))),
        "image/png",
      ),
    );
  } finally {
    URL.revokeObjectURL(objectUrl);
  }
}

function browserCanCopyImage(): boolean {
  return (
    window.isSecureContext &&
    typeof ClipboardItem !== "undefined" &&
    typeof navigator.clipboard?.write === "function"
  );
}

/** Join Threadknot's POSIX-normalized relative paths to a host-native project root. */
export function absoluteProjectPath(root: string, relative: string): string {
  const windows = /^[a-zA-Z]:[\\/]/.test(root) || root.startsWith("\\\\");
  const separator = windows ? "\\" : "/";
  const normalizedRelative = relative.replace(/^[\\/]+/, "").replace(/[\\/]/g, separator);
  const hasTrailingSeparator = root.endsWith("/") || root.endsWith("\\");
  return `${root}${hasTrailingSeparator ? "" : separator}${normalizedRelative}`;
}

function ActionButton({
  action,
  state,
  disabled,
  title,
  onClick,
}: {
  action: CopyAction;
  state: CopyState;
  disabled?: boolean;
  title: string;
  onClick: () => void;
}) {
  const idleLabel = IDLE_LABELS[action];
  const label = state === "copied" ? "Copied" : state === "error" ? "Copy failed" : idleLabel;
  const Icon = state === "copied" ? CheckIcon : state === "error" ? XIcon : IDLE_ICONS[action];

  return (
    <button
      type="button"
      className={`files-action files-copy-action ${state}`}
      onClick={onClick}
      disabled={disabled}
      aria-label={idleLabel}
      title={state === "error" ? `${label}. ${title}` : title}
    >
      <Icon size={13} />
      <span className="files-action-label">{label}</span>
    </button>
  );
}

/** The clipboard actions shared by project-file and artifact viewers. Pass
 * `imageUrl` when the viewer is showing an image to offer copying its pixels. */
export function ViewerClipboardActions({
  absolutePath,
  fileTarget,
  pathOnly = false,
  imageUrl,
}: {
  absolutePath: string;
  fileTarget: FileTarget;
  pathOnly?: boolean;
  imageUrl?: string | null;
}) {
  const { state: appState } = useStore();
  const [fileState, setFileState] = useState<CopyState>("idle");
  const [pathState, setPathState] = useState<CopyState>("idle");
  const [imageState, setImageState] = useState<CopyState>("idle");
  const [fileError, setFileError] = useState("");
  const [pathError, setPathError] = useState("");
  const [imageError, setImageError] = useState("");
  const timers = useRef<Partial<Record<CopyAction, number>>>({});

  useEffect(
    () => () => {
      for (const timer of Object.values(timers.current)) window.clearTimeout(timer);
    },
    [],
  );

  const flash = (
    action: CopyAction,
    setter: (value: CopyState) => void,
    value: CopyState,
  ) => {
    const pending = timers.current[action];
    if (pending) window.clearTimeout(pending);
    setter(value);
    timers.current[action] = window.setTimeout(() => setter("idle"), 1800);
  };

  const copyFile = async () => {
    if (!appState.isTauri) return;
    setFileError("");
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      if (fileTarget.kind === "artifact") {
        await invoke("copy_artifact_file", { artifactId: fileTarget.artifactId });
      } else {
        await invoke("copy_project_file", {
          projectId: fileTarget.projectId,
          path: fileTarget.path,
        });
      }
      flash("file", setFileState, "copied");
    } catch (error) {
      setFileError(String(error));
      flash("file", setFileState, "error");
    }
  };

  const copyPath = async () => {
    setPathError("");
    const copied = await copyText(absolutePath);
    if (copied) {
      flash("path", setPathState, "copied");
    } else {
      setPathError("The clipboard is unavailable.");
      flash("path", setPathState, "error");
    }
  };

  const canCopyImage = appState.isTauri || browserCanCopyImage();
  const copyImage = async () => {
    if (!imageUrl || !canCopyImage) return;
    setImageError("");
    try {
      if (appState.isTauri) {
        // WebKitGTK can't write images to the clipboard from JavaScript, so
        // the PNG goes to the native clipboard as a raw invoke body.
        const png = new Uint8Array(await (await imageAsPng(imageUrl)).arrayBuffer());
        const { invoke } = await import("@tauri-apps/api/core");
        await invoke("copy_image", png);
      } else {
        // Hand the clipboard a promise so Safari still counts the click as
        // the user gesture while the image downloads.
        await navigator.clipboard.write([new ClipboardItem({ "image/png": imageAsPng(imageUrl) })]);
      }
      flash("image", setImageState, "copied");
    } catch (error) {
      setImageError(error instanceof Error ? error.message : String(error));
      flash("image", setImageState, "error");
    }
  };

  return (
    <>
      {imageUrl && <ActionButton
        action="image"
        state={imageState}
        disabled={!canCopyImage}
        title={
          imageError ||
          (canCopyImage
            ? "Copy the image to the clipboard"
            : "Copying images needs the desktop app or an HTTPS connection")
        }
        onClick={() => void copyImage()}
      />}
      {!pathOnly && <ActionButton
        action="file"
        state={fileState}
        disabled={!appState.isTauri}
        title={
          fileError ||
          (appState.isTauri
            ? "Copy the file to the system clipboard"
            : "Copy file is available in the Threadknot desktop app")
        }
        onClick={() => void copyFile()}
      />}
      <ActionButton
        action="path"
        state={pathState}
        title={pathError || `Copy absolute path: ${absolutePath}`}
        onClick={() => void copyPath()}
      />
    </>
  );
}
