import AsyncStorage from '@react-native-async-storage/async-storage';
import * as SecureStore from 'expo-secure-store';
import * as React from 'react';
import { Platform } from 'react-native';
import {
  ApiError,
  normalizeServerUrl,
  pairServer,
  pairServerWithCode,
  parsePairingPayload,
  probeServer,
  sendTestPush,
  unpairDevice,
} from './api';
import { deviceLabel, registerPushForServer } from './push';
import type { ServerProfile } from './types';

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

  // Load metadata + credentials once at startup, then best-effort re-sync the
  // push registration with every server (token rollover, changed prefs).
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
          if (c) void registerPushForServer(p, c).catch(() => undefined);
        }
      } finally {
        setLoaded(true);
      }
    })();
  }, []);

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

      async addServer(input: string, nickname?: string) {
        const { baseUrl, token } = normalizeServerUrl(input);
        if (!token) {
          throw new ApiError(
            'bad-url',
            "The URL is missing its token. In Threadknot, open Settings and copy the full LAN URL (it ends with ?token=…)."
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
          notificationsEnabled: true,
          createdAt: nowIso(),
          updatedAt: nowIso(),
        };
        setCredentials((c) => ({ ...c, [id]: pair.credential }));
        commit([...profilesRef.current, profile], id);
        void registerPushForServer(profile, pair.credential).catch(() => undefined);
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
          // doesn't accumulate a dead entry per re-pair.
          const oldCred = credsRef.current[id];
          if (oldCred) void unpairDevice(existing.baseUrl, oldCred).catch(() => undefined);
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
        void registerPushForServer(profile, pair.credential).catch(() => undefined);
        return profile;
      },

      async removeServer(id: string) {
        const p = profilesRef.current.find((x) => x.id === id);
        const cred = credsRef.current[id];
        if (p && cred) void unpairDevice(p.baseUrl, cred).catch(() => undefined);
        await SecureStore.deleteItemAsync(credKey(id)).catch(() => undefined);
        setCredentials((c) => {
          const next = { ...c };
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
            commit(
              profilesRef.current.map((p) =>
                p.id === id
                  ? { ...p, baseUrl, serverName: info.name, version: info.version, updatedAt: nowIso() }
                  : p
              ),
              activeRef.current
            );
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
          updatedAt: nowIso(),
        };
        commit(
          profilesRef.current.map((p) => (p.id === id ? updated : p)),
          activeRef.current
        );
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
        await registerPushForServer(updated, cred);
      },

      async testPush(id: string) {
        const profile = requireProfile(id);
        const cred = requireCredential(id);
        await registerPushForServer(profile, cred).catch(() => undefined);
        await sendTestPush(profile.baseUrl, cred);
      },
    };
  }, [loaded, profiles, activeId, credentials, commit]);

  return <ServersContext.Provider value={value}>{children}</ServersContext.Provider>;
}
