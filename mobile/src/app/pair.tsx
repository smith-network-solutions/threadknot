import { BrandMark } from '@/components/BrandMark';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { Text } from '@/components/ui/text';
import { normalizeServerUrl } from '@/lib/api';
import { useServers } from '@/lib/servers';
import { useLocalSearchParams, useRouter } from 'expo-router';
import { ShieldAlert } from 'lucide-react-native';
import * as React from 'react';
import { ActivityIndicator, View } from 'react-native';
import { useSafeAreaInsets } from 'react-native-safe-area-context';

/**
 * `threadknot://pair?u=<origin>&c=<code>` — where the deep link lands when the
 * pairing QR is scanned by the phone's own camera app rather than from inside
 * Threadknot.
 *
 * This ALWAYS asks first. Unlike the in-app scanner, a deep link is not proof
 * that anyone pointed a camera at anything: any app or web page on the device
 * can fire one, and silently pairing would let a hostile page install its own
 * server profile — whose pages then render in our WebView. Showing the origin
 * and requiring a tap is what makes the two paths equally safe.
 */
export default function PairFromLink() {
  const router = useRouter();
  const insets = useSafeAreaInsets();
  const { addServerByScan } = useServers();
  const { u, c } = useLocalSearchParams<{ u?: string; c?: string }>();
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);

  const origin = typeof u === 'string' ? u : '';
  const code = typeof c === 'string' ? c : '';

  const insecureRemote = React.useMemo(() => {
    try {
      return normalizeServerUrl(origin).insecureRemote;
    } catch {
      return false;
    }
  }, [origin]);

  function onPair() {
    setBusy(true);
    setError(null);
    void addServerByScan(`threadknot://pair?u=${encodeURIComponent(origin)}&c=${encodeURIComponent(code)}`)
      .then(() => router.replace('/'))
      .catch((e: unknown) => {
        setError(e instanceof Error ? e.message : String(e));
        setBusy(false);
      });
  }

  return (
    <View
      className="flex-1 justify-center gap-6 bg-background px-6"
      style={{ paddingTop: insets.top, paddingBottom: insets.bottom }}
    >
      <View className="items-center gap-3">
        <BrandMark />
        <Text className="text-2xl font-bold tracking-tight">Pair with this server?</Text>
      </View>

      {!origin || !code ? (
        <Alert icon={ShieldAlert} variant="destructive">
          <AlertTitle>Incomplete pairing link</AlertTitle>
          <AlertDescription>
            Show a fresh QR on the desktop (Settings → Phone & access → pair a phone) and scan
            it again.
          </AlertDescription>
        </Alert>
      ) : (
        <>
          <View className="gap-2 rounded-xl border border-border bg-card p-4">
            <Text className="text-xs uppercase tracking-widest text-muted-foreground">server</Text>
            <Text className="font-mono">{origin}</Text>
          </View>
          <Text className="text-center text-sm text-muted-foreground">
            Only continue if this is your own machine and you just asked it to show a pairing
            code.
          </Text>

          {insecureRemote && (
            <Alert icon={ShieldAlert} variant="destructive">
              <AlertTitle>Unencrypted remote connection</AlertTitle>
              <AlertDescription>
                This is plain HTTP to a host outside your local network — your sessions would
                travel unencrypted.
              </AlertDescription>
            </Alert>
          )}

          <View className="gap-3">
            <Button size="lg" onPress={onPair} disabled={busy}>
              {busy ? <ActivityIndicator /> : <Text>Pair</Text>}
            </Button>
            <Button size="lg" variant="outline" onPress={() => router.replace('/')} disabled={busy}>
              <Text>Not now</Text>
            </Button>
          </View>
        </>
      )}

      {error && (
        <Alert icon={ShieldAlert} variant="destructive">
          <AlertTitle>Couldn’t pair</AlertTitle>
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}
    </View>
  );
}
