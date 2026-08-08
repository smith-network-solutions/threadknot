# Remote access and relay security

> Status: DECISION RECORD + HARDENING PLAN (2026-08-07)
>
> **Phases 0, 1 and 2 are implemented** (2026-08-07): SEC-001 to SEC-011 and
> SEC-013 are closed. Each finding below carries its own status line; the
> regression matrix (`src-tauri/tests/authorization_matrix.rs`) runs in the
> normal `cargo test` gate.
>
> **SEC-012 is closed** (2026-08-08): peer links are TLS against a certificate
> authority pinned at pairing, credentials are per-link and header-borne, and
> routed requests carry the originating caller's grants. `MESH_VERSION` is 2.
>
> **SEC-014 is closed on the desktop** (2026-08-08): bounded per-connection
> queues with a defined policy per frame class, per-principal session caps, frame
> size caps, a per-connection request rate limit, and streamed byte endpoints.
> The relay-side half (per-installation bandwidth quotas) lives in the relay's
> own limits.
>
> The TLS-termination decision is settled: edge termination, see
> `RELAY-BUILD-PLAN.md`. Do not expose the LAN listener through a relay; point a
> connector at the strict loopback ingress instead. Remaining before beta: the
> **physical mobile download test** (SEC-011), and the load test in Stage 8.

## Purpose

Threadknot already serves its complete UI and protocol from one local Axum
server. A proprietary relay can give each installation a stable public HTTPS
origin while the desktop makes an outbound connection through NAT/CGNAT. The
relay transports traffic only; projects, agent CLIs, credentials, terminals,
browser profiles, and persisted threads remain on the user's machines.

This is a high-risk remote-administration surface, not an ordinary chat site.
Depending on the granted capabilities, a paired device may run coding agents,
edit source, execute shell commands, use logged-in browser identities, and
route work to paired machines. A leaked credential can therefore become a
workstation or fleet compromise.

This document records the product decisions, security invariants, confirmed
holes in the current implementation, and the patch/release plan for a public
relay.

## Product decisions

### Proprietary relay is the priority

- Build toward a Threadknot-operated relay with a stable origin such as
  `https://<installation>.remote.threadknot.ai`.
- The desktop connector initiates the connection outbound. Users do not open a
  router port and remote clients do not install a VPN.
- Tailscale Serve/Funnel remain useful interim and diagnostic transports, but
  they are not the primary product direction.
- The public hostname is an address, never a credential. Knowing it grants no
  Threadknot access.
- Remote access is opt-in and off by default. Disabling it invalidates the
  connector registration and all remote sessions.

### Pairing grants explicit capabilities

The desktop owner chooses a device's capabilities when creating its pairing QR
or code. The pairing code is bound server-side to those grants; the joining
client cannot request, add, or widen them. The owner can later edit or revoke
the grants from the desktop.

The minimum capability model should separate these consequences:

| Capability | Authority | Notes |
|---|---|---|
| `threads` | View threads, send/steer/interrupt turns, answer approvals/questions | Running an agent may itself read, edit, or execute according to that thread's access setting. This is already a powerful grant. |
| `files` | Browse, preview, and download project/artifact files | Mutating files should not be implied by read access. Agent edits remain governed by `threads` plus the thread access setting. |
| `git` | View repository state | Git mutations should be a separate grant or explicitly included by the owner. |
| `terminal` | Create and attach to local PTYs | Equivalent to an interactive shell on the target machine. Off by default for public-browser pairings. *(Shipped default is ON — see SEC-004.)* |
| `browser` | View and drive disposable/unsigned browser sessions | Does not grant durable signed-in browser profiles. |
| `signedBrowser` | Select and drive durable signed-in browser profiles | High-risk identity authority; separately checked and off by default. |
| `mesh` | Exercise granted capabilities on paired Threadknot machines | Never widens another capability; for example, `mesh` + `files` does not imply `terminal`. Off by default. *(Shipped default is ON — see SEC-004.)* |

Machine administration remains Master-only in v1 and is not a pairing
checkbox. This includes device/peer management, Threadknot updates, Library
installs, browser-profile creation/update/deletion, connector ownership, relay
hostname changes, and capability grants.

Existing paired devices must not silently become public-relay devices. They
remain usable on their existing local transport, but require an explicit
desktop authorization step before remote access is enabled for them.

### Authentication boundaries

Three credentials have separate jobs and must never substitute for one another:

1. **Master credential** — local desktop administration. It never crosses the
   public relay and is never returned to a Device principal.
2. **Device credential/session** — per paired phone/browser, revocable and
   capability-scoped. The server stores only its hash.
3. **Connector credential** — authenticates one Threadknot installation to the
   relay and is bound by the control plane to exactly one installation and
   hostname. It grants no application session by itself.

The relay is not an authentication bypass. Every forwarded HTTP request and
WebSocket is still authenticated and authorized by the local Threadknot.

