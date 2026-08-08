import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from '@/components/ui/alert-dialog';
import { Button } from '@/components/ui/button';
import { Icon } from '@/components/ui/icon';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Separator } from '@/components/ui/separator';
import { Switch } from '@/components/ui/switch';
import { Text } from '@/components/ui/text';
import { useServers } from '@/lib/servers';
import { useLocalSearchParams, useRouter } from 'expo-router';
import { BellRing, ChevronLeft, Trash2 } from 'lucide-react-native';
import * as React from 'react';
import { ActivityIndicator, Alert as RNAlert, KeyboardAvoidingView, Platform, Pressable, ScrollView, View } from 'react-native';
import { useSafeAreaInsets } from 'react-native-safe-area-context';

export default function ServerDetail() {
  const router = useRouter();
  const insets = useSafeAreaInsets();
  const { id } = useLocalSearchParams<{ id: string }>();
  const { profiles, renameServer, updateUrl, setNotifications, testPush, removeServer } =
    useServers();
  const profile = profiles.find((p) => p.id === id);

  const [name, setName] = React.useState(profile?.name ?? '');
  const [url, setUrl] = React.useState(profile?.baseUrl ?? '');
  const [busy, setBusy] = React.useState<string | null>(null);

  if (!profile) {
    return (
      <View className="flex-1 items-center justify-center bg-background">
        <Text className="text-muted-foreground">Server not found.</Text>
      </View>
    );
  }

  const run = (label: string, fn: () => Promise<void>, doneMsg?: string) => {
    void (async () => {
      setBusy(label);
      try {
        await fn();
        if (doneMsg) RNAlert.alert(doneMsg);
      } catch (e) {
        RNAlert.alert('Error', e instanceof Error ? e.message : String(e));
      } finally {
        setBusy(null);
      }
    })();
  };

  return (
    <KeyboardAvoidingView
      behavior={Platform.OS === 'ios' ? 'padding' : undefined}
      className="flex-1 bg-background"
    >
      <ScrollView
        keyboardShouldPersistTaps="handled"
        contentContainerStyle={{ paddingTop: insets.top + 8, paddingBottom: insets.bottom + 24 }}
        className="px-4"
      >
        <View className="mb-4 flex-row items-center gap-2">
          <Pressable
            onPress={() => router.back()}
            className="h-9 w-9 items-center justify-center rounded-full active:bg-accent"
          >
            <Icon as={ChevronLeft} className="size-5 text-muted-foreground" />
          </Pressable>
          <Text className="flex-1 text-lg font-semibold" numberOfLines={1}>
            {profile.name}
          </Text>
        </View>

        <View className="gap-6">
          <View className="gap-2">
            <Label>Nickname</Label>
            <View className="flex-row gap-2">
              <Input className="flex-1" value={name} onChangeText={setName} />
              <Button
                variant="outline"
                disabled={busy != null || name.trim() === profile.name}
                onPress={() => run('rename', () => renameServer(profile.id, name))}
              >
                <Text>Save</Text>
              </Button>
            </View>
          </View>

          <View className="gap-2">
            <Label>Server URL</Label>
            <Input
              value={url}
              onChangeText={setUrl}
              autoCapitalize="none"
              autoCorrect={false}
              keyboardType="url"
            />
            <Text className="text-xs text-muted-foreground">
              Moved the server (new LAN IP, new tunnel)? Paste the new URL. If this phone was
              un-paired, include ?token=… to re-pair.
            </Text>
            <Button
              variant="outline"
              disabled={busy != null || url.trim() === profile.baseUrl}
              onPress={() =>
                run('url', () => updateUrl(profile.id, url), 'Server URL updated')
              }
            >
              {busy === 'url' ? <ActivityIndicator color="#d8dde9" /> : <Text>Update URL</Text>}
            </Button>
          </View>

          <Separator />

          <View className="flex-row items-center justify-between">
            <View className="flex-1 pr-4">
              <Text className="font-medium">Notifications</Text>
              <Text className="text-xs text-muted-foreground">
                Turn completions, approvals, and questions from this server.
              </Text>
            </View>
            <Switch
              checked={profile.notificationsEnabled}
              onCheckedChange={(v) =>
                run('notif', () => setNotifications(profile.id, v))
              }
            />
          </View>

          <Button
            variant="secondary"
            disabled={busy != null || !profile.notificationsEnabled}
            onPress={() =>
              run('test', () => testPush(profile.id), 'Test sent — it should arrive within seconds')
            }
          >
            <Icon as={BellRing} className="size-4 text-foreground" />
            {busy === 'test' ? <ActivityIndicator color="#d8dde9" /> : <Text>Send test notification</Text>}
          </Button>

          <Separator />

          <View className="gap-1">
            <Text className="text-xs text-muted-foreground">
              Reached via:{' '}
              {profile.ingress === 'remote'
                ? 'Threadknot relay (session cookie)'
                : 'local network (access token)'}
            </Text>
            <Text className="text-xs text-muted-foreground">Host: {profile.serverName ?? '—'}</Text>
            <Text className="text-xs text-muted-foreground">Threadknot v{profile.version ?? '?'}</Text>
            <Text className="text-xs text-muted-foreground">Server ID: {profile.serverId}</Text>
            <Text className="text-xs text-muted-foreground">Device ID: {profile.deviceId}</Text>
          </View>

          <AlertDialog>
            <AlertDialogTrigger asChild>
              <Button variant="destructive" disabled={busy != null}>
                <Icon as={Trash2} className="size-4 text-white" />
                <Text>Remove server</Text>
              </Button>
            </AlertDialogTrigger>
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>Remove {profile.name}?</AlertDialogTitle>
                <AlertDialogDescription>
                  This un-pairs the phone (revoking its credential on the server) and deletes the
                  saved connection. Your projects and threads on the server are untouched.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>
                  <Text>Cancel</Text>
                </AlertDialogCancel>
                <AlertDialogAction
                  onPress={() =>
                    run('remove', async () => {
                      await removeServer(profile.id);
                      router.dismissTo('/');
                    })
                  }
                >
                  <Text>Remove</Text>
                </AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
        </View>
      </ScrollView>
    </KeyboardAvoidingView>
  );
}
