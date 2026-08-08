import { BrandMark } from '@/components/BrandMark';
import { Button } from '@/components/ui/button';
import { Icon } from '@/components/ui/icon';
import { Text } from '@/components/ui/text';
import { useLock } from '@/lib/lock';
import { Fingerprint, ShieldAlert } from 'lucide-react-native';
import * as React from 'react';
import { Linking, View } from 'react-native';

/** Opaque cover rendered above everything while the app is locked or
 * backgrounded. Mounted content (WebViews) stays warm underneath. */
export function LockOverlay() {
  const { status, obscured, unlock, recheck } = useLock();
  if (status === 'unlocked' && !obscured) return null;

  return (
    <View className="absolute inset-0 z-50 items-center justify-center bg-background px-8">
      <View className="items-center gap-4">
        <BrandMark />
        <Text className="text-2xl font-bold tracking-tight">Threadknot</Text>

        {status === 'unavailable' ? (
          <View className="items-center gap-4">
            <Icon as={ShieldAlert} className="size-6 text-destructive" />
            <Text className="text-center text-muted-foreground">
              Your device has no screen lock. Threadknot stores server credentials and refuses to open
              without one — add a passcode or biometrics in system settings.
            </Text>
            <View className="flex-row gap-3">
              <Button variant="outline" onPress={() => void Linking.openSettings()}>
                <Text>Open settings</Text>
              </Button>
              <Button onPress={() => void recheck()}>
                <Text>Retry</Text>
              </Button>
            </View>
          </View>
        ) : status === 'locked' || status === 'checking' ? (
          <View className="items-center gap-4">
            <Text className="text-center text-muted-foreground">Locked</Text>
            <Button size="lg" onPress={() => void unlock()} disabled={status === 'checking'}>
              <Icon as={Fingerprint} className="size-5 text-primary-foreground" />
              <Text>Unlock</Text>
            </Button>
          </View>
        ) : null}
      </View>
    </View>
  );
}
