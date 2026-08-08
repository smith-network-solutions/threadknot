/** A configured Threadknot server. Metadata only — the device credential lives in
 * SecureStore under `threadknot.cred.<id>`, never in AsyncStorage. */
export interface ServerProfile {
  /** Local profile id (also the SecureStore credential key suffix). */
  id: string;
  /** Stable Threadknot install identity — push payloads route on this. */
  serverId: string;
  /** User-facing nickname (defaults to the server's hostname). */
  name: string;
  /** Hostname reported by the server at pair time. */
  serverName?: string;
  /** Server version reported at pair time (informational). */
  version?: string;
  /** Normalized origin, e.g. `http://192.168.0.54:42800` or
   * `https://<install>.remote.threadknot.ai` — no path, no port assumed, no token. */
  baseUrl: string;
  /** This phone's device registration id on that server. */
  deviceId: string;
  /** Which of the server's two doors `baseUrl` leads to, learned by probing
   * `POST /api/session` (see `bootstrapSession`):
   *
   * - `compat` — the LAN/Tauri listener. Credentials may ride the URL, and the
   *   `Secure` session cookie is deliberately unavailable there (SEC-006).
   * - `remote` — the strict ingress behind the hosted relay. Any
   *   credential-bearing query key is a **400**, so authentication is a cookie
   *   (for the WebView) or an `Authorization` header (for native requests).
   *
   * **Absent means `compat`.** Profiles written by older builds have no field,
   * and an https tunnel (Tailscale Funnel, ngrok) still points at the compat
   * listener — so the scheme cannot be used to guess this. Only the server's own
   * answer decides. */
  ingress?: IngressKind;
  notificationsEnabled: boolean;
  createdAt: string;
  updatedAt: string;
}

export type IngressKind = 'compat' | 'remote';

/** A live cookie session against a `remote` origin.
 *
 * In memory only, never persisted: the CSRF token is worthless without the
 * `HttpOnly` cookie it is derived from, and the cookie lives in the platform
 * cookie jar where neither this app's JavaScript nor a page can read it. */
export interface RemoteSession {
  /** Double-submit token for cookie-authenticated state changes. */
  csrf: string;
  /** Bumped on every re-bootstrap. The WebView keys off this so a new session
   * gets a new native web view — which is the only moment react-native-webview
   * copies the shared cookie jar into the WKWebView store. */
  generation: number;
}

export interface ServerInfo {
  app: string;
  version: string;
  serverId: string;
  name: string;
}

export interface PairResult {
  serverId: string;
  serverName: string;
  version: string;
  deviceId: string;
  credential: string;
  /** Capabilities the owner bound to the pairing code. Informational here — the
   * server enforces them; the phone must never re-assert them. */
  capabilities?: string[];
}

/** Data payload carried by every Threadknot push notification. */
export interface PushData {
  version: number;
  serverId: string;
  projectId: string;
  threadId: string;
  eventKind: string;
}

/** Messages the web app posts to the shell (see threadknot/src/lib/native.ts). */
export type WebToNativeMessage =
  | { type: 'ready'; serverId?: string; serverName?: string }
  | { type: 'routeChanged'; projectId?: string; threadId?: string }
  | { type: 'connectionChanged'; conn: 'connecting' | 'online' | 'offline' }
  | { type: 'clipboardRead'; requestId: string }
  | { type: 'reloadRequest' };
