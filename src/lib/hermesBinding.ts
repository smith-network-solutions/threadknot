import type { Thread } from "./protocol";

/**
 * A chat's relationship to a Hermes gateway, which outlives the agent actually
 * running its turns.
 *
 * Handing a workspace chat to a Hermes agent used to move it: workspace
 * sections dropped every `agent === "hermes"` thread and the Hermes view
 * collected them, so the card vanished from the folder it belongs to. It now
 * stays where it was born and wears its gateway's badge, and the Hermes view
 * lists it too — lit while Hermes holds the next turn, greyed once a local
 * agent has it back. `ThreadSettings.hermesAgentId` is what makes the second
 * state expressible: switching agents overwrites `model`, so without a field
 * that survives the switch there is nothing left to grey out.
 */

/** The gateway a chat belongs to, or undefined if it has never had one.
 *
 *  `settings.model` is the fallback for chats persisted before `hermesAgentId`
 *  existed: on Hermes the two are the same id, so a running chat groups
 *  correctly on the first render after an upgrade. (A chat switched AWAY from
 *  Hermes by an older build lost the id for good — it simply has no binding.) */
export function hermesGatewayId(thread: Thread): string | undefined {
  return (
    thread.settings.hermesAgentId ||
    (thread.agent === "hermes" ? thread.settings.model || undefined : undefined)
  );
}

/** Hermes runs this chat's next turn. */
export function hermesActive(thread: Thread): boolean {
  return thread.agent === "hermes";
}

/** The chat belongs to a gateway but a local agent currently holds it — the
 *  greyed state in the Hermes view, and no badge in the workspace. */
export function hermesDormant(thread: Thread): boolean {
  return thread.agent !== "hermes" && !!hermesGatewayId(thread);
}