### Relay confidentiality decision — SETTLED (2026-08-07): edge termination

The section below is kept as the record of the trade that was weighed. **The
decision is made**: the relay terminates TLS at the edge. Its final paragraph is
still binding — Threadknot must never claim operators are technically unable to
inspect traffic — but nothing here is open.

A first-party relay can terminate browser TLS at the edge and create a second
encrypted hop to the desktop, or it can forward end-to-end encrypted traffic
that terminates on the desktop. Edge termination is simpler for account gates,
abuse controls, and operations, but the relay can then inspect source code,
terminal output, browser traffic, and credentials. Desktop termination reduces
that breach and liability surface but requires certificate/key provisioning and
SNI-aware routing.

Until this is decided and implemented, Threadknot must not claim that relay
operators are technically unable to inspect traffic. In either design, TLS is
mandatory on every non-loopback hop and secrets/content must not appear in
relay access logs.

## Required security invariants

These are release requirements, not best-effort guidelines:

1. A Device response never contains the master credential or a peer credential.
2. The remote ingress rejects the master credential and all credential-bearing
   query parameters, even when the credential itself is valid.
3. Device authorization uses server-stored capabilities. Client payloads never
   self-assert capabilities.
4. Capability checks run before mesh routing, so a Device cannot be forwarded
   to a peer as Master and launder a denied action.
5. Authority-bearing payload fields receive the same checks as privileged RPC
   kinds.
6. Revocation or capability reduction immediately closes affected `/ws`,
   `/term`, and `/browser` connections.
7. Browser sessions use host-scoped `Secure; HttpOnly; SameSite=Strict`
   cookies. Native requests use bearer credentials from SecureStore.
8. WebSocket handshakes validate an exact trusted `Origin`; remote HTTP uses an
   explicit origin policy and CSRF protection.
9. The connector can forward only to Threadknot's dedicated loopback remote
   listener. It cannot choose arbitrary local IPs or ports.
10. The relay assigns hostnames server-side and prevents cross-installation
    routing, registration, and credential reuse.
11. Authentication data, query strings, prompts, file contents, and WebSocket
    frames are excluded from routine relay logs.
12. Unknown, unauthorized, and offline installations return non-enumerating
    responses wherever practical.

## Confirmed current security holes

### P0 — blocks any public relay

#### SEC-001: Device `hello` leaks the master credential

`server.rs` returns `state.lan_url` from `hello` to every authenticated
principal. `lan_url` contains `config.token`, which authenticates as Master. A
paired Device can authenticate once, read `hello.lanUrl`, extract the master
token, and reconnect with every administrative permission.

Required patch:

- Return the token-bearing LAN URL only to Master.
- Return an origin-only address to Device.
- Audit every Device-visible response for master and peer credentials.
- Add a regression test proving Device `hello` cannot contain `config.token`.

Evidence: `src-tauri/src/server.rs` (`authenticate`, `lan_url`, and the `hello`
response).

**Status: closed.** `hello` returns the token-bearing `lanUrl` only to Master;
a Device gets `lan_origin()` (address, no credential). `hello` also now reports
`principal` and the connection's `capabilities` so the UI can hide what was
never granted. Tests: `sec001_device_hello_never_carries_the_master_token`,
`sec001_no_device_visible_response_serializes_a_peer_or_master_secret` — the
latter sweeps every Device-reachable response for the master token rather than
asserting on one field.

#### SEC-002: Device-to-peer terminal privilege laundering

`term::ws_handler` checks only that some credential authenticated. When a
remote `machineId` is supplied, it splices the caller onto the peer and
`peernet` replaces the caller's credential with the peer's master token. The
browser splice already has the missing Master check.

Required patch:

- Preserve the resolved principal in `term::ws_handler`.
- Require both `terminal` and `mesh` capabilities for Device-to-peer terminal
  access after the capability model exists.
- Until then, require Master before any peer terminal splice.
- Test Device/Master against local/peer terminal upgrades.

Evidence: `src-tauri/src/term.rs`, `src-tauri/src/peernet.rs`, and the correctly
guarded equivalent in `src-tauri/src/browser.rs`.

**Status: closed.** `term::ws_handler` keeps the resolved principal instead of
discarding it, requires `terminal` for any pty, and requires `terminal` + `mesh`
before a peer splice — both checked before `peernet` swaps in the peer's master
token. The browser splice stays Master-only (stricter than `browser` + `mesh`):
the splice runs as the peer's owner, so the signed-in check would pass over
there, and from this side the remote thread's settings are not visible. Tests:
`sec002_term_socket_checks_the_principal_not_just_the_token`,
`sec002_mesh_routing_is_refused_before_the_credential_swap`.

#### SEC-003: Privileged browser fields bypass RPC-kind guards

`browser.profile.*` mutations and the driven signed-browser socket are
Master-only, but a Device can enumerate profiles and submit full
`ThreadSettings` containing `browserProfileId` or `claudeChrome`. The browser
resolver trusts those fields, allowing an agent to operate the owner's signed-in
browser identity without passing through the guarded browser endpoint.

