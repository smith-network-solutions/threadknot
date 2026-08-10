import type { Agent } from "./protocol";

/**
 * Hermes remains fully supported by the protocol, store, and backend, but its
 * surfaces (sidebar tile, agent pickers, scheduled runs) only render when BOTH
 * gates pass:
 *
 * 1. The machine has gateways registered. The hello payload's hermes entry
 *    reports `available` straight from the local registry (hermes.json), never
 *    from a network probe, so an install with no gateways configured never
 *    surfaces a trace of the feature.
 * 2. The owner has flipped the enable toggle in Settings > agents. Off by
 *    default, per machine: registering a gateway makes the settings block
 *    (and the toggle) appear, but nothing else lights up until it is switched
 *    on deliberately.
 */

const H_KEY = "threadknot.hermesEnabled";

/** Fired on window after the enable toggle changes. detail: the new boolean. */
export const HERMES_ENABLED_EVENT = "threadknot:hermesenabled";

let hermesConfigured = false;
let hermesEnabled = readEnabled();

function readEnabled(): boolean {
  try {
    return localStorage.getItem(H_KEY) === "1";
  } catch {
    return false;
  }
}

/** Fed by the store's hello reducer: the one place every hello lands. */
export function setHermesConfigured(configured: boolean): void {
  hermesConfigured = configured;
}

/** Whether this machine has Hermes gateways registered at all. Gates only the
 *  Settings management block, which must stay reachable while the feature is
 *  toggled off (it is where the toggle lives). */
export function hermesRegistered(): boolean {
  return hermesConfigured;
}

export function getHermesEnabled(): boolean {
  return hermesEnabled;
}

export function setHermesEnabled(on: boolean): void {
  hermesEnabled = on;
  try {
    localStorage.setItem(H_KEY, on ? "1" : "0");
  } catch {
    // Locked-down storage: the toggle still works for this run.
  }
  window.dispatchEvent(new CustomEvent<boolean>(HERMES_ENABLED_EVENT, { detail: on }));
}

/** Whether the Hermes surfaces beyond Settings should render. */
export function showHermesAgents(): boolean {
  return hermesConfigured && hermesEnabled;
}

export function isAgentVisible(agent: Agent): boolean {
  return agent !== "hermes" || showHermesAgents();
}
