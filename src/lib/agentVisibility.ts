import type { Agent } from "./protocol";

/**
 * Hermes remains fully supported by the protocol, store, and backend. Keep its
 * visibility as a UI-only switch so it can be restored without reviving any
 * removed functionality or migrating persisted data.
 */
export const SHOW_HERMES_AGENTS = false;

export function isAgentVisible(agent: Agent): boolean {
  return SHOW_HERMES_AGENTS || agent !== "hermes";
}

