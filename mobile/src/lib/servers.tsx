import AsyncStorage from '@react-native-async-storage/async-storage';
import * as SecureStore from 'expo-secure-store';
import * as React from 'react';
import { Platform } from 'react-native';
import {
  ApiError,
  bootstrapSession,
  endSession,
  normalizeServerUrl,
  pairServer,
  isRelayOrigin,
  pairServerWithCode,
  parsePairingPayload,
  probeServer,
  sendTestPush,
  unpairDevice,
} from './api';
import { deviceLabel, registerPushForServer } from './push';
import type { IngressKind, RemoteSession, ServerProfile } from './types';

const STORE_KEY = 'threadknot.servers.v1';

function credKey(profileId: string): string {
  return `threadknot.cred.${profileId}`;
}

function uid(): string {
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

function nowIso(): string {
  return new Date().toISOString();
}

interface PersistedState {
  profiles: ServerProfile[];
  activeId: string | null;
}

export interface ServersValue {
  loaded: boolean;
  profiles: ServerProfile[];
  activeId: string | null;
  active: ServerProfile | null;
  /** In-memory device credentials keyed by profile id (from SecureStore). */
  credentials: Record<string, string>;
  /** Live cookie sessions for `ingress: 'remote'` profiles, keyed by profile id.
   * Absent for LAN profiles, which authenticate by token and have no session. */
  sessions: Record<string, RemoteSession>;
  /** Re-open a remote profile's cookie session — after a revoke, a 30-day idle
   * expiry, or a spell offline. No-op for a LAN profile. */
  refreshSession(id: string): Promise<void>;
  addServer(input: string, nickname?: string): Promise<ServerProfile>;
  /** Add (or re-pair) a server from a scanned `threadknot://pair?…` QR. */
  addServerByScan(payload: string, nickname?: string): Promise<ServerProfile>;
  removeServer(id: string): Promise<void>;
  renameServer(id: string, name: string): Promise<void>;
  /** Point an existing profile at a new URL (same serverId), re-pairing if the
   * pasted URL carries a fresh master token. */
  updateUrl(id: string, input: string): Promise<void>;
  setActive(id: string): void;
  setNotifications(id: string, enabled: boolean): Promise<void>;
  testPush(id: string): Promise<void>;
}

const ServersContext = React.createContext<ServersValue | null>(null);

export function useServers(): ServersValue {
  const v = React.useContext(ServersContext);
  if (!v) throw new Error('useServers outside ServersProvider');
  return v;
}

export function ServersProvider({ children }: { children: React.ReactNode }) {
  const [loaded, setLoaded] = React.useState(false);
  const [profiles, setProfiles] = React.useState<ServerProfile[]>([]);
  const [activeId, setActiveId] = React.useState<string | null>(null);
  const [credentials, setCredentials] = React.useState<Record<string, string>>({});
  const [sessions, setSessions] = React.useState<Record<string, RemoteSession>>({});
  // Monotonic, app-wide: the WebView keys off it, so it only has to change.
  const sessionGen = React.useRef(0);

  const persist = React.useCallback((next: PersistedState) => {
    void AsyncStorage.setItem(STORE_KEY, JSON.stringify(next)).catch(() => undefined);
  }, []);

  const commit = React.useCallback(
    (nextProfiles: ServerProfile[], nextActive: string | null) => {
      setProfiles(nextProfiles);
      setActiveId(nextActive);
      persist({ profiles: nextProfiles, activeId: nextActive });
    },
    [persist]
  );

  const profilesRef = React.useRef(profiles);
  profilesRef.current = profiles;
  const activeRef = React.useRef(activeId);
  activeRef.current = activeId;
  const credsRef = React.useRef(credentials);
  credsRef.current = credentials;

  /** Open (or re-open) a cookie session and learn which door this origin is.
   *
   * Returns both halves so the caller can persist `ingress` on the profile and
   * use the CSRF token immediately — React state is not readable in the same
   * tick, and the very next thing a caller does is a state-changing POST. */
  const openSession = React.useCallback(
    async (
      profileId: string,
      baseUrl: string,
      credential: string
    ): Promise<{ ingress: IngressKind; session: RemoteSession | null }> => {
      const generation = ++sessionGen.current;
      const probe = await bootstrapSession(baseUrl, credential, generation);
      setSessions((prev) => {
        const next = { ...prev };
        if (probe.session) next[profileId] = probe.session;
        else delete next[profileId];
        return next;
      });
      return probe;
    },
    []
  );

  /** Record which door a profile turned out to be, if it changed. */
  const rememberIngress = React.useCallback(
    (profileId: string, ingress: IngressKind) => {
      const current = profilesRef.current.find((p) => p.id === profileId);
      if (!current || current.ingress === ingress) return;
      commit(
        profilesRef.current.map((p) => (p.id === profileId ? { ...p, ingress } : p)),
        activeRef.current
      );
    },
    [commit]
  );

  // Load metadata + credentials once at startup, then per server: open a cookie
  // session if it needs one, and best-effort re-sync the push registration
  // (token rollover, changed prefs). Session first — the push call needs its
  // CSRF token, and the WebView needs the cookie in the jar before it mounts.
  React.useEffect(() => {
    void (async () => {
      try {
        const raw = await AsyncStorage.getItem(STORE_KEY);
        const state: PersistedState = raw ? JSON.parse(raw) : { profiles: [], activeId: null };
        const creds: Record<string, string> = {};
        for (const p of state.profiles) {
          const c = await SecureStore.getItemAsync(credKey(p.id)).catch(() => null);
          if (c) creds[p.id] = c;
        }
        setProfiles(state.profiles);
        setActiveId(state.activeId ?? state.profiles[0]?.id ?? null);
        setCredentials(creds);
        for (const p of state.profiles) {
          const c = creds[p.id];
          if (!c) continue;
          void (async () => {
            let csrf: string | undefined;
            // A profile already known to be `compat` is not probed again: the
            // LAN listener has no session to give, and an unreachable one would
            // only cost a timeout ahead of the push sync it also blocks.
            if (p.ingress !== 'compat') {
              const probe = await openSession(p.id, p.baseUrl, c).catch(() => null);
              if (probe) {
                rememberIngress(p.id, probe.ingress);
                csrf = probe.session?.csrf;
              }
            }
            await registerPushForServer(p, c, csrf).catch(() => undefined);
          })();
        }
      } finally {
        setLoaded(true);
      }
    })();
    // Runs exactly once at mount by design; the callbacks it uses are stable.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const value = React.useMemo<ServersValue>(() => {
    const requireProfile = (id: string): ServerProfile => {
      const p = profilesRef.current.find((x) => x.id === id);
      if (!p) throw new Error('Unknown server');
      return p;
    };
    const requireCredential = (id: string): string => {
      const c = credsRef.current[id];
      if (!c) throw new Error('Missing credential for this server — remove and re-add it.');
      return c;
    };

    return {
      loaded,
      profiles,
      activeId,
      active: profiles.find((p) => p.id === activeId) ?? null,
      credentials,
      sessions,

      async refreshSession(id: string) {
        const profile = requireProfile(id);
        const cred = requireCredential(id);
        const probe = await openSession(id, profile.baseUrl, cred);
        rememberIngress(id, probe.ingress);
      },

      async addServer(input: string, nickname?: string) {
        const { baseUrl, token } = normalizeServerUrl(input);
        if (!token) {
          // Two different problems wear the same shape here, and the old message
          // assumed the wrong one. A relay address has NO token to be missing —
          // the strict ingress refuses a credential in a URL with a 400 — so
          // telling someone to go and find one sent them looking for something
          // that cannot exist. A pairing code is the answer for both cases; it is
          // simply mandatory for the remote one.
          throw new ApiError(
            'bad-url',
            isRelayOrigin(baseUrl)
              ? 'A Threadknot relay address is paired with a code, not a token — there is no token for it to be missing. On the desktop: Settings → pair a phone → remote, then scan the QR or type the code shown beneath it.'
              : "This URL has no token. Either paste the full LAN URL from Threadknot Settings (it ends with ?token=…), or leave the URL as-is and enter a pairing code from Settings → pair a phone."
          );
        }
        const info = await probeServer(baseUrl, token);
        const existing = profilesRef.current.find((p) => p.serverId === info.serverId);
        if (existing) {
          throw new ApiError(
            'server',
            `This server is already configured as “${existing.name}”. Edit it instead.`
          );
        }
        const pair = await pairServer(baseUrl, token, deviceLabel(), Platform.OS);
        const id = uid();
        await SecureStore.setItemAsync(credKey(id), pair.credential, {
          keychainAccessible: SecureStore.WHEN_UNLOCKED_THIS_DEVICE_ONLY,
        });
        const profile: ServerProfile = {
          id,
          serverId: pair.serverId,
          name: nickname?.trim() || pair.serverName || baseUrl.replace(/^https?:\/\//, ''),
          serverName: pair.serverName,
          version: pair.version,
          baseUrl,
          deviceId: pair.deviceId,
          // The pasted-URL path carries a master token, which only the compat
          // listener accepts at all (`mobile_pair_handler` refuses it remotely),
          // so this is settled before we ask. Asking anyway keeps one source of
          // truth: a profile later re-pointed at a relay origin re-probes.
          ingress: 'compat',
          notificationsEnabled: true,
          createdAt: nowIso(),
          updatedAt: nowIso(),
        };
        setCredentials((c) => ({ ...c, [id]: pair.credential }));
        commit([...profilesRef.current, profile], id);
        const probe = await openSession(id, baseUrl, pair.credential).catch(() => null);
        if (probe) rememberIngress(id, probe.ingress);
        void registerPushForServer(profile, pair.credential, probe?.session?.csrf).catch(
          () => undefined
        );
        return profile;
      },

      async addServerByScan(payload: string, nickname?: string) {
        const { baseUrl, code } = parsePairingPayload(payload);
        // The code is single-use, so unlike the pasted-URL path we cannot probe
        // for a duplicate first — redeeming it IS how we learn which server
        // this is. That makes re-scanning a configured server the natural way
        // to re-pair a phone whose credential was revoked or lost.
        const pair = await pairServerWithCode(baseUrl, code, deviceLabel(), Platform.OS);
        const existing = profilesRef.current.find((p) => p.serverId === pair.serverId);
        const id = existing?.id ?? uid();

        if (existing) {
          // Retire the stale registration so the desktop's paired-phones list
          // doesn't accumulate a dead entry per re-pair. Sign the old cookie
          // session out *first*, and await it: un-pairing revokes sessions
          // server-side but leaves the dead cookie in this phone's jar, and the
          // strict ingress answers an unresolvable cookie with 401 instead of
          // falling through to the `Authorization` header — so the leftover
          // would break the fresh credential we are about to store.
          const oldCred = credsRef.current[id];
          const oldSession = sessions[id];
          if (oldCred) {
            // Only when there is a session to end — a LAN profile has none, and
            // waiting out a request timeout against a server that may not be
            // reachable is not something a re-pair should do for nothing.
            if (oldSession) await endSession(existing.baseUrl, oldSession.csrf);
            void unpairDevice(existing.baseUrl, oldCred, oldSession?.csrf).catch(() => undefined);
          }
          setSessions((prev) => {
            const next = { ...prev };
            delete next[id];
            return next;
          });
        }

        await SecureStore.setItemAsync(credKey(id), pair.credential, {
          keychainAccessible: SecureStore.WHEN_UNLOCKED_THIS_DEVICE_ONLY,
        });
        const profile: ServerProfile = {
          id,
          serverId: pair.serverId,
          name:
            nickname?.trim() ||
            existing?.name ||
            pair.serverName ||
            baseUrl.replace(/^https?:\/\//, ''),
          serverName: pair.serverName,
          version: pair.version,
          baseUrl,
          deviceId: pair.deviceId,
          // Unknown until asked. The QR's origin does not settle it: a relay
          // origin is https, but so is a Tailscale Funnel or ngrok tunnel to the
          // compat listener, and reading the scheme as "remote" would put a LAN
          // profile into cookie mode where there is no cookie to be had.
          ingress: undefined,
          notificationsEnabled: existing?.notificationsEnabled ?? true,
          createdAt: existing?.createdAt ?? nowIso(),
          updatedAt: nowIso(),
        };
        setCredentials((c) => ({ ...c, [id]: pair.credential }));
        commit(
          existing
            ? profilesRef.current.map((p) => (p.id === id ? profile : p))
            : [...profilesRef.current, profile],
          id
        );
        // Awaited, unlike the push sync: on a remote origin the WebView cannot
        // authenticate at all until the cookie is in the jar, and the caller
        // navigates to it the moment this resolves.
        const probe = await openSession(id, baseUrl, pair.credential).catch(() => null);
        if (probe) rememberIngress(id, probe.ingress);
        void registerPushForServer(profile, pair.credential, probe?.session?.csrf).catch(
          () => undefined
        );
        return profile;
      },

      async removeServer(id: string) {
        const p = profilesRef.current.find((x) => x.id === id);
        const cred = credsRef.current[id];
        const session = sessions[id];
        if (p && cred) {
          // Cookie first, credential second — same reason as the re-pair path:
          // the un-pair kills the session server-side but only this response
          // clears the cookie out of the phone's jar, and a dead cookie left
          // there 401s requests that the bearer alone would have satisfied.
          // Skipped when there is no session (every LAN profile), so removing an
          // unreachable server stays instant.
          if (session) await endSession(p.baseUrl, session.csrf);
          void unpairDevice(p.baseUrl, cred, session?.csrf).catch(() => undefined);
        }
        await SecureStore.deleteItemAsync(credKey(id)).catch(() => undefined);
        setCredentials((c) => {
          const next = { ...c };
          delete next[id];
          return next;
        });
        setSessions((s) => {
          const next = { ...s };
          delete next[id];
          return next;
        });
        const nextProfiles = profilesRef.current.filter((x) => x.id !== id);
        const nextActive =
          activeRef.current === id ? (nextProfiles[0]?.id ?? null) : activeRef.current;
        commit(nextProfiles, nextActive);
      },

      async renameServer(id: string, name: string) {
        const trimmed = name.trim();
        if (!trimmed) return;
        commit(
          profilesRef.current.map((p) =>
            p.id === id ? { ...p, name: trimmed, updatedAt: nowIso() } : p
          ),
          activeRef.current
        );
      },

      async updateUrl(id: string, input: string) {
        const profile = requireProfile(id);
        const { baseUrl, token } = normalizeServerUrl(input);
        // Prefer the existing device credential: a URL change (new tunnel, new
        // LAN IP) does not need re-pairing as long as the serverId matches.
        const cred = credsRef.current[id];
        if (cred) {
          try {
            const info = await probeServer(baseUrl, cred);
            if (info.serverId !== profile.serverId) {
              throw new ApiError('server', 'That URL points at a DIFFERENT Threadknot server. Add it as a new server instead.');
            }
            // The old origin's cookie is worthless at the new one (the session
            // cookie is host-scoped, deliberately, so it can never reach a
            // sibling installation on the relay domain) — drop it and ask the
            // new address which door it is.
            const previous = sessions[id];
            if (previous) await endSession(profile.baseUrl, previous.csrf);
            commit(
              profilesRef.current.map((p) =>
                p.id === id
                  ? { ...p, baseUrl, serverName: info.name, version: info.version, updatedAt: nowIso() }
                  : p
              ),
              activeRef.current
            );
            const probe = await openSession(id, baseUrl, cred).catch(() => null);
            if (probe) rememberIngress(id, probe.ingress);
            return;
          } catch (e) {
            // Credential revoked server-side → fall through to token re-pair.
            if (!(e instanceof ApiError) || e.code !== 'unauthorized' || !token) throw e;
          }
        }
        if (!token) {
          throw new ApiError(
            'unauthorized',
            'This device is no longer paired. Paste the full URL (with ?token=…) to re-pair.'
          );
        }
        const info = await probeServer(baseUrl, token);
        if (info.serverId !== profile.serverId) {
          throw new ApiError('server', 'That URL points at a DIFFERENT Threadknot server. Add it as a new server instead.');
        }
        const pair = await pairServer(baseUrl, token, deviceLabel(), Platform.OS);
        await SecureStore.setItemAsync(credKey(id), pair.credential, {
          keychainAccessible: SecureStore.WHEN_UNLOCKED_THIS_DEVICE_ONLY,
        });
        setCredentials((c) => ({ ...c, [id]: pair.credential }));
        const updated: ServerProfile = {
          ...profile,
          baseUrl,
          serverName: pair.serverName,
          version: pair.version,
          deviceId: pair.deviceId,
          // This branch re-paired with a master token, which only the compat
          // listener accepts.
          ingress: 'compat',
          updatedAt: nowIso(),
        };
        commit(
          profilesRef.current.map((p) => (p.id === id ? updated : p)),
          activeRef.current
        );
        setSessions((s) => {
          const next = { ...s };
          delete next[id];
          return next;
        });
        void registerPushForServer(updated, pair.credential).catch(() => undefined);
      },

      setActive(id: string) {
        if (!profilesRef.current.some((p) => p.id === id)) return;
        commit(profilesRef.current, id);
      },

      async setNotifications(id: string, enabled: boolean) {
        const profile = requireProfile(id);
        const cred = requireCredential(id);
        const updated = { ...profile, notificationsEnabled: enabled, updatedAt: nowIso() };
        commit(
          profilesRef.current.map((p) => (p.id === id ? updated : p)),
          activeRef.current
        );
        await registerPushForServer(updated, cred, sessions[id]?.csrf);
      },

      async testPush(id: string) {
        const profile = requireProfile(id);
        const cred = requireCredential(id);
        const csrf = sessions[id]?.csrf;
        await registerPushForServer(profile, cred, csrf).catch(() => undefined);
        await sendTestPush(profile.baseUrl, cred, csrf);
      },
    };
  }, [loaded, profiles, activeId, credentials, sessions, commit, openSession, rememberIngress]);

  return <ServersContext.Provider value={value}>{children}</ServersContext.Provider>;
}
