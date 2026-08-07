/** Pending "open this thread" intent from a tapped push notification. It is
 * held here (not in navigation state) so it survives the biometric gate: the
 * home screen consumes it only once the app is unlocked and the target
 * server's WebView exists. */
export interface NavIntent {
  serverId: string;
  projectId?: string;
  threadId?: string;
}

let pending: NavIntent | null = null;
const listeners = new Set<() => void>();

export function setNavIntent(intent: NavIntent): void {
  pending = intent;
  listeners.forEach((fn) => fn());
}

export function peekNavIntent(): NavIntent | null {
  return pending;
}

export function clearNavIntent(): void {
  pending = null;
}

export function subscribeNavIntent(fn: () => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}
