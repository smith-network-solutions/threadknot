import '../global.css';

import { LockOverlay } from '@/components/LockScreen';
import { LockProvider } from '@/lib/lock';
import { setNavIntent } from '@/lib/nav-intent';
import { ServersProvider } from '@/lib/servers';
import { PortalHost } from '@rn-primitives/portal';
import * as Notifications from 'expo-notifications';
import { Stack } from 'expo-router';
import { StatusBar } from 'expo-status-bar';
import { colorScheme } from 'nativewind';
import * as React from 'react';
import { View } from 'react-native';
import { SafeAreaProvider } from 'react-native-safe-area-context';

// Threadknot is dark-only, matching the desktop console.
colorScheme.set('dark');

// Foreground notifications still show as banners: the user may be looking at
// a different server or thread than the one that needs attention.
Notifications.setNotificationHandler({
  handleNotification: async () => ({
    shouldShowBanner: true,
    shouldShowList: true,
    shouldPlaySound: false,
    shouldSetBadge: false,
  }),
});

function extractIntent(resp: Notifications.NotificationResponse | null): void {
  const data = resp?.notification.request.content.data as
    | { serverId?: unknown; projectId?: unknown; threadId?: unknown }
    | undefined;
  if (!data || typeof data.serverId !== 'string') return;
  setNavIntent({
    serverId: data.serverId,
    projectId: typeof data.projectId === 'string' && data.projectId ? data.projectId : undefined,
    threadId: typeof data.threadId === 'string' && data.threadId ? data.threadId : undefined,
  });
}

/** Capture notification taps: warm-app taps via the listener, cold starts via
 * the last-response query. The intent waits behind the biometric gate. */
function useNotificationRouting() {
  React.useEffect(() => {
    void Notifications.getLastNotificationResponseAsync().then(extractIntent);
    const sub = Notifications.addNotificationResponseReceivedListener(extractIntent);
    return () => sub.remove();
  }, []);
}

export default function RootLayout() {
  useNotificationRouting();
  return (
    <SafeAreaProvider>
      <ServersProvider>
        <LockProvider>
          <View className="dark flex-1 bg-background">
            <Stack
              screenOptions={{
                headerShown: false,
                contentStyle: { backgroundColor: '#0b0d12' },
                animation: 'fade',
              }}
            />
            <LockOverlay />
            <PortalHost />
          </View>
          <StatusBar style="light" />
        </LockProvider>
      </ServersProvider>
    </SafeAreaProvider>
  );
}
