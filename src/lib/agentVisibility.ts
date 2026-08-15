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
 *    default, per machine: registering a gateway makes the surfaces eligible,
 *    but nothing lights up until it is switched on deliberately.
 *
 * Neither gate covers the Settings > agents block itself, which always renders.
 * It holds both the toggle and the only form that registers a gateway, so
 * gating it on `hermesConfigured` made the first gateway unaddable: you needed
 * a registered gateway to reach the form that registers one. A fresh install
 * could never get past it.
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
