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
      </div>
    </div>
  );
}
