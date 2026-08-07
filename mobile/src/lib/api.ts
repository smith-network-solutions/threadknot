import type { PairResult, ServerInfo } from './types';

const TIMEOUT_MS = 10_000;

export type ApiErrorCode = 'unreachable' | 'unauthorized' | 'not-threadknot' | 'server' | 'bad-url';

export class ApiError extends Error {
  code: ApiErrorCode;
  constructor(code: ApiErrorCode, message: string) {
    super(message);
    this.code = code;
  }
}

async function request(url: string, init?: RequestInit): Promise<Response> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), TIMEOUT_MS);
  try {
    return await fetch(url, { ...init, signal: controller.signal });
  } catch {
    throw new ApiError('unreachable', 'Could not reach the server. Check the URL and your network.');
  } finally {
    clearTimeout(timer);
  }
}

export interface NormalizedUrl {
  /** Origin only — `https://host[:port]`, no path, no credentials, no token. */
  baseUrl: string;
  /** Master token extracted from `?token=…`, if the URL carried one. */
  token: string | null;
  /** Plain HTTP to a non-private host — worth a visible warning. */
  insecureRemote: boolean;
}

function isPrivateHost(host: string): boolean {
  if (host === 'localhost' || host.endsWith('.local') || host.endsWith('.ts.net')) return true;
  const m = host.match(/^(\d+)\.(\d+)\.(\d+)\.(\d+)$/);
  if (!m) return false;
  const [a, b] = [Number(m[1]), Number(m[2])];
  if (a === 10 || a === 127) return true;
  if (a === 192 && b === 168) return true;
  if (a === 172 && b >= 16 && b <= 31) return true;
  if (a === 100 && b >= 64 && b <= 127) return true; // CGNAT (Tailscale)
  return false;
}

/** Validate + normalize a pasted Threadknot URL (LAN, Tailscale, ngrok, …). */
export function normalizeServerUrl(input: string): NormalizedUrl {
  let raw = input.trim();
  if (raw.length === 0) throw new ApiError('bad-url', 'Enter your Threadknot server URL.');
  if (!/^[a-z][a-z0-9+.-]*:\/\//i.test(raw)) raw = `http://${raw}`;
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    throw new ApiError('bad-url', 'That does not look like a valid URL.');
  }
  if (url.protocol !== 'http:' && url.protocol !== 'https:') {
    throw new ApiError('bad-url', 'Only http:// and https:// URLs are supported.');
  }
  if (url.username || url.password) {
    throw new ApiError('bad-url', 'URLs with embedded usernames/passwords are not supported.');
  }
  if (url.pathname !== '/' && url.pathname !== '') {
    throw new ApiError(
      'bad-url',
      'Threadknot must be served at the root of the URL — subpaths are not supported yet.'
    );
  }
  return {
    baseUrl: url.origin,
    token: url.searchParams.get('token'),
    insecureRemote: url.protocol === 'http:' && !isPrivateHost(url.hostname),
  };
}

/** A scanned pairing QR: which server, and the one-time code that proves we
 *  were physically in front of its screen. */
export interface ScannedPairing {
  baseUrl: string;
  code: string;
  insecureRemote: boolean;
}

/** Parse a `threadknot://pair?u=<origin>&c=<code>` QR payload.
 *
 *  Deliberately strict: a camera will happily hand us any QR in the frame —
 *  a wifi code, a product barcode, a URL from a poster — and the failure we
 *  want is "that is not a Threadknot pairing code", not a confusing network
 *  error twenty seconds later. */
