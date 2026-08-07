import { useEffect, useRef, useState } from "react";
import { copyText } from "../../lib/format";
import { useStore } from "../../state/store";
import { CheckIcon, CopyIcon, FileIcon, XIcon } from "../icons";

type FileTarget =
  | { kind: "project"; projectId: string; path: string }
  | { kind: "artifact"; artifactId: string };

type CopyState = "idle" | "copied" | "error";

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
  action: "file" | "path";
  state: CopyState;
  disabled?: boolean;
  title: string;
  onClick: () => void;
}) {
  const idleLabel = action === "file" ? "Copy file" : "Copy path";
  const label = state === "copied" ? "Copied" : state === "error" ? "Copy failed" : idleLabel;
  const Icon =
    state === "copied" ? CheckIcon : state === "error" ? XIcon : action === "file" ? FileIcon : CopyIcon;

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

/** The two clipboard actions shared by project-file and artifact viewers. */
export function ViewerClipboardActions({
  absolutePath,
  fileTarget,
}: {
  absolutePath: string;
  fileTarget: FileTarget;
}) {
  const { state: appState } = useStore();
  const [fileState, setFileState] = useState<CopyState>("idle");
  const [pathState, setPathState] = useState<CopyState>("idle");
  const [fileError, setFileError] = useState("");
  const [pathError, setPathError] = useState("");
  const timers = useRef<{ file?: number; path?: number }>({});

  useEffect(
    () => () => {
      if (timers.current.file) window.clearTimeout(timers.current.file);
      if (timers.current.path) window.clearTimeout(timers.current.path);
    },
    [],
  );

  const flash = (
    action: "file" | "path",
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

  return (
    <>
      <ActionButton
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
      />
      <ActionButton
        action="path"
        state={pathState}
        title={pathError || `Copy absolute path: ${absolutePath}`}
        onClick={() => void copyPath()}
      />
    </>
  );
}