Affected settings-ingestion paths:

- `thread.create`
- `thread.setAgent`
- `thread.setSettings`
- `schedule.create`
- `schedule.update`

Required patch:

- Centralize authorization for `ThreadSettings` before mesh routing.
- Require `signedBrowser` for both `browserProfileId` and `claudeChrome`.
- Reject unauthorized fields explicitly; never silently widen or preserve a
  client-requested authority change.
- Test each path locally and with a peer `machineId`.

Evidence: `src-tauri/src/protocol.rs`, `src-tauri/src/server.rs`,
`src-tauri/src/lib.rs`, and `src-tauri/src/mcp.rs`.

**Status: closed.** `authorize_settings` runs in `handle_request` before mesh
routing, on all five ingestion paths, and requires `signedBrowser` for both
`browserProfileId` and `claudeChrome`. It also covers the same authority reached
by another door: `turn.start` / `turn.steer` / `thread.review` /
`thread.parley.start` / `schedule.run` against a thread or schedule the owner
already bound to a signed-in profile. `browser.profile.list` is now
non-enumerating without `signedBrowser` — it answers "you have none" rather than
refusing, since the id list is precisely what a caller needs in order to smuggle
a profile into settings, and the picker that requests it on open should render
empty instead of erroring (invariant 12). Tests: `sec003_*`,
`signed_in_profiles_are_not_enumerable_without_the_grant`.

**Residual gap:** for a request *routed to a peer*, the remote thread's stored
settings cannot be inspected from here, so a device holding `mesh` + `threads`
can send a turn to a peer thread that machine bound to a signed-in profile.
Closing it needs principal propagation across the mesh (see SEC-012), not a
caller-side check.

#### SEC-004: Device is currently one broad principal, not capability-scoped

`Principal::Device(String)` identifies a device but carries no grants. Most
operations are allowed unless a hand-maintained Master-only condition rejects
their RPC kind. That model cannot implement the selected pairing behavior and
has already missed field-level and mesh-laundering paths.

Required patch:

- Persist a versioned capability set on every `MobileDevice`.
- Bind pending pairing codes to the desktop-selected grants.
- Resolve Device authentication to an identity whose current grants can be
  checked centrally.
- Add Master-only capability update APIs and Settings UI.
- Treat capability reductions like revocation for affected live sockets.
- Require explicit remote authorization for legacy paired devices.

Evidence: `src-tauri/src/mobile.rs` and authorization branches in
`src-tauri/src/server.rs`.

**Status: closed, with one deliberate deviation.** `Capability` (`threads`,
`files`, `git`, `terminal`, `browser`, `signedBrowser`, `mesh`) is persisted per
`MobileDevice` with a `capabilitiesVersion`, bound to the pending pairing code
by `mobile.pair.begin`, and resolved into `Principal::Device(DeviceGrant)` on
every authentication — so a reduction takes effect on the next request with no
client cooperation. Unknown capability names are dropped, never honoured.
`mobile.device.setCapabilities` is the Master-only editing API.

**Deviation:** the default grant set is everything except `signedBrowser`,
rather than this document's "terminal and mesh off by default". Shipping the
document's defaults would silently remove the terminal tab and the fleet view
from every phone already in the field, and from every phone paired tomorrow.
`signedBrowser` is never implicit on any path, including legacy records.

Now that the pairing dialog can express grants, moving the *new-pairing* default
to this document's (terminal and mesh off) costs only a checkbox at pair time,
while legacy records keep the wide set. That is a product call — flipping it is
a one-line change to `default_capabilities()` plus a separate constant for the
legacy backfill, and the matrix already covers both.

