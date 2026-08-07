import { ServerSwitcher, type ConnState } from '@/components/ServerSwitcher';
import { WebViewPool, type PoolHandle } from '@/components/WebViewPool';
import { useLock } from '@/lib/lock';
import { clearNavIntent, peekNavIntent, subscribeNavIntent } from '@/lib/nav-intent';
import { useServers } from '@/lib/servers';
import { Redirect } from 'expo-router';
import * as React from 'react';
import { ActivityIndicator, Alert, View } from 'react-native';
import { useSafeAreaInsets } from 'react-native-safe-area-context';

export default function Home() {
  const { loaded, profiles, activeId, credentials, setActive } = useServers();
  const { status } = useLock();
  const insets = useSafeAreaInsets();
  const poolRef = React.useRef<PoolHandle>(null);
  const [conn, setConn] = React.useState<Record<string, ConnState>>({});
  const [intentTick, setIntentTick] = React.useState(0);

  React.useEffect(() => subscribeNavIntent(() => setIntentTick((t) => t + 1)), []);

  // Consume a pending push-tap: only once unlocked, and only if the target
  // server is still configured. The pool queues the thread navigation until
  // that server's page reports ready.
  React.useEffect(() => {
    if (status !== 'unlocked' || !loaded) return;
    const intent = peekNavIntent();
    if (!intent) return;
    const target = profiles.find((p) => p.serverId === intent.serverId);
    if (!target) {
      clearNavIntent();
      if (profiles.length > 0) {
        Alert.alert(
          'Unknown server',
          'This notification came from a server that is not configured on this phone.'
        );
      }
      return;
    }
    if (activeId !== target.id) setActive(target.id);
    if (intent.threadId) {
      poolRef.current?.navigate(target.id, {
        projectId: intent.projectId,
        threadId: intent.threadId,
      });
    }
    clearNavIntent();
  }, [status, loaded, intentTick, profiles, activeId, setActive]);

  const onConnChange = React.useCallback((profileId: string, c: ConnState) => {
    setConn((prev) => (prev[profileId] === c ? prev : { ...prev, [profileId]: c }));
  }, []);

  if (!loaded) {
    return (
      <View className="flex-1 items-center justify-center bg-background">
        <ActivityIndicator color="#d9a35c" />
      </View>
    );
  }
  if (profiles.length === 0) {
    return <Redirect href="/servers/add" />;
  }

  return (
    <View className="flex-1 bg-background" style={{ paddingTop: insets.top }}>
      <ServerSwitcher profiles={profiles} activeId={activeId} conn={conn} onSelect={setActive} />
      <WebViewPool
        ref={poolRef}
        profiles={profiles}
        credentials={credentials}
        activeId={activeId}
        onConnChange={onConnChange}
      />
    </View>
  );
}
