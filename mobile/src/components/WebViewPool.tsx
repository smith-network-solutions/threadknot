import { Button } from '@/components/ui/button';
import { Text } from '@/components/ui/text';
import type { ConnState } from '@/components/ServerSwitcher';
import { sameOrigin } from '@/lib/api';
import { downloadAndShare, isFileUrl } from '@/lib/download';
import type { RemoteSession, ServerProfile, WebToNativeMessage } from '@/lib/types';
import * as Clipboard from 'expo-clipboard';
import * as React from 'react';
import { ActivityIndicator, AppState, Linking, Platform, View } from 'react-native';
import { WebView, type WebViewMessageEvent } from 'react-native-webview';

export interface PoolNav {
  projectId?: string;
  threadId: string;
}

export interface PoolHandle {
  /** Deliver (or queue until the page reports ready) a thread navigation. */
  navigate(profileId: string, nav: PoolNav): void;
  reload(profileId: string): void;
}

interface Props {
  profiles: ServerProfile[];
  credentials: Record<string, string>;
  /** Cookie sessions for `ingress: 'remote'` profiles, keyed by profile id. */
  sessions: Record<string, RemoteSession>;
  activeId: string | null;
  onConnChange(profileId: string, conn: ConnState): void;
  /** Re-open a remote profile's session (revoked, idled out, or offline). */
  onSessionRetry(profileId: string): Promise<void>;
}

/** How many server WebViews stay warm. Switching among the most recent ones
 * is instant; older ones remount with a brief load. */
const POOL_CAP = 3;

function parseMessage(raw: string): WebToNativeMessage | null {
  try {
    const msg = JSON.parse(raw) as unknown;
    if (typeof msg !== 'object' || msg === null) return null;
    const t = (msg as { type?: unknown }).type;
    if (
      t === 'ready' ||
      t === 'routeChanged' ||
      t === 'connectionChanged' ||
      t === 'reloadRequest' ||
      (t === 'clipboardRead' && typeof (msg as { requestId?: unknown }).requestId === 'string')
    ) {
      return msg as WebToNativeMessage;
    }
    return null;
  } catch {
    return null;
  }
}