The desktop Settings UI is in: each paired phone lists the grants it holds and
toggles them individually (`PairedPhoneRow` / `CapabilityPicker`), and the
pairing dialog picks the grants a QR will carry before minting it — each grant
labelled by consequence ("a real shell on this machine", "act as your logged-in
accounts") rather than by endpoint.

**Not done:** the explicit remote-authorization step for legacy devices — it has
nothing to authorize until the relay exists.

#### SEC-005: No authorization regression matrix

Existing mobile tests cover credential hashing, single-use/expiry/cancellation
of pairing codes, revocation persistence, and notification scoping. They do not
exercise authenticated HTTP/WS handlers, principal differences, mesh routing,
or privileged payload fields.

Required patch:

- Add a principal × capability × endpoint × payload × local/peer matrix.
- Include negative assertions that no Device response serializes master/peer
  secrets.
- Exercise actual HTTP and WebSocket handlers where the boundary lives.

**Status: closed.** `src-tauri/tests/authorization_matrix.rs` — 41 tests over
the real `server::handle_request` and a really-bound `build_router`, covering
every bullet above.

### P1 — required before external beta

#### SEC-006: Credentials ride in every resource URL

The frontend appends credentials to `/ws`, `/attachment`, `/file`,
`/artifact-file`, `/term`, and `/browser`. Plain browser credentials also live
in `localStorage`. Public relays, diagnostics, browser extensions, copied URLs,
and XSS can expose them.

Required patch:

- Add a session-bootstrap endpoint.
- Exchange pairing proof/native bearer credentials for an opaque browser
  session cookie.
- Remove credentials from all resource and WebSocket URL builders in remote
  mode.
- Retain legacy query authentication only on the LAN/Tauri ingress during a
  migration window.

Evidence: `src/lib/discovery.ts`.

**Status: closed for remote connections.** `POST /api/session` exchanges a
one-time pairing code, or a native device bearer, for a host-scoped `HttpOnly;
Secure; SameSite=Strict` cookie; only its hash is stored, and it resolves its
authority through the device on every request, so narrowing a grant reaches a
cookie already in a jar. `discovery.ts` omits the `token` parameter entirely in
remote mode rather than sending a blank one — the strict ingress refuses any URL
carrying a credential key, so a blank one would 400 every image and download.
A browser is never handed a bearer credential at all: the plaintext is dropped
at pair time and only the native shell, which has a keychain, receives one.
The LAN keeps query authentication during the migration window, deliberately —
the cookie is `Secure`, so a browser on the plain-http LAN address would drop it
silently. Tests: `sec006_*`.

#### SEC-007: Remote and legacy authentication share one ingress

The application currently builds one router and binds it to `0.0.0.0:42800`.
If legacy query authentication remains there, a connector aimed at that port
also exposes the legacy policy publicly.

Required patch:

- Keep the compatibility listener for LAN/Tauri clients.
- Add a separate loopback-only listener with `IngressPolicy::Remote`.
- Point the proprietary connector only at the strict listener.
- Reject query credentials and the master credential on remote ingress.
- Allow only static bootstrap, pairing-code redemption, cookie sessions, and
  native Device bearer authentication as explicitly designed.

Evidence: router construction and bind in `src-tauri/src/server.rs`.

**Status: closed.** `IngressPolicy` (`ingress.rs`) is carried on the state, and
`server::run` binds a second listener on `127.0.0.1:<port+1>` with
`IngressPolicy::Remote`. The policy is a property of the *socket* because it
cannot be anything else: a header asserting "I came from the relay" is spoofable
from the LAN, and the source address cannot tell the connector apart from the
desktop's own webview, which is also `127.0.0.1`. The strict router refuses
credential-bearing query keys with a 400 (even valid ones), refuses the master
credential with a 403 however presented, refuses the master-token pairing path,
and does not mount `/mcp` or `/api/peer/pair` at all. It is bound
unconditionally — loopback, so binding it exposes nothing — and refuses
everything with 503 until remote access is switched on, which is what makes the
switch instant rather than "next launch". Tests: `sec007_*`,
`remote_access_off_refuses_everything_on_the_strict_ingress`.

#### SEC-008: Revocation does not close live connections

Authentication is resolved once during WebSocket upgrade. Removing a device
from `mobile.json` prevents its next connection but leaves established app,
terminal, and browser sockets alive.

Required patch:

- Track active connections by device/session id.
- Close them immediately on revoke, unpair, remote-disable, credential rotate,
  or relevant capability reduction.
- Periodically revalidate long-lived remote sessions as defense in depth.

Evidence: `ws_handler`/`handle_socket` in `src-tauri/src/server.rs` and
`MobileStore::revoke` in `src-tauri/src/mobile.rs`.

**Status: closed for `/ws`, `/term` and `/browser`.** `SessionRegistry`
(`src-tauri/src/sessions.rs`) tracks every authenticated socket by device id;
`revoke`, `/api/mobile/unpair` and `setCapabilities` close them immediately by
dropping the bridge future. Tests: `sec008_*`. Periodic revalidation of
long-lived sessions is not implemented — it is defence in depth for the relay,
which does not exist yet.

#### SEC-009: Permissive CORS and no WebSocket Origin validation

The server installs `CorsLayer::permissive()` and does not validate the browser
`Origin` during WebSocket upgrade. Query bearer authentication currently limits
drive-by attacks because another site lacks the secret; cookie authentication
would make missing Origin/CSRF defenses directly exploitable.

Required patch:

- Replace permissive CORS with explicit Tauri, LAN, and provisioned-remote
  policies.
- Validate exact allowed origins for every browser WebSocket.
- Add CSRF protection to cookie-authenticated state changes.
- Add CSP, HSTS on the public origin, `Referrer-Policy: no-referrer`, and
  appropriate `Cache-Control` for sensitive responses.

**Status: closed.** On the strict ingress the CORS layer is simply absent, so a
browser will not hand another origin's script the response — the only defence
that still means anything once authentication is a cookie. Cookie-authenticated
state changes carry a double-submit token derived from the cookie itself
(`HttpOnly`, so another origin cannot read it and therefore cannot compute the
token, and nothing extra to store or keep in sync). Responses carry HSTS, a CSP
with `frame-ancestors 'none'` and `object-src 'none'`, `Referrer-Policy:
no-referrer` and `nosniff`; HSTS is deliberately *not* set on the LAN listener,
where pinning a plain-http origin to https would brick it for six months.
Tests: `sec009_*`. Additionally, every WebSocket upgrade (`/ws`, `/term`, `/browser`)
validates `Origin`: absent (native clients), `tauri://`, loopback, or exactly the
`Host` the request was addressed to. Test:
`websocket_upgrades_validate_the_origin`.

The **LAN** listener still ships `CorsLayer::permissive()` and no CSRF layer, and
that is deliberate rather than outstanding: it authenticates by query token, so
another origin's script lacks the secret, and tightening it would break the Expo
shell's own fetches for no gain. The distinction that matters is per *socket* —
strict ingress: no CORS headers at all, CSRF required on cookie-authenticated
state changes; LAN: permissive, no CSRF, bearer-authenticated.

Evidence: `src-tauri/src/server.rs`.

#### SEC-010: Pairing QR always advertises the LAN origin

The QR payload derives from `lan_origin()`, which derives from the local IP. A
remote phone receives an unreachable `192.168.x.x`-style origin. Trusting the
request `Host` header as a replacement would permit pairing redirection.

Required patch:

- Store a relay-provisioned, installation-bound `remoteOrigin`.
- Let the desktop choose LAN or remote pairing presentation.
- Generate remote QR payloads only from the trusted configured origin.
- Never derive the pairing origin from arbitrary forwarded headers.

Evidence: pairing helpers and `mobile.pair.begin` in
`src-tauri/src/server.rs`.

**Status: closed.** `remote.rs` stores an installation-bound origin, set by the
owner (later, by the relay's control plane at registration) and validated:
https only, no credentials, no path, no query, no IP or `localhost`.
`mobile.pair.begin` takes `target: "lan" | "remote"` and reads the remote
address from that configuration, never from the request. The pairing dialog only
offers the choice once the machine actually has a provisioned origin and remote
access is on. Tests: `sec010_*`, `origins_that_could_redirect_a_pairing_are_refused`.

#### SEC-011: Native downloads will not inherit browser cookies

The Expo shell intercepts `/file`, `/attachment`, and `/artifact-file` and uses
Expo FileSystem, a network stack separate from the WebView cookie jar. Removing
URL credentials without changing this path breaks authenticated downloads, and
the current final fallback opens the protected URL in a system browser.

Required patch:

- Pass the SecureStore Device credential as an `Authorization` bearer header to
  Expo FileSystem.
- Do not send protected download URLs to an unauthenticated browser fallback.
- Physically test all three endpoints on iOS and Android.

Evidence: `mobile/src/lib/download.ts` and
`mobile/src/components/WebViewPool.tsx`.

**Status: closed in code, not yet physically tested.** `downloadAndShare` takes
the SecureStore credential and sends it as an `Authorization` header, since Expo
FileSystem is a separate network stack from the WebView and inherits neither its
cookie jar nor the credential injected into page scope. The system-browser
fallback is gone: handing a protected URL to an app with no credential is a
confusing 401 on a good day and a protected URL in another app's synced history
on a bad one — a failed download now says so. **Still required before beta:**
physically exercise `/file`, `/attachment` and `/artifact-file` on iOS and
Android, per this finding's original patch note.

#### SEC-012: Peer master credentials travel over plaintext LAN WebSockets

Peer connections and socket splices construct `ws://...?...token=<peer master
token>`; byte proxies similarly attach the peer master token over plain HTTP.
A hostile LAN observer or proxy can steal fleet-level credentials. A public
relay does not directly create this bug, but exposing a gateway makes the mesh
part of the remote blast radius.

Required patch:

- Replace peer use of machine master credentials with dedicated, rotatable peer
  credentials.
- Authenticate peer identity independently of an address.
- Encrypt peer transport (`wss`/mTLS or an authenticated encrypted mesh
  transport), including socket splices and byte proxies.
- Never place peer credentials in URLs.

Evidence: `src-tauri/src/peernet.rs` and the peer pairing exchange in
`src-tauri/src/server.rs`.

**Status: closed (2026-08-08).** `MESH_VERSION` is **2**.

**Identity.** Each machine mints a self-signed CA plus a leaf once
(`mesh.rs`, `MeshIdentity`), and pairing exchanges the CA. Two certificates
rather than one because `rustls` builds a path to a trust *anchor*, and a bare
self-signed leaf is not usable as its own — pinning one would mean hand-rolling
the checks webpki already does correctly. The leaf's SAN is a synthetic
`<machine-id>.threadknot.mesh`, and the connecting side overrides resolution for
that name to whichever address hint it is trying. TLS therefore answers "is this
really machine X" while the address stays disposable, which is the invariant the
rest of `peernet` already depends on. Each peer gets its own single-anchor client
config: one shared store holding every peer's CA would let any paired machine
impersonate any other.

**Credentials.** Per-link and per-direction, independently rotatable, stored as a
hash on the accepting side (`Peer::outbound_credential` /
`inbound_credential_hash`). `Peer.token` is gone. Credentials travel in an
`Authorization` header on every path — the persistent peer socket, the `/term`
and `/browser` splices, and the byte proxy. `PeerRegistry::authenticate` scans
all pairs in constant time rather than being told which one to check, so a caller
cannot grind one pair at a time.

**Confidentiality.** A third listener, TLS on `0.0.0.0:<port+2>`, with
`IngressPolicy::Mesh`. It accepts exactly one thing: a peer credential in a
header. A master credential is a 403 there and a URL credential is a 400 — both
even when valid, for the same reason the strict ingress refuses them. Not mTLS:
the client is already authenticated by a rotatable per-link credential, and a
client-certificate verifier would need its root set rebuilt on every pair and
unpair for nothing.

**Pairing.** Two phases. `GET /api/peer/identity` is unauthenticated and returns
only public data — machine id, name, CA, mesh port — plus a single-use,
short-lived challenge. `POST /api/peer/pair` then runs on the mesh listener, over
TLS pinned to that CA, and proves knowledge of the responder's master token with
an HMAC instead of transmitting it. The proof covers the **fingerprint of the CA
the initiator actually saw**, so an attacker who intercepts phase 1 and
substitutes a certificate receives a proof computed over their own fingerprint,
which the real machine recomputes and rejects. That closes the
trust-on-first-use hole rather than accepting it. What is exchanged is a pair of
freshly minted per-link credentials; no master token is transmitted or stored.

**Mesh principal propagation** (folded in, per the build plan). `Principal::Peer`
carries the *originating* caller's grants, so a routed request is no longer
evaluated as the peer's owner. The assertion rides
`X-Threadknot-Mesh-Grants` for connection-scoped calls and a `mesh` sibling of
the request frame for the multiplexed peer socket — a sibling of `payload`, not a
field in it, so a client controlling a payload cannot smuggle one. It is
**discarded** unless the connection authenticated as a peer, and an unrecognised
capability name is dropped rather than honoured. `is_local_master()` and
`is_owner()` split what used to be one check: a peer acting for its own owner may
administer this machine (the fleet view always could) but never receives this
machine's master token, and a peer acting for a *device* now passes neither.
This also closes **SEC-003's residual gap**: the far side can inspect its own
thread's settings and enforce `signedBrowser` for itself.

**Legacy pairs are refused, not downgraded.** A v1 pair has no pinned CA and no
per-link credential, so the only way to reach it is the old plaintext URL.
Keeping that as a fallback would mean an attacker on the LAN need only wait for
the one machine nobody updated — the same as not shipping the fix. `peer.list`
reports `needsUpgrade` and Settings shows "update needed" rather than "offline",
because a pair that will never connect is a different problem from a sleeping
machine and "offline" sends someone hunting a network fault.

**Deliberately not relaxed.** The peer *browser* splice stays owner-only even
though propagation now makes `browser` + `mesh` sound. Relaxing it widens
authority over stored logins, which is a product decision rather than a
consequence of a transport change; `browser.rs` marks the exact check to change.

Tests: `sec012_*` in `tests/authorization_matrix.rs` (9 cases), `mesh.rs` and
`peers.rs` unit tests, and `scripts/mesh-smoke.py`, which pairs two real
instances and asserts the link forms over pinned TLS, that grants are enforced
across it, and that no secret reaches a client-facing response.

#### SEC-013: `server.json` permissions are not explicitly restricted

`server.json` contains the master credential and is written with
`std::fs::write` without explicitly enforcing owner-only permissions. Its
effective permissions depend on the parent directory and process umask.

Required patch:

- On Unix, create/repair `server.json` to mode `0600` and the sensitive data
  directory to `0700` where compatible with rename fallback behavior.
- Use owner-only ACLs on Windows.
- Apply the rule to every rewrite/migration path and add platform-appropriate
  tests.

Evidence: `Store::server_config` in `src-tauri/src/store.rs`; compare the
explicit `0600` handling in `src-tauri/src/peers.rs`.

**Status: closed on Unix.** `store::write_private` creates secret files at
`0600` (not chmod-after-write, which leaves a readable window), `restrict_file`
repairs an existing permissive `server.json` on every open, and `restrict_dir`
forces the data dir to `0700`. `mobile.json` gets the same treatment. Windows
still relies on the user profile's inherited ACL; an explicit owner-only ACL
pass is outstanding. Tests: `sec013_secret_files_are_owner_only`,
`an_existing_permissive_server_json_is_repaired`.

#### SEC-014: Public resource and connection exhaustion controls are incomplete

The app limits in-flight RPC requests per socket, but uses unbounded outbound
channels and has no public-ingress connection, bandwidth, or installation
quota model. Browser screencasts and large file transfers are especially
expensive.

Required patch:

- Replace unbounded per-connection queues with bounded queues and defined
  slow-client behavior.
- Enforce edge and origin limits for connection count, frame/request size,
  file size, request rate, bandwidth, terminal/browser sessions, and agent-run
  concurrency.
- Add per-installation quotas, billing alerts, and emergency disable controls.
- Load-test reconnect storms, offline installations, slow clients, and large
  transfers.

**Status: closed on the desktop (2026-08-08).** Every number lives in
`src-tauri/src/limits.rs` with the failure it prevents recorded beside it, so
"generous" is distinguishable from "arbitrary".

**Bounded outbound queues, with a policy per frame class.** One FIFO of 256
frames per socket. A **response** blocks up to 30s then disconnects — dropping it
would leave a client waiting forever on a request id that no longer exists
anywhere, and a socket that has accepted nothing in 30s is dead. A **persisted
event** blocks 10s then disconnects: nothing waits on the specific frame, but it
carries status, badges and notifications the UI has no other way to learn, so a
silent drop leaves the UI quietly wrong. A **delta** (`seq < 0`) is dropped when
full, because the persisted event that closes the message supersedes it.

Two deliberate deviations, both commented at the source. **One FIFO, not a
priority lane**: a lane letting responses overtake queued deltas would deliver a
delta *after* the event that superseded it, and the client applies deltas
unconditionally, so it would re-render text it had already replaced — ordering is
worth more than the latency a lane saves. **Broadcast `Lagged` still continues**:
it cannot be told whether the skipped frames were deltas or state, and
disconnecting would put any client slower than a streaming turn into a reconnect
loop.

Relayed peer frames are classified by a targeted parse, and anything unparseable
is treated as persisted — so a peer cannot make this machine drop state by
sending a malformed frame.

**Caps**: 32 MiB per `/ws` frame (8 MiB on `/term` and `/browser`); 50 req/s with
a 200 burst per *connection* rather than per principal, because a shared bucket
would be a mutex on the hottest path; 32 app sockets, 24 terminal sockets and 8
browser sockets per principal; 32 live ptys and 8 live Chromes globally, checked
at spawn rather than at the socket, because a pty outlives its socket and the
socket cap alone lets an attach/detach loop leak shells. Over-limit requests get
an error naming the limit rather than silence.

**Two real bugs surfaced by the work**, neither of them a quota: the peer
**browser splice had no session guard at all**, so it was invisible to revocation
and to the caps; and `/file`, `/attachment` and `/artifact-file` used
`std::fs::read`, so a 4 GB recording cost 4 GB of resident memory and a `<video>`
range request read the whole file to serve 200 KB. Those endpoints now stream in
64 KB chunks with an explicit `Content-Length`, and `Body::from_stream`
backpressures, so no size cap is needed.

Tests: `src-tauri/src/limits.rs`, `src-tauri/tests/limits.rs`, plus cases in
`sessions.rs` and `peernet.rs` — 17 in total.

**Not done, deliberately.** Per-installation *bandwidth* quotas are relay-side
and live there. An agent-run concurrency cap was skipped as a product decision
rather than a remote-exhaustion vector: starting a turn already needs `threads`,
and the cost is a CLI process the owner can see. The load test in Stage 8 is
still outstanding.

Evidence: `MAX_INFLIGHT_REQUESTS` and `unbounded_channel` in
`src-tauri/src/server.rs`, the peer request queue in `src-tauri/src/peernet.rs`,
and browser/file streaming endpoints.

## Proprietary relay security design

The relay has two distinct planes:

### Control plane

- Registers an installation using a connector public key or equivalent strong
  credential.
- Assigns the public hostname; the connector cannot choose or steal one.
- Issues short-lived connector authorization and supports rotation/revocation.
- Records only operational metadata necessary for ownership, health, limits,
  and incident response.
- Provides a global and per-installation kill switch.

### Data plane

- Accepts public HTTPS/WSS and routes solely by the control-plane binding.
- Carries streaming HTTP, normal WebSockets, binary terminal/browser sockets,
  backpressure, cancellation, and disconnects without protocol confusion.
- Opens no path to arbitrary LAN services; the connector dials only the strict
  Threadknot loopback listener.
- Enforces tenant isolation and resource quotas before forwarding.
- Returns a generic locked/offline response without exposing installation
  metadata.

The connector credential authenticates the installation to the relay. The
device credential authenticates the human-controlled client to local
Threadknot. Possession of either one never grants the authority of the other.

## Patch order

### Phase 0 — repair existing authorization bugs — **DONE (2026-08-07)**

1. ~~Fix Device `hello` master-token disclosure.~~
2. ~~Block Device peer-terminal laundering.~~
3. ~~Add centralized privileged-field checks for `ThreadSettings`.~~
4. ~~Enforce owner-only master-token file permissions.~~
5. ~~Add the initial authorization regression matrix.~~

Delivered alongside them, out of order because they were cheap and adjacent:
the capability model itself (SEC-004), immediate session revocation (SEC-008),
and WebSocket `Origin` validation (the WS half of SEC-009).

These fixes improve today's LAN/mobile product and should ship independently
of relay work.

### Phase 1 — capability-scoped pairing

1. ~~Define and persist versioned Device capabilities.~~
2. ~~Bind pending pairing codes to desktop-selected grants.~~
3. Add Master-only grant editing (**done** — `mobile.device.setCapabilities`)
   and explicit remote authorization (blocked on the relay).
4. ~~Enforce capabilities at local handlers, payload fields, byte endpoints,
   socket upgrades, and before mesh routing.~~
5. ~~Track active sessions and apply revocation/grant reductions immediately.~~
6. ~~Add capability-aware Settings and pairing UI with clear consequence
   labels.~~ Settings → paired phones now expands to a per-grant checklist
   (`PairedPhoneRow` / `CapabilityPicker`), and the pairing dialog picks the
   grants the QR will carry before it mints the code.

### Phase 2 — strict remote application ingress — DONE

1. ~~Add the loopback-only remote listener and ingress policy.~~
   `ingress.rs`; bound by `spawn_remote_listener` on `127.0.0.1:<port+1>`.
2. ~~Add browser session bootstrap/cookies and remove remote query
   credentials.~~ `POST /api/session`; `discovery.ts` omits the parameter
   entirely in remote mode.
3. ~~Add native bearer downloads and remove protected browser fallback.~~
   `mobile/src/lib/download.ts`. **Not physically tested on iOS/Android yet.**
4. ~~Add exact Origin/CORS/CSRF/header policy.~~
5. ~~Add provisioned `remoteOrigin` and LAN/remote QR selection.~~ `remote.rs`.

**How it fits together.** Two listeners: the unchanged `0.0.0.0` compatibility
ingress (LAN browsers, the Tauri webview, phones on the network, peers) and a
loopback-only strict ingress the connector dials. The policy is a property of
the socket because it cannot be anything else — a header claiming to come from
the relay is spoofable from the LAN, and the source address cannot distinguish
the connector from the desktop's own webview, which is also `127.0.0.1`.

On the strict ingress: credential-bearing query keys are a 400 even when valid;
the master credential is a 403 however presented; `/mcp` and `/api/peer/pair`
are not mounted at all; `POST /api/session` exchanges a one-time pairing code
(or a native device bearer) for a host-scoped `HttpOnly; Secure;
SameSite=Strict` cookie; cookie-authenticated state changes need a
double-submit token derived from the cookie; and responses carry HSTS, CSP with
`frame-ancestors 'none'`, `Referrer-Policy: no-referrer` and no CORS headers at
all. Remote access is off by default — the socket is always bound (it is
loopback, so binding it exposes nothing), which makes the switch instant in the
direction that matters: turning it off drops every browser session and every
socket opened through that ingress, and leaves the LAN untouched.

### Phase 2.5 — encrypted mesh — DONE (2026-08-08)

SEC-012, with mesh principal propagation folded in while the protocol was open.
See the finding above. This had to land before a public gateway, because exposing
one pulls the whole mesh into the remote blast radius.

### Phase 3 — proprietary relay and connector

1. Implement installation registration, hostname binding, and key rotation.
2. Implement the outbound multiplexed connector to the strict listener.
3. Support every HTTP/WS/binary streaming path with bounded backpressure.
4. Add tenant isolation, quotas, rate limits, health/offline behavior, and kill
   switches.
5. Resolve and implement the relay TLS/confidentiality model.

### Phase 4 — release verification

1. Run the full principal/capability/endpoint/payload/local-peer matrix.
2. Test iOS, Android, and ordinary browsers over the real relay.
3. Test immediate revoke, capability reduction, remote-disable, key rotation,
   offline/reconnect, and relay failover.
4. Perform load/fuzz testing on HTTP, WebSocket, terminal, browser, and tunnel
   framing.
5. Complete an external threat-model review and penetration test.
6. Document incident response, credential rotation, privacy behavior, and the
   relay operator's visibility before public beta.

## Release gates

Threadknot may not expose the current LAN listener directly through the public
relay. An external beta requires all P0/P1 findings above to be closed or an
explicitly documented equivalent control, plus:

- No master credential accepted or disclosed on remote ingress.
- Capability grants selected by the desktop owner and enforced before routing.
- Immediate device/session revocation confirmed against live sockets.
- Cross-tenant routing and hostname-claim tests passing.
- No credentials in remote URLs or routine logs.
- No peer credential in any URL or plaintext frame (SEC-012).
- Physical mobile download and reconnect passes.
- External security review completed with no unresolved critical/high finding.
