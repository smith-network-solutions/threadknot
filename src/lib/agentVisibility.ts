import type { Agent } from "./protocol";

/**
 * Hermes remains fully supported by the protocol, store, and backend, but its
 * UI only exists on machines that actually have gateways registered. The hello
 * payload's hermes entry reports `available` straight from the local registry
 * (hermes.json), never from a network probe, so an install with no gateways
 * configured never surfaces a trace of the feature, while a machine with
 * agents registered gets its whole Hermes surface back automatically. That
 * keeps private fleets private without a public-facing switch.
 */
let hermesConfigured = false;

/** Fed by the store's hello reducer: the one place every hello lands. */
export function setHermesConfigured(configured: boolean): void {
  hermesConfigured = configured;
}

/** Whether the Hermes surfaces (sidebar agents view, settings block, badge
 *  counts) should render. Dynamic: false until a hello proves gateways exist. */
export function showHermesAgents(): boolean {
  return hermesConfigured;
}

export function isAgentVisible(agent: Agent): boolean {
  return hermesConfigured || agent !== "hermes";
}