export const WebViewPool = React.forwardRef<PoolHandle, Props>(function WebViewPool(
  { profiles, credentials, sessions, activeId, onConnChange, onSessionRetry },
  ref
) {
  // LRU of mounted profile ids, most recent last; always contains activeId.
  const [mounted, setMounted] = React.useState<string[]>([]);
  const [downloading, setDownloading] = React.useState(false);
  const [retrying, setRetrying] = React.useState<string | null>(null);
  const views = React.useRef(new Map<string, WebView | null>());
  const ready = React.useRef(new Set<string>());
  const pendingNav = React.useRef(new Map<string, PoolNav>());

  React.useEffect(() => {
    if (!activeId) return;
    setMounted((prev) => {
      const next = prev.filter((id) => id !== activeId && profiles.some((p) => p.id === id));
      next.push(activeId);
      while (next.length > POOL_CAP) {
        const evicted = next.shift();
        if (evicted) {
          ready.current.delete(evicted);
          views.current.delete(evicted);
        }
      }
      return next;
    });
  }, [activeId, profiles]);

  // A new session generation remounts that profile's WebView (see the `key`
  // below), so the page it had reported ready is gone. Forget the flag or the
  // next queued push-tap navigation is injected into a view that never arrives.
  const generations = React.useRef(new Map<string, number>());
  React.useEffect(() => {
    for (const [id, session] of Object.entries(sessions)) {
      if (generations.current.get(id) !== session.generation) {
        generations.current.set(id, session.generation);
        ready.current.delete(id);
      }
    }
  }, [sessions]);

  const dispatchToWeb = React.useCallback((profileId: string, message: object) => {
    const view = views.current.get(profileId);
    if (!view) return;
    const msg = JSON.stringify(message);
    view.injectJavaScript(
      `window.__threadknotNative && window.__threadknotNative.dispatch(${JSON.stringify(msg)}); true;`
    );
  }, []);

  // React Native owns the reliable foreground signal. The JavaScript inside a
  // suspended WebView may never observe its WebSocket closing, so tell every
  // warm page to renew its connection and replay its selected chat on wake.
  const missingSessions = React.useRef<string[]>([]);
  React.useEffect(() => {
    // `!== 'compat'` rather than `=== 'remote'`: a profile paired while the
    // session bootstrap happened to fail is left with the door unknown, and
    // "unknown" renders as LAN. Re-probing it here is what corrects that
    // without waiting for the next cold start.
    missingSessions.current = profiles
      .filter((p) => p.ingress !== 'compat' && !sessions[p.id])
      .map((p) => p.id);
  }, [profiles, sessions]);
  React.useEffect(() => {
    let wasAway = AppState.currentState !== 'active';
    const subscription = AppState.addEventListener('change', (next) => {
      if (next === 'active') {
        if (wasAway) {
          for (const id of views.current.keys()) {
            dispatchToWeb(id, { type: 'resume' });
          }
          // A phone that was asleep may have lost its session to the 30-day
          // idle expiry, a revoke, or simply having been offline when the app
          // started. Bounded by resume events, so this cannot spin.
          for (const id of missingSessions.current) {
            void onSessionRetry(id).catch(() => undefined);
          }
        }
        wasAway = false;
      } else {
        wasAway = true;
      }
    });
    return () => subscription.remove();
  }, [dispatchToWeb, onSessionRetry]);

  const inject = React.useCallback(
    (profileId: string, nav: PoolNav) => {
      dispatchToWeb(profileId, {
        type: 'navigate',
        projectId: nav.projectId,
        threadId: nav.threadId,
      });
    },
    [dispatchToWeb]
  );

  /** Real navigation reload: the bundle is refetched (the server serves
   * `index.html` as `no-cache`), not just the SPA re-mounted. */
  const reloadView = React.useCallback((profileId: string) => {
    ready.current.delete(profileId);
    views.current.get(profileId)?.reload();
  }, []);

  React.useImperativeHandle(
    ref,
    () => ({
      navigate(profileId, nav) {
        if (ready.current.has(profileId)) inject(profileId, nav);
        else pendingNav.current.set(profileId, nav);
      },
      reload: reloadView,
    }),
    [inject, reloadView]
  );

  const onMessage = React.useCallback(
    (profileId: string) => (event: WebViewMessageEvent) => {
      const msg = parseMessage(event.nativeEvent.data);
      if (!msg) return;
      if (msg.type === 'ready') {
        ready.current.add(profileId);
        const nav = pendingNav.current.get(profileId);
        if (nav) {
          pendingNav.current.delete(profileId);
          inject(profileId, nav);
        }
      } else if (msg.type === 'connectionChanged') {
        onConnChange(profileId, msg.conn);
      } else if (msg.type === 'reloadRequest') {
        // Pull-to-refresh in the web UI. Reloading from here (rather than
        // letting the page call location.reload()) restarts the WebView's own
        // load, so a wedged page recovers and `startInLoadingState` shows the
        // native connecting spinner.
        reloadView(profileId);
      } else if (msg.type === 'clipboardRead') {
        void Clipboard.getStringAsync()
          .then((text) => {
            dispatchToWeb(profileId, {
              type: 'clipboardResult',
              requestId: msg.requestId,
              text,
            });
          })
          .catch((error: unknown) => {
            dispatchToWeb(profileId, {
              type: 'clipboardResult',
              requestId: msg.requestId,
              error: error instanceof Error ? error.message : String(error),
            });
          });
      }
    },
    [dispatchToWeb, inject, onConnChange, reloadView]
  );

  return (
    <View className="flex-1 bg-background">
      {mounted.map((id) => {
        const profile = profiles.find((p) => p.id === id);
        const credential = profile ? credentials[id] : undefined;
        if (!profile || !credential) return null;
        const isActive = id === activeId;
        const remote = profile.ingress === 'remote';
        const session = sessions[id];

        // A remote page has no way to authenticate itself: the strict ingress
        // refuses a credential in a URL outright, so the cookie in the platform
        // jar is the only thing that will open `/ws`, an attachment or a
        // terminal. Mounting before it exists would give a WebView whose every
        // request 401s, and — worse on iOS — react-native-webview copies the
        // shared jar into the WKWebView store when the view loads its source,
        // so a cookie that arrives afterwards is not picked up until a reload.
        if (remote && !session) {
          if (!isActive) return null;
          return (
            <View key={id} className="absolute inset-0" style={{ zIndex: 1 }}>
              <View className="flex-1 items-center justify-center gap-4 bg-background px-8">
                <Text className="text-center text-lg font-semibold">
                  Signing in to {profile.name}
                </Text>
                <Text className="text-center text-sm text-muted-foreground">
                  {profile.baseUrl}
                  {'\n'}This machine is reached over the Threadknot relay, which needs a session
                  before the console can load.
                </Text>
                <Button
                  variant="outline"
                  disabled={retrying === id}
                  onPress={() => {
                    setRetrying(id);
                    void onSessionRetry(id)
                      .catch(() => undefined)
                      .finally(() => setRetrying(null));
                  }}
                >
                  {retrying === id ? (
                    <ActivityIndicator color="#d8dde9" />
                  ) : (
                    <Text>Try again</Text>
                  )}
                </Button>
              </View>
            </View>
          );
        }

        // Two shapes of bootstrap, because the two ingresses authenticate
        // differently and the page has to know which it is in:
        //
        // - LAN (`compat`): the device credential goes into page scope, and the
        //   page appends it as `?token=`. Kept deliberately — SEC-006 leaves
        //   query authentication on the LAN listener during the migration
        //   window, because the session cookie is `Secure` and a browser on a
        //   plain-http LAN address drops it silently.
        // - Remote: no credential at all. Authentication is the `HttpOnly`
        //   cookie the shell already bootstrapped, which the page can neither
        //   read nor leak; what it does get is the CSRF half, which is exactly
        //   the part it is meant to hold.
        const bootstrap = JSON.stringify({
          ...(remote ? { csrf: session?.csrf ?? '' } : { token: credential }),
          serverId: profile.serverId,
          platform: Platform.OS,
          capabilities: ['clipboard-read', 'reload', ...(remote ? ['cookie-session'] : [])],
        });
        return (
          <View
            // The session generation is part of the key so a re-bootstrapped
            // session gets a brand-new native web view. That remount is what
            // makes the fresh cookie take effect: the shared-jar copy happens
            // when the view loads its source, not continuously.
            key={remote ? `${id}:${session?.generation ?? 0}` : id}
            pointerEvents={isActive ? 'auto' : 'none'}
            className="absolute inset-0"
            style={{ opacity: isActive ? 1 : 0, zIndex: isActive ? 1 : 0 }}
          >
            <WebView
              ref={(v) => {
                views.current.set(id, v);
              }}
              source={{ uri: `${profile.baseUrl}/` }}
              // Injected into page scope before any app code runs, so nothing
              // secret has to ride the URL or web storage.
              injectedJavaScriptBeforeContentLoaded={`window.__THREADKNOT_NATIVE__ = ${bootstrap}; true;`}
              // iOS only, and load-bearing for the remote path: without it the
              // WKWebView keeps a private cookie store and never sees the
              // session that this app's own HTTP stack put in
              // `NSHTTPCookieStorage`. Android's WebView already shares its
              // CookieManager with React Native's OkHttp jar.
              //
              // Set unconditionally rather than only for remote profiles: this
              // is a construction-time native option, so flipping it would mean
              // remounting, and it costs a LAN profile nothing — the session
              // cookie is host-scoped and `Secure`, so it is never offered to
              // another installation's hostname or to a plain-http LAN address.
              sharedCookiesEnabled
              onMessage={onMessage(id)}
              onContentProcessDidTerminate={() => {
                // iOS reclaims background WebViews under memory pressure.
                ready.current.delete(id);
                views.current.get(id)?.reload();
              }}
              onShouldStartLoadWithRequest={(req) => {
                // File downloads must not become page navigations — the SPA
                // has no back button, so the user would be stranded.
                if (isFileUrl(req.url, profile.baseUrl)) {
                  setDownloading(true);
                  void downloadAndShare(req.url, credential).finally(() =>
                    setDownloading(false)
                  );
                  return false;
                }
                // Origin equality, not a prefix match: `baseUrl` has no trailing
                // slash, so `startsWith` would also accept
                // `https://<host>.attacker.example` as same-origin and render an
                // attacker's page inside the shell — with our cookie jar
                // attached to anything it could talk the WebView into loading.
                if (sameOrigin(req.url, profile.baseUrl) || req.url.startsWith('about:')) {
                  return true;
                }
                void Linking.openURL(req.url).catch(() => undefined);
                return false;
              }}
              onFileDownload={({ nativeEvent }) => {
                // iOS: WKWebView flagged a response as a download attachment.
                setDownloading(true);
                void downloadAndShare(nativeEvent.downloadUrl, credential).finally(() =>
                  setDownloading(false)
                );
              }}
              setSupportMultipleWindows={false}
              domStorageEnabled
              allowsBackForwardNavigationGestures={false}
              keyboardDisplayRequiresUserAction={false}
              allowsInlineMediaPlayback
              contentInsetAdjustmentBehavior="never"
              style={{ flex: 1, backgroundColor: '#0b0d12' }}
              startInLoadingState
              renderLoading={() => (
                <View className="absolute inset-0 items-center justify-center bg-background">
                  <ActivityIndicator color="#d9a35c" />
                  <Text className="mt-3 text-sm text-muted-foreground">
                    Connecting to {profile.name}…
                  </Text>
                </View>
              )}
              renderError={() => (
                <View className="absolute inset-0 items-center justify-center gap-4 bg-background px-8">
                  <Text className="text-center text-lg font-semibold">
                    Can’t reach {profile.name}
                  </Text>
                  <Text className="text-center text-sm text-muted-foreground">
                    {profile.baseUrl}
                    {'\n'}Check that Threadknot is running and this device can reach it.
                  </Text>
                  <Button variant="outline" onPress={() => views.current.get(id)?.reload()}>
                    <Text>Retry</Text>
                  </Button>
                </View>
              )}
            />
          </View>
        );
      })}
      {downloading && (
        <View
          pointerEvents="none"
          className="absolute bottom-6 left-0 right-0 items-center"
          style={{ zIndex: 2 }}
        >
          <View className="flex-row items-center gap-2 rounded-full border border-border bg-popover px-4 py-2">
            <ActivityIndicator size="small" color="#d9a35c" />
            <Text className="text-sm text-muted-foreground">Downloading…</Text>
          </View>
        </View>
      )}
    </View>
  );
});
