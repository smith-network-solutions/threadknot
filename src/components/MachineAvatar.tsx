import type { CSSProperties } from "react";
import type { AppState } from "../state/store";
import { useAvatarHoverPreview } from "./AvatarHoverPreview";

/** Up to two initials from a machine name ("Oz Desktop" -> "OD"). */
export function nameInitials(name: string): string {
  const words = name.trim().split(/\s+/).filter(Boolean);
  if (words.length === 0) return "?";
  const first = words[0][0] ?? "";
  const second =
    words.length > 1 ? (words[words.length - 1][0] ?? "") : (words[0][1] ?? "");
  return (first + second).toUpperCase();
}

export interface MachineLook {
  image?: string;
  color?: string;
  name: string;
}

/** Resolve how a machine should look on THIS device: the local machine uses
 *  its own advertised appearance (hello); a peer prefers the local override
 *  (peer.setAppearance) over what it advertises, else initials from its name.
 *
 *  A server we are a guest on is a machine too, and it is NOT in `peers` — that
 *  is the whole point of a guest link. Before it was looked up here it fell to
 *  the unknown-machine fallback, so a box you are actively connected to was
 *  labelled "remote machine" in every menu that renders a machine by id. */
export function machineLook(state: AppState, machineId: string | undefined): MachineLook {
  const localId = state.hello?.machineId;
  if (!machineId || !localId || machineId === localId) {
    return {
      image: state.hello?.avatar,
      color: state.hello?.color,
      name: state.hello?.friendlyName ?? "this machine",
    };
  }
  const peer = state.peers.find((p) => p.machineId === machineId);
  if (peer) {
    return {
      image: peer.avatarOverride ?? peer.avatar,
      color: peer.colorOverride ?? peer.color,
      name: peer.name,
    };
  }
  const server = state.servers.find((s) => s.machineId === machineId);
  if (server) return { name: server.name };
  return { name: "remote machine" };
}

/** Round machine badge: image when set, else initials; ringed in the machine's
 *  accent color when one is chosen. `preview={false}` opts out of the shared
 *  hover card when the surrounding control already provides the identity. */
export function MachineAvatar({
  image,
  color,
  name,
  size = 22,
  className,
  preview: wantPreview = true,
}: MachineLook & { size?: number; className?: string; preview?: boolean }) {
  // Called unconditionally (hook rules); an empty look disables it internally.
  const preview = useAvatarHoverPreview(
    wantPreview ? { image, color, name } : { name },
  );
  const style: CSSProperties = {
    width: size,
    height: size,
    fontSize: Math.max(7, Math.round(size * 0.42)),
  };
  if (color) (style as Record<string, string | number>)["--machine-color"] = color;
  return (
    <>
      <span
        className={`sidebar-avatar machine-avatar${image ? " has-image" : ""}${
          color ? " has-color" : ""
        }${className ? ` ${className}` : ""}`}
        style={style}
        {...preview.hoverProps}
      >
        {image ? <img src={image} alt="" /> : <span aria-hidden>{nameInitials(name)}</span>}
      </span>
      {preview.portal}
    </>
  );
}
