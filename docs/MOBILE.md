# Threadknot Mobile — Expo companion app (`threadknot/mobile/`)

A native shell around the existing Threadknot web UI: biometric lock, multi-server
switching, and push notifications that deep-link straight to the thread that
needs attention. Everything agent-facing stays in the WebView; the native layer
only handles servers, security, and notifications.

```
Threadknot server A ─┐
Threadknot server B ─┼─ Expo Push Service ──→ Threadknot Mobile (Expo, threadknot/mobile)
Threadknot server C ─┘                          ├─ biometric/privacy gate (expo-local-authentication)
      ▲                                     ├─ server profiles (AsyncStorage) + credentials (SecureStore)
      │ pair once with master token         ├─ notification → server → project → thread routing
      └─ revocable device credentials       └─ warm WebView pool (LRU 3) → existing web UI
```

## How the pieces fit

### Server side (Rust, `src-tauri/src/`)

- **`mobile.rs`** — paired-device registry (`~/.threadknot/mobile.json`). Pairing
  exchanges the master token for a revocable `amd_…` device credential; only a
  sha256 hash is stored. `ServerState::authenticate` accepts master token OR
  device credential on every endpoint (`/ws`, `/attachment`, `/file`, `/term`,
  `/browser`, …), returning a `Principal` (device principals cannot run
  `mobile.device.*` admin requests).
- **`push.rs`** — Expo push dispatcher. `Hub::emit` mirrors `turn_completed`,
  `approval_request`, `question_request` (+ opt-in `error`) into a bounded
  queue; the worker batches to the Expo API with retry/backoff, tracks
  tickets→receipts, and disables `DeviceNotRegistered` tokens. Override the
  API with `THREADKNOT_EXPO_PUSH_URL` / `THREADKNOT_EXPO_RECEIPTS_URL` for tests.
- **Per-device workspace subscriptions** — `MobileDevice` carries
  `notifyScope` (`all` | `selected` | `none`) plus `notifyWorkspaces`, one list
  read two ways: a mute list under `all`, an allowlist under `selected`. The
  push worker filters on the job's `workspace_id` (resolved from the thread's
  project via `Store::workspace_for_project`), so two people sharing one Threadknot
  each hear only their own work — and it holds while the phone is asleep, which
  a client-side filter cannot. `only_device` jobs (the push test) bypass it.
  Under `selected`, an unresolvable workspace fails closed.
- **`store.rs`** — `server.json` now carries a stable `server_id` (UUID,
  auto-migrated on first load). Push payloads route on it, so it survives
  token/port/URL changes.
- **HTTP API** (all POST bodies JSON; auth via `Authorization: Bearer …`):
  - `GET  /api/server-info?token=…` → `{app:"threadknot", version, serverId, name}`
  - `POST /api/mobile/pair` (master token) → `{serverId, serverName, deviceId, credential}`
  - `POST /api/mobile/push` (device cred) — `{expoPushToken?, notificationsEnabled?,
    notifyErrors?, notifyScope?, notifyWorkspaces?, deviceName?}`. Every field is
    optional and absent means "leave unchanged" — an empty `notifyWorkspaces`
    array is a real instruction to clear the list, so the two stay distinct.
  - `POST /api/mobile/push/test` (device cred) — round-trip test notification
  - `POST /api/mobile/unpair` (device cred) — device removes itself
- **WS requests**: `hello` now returns `serverId`/`serverName`;
  `mobile.device.list` / `mobile.device.revoke` are master-only (desktop
  Settings popover shows paired phones with a revoke button).

### Web side (`src/lib/native.ts`)

The mobile shell injects `window.__THREADKNOT_NATIVE__ = {token, serverId,
platform}` before page load (credential never rides the URL or web storage)
and calls `window.__threadknotNative.dispatch({type:'navigate', threadId, …})` for
push-tap routing. The web app posts back `ready` / `routeChanged` /
`connectionChanged` / `clipboardRead` / `reloadRequest` via
`ReactNativeWebView.postMessage`. In the native shell, web chime/vibrate/system
notifications are suppressed (push covers them); toasts stay.

`bootstrap.capabilities` is how the page learns what *this* shell build can do
(`clipboard-read`, `reload`) — installed older builds advertise less, so every
feature gated on it needs a browser fallback.

