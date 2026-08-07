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
  /** Normalized origin, e.g. `http://192.168.0.54:42800` — no path, no token. */
  baseUrl: string;
  /** This phone's device registration id on that server. */
  deviceId: string;
  notificationsEnabled: boolean;
  createdAt: string;
  updatedAt: string;
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
