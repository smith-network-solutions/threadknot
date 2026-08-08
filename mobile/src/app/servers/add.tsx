import { BrandMark } from '@/components/BrandMark';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { Icon } from '@/components/ui/icon';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Text } from '@/components/ui/text';
import { isRelayOrigin, normalizeServerUrl, pairingPayloadFor } from '@/lib/api';
import { useServers } from '@/lib/servers';
import { useRouter } from 'expo-router';
import { ChevronLeft, QrCode, ShieldAlert } from 'lucide-react-native';
import * as React from 'react';
import { ActivityIndicator, KeyboardAvoidingView, Platform, Pressable, ScrollView, View } from 'react-native';
import { useSafeAreaInsets } from 'react-native-safe-area-context';

export default function AddServer() {
  const router = useRouter();
  const insets = useSafeAreaInsets();
  const { profiles, addServer, addServerByScan } = useServers();
  const [url, setUrl] = React.useState('');
  const [code, setCode] = React.useState('');
  const [nickname, setNickname] = React.useState('');
  const [error, setError] = React.useState<string | null>(null);
  const [busy, setBusy] = React.useState(false);
  const firstRun = profiles.length === 0;

  const parsed = React.useMemo(() => {
    try {
      return normalizeServerUrl(url);
    } catch {
      return null;
    }
  }, [url]);
  const insecureRemote = parsed?.insecureRemote ?? false;
  // A relay address cannot be paired with a token, so the code is not optional
  // there — surfacing that while they type beats a refusal after they submit.
  const needsCode = parsed != null && !parsed.token && isRelayOrigin(parsed.baseUrl);

  async function onSubmit() {
    setError(null);
    setBusy(true);
    try {
      const typed = code.trim();
      if (typed) {
        // Reuse the scan path verbatim rather than adding a second pairing
        // route: it is the one that already handles a relay origin, a cookie
        // session and the ingress probe.
        const base = parsed?.baseUrl ?? normalizeServerUrl(url).baseUrl;
        await addServerByScan(pairingPayloadFor(base, typed), nickname);
      } else {
        await addServer(url, nickname);
      }
      router.replace('/');
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <KeyboardAvoidingView
      behavior={Platform.OS === 'ios' ? 'padding' : undefined}
      className="flex-1 bg-background"
    >
      <ScrollView
        keyboardShouldPersistTaps="handled"
        contentContainerStyle={{ paddingTop: insets.top + 12, paddingBottom: insets.bottom + 24 }}
        className="flex-1 px-5"
      >
        {!firstRun && (
          <Pressable
            onPress={() => router.back()}
            className="mb-4 -ml-2 h-9 w-9 items-center justify-center rounded-full active:bg-accent"
          >
            <Icon as={ChevronLeft} className="size-5 text-muted-foreground" />
          </Pressable>
        )}

        <View className="mb-8 items-center gap-3 pt-6">
          <BrandMark />
          <Text className="text-2xl font-bold tracking-tight">
            {firstRun ? 'Welcome aboard' : 'Add a server'}
          </Text>
          <Text className="text-center text-muted-foreground">
            {firstRun
              ? 'Connect your first Threadknot server. Scan the QR from the desktop app’s Settings — or paste a LAN / Tailscale / ngrok URL that reaches it.'
              : 'Scan the QR from that server’s Settings, or paste its full URL (it includes ?token=…).'}
            {'\n\n'}
            {/* A hosted-relay address never accepts a token URL: the strict
                ingress refuses a credential in a URL however it arrives. Saying
                so here beats discovering it as a refusal after typing a hostname. */}
            Reaching a machine from outside your network (a
            <Text className="font-mono text-muted-foreground"> remote.threadknot.ai </Text>
            address) is paired with a code rather than a token — scan the QR, or
            enter the code shown beneath it.
          </Text>
        </View>

        <View className="gap-5">
          {/* Scanning is the path we want people on: the QR carries a one-time
              code, so the master token never lands on the phone at all. */}
          <Button size="lg" onPress={() => router.push('/servers/scan')}>
            <Icon as={QrCode} className="size-5 text-primary-foreground" />
            <Text>Scan QR code</Text>
          </Button>

          <View className="flex-row items-center gap-3">
            <View className="h-px flex-1 bg-border" />
            <Text className="text-xs uppercase tracking-widest text-muted-foreground">or</Text>
            <View className="h-px flex-1 bg-border" />
          </View>

          <View className="gap-2">
            <Label>Server URL</Label>
            <Input
              value={url}
              onChangeText={setUrl}
              placeholder="https://your-machine.remote.threadknot.ai"
              autoCapitalize="none"
              autoCorrect={false}
              keyboardType="url"
              autoFocus
            />
          </View>
          <View className="gap-2">
            <Label>{needsCode ? 'Pairing code' : 'Pairing code (optional)'}</Label>
            <Input
              value={code}
              onChangeText={setCode}
              placeholder="ABCDE-FGHIJ"
              autoCapitalize="characters"
              autoCorrect={false}
              autoComplete="off"
            />
            <Text className="text-xs text-muted-foreground">
              {needsCode
                ? 'Required for a relay address. Desktop: Settings → pair a phone → remote.'
                : 'Use this instead of a token URL. Desktop: Settings → pair a phone.'}
            </Text>
          </View>

          <View className="gap-2">
            <Label>Nickname (optional)</Label>
            <Input
              value={nickname}
              onChangeText={setNickname}
              placeholder="Home rig"
              autoCorrect={false}
            />
          </View>

          {insecureRemote && (
            <Alert icon={ShieldAlert} variant="destructive">
              <AlertTitle>Unencrypted remote connection</AlertTitle>
              <AlertDescription>
                This is plain HTTP to a host outside your local network — your token and sessions
                would travel unencrypted. Prefer Tailscale or an HTTPS tunnel (ngrok).
              </AlertDescription>
            </Alert>
          )}

          {error && (
            <Alert icon={ShieldAlert} variant="destructive">
              <AlertTitle>Couldn’t connect</AlertTitle>
              <AlertDescription>{error}</AlertDescription>
            </Alert>
          )}

          <Button
            size="lg"
            variant="outline"
            onPress={() => void onSubmit()}
            disabled={busy || url.trim().length === 0}
          >
            {busy ? <ActivityIndicator /> : <Text>Connect & pair</Text>}
          </Button>

          <Text className="text-center text-xs text-muted-foreground">
            Pairing swaps the master token for a revocable credential owned by this phone. The
            master token is never stored here.
          </Text>
        </View>
      </ScrollView>
    </KeyboardAvoidingView>
  );
}