export function parsePairingPayload(raw: string): ScannedPairing {
  const bad = () =>
    new ApiError('bad-url', 'That is not a Threadknot pairing code. On the desktop, open Settings → Phone & access → pair a phone.');
  const text = raw.trim();
  // Matched by prefix rather than `new URL()`: `threadknot:` is a non-special
  // scheme, and how those parse has moved around between RN's URL and Expo's
  // polyfill. URLSearchParams is the part we can rely on everywhere.
  const prefix = 'threadknot://pair?';
  if (!text.toLowerCase().startsWith(prefix)) throw bad();
  const params = new URLSearchParams(text.slice(prefix.length));
  const origin = params.get('u');
  const code = params.get('c');
  if (!origin || !code) throw bad();
  // Reuse the pasted-URL rules so a QR can't smuggle in a shape the manual
  // path rejects (subpaths, embedded credentials, exotic schemes).
  const { baseUrl, insecureRemote } = normalizeServerUrl(origin);
  return { baseUrl, code, insecureRemote };
}

/** Identity probe — works with the master token or a device credential. */
export async function probeServer(baseUrl: string, token: string): Promise<ServerInfo> {
  const res = await request(`${baseUrl}/api/server-info?token=${encodeURIComponent(token)}`);
  if (res.status === 401) {
    throw new ApiError('unauthorized', 'The server rejected this token. Copy a fresh URL from Threadknot.');
  }
  if (!res.ok) throw new ApiError('server', `Server error (HTTP ${res.status}).`);
  let info: ServerInfo;
  try {
    info = (await res.json()) as ServerInfo;
  } catch {
    throw new ApiError('not-threadknot', 'That URL responded, but it is not a Threadknot server.');
  }
  if (info.app !== 'threadknot' || typeof info.serverId !== 'string') {
    throw new ApiError('not-threadknot', 'That URL responded, but it is not a Threadknot server.');
  }
  return info;
}

/** Exchange the master token for a revocable per-device credential. */
export async function pairServer(
  baseUrl: string,
  masterToken: string,
  deviceName: string,
  platform: string
): Promise<PairResult> {
  const res = await request(`${baseUrl}/api/mobile/pair`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${masterToken}` },
    body: JSON.stringify({ deviceName, platform }),
  });
  if (res.status === 401) {
    throw new ApiError('unauthorized', 'Pairing needs the full URL (with ?token=…) from Threadknot Settings.');
  }
  if (!res.ok) throw new ApiError('server', `Pairing failed (HTTP ${res.status}).`);
  return (await res.json()) as PairResult;
}

/** Redeem a scanned one-time code for a device credential. Same endpoint as
 *  `pairServer`, but the phone never sees the master token — the QR carried a
 *  single-use code instead. */
export async function pairServerWithCode(
  baseUrl: string,
  code: string,
  deviceName: string,
  platform: string
): Promise<PairResult> {
  const res = await request(`${baseUrl}/api/mobile/pair`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ pairingCode: code, deviceName, platform }),
  });
  if (res.status === 401) {
    throw new ApiError(
      'unauthorized',
      'That code has expired or was already used. Show a fresh QR on the desktop and scan again.'
    );
  }
  if (!res.ok) throw new ApiError('server', `Pairing failed (HTTP ${res.status}).`);
  return (await res.json()) as PairResult;
}

async function devicePost(baseUrl: string, credential: string, path: string, body: object) {
  const res = await request(`${baseUrl}${path}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${credential}` },
    body: JSON.stringify(body),
  });
  if (res.status === 401) {
    throw new ApiError('unauthorized', 'This device is no longer paired with the server.');
  }
  if (!res.ok) throw new ApiError('server', `Request failed (HTTP ${res.status}).`);
  return res;
}

export async function updatePushRegistration(
  baseUrl: string,
  credential: string,
  body: {
    expoPushToken?: string;
    notificationsEnabled?: boolean;
    notifyErrors?: boolean;
    deviceName?: string;
  }
): Promise<void> {
  await devicePost(baseUrl, credential, '/api/mobile/push', body);
}

export async function sendTestPush(baseUrl: string, credential: string): Promise<void> {
  await devicePost(baseUrl, credential, '/api/mobile/push/test', {});
}

export async function unpairDevice(baseUrl: string, credential: string): Promise<void> {
  await devicePost(baseUrl, credential, '/api/mobile/unpair', {});
}
