import type { IngressKind, PairResult, RemoteSession, ServerInfo } from './types';

const TIMEOUT_MS = 10_000;

/** Double-submit header the strict ingress pairs with the session cookie.
 * Must match `CSRF_HEADER` in `src-tauri/src/ingress.rs`. */
const CSRF_HEADER = 'x-threadknot-csrf';

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

/** Whether `url` is on exactly `baseUrl`'s origin.
 *
 * A `startsWith(baseUrl)` test is not this. `baseUrl` has no trailing slash, so
 * `https://rig.remote.threadknot.ai` is a prefix of
 * `https://rig.remote.threadknot.ai.example.com/` — which is how an attacker's
 * host gets treated as same-origin by a WebView allowlist or a download
 * interceptor. Compare parsed origins instead. */
export function sameOrigin(url: string, baseUrl: string): boolean {
  try {
    return new URL(url).origin === new URL(baseUrl).origin;
  } catch {
    return false;
  }
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

/** Whether this origin is a Threadknot hosted-relay address.
 *
 *  Used only to pick the right *explanation* when a pasted URL has no token: a
 *  relay address has none to be missing, and saying "copy the URL with the token"
 *  for one sends someone hunting for a string that does not exist. Never used to
 *  gate anything — a self-hosted origin in front of the strict ingress behaves
 *  identically and must keep working. */
export function isRelayOrigin(baseUrl: string): boolean {
  try {
    return new URL(baseUrl).hostname.endsWith('.remote.threadknot.ai');
  } catch {
    return false;
  }
}

/** Build the QR payload shape from a base URL and a code typed by hand.
 *
 *  Typing beats scanning more often than the original design assumed: the QR
 *  needs the desktop's screen visible, and the common case is a phone that is
 *  not in the same room. Routed through the same `parsePairingPayload` +
 *  `addServerByScan` path as a real scan rather than a parallel one, so there is
 *  one code path to keep correct. Hyphens and case are cosmetic — the desktop
 *  displays `ABCDE-FGHIJ` and the payload carries `ABCDEFGHIJ`. */
export function pairingPayloadFor(baseUrl: string, code: string): string {
  const clean = code.replace(/[\s-]/g, '').toUpperCase();
  return `threadknot://pair?u=${encodeURIComponent(baseUrl)}&c=${encodeURIComponent(clean)}`;
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
 *  error twenty seconds later.
 *
 *  `<origin>` is a LAN address *or* a relay hostname: for a remote pairing the
 *  desktop emits `https://<install>.remote.threadknot.ai` with **no port and no
 *  private IP** (`mobile.pair.begin` with `target: "remote"`). Nothing below may
 *  assume either — `URL.origin` already omits the default port, and
 *  `isPrivateHost` only ever gates the plain-HTTP warning. */
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

/** Identity probe — works with the master token or a device credential.
 *
 * The credential travels in an `Authorization` header, never `?token=`. The
 * strict remote ingress refuses *any* credential-bearing query key with a 400
 * even when the value is valid (`ingress.rs`, `CREDENTIAL_QUERY_KEYS`), so a
 * query token here would not merely leak into logs and `Referer` — it would fail
 * every probe against a relay origin. The compat listener accepts the header
 * just as happily, so there is one code path for both doors. */
export async function probeServer(baseUrl: string, token: string): Promise<ServerInfo> {
  const res = await request(`${baseUrl}/api/server-info`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  if (res.status === 401) {
    throw new ApiError('unauthorized', 'The server rejected this token. Copy a fresh URL from Threadknot.');
  }
  // The strict ingress refuses the master credential however it is presented,
  // even when valid — a machine's administrative key has no business crossing a
  // relay. So a pasted `?token=…` URL cannot be a relay address, and the honest
  // answer is "scan instead", not "server error".
  if (res.status === 403) {
    throw new ApiError(
      'unauthorized',
      'That address only accepts a scanned pairing code, not a token URL. On the desktop: Settings → Phone & access → pair a phone.'
    );
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
  // Remote ingress: `mobile_pair_handler` answers the master-token path with 403
  // outright. Nothing the phone can do about it except scan.
  if (res.status === 403) {
    throw new ApiError(
      'unauthorized',
      'That address pairs by scanned code only. On the desktop: Settings → Phone & access → pair a phone.'
    );
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

/** Which door this origin leads to, plus a live cookie session if it is the
 *  strict one. `session` is null on the compat listener, where there is none. */
export interface SessionProbe {
  ingress: IngressKind;
  session: RemoteSession | null;
}

/** Exchange this device's SecureStore credential for an `HttpOnly; Secure;
 *  SameSite=Strict` cookie session, and learn which ingress we are talking to.
 *
 *  `POST /api/session` is mounted on both listeners but only *answers* on the
 *  strict one; the compat listener returns **404** with "browser sessions are
 *  for remote connections". That 404 is the probe: an https tunnel to port 42800
 *  is still the compat listener, so the URL scheme cannot tell the two apart and
 *  guessing from it would put a LAN profile into cookie mode (or worse, a relay
 *  profile into token mode, where every request 400s).
 *
 *  The cookie itself never passes through this code. It is set by the platform
 *  HTTP stack into the shared cookie jar, which is what the WebView then reads —
 *  the whole point being that neither this app's JavaScript nor the page can
 *  read or copy it. What we keep is the CSRF half, which the page *is* meant to
 *  hold.
 *
 *  Returns `null` when the server has no session to give (offline, remote access
 *  switched off) — distinct from a `compat` answer, which is a real result. */
export async function bootstrapSession(
  baseUrl: string,
  credential: string,
  generation: number
): Promise<SessionProbe> {
  const res = await request(`${baseUrl}/api/session`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${credential}` },
    // The bearer branch of the handler ignores the body, but axum's `Json`
    // extractor rejects an empty one outright.
    body: '{}',
  });
  if (res.status === 404) return { ingress: 'compat', session: null };
  if (res.status === 401) {
    throw new ApiError('unauthorized', 'This device is no longer paired with the server.');
  }
  if (res.status === 503) {
    throw new ApiError('server', 'Remote access is switched off on that machine.');
  }
  if (!res.ok) throw new ApiError('server', `Could not open a session (HTTP ${res.status}).`);
  let csrf = '';
  try {
    csrf = ((await res.json()) as { csrf?: string }).csrf ?? '';
  } catch {
    throw new ApiError('not-threadknot', 'That URL responded, but it is not a Threadknot server.');
  }
  if (!csrf) {
    throw new ApiError('server', 'The server opened a session without a CSRF token.');
  }
  return { ingress: 'remote', session: { csrf, generation } };
}

/** `DELETE /api/session` — drop this device's cookie sessions server-side and,
 *  because the response clears the cookie, evict it from this phone's jar too.
 *
 *  That second half is why this is called before un-pairing rather than left to
 *  the un-pair (which already revokes sessions server-side). A dead cookie left
 *  in the jar is not inert: the strict ingress checks the cookie **first** and
 *  answers an unresolvable one with 401 rather than falling through to the
 *  `Authorization` header, so a stale cookie would break otherwise-valid bearer
 *  requests — downloads included — until it expired. */
export async function endSession(baseUrl: string, csrf?: string): Promise<void> {
  await request(`${baseUrl}/api/session`, {
    method: 'DELETE',
    headers: csrf ? { [CSRF_HEADER]: csrf } : undefined,
  }).catch(() => undefined);
}

async function devicePost(
  baseUrl: string,
  credential: string,
  path: string,
  body: object,
  csrf?: string
) {
  const res = await request(`${baseUrl}${path}`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      Authorization: `Bearer ${credential}`,
      // The platform HTTP stack shares its cookie jar with the WebView (that is
      // how the WebView gets its session at all), so once a remote session
      // exists these requests carry the cookie whether we want them to or not —
      // and the server resolves a request by cookie before it looks at the
      // header, then demands the double-submit proof. Without this, every push
      // registration and un-pair against a relay origin is a 403.
      ...(csrf ? { [CSRF_HEADER]: csrf } : {}),
    },
    body: JSON.stringify(body),
  });
  if (res.status === 401) {
    throw new ApiError('unauthorized', 'This device is no longer paired with the server.');
  }
  if (res.status === 403) {
    throw new ApiError('unauthorized', 'The server rejected this session — reopen the app to renew it.');
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
  },
  csrf?: string
): Promise<void> {
  await devicePost(baseUrl, credential, '/api/mobile/push', body, csrf);
}

export async function sendTestPush(
  baseUrl: string,
  credential: string,
  csrf?: string
): Promise<void> {
  await devicePost(baseUrl, credential, '/api/mobile/push/test', {}, csrf);
}

export async function unpairDevice(
  baseUrl: string,
  credential: string,
  csrf?: string
): Promise<void> {
  await devicePost(baseUrl, credential, '/api/mobile/unpair', {}, csrf);
}
