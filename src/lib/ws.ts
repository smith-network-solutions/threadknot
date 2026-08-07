import type {
  EventFrame,
  HermesStatusesFrame,
  RequestMap,
  RequestType,
  ServerFrame,
  StateChangedFrame,
  UsageFrame,
} from "./protocol";

export type ConnState = "connecting" | "online" | "offline";

interface Pending {
  resolve: (data: unknown) => void;
  reject: (err: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

const REQUEST_TIMEOUT_MS = 30_000;
const BACKOFF_BASE_MS = 600;
const BACKOFF_MAX_MS = 15_000;

/**
 * Typed WebSocket client for the Threadknot protocol.
 * - request(type, payload) -> Promise, correlated by id
 * - auto-reconnect with exponential backoff + jitter
 * - onOpen(isReconnect) lets the app replay state (hello / thread.get)
 */
export class ThreadknotClient {
  private ws: WebSocket | null = null;
  private url: string | null = null;
  private nextId = 1;
  private pending = new Map<number, Pending>();
  private attempts = 0;
  private disposed = false;
  private everOpened = false;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private lastForcedReconnectAt = 0;

  onEvent: ((frame: EventFrame) => void) | null = null;
  onStateChanged: ((frame: StateChangedFrame) => void) | null = null;
  onUsage: ((frame: UsageFrame) => void) | null = null;
  onHermesStatuses: ((frame: HermesStatusesFrame) => void) | null = null;
  onStatus: ((state: ConnState) => void) | null = null;
  onOpen: ((isReconnect: boolean) => void) | null = null;

  connect(url: string): void {
    this.url = url;
    this.openSocket();
  }

  /**
   * Replace the current transport immediately. Mobile WebViews can suspend
   * without delivering a close event, leaving a dead socket reporting OPEN
   * when the app wakes. A fresh socket also drives onOpen's normal reconnect
   * resync, including replaying the transcript that is already on screen.
   */
  reconnect(): void {
    if (this.disposed || !this.url) return;
    const now = Date.now();
    // Native AppState and the WebView's visibility event usually arrive
    // together. Treat them as one wake rather than starting two resyncs.
    if (now - this.lastForcedReconnectAt < 1_000) return;
    this.lastForcedReconnectAt = now;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    const previous = this.ws;
    this.ws = null;
    this.failPending(new Error("connection refreshing"));
    try {
      previous?.close();
    } catch {
      // A suspended WebView may surface an already-invalid transport here.
    }
    this.openSocket();
  }

  private openSocket(): void {
    if (this.disposed || !this.url) return;
    this.onStatus?.("connecting");
    let ws: WebSocket;
    try {
      ws = new WebSocket(this.url);
    } catch {
      this.scheduleReconnect();
      return;
    }
    this.ws = ws;

    ws.onopen = () => {
      if (ws !== this.ws) return;
      this.attempts = 0;
      const isReconnect = this.everOpened;
      this.everOpened = true;
      this.onStatus?.("online");
      this.onOpen?.(isReconnect);
    };

    ws.onmessage = (msg) => {
      if (ws !== this.ws) return;
      let frame: ServerFrame;
      try {
        frame = JSON.parse(String(msg.data)) as ServerFrame;
      } catch {
        return;
      }
      if (frame.type === "response") {
        const entry = this.pending.get(frame.id);
        if (!entry) return;
        this.pending.delete(frame.id);
        clearTimeout(entry.timer);
        if (frame.ok) entry.resolve(frame.data ?? {});
        else entry.reject(new Error(frame.error ?? "request failed"));
      } else if (frame.type === "event") {
        this.onEvent?.(frame);
      } else if (frame.type === "state.changed") {
        this.onStateChanged?.(frame);
      } else if (frame.type === "usage") {
        this.onUsage?.(frame);
      } else if (frame.type === "hermes.statuses") {
        this.onHermesStatuses?.(frame);
      }
    };

    ws.onclose = () => {
      if (ws !== this.ws) return;
      this.ws = null;
      this.failPending(new Error("connection lost"));
      if (!this.disposed) {
        this.onStatus?.("offline");
        this.scheduleReconnect();
      }
    };

    ws.onerror = () => {
      // onclose follows; nothing to do here.
    };
  }

  private scheduleReconnect(): void {
    if (this.disposed || this.reconnectTimer) return;
    const base = Math.min(BACKOFF_MAX_MS, BACKOFF_BASE_MS * 2 ** this.attempts);
    const jitter = base * (0.75 + Math.random() * 0.5);
    this.attempts += 1;
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.openSocket();
    }, jitter);
  }

  private failPending(err: Error): void {
    for (const [, entry] of this.pending) {
      clearTimeout(entry.timer);
      entry.reject(err);
    }
    this.pending.clear();
  }

  request<K extends RequestType>(
    type: K,
    payload: RequestMap[K]["payload"],
  ): Promise<RequestMap[K]["data"]> {
    return new Promise((resolve, reject) => {
      if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
        reject(new Error("not connected"));
        return;
      }
      const id = this.nextId++;
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`request timed out: ${type}`));
      }, REQUEST_TIMEOUT_MS);
      this.pending.set(id, {
        resolve: resolve as (data: unknown) => void,
        reject,
        timer,
      });
      this.ws.send(JSON.stringify({ id, type, payload: payload ?? {} }));
    });
  }

  dispose(): void {
    this.disposed = true;
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    this.failPending(new Error("client disposed"));
    this.ws?.close();
    this.ws = null;
  }
}
