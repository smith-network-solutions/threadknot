import { useStore } from "../state/store";
import { XIcon } from "./icons";

export interface MachineChoice {
  machineId: string;
  label: string;
}

/**
 * First step of "add workspace": which machine the new workspace's folder
 * lives on. This machine first, then every paired peer (offline ones listed
 * but disabled) — same machine list the workspace roots manager uses.
 *
 * Servers are listed separately because the outcome is different, not just the
 * destination: a workspace made on a peer is OUR record naming a remote root,
 * replicated to our peers, whereas one made on a server is created in THEIR
 * store and leaves nothing here. Same button, two meanings, so they do not
 * share a list.
 */
export function NewWorkspaceModal({
  onClose,
  onChoose,
}: {
  onClose: () => void;
  onChoose: (machine: MachineChoice) => void;
}) {
  const { state } = useStore();
  const localId = state.hello?.machineId ?? "";
  const machines: (MachineChoice & { online: boolean })[] = [
    {
      machineId: localId,
      label: state.hello?.friendlyName ?? "this machine",
      online: true,
    },
    ...state.peers.map((p) => ({
      machineId: p.machineId,
      label: p.name,
      online: !!p.online,
    })),
  ];
  // `files` is what lets us browse their disk to choose the folder; without it
  // the picker would open on nothing.
  const servers = state.servers
    .filter((s) => s.machineId && s.capabilities.includes("files"))
    .map((s) => ({ machineId: s.machineId, label: s.name, online: !!s.online }));

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal roots-modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <span>New workspace</span>
          <button className="icon-btn" aria-label="Close" onClick={onClose}>
            <XIcon size={14} />
          </button>
        </div>

        <div className="settings-label">create it on</div>
        <div className="roots-attach">
          {machines.map((mc) => (
            <button
              key={mc.machineId}
              type="button"
              className="settings-toggle"
              disabled={!mc.online}
              title={
                mc.online
                  ? `Pick a folder on ${mc.label}`
                  : `${mc.label} is offline`
              }
              onClick={() => onChoose({ machineId: mc.machineId, label: mc.label })}
            >
              {mc.label}
              {mc.online ? "" : " (offline)"}
            </button>
          ))}
        </div>

        {servers.length > 0 && (
          <>
            <div className="settings-label">or on a server you work on</div>
            <div className="roots-attach">
              {servers.map((mc) => (
                <button
                  key={mc.machineId}
                  type="button"
                  className="settings-toggle"
                  disabled={!mc.online}
                  title={
                    mc.online
                      ? `Create it on ${mc.label}. It stays in their catalog — nothing is stored on this machine.`
                      : `${mc.label} is offline`
                  }
                  onClick={() => onChoose({ machineId: mc.machineId, label: mc.label })}
                >
                  {mc.label}
                  {mc.online ? "" : " (offline)"}
                </button>
              ))}
            </div>
          </>
        )}
      </div>
    </div>
  );
}