**Pull to refresh** (`src/components/PullToRefresh.tsx`): the gesture lives in
the web app, not the shell, because the SPA's own scrollers (`.feed-scroll`, the
sidebar) are what decide whether a pull is available — a native `RefreshControl`
wrapping the WebView would have to fight them for the drag. Touch-primary
devices only, and never inside terminals, the browser pane, the composer, or a
modal. Committing posts `reloadRequest`, and the shell calls `webview.reload()`
so the bundle is genuinely refetched (`index.html` is `no-cache`) and a wedged
page recovers; in a plain phone browser it falls back to `location.reload()`.
Either way the app restores the thread it was on from `localStorage`.

### Mobile app (`mobile/`)

- Expo SDK 57 + expo-router, TypeScript, NativeWind 4 + vendored
  [react-native-reusables](https://reactnativereusables.com) components
  (`src/components/ui/`), themed to the Threadknot navy/brass console.
- `src/lib/servers.tsx` — profiles in AsyncStorage, credentials in SecureStore
  (`WHEN_UNLOCKED_THIS_DEVICE_ONLY`). Add flow: normalize URL → probe
  `/api/server-info` → pair → store credential → register push. URL changes
  re-probe with the existing credential (no re-pair needed unless revoked).
- `src/lib/lock.tsx` — biometric gate on cold start, and on return to
  foreground after more than 5 minutes in the background (quick app switches
  stay friction-free); opaque privacy overlay whenever the app is inactive;
  refuses to run with no device security enrolled.
- `src/components/WebViewPool.tsx` — warm LRU pool (3) of WebViews, one per
  server; instant switching; external links open in the system browser; only
  same-origin navigation allowed inside. On app resume it signals every warm
  page to replace its possibly stale WebSocket; the web app's normal reconnect
  path then refreshes whichever transcript is already open in place.
- `src/app/_layout.tsx` — notification tap capture (cold start included) →
  nav intent held behind the biometric gate → home screen activates the right
  server and injects the thread navigation once the page reports ready.

## EAS / building

Project: `@servicestorm/threadknot-mobile` (id `ec582486-06b9-4315-841a-6ea0171d45b7`),
already linked in `app.json`. This is a *new* EAS project created at the
rebrand — `79f628b0-…` was Armada's, and every push token issued against it is
dead, so a phone must re-pair to get one that routes. Profiles in `eas.json`: `development`
(dev client, internal), `preview` (internal), `production`.

```bash
cd threadknot/mobile
npm run typecheck            # tsc --noEmit
npx expo-doctor              # config sanity
npx expo export --platform android   # bundle check without a device

# First real builds (biometrics + push need a dev build, NOT Expo Go):
npx eas-cli build --profile development --platform android
npx eas-cli build --profile development --platform ios
npx expo start --dev-client  # then open from the dev build
```

### One-time push credential setup (manual, needs accounts)

- **Android (FCM)**: create a Firebase project for
  `com.smithnetworksolutions.threadknot`, then `eas credentials` → Android →
  upload the FCM V1 service-account key. Without it, Android pushes are
  dropped by Expo.
- **iOS (APNs)**: requires an Apple Developer account; `eas credentials` →
  iOS → let EAS manage the push key. Face ID string and ATS exceptions
  (`NSAllowsArbitraryLoadsInWebContent`, local networking) are already in
  `app.json`.

## Security model

- The master token authorizes pairing only; the phone keeps a per-device
  `amd_…` credential (revocable from desktop Settings) in SecureStore.
- Server stores only credential hashes; the device list never serializes them.
- Push payloads carry ids only (`serverId/projectId/threadId/eventKind`) — no
  URLs, tokens, prompts, or tool output.
- HTTP to non-private hosts triggers a visible warning in the add-server flow;
  LAN/Tailscale/CGNAT ranges are considered private.

## Smoke-tested (2026-07-21, headless server + fake Expo endpoint)

pair → device auth on HTTP + WS → `hello.serverId` → admin-gating for device
principals → push registration → test push delivered to a mock Expo API with
correct payload/channel → revoke → credential rejected (401). Rust unit tests
cover the credential store and dead-token cleanup (`mobile.rs` tests).

## Still needs a physical device pass

Foreground/background/killed delivery, cold-start tap routing, biometric
cancel/lockout paths, keyboard/safe-area behavior in the WebView, and the
three-server switching matrix (LAN HTTP / Tailscale / ngrok HTTPS).
