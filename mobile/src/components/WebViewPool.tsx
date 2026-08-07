import { Button } from '@/components/ui/button';
import { Text } from '@/components/ui/text';
import type { ConnState } from '@/components/ServerSwitcher';
import { downloadAndShare, isFileUrl } from '@/lib/download';
import type { ServerProfile, WebToNativeMessage } from '@/lib/types';
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
  activeId: string | null;
  onConnChange(profileId: string, conn: ConnState): void;
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
  { profiles, credentials, activeId, onConnChange },
  ref
) {
  // LRU of mounted profile ids, most recent last; always contains activeId.
  const [mounted, setMounted] = React.useState<string[]>([]);
  const [downloading, setDownloading] = React.useState(false);
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
  React.useEffect(() => {
    let wasAway = AppState.currentState !== 'active';
    const subscription = AppState.addEventListener('change', (next) => {
      if (next === 'active') {
        if (wasAway) {
          for (const id of views.current.keys()) {
            dispatchToWeb(id, { type: 'resume' });
          }
        }
        wasAway = false;
      } else {
        wasAway = true;
      }
    });
    return () => subscription.remove();
  }, [dispatchToWeb]);

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
        const bootstrap = JSON.stringify({
          token: credential,
          serverId: profile.serverId,
          platform: Platform.OS,
          capabilities: ['clipboard-read', 'reload'],
        });
        return (
          <View
            key={id}
            pointerEvents={isActive ? 'auto' : 'none'}
            className="absolute inset-0"
            style={{ opacity: isActive ? 1 : 0, zIndex: isActive ? 1 : 0 }}
          >
            <WebView
              ref={(v) => {
                views.current.set(id, v);
              }}
              source={{ uri: `${profile.baseUrl}/` }}
              // The credential is injected into page scope before any app code
              // runs — it never appears in the URL or web storage.
              injectedJavaScriptBeforeContentLoaded={`window.__THREADKNOT_NATIVE__ = ${bootstrap}; true;`}
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
                  void downloadAndShare(req.url).finally(() => setDownloading(false));
                  return false;
                }
                if (req.url.startsWith(profile.baseUrl) || req.url.startsWith('about:')) {
                  return true;
                }
                void Linking.openURL(req.url).catch(() => undefined);
                return false;
              }}
              onFileDownload={({ nativeEvent }) => {
                // iOS: WKWebView flagged a response as a download attachment.
                setDownloading(true);
                void downloadAndShare(nativeEvent.downloadUrl).finally(() =>
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
