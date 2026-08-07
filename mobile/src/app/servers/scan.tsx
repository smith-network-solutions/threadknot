import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { Icon } from '@/components/ui/icon';
import { Text } from '@/components/ui/text';
import { parsePairingPayload } from '@/lib/api';
import { useServers } from '@/lib/servers';
import { CameraView, useCameraPermissions } from 'expo-camera';
import { useRouter } from 'expo-router';
import { ChevronLeft, ShieldAlert } from 'lucide-react-native';
import * as React from 'react';
import { ActivityIndicator, Pressable, View } from 'react-native';
import { useSafeAreaInsets } from 'react-native-safe-area-context';

/**
 * Point the camera at the QR the desktop shows under Settings → Phone & access
 * → "pair a phone". The QR carries a single-use code, not the machine's master
 * token, so the redeem either works once or is already dead.
 */
export default function ScanServer() {
  const router = useRouter();
  const insets = useSafeAreaInsets();
  const { addServerByScan } = useServers();
  const [permission, requestPermission] = useCameraPermissions();
  const [error, setError] = React.useState<string | null>(null);
  const [busy, setBusy] = React.useState(false);
  // A scanned QR whose origin is plain HTTP to a non-private host. The pasted
  // URL flow warns about this before connecting; scanning must not be the
  // quiet way around that warning, so it holds here for an explicit tap.
  const [confirmInsecure, setConfirmInsecure] = React.useState<string | null>(null);
  // The camera fires this repeatedly while a code is in frame; a ref (not
  // state) is what actually stops the second call, since state hasn't
  // committed yet by the time the next frame arrives.
  const handled = React.useRef(false);

  const pair = React.useCallback(
    (raw: string) => {
      setBusy(true);
      setError(null);
      void addServerByScan(raw)
        .then(() => router.replace('/'))
        .catch((e: unknown) => {
          setError(e instanceof Error ? e.message : String(e));
          setBusy(false);
          setConfirmInsecure(null);
          // Let them line up another code without leaving the screen — a
          // mis-scan or an expired QR is the common case, not a dead end.
          handled.current = false;
        });
    },
    [addServerByScan, router]
  );

  const onScanned = React.useCallback(
    (raw: string) => {
      if (handled.current) return;
      // Parse before redeeming: the code is single-use, so a payload we would
      // have questioned must be questioned while it is still spendable.
      let scanned;
      try {
        scanned = parsePairingPayload(raw);
      } catch {
        // A camera reports every QR in frame — a poster, a wifi code, the
        // sticker on a laptop. Those aren't failures, they're just not ours.
        // Staying silent leaves the viewfinder usable; setting an error here
        // would strobe one on every frame until the right code lines up.
        return;
      }
      handled.current = true;
      setError(null);
      if (scanned.insecureRemote) {
        setConfirmInsecure(raw);
        return;
      }
      pair(raw);
    },
    [pair]
  );

  return (
    <View className="flex-1 bg-background" style={{ paddingTop: insets.top + 12 }}>
      <View className="flex-row items-center gap-2 px-5">
        <Pressable
          onPress={() => router.back()}
          className="-ml-2 h-9 w-9 items-center justify-center rounded-full active:bg-accent"
        >
          <Icon as={ChevronLeft} className="size-5 text-muted-foreground" />
        </Pressable>
        <Text className="text-lg font-semibold">Scan pairing code</Text>
      </View>

      <View className="flex-1 items-center justify-center gap-5 px-5">
        {!permission ? (
          <ActivityIndicator />
        ) : !permission.granted ? (
          <View className="w-full gap-4">
            <Text className="text-center text-muted-foreground">
              {permission.canAskAgain
                ? 'Threadknot needs the camera to read the pairing QR code. Nothing is recorded or uploaded — the frame is scanned on this device.'
                : 'Camera access is off for Threadknot. Enable it in your device settings, or go back and paste the LAN URL instead.'}
            </Text>
            {permission.canAskAgain && (
              <Button size="lg" onPress={() => void requestPermission()}>
                <Text>Allow camera</Text>
              </Button>
            )}
          </View>
        ) : confirmInsecure ? (
          <View className="w-full gap-4">
            <Alert icon={ShieldAlert} variant="destructive">
              <AlertTitle>Unencrypted remote connection</AlertTitle>
              <AlertDescription>
                That code points at a host outside your local network over plain HTTP — your
                sessions would travel unencrypted. Prefer Tailscale or an HTTPS tunnel (ngrok).
              </AlertDescription>
            </Alert>
            <Button size="lg" variant="outline" onPress={() => pair(confirmInsecure)} disabled={busy}>
              {busy ? <ActivityIndicator /> : <Text>Pair anyway</Text>}
            </Button>
            {/* Leaves the screen rather than re-arming the scanner: the same
                QR is still in frame, so re-arming would just put this prompt
                straight back up and "Cancel" would do nothing visible. */}
            <Button size="lg" onPress={() => router.back()} disabled={busy}>
              <Text>Cancel</Text>
            </Button>
          </View>
        ) : (
          <>
            <View className="aspect-square w-full max-w-sm overflow-hidden rounded-2xl border border-border bg-card">
              <CameraView
                style={{ flex: 1 }}
                facing="back"
                barcodeScannerSettings={{ barcodeTypes: ['qr'] }}
                onBarcodeScanned={busy ? undefined : ({ data }) => onScanned(data)}
              />
            </View>
            <Text className="text-center text-muted-foreground">
              {busy
                ? 'Pairing…'
                : 'On the desktop: Settings → Phone & access → “pair a phone”.'}
            </Text>
            {busy && <ActivityIndicator />}
          </>
        )}

        {error && (
          <Alert icon={ShieldAlert} variant="destructive">
            <AlertTitle>Couldn’t pair</AlertTitle>
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}
      </View>
    </View>
  );
}
