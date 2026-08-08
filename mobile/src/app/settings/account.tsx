import { Bullet, P, Section } from '@/components/Prose';
import { ScreenHeader } from '@/components/ScreenHeader';
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
import { Card, CardContent } from '@/components/ui/card';
import { Icon } from '@/components/ui/icon';
import { Text } from '@/components/ui/text';
import { LEGAL, mailSupport } from '@/lib/legal';
import { useServers } from '@/lib/servers';
import { Clock, Trash2 } from 'lucide-react-native';
import * as React from 'react';
import { ScrollView, View } from 'react-native';
import { useSafeAreaInsets } from 'react-native-safe-area-context';

export default function AccountScreen() {
  const insets = useSafeAreaInsets();
  const { profiles } = useServers();
  const [requested, setRequested] = React.useState(false);

  // NOTE: deliberately inert. The relay has no account-deletion endpoint yet,
  // so this records the intent in local component state and nothing else —
  // no request is sent, and nothing on this phone or the relay is touched.
  // Wire it to DELETE /account (see LEGAL.deleteAccount) before shipping to
  // the App Store; until then this screen is a compliance placeholder.
  function requestDeletion() {
    setRequested(true);
  }

  return (
    <View className="flex-1 bg-background" style={{ paddingTop: insets.top + 8 }}>
      <ScreenHeader title="Account & data" />

      <ScrollView
        contentContainerStyle={{ paddingBottom: insets.bottom + 40 }}
        contentContainerClassName="gap-7 px-4"
      >
        <Section title="What Threadknot stores">
          <P>
            Threadknot is a client for servers you run. Your projects, threads and agent output
            live on those machines — not on ours. What exists off your own hardware is only what
            it takes to reach them.
          </P>
          <Bullet>
            On this phone: each paired server&apos;s address and access token, held in the
            device keychain behind Face ID / Touch ID.
          </Bullet>
          <Bullet>
            On the Threadknot relay, only if you paired a remote address: your account
            identifier, the relay addresses you paired, and a push token so a machine can wake
            your phone when an agent needs you.
          </Bullet>
          <Bullet>
            Nothing else. Threadknot does not collect analytics, does not profile you, and never
            sees the contents of a thread.
          </Bullet>
        </Section>

        <Section title="Removing a single server">
          <P>
            Deleting one server from this phone does not need an account deletion — open
            Settings → Servers, pick the server and remove it. Its token is wiped from the
            keychain immediately.
            {profiles.length > 0 ? ` You have ${profiles.length} paired right now.` : ''}
          </P>
        </Section>

        <Section title="Deleting your account">
          <P>
            This removes your Threadknot account and everything the relay holds for it. It does
            not touch the machines you run Threadknot on, or any work stored there.
          </P>

          {requested ? (
            <Card className="border-brass/40 py-4">
              <CardContent className="flex-row gap-3">
                <Icon as={Clock} className="mt-0.5 size-5 text-brass" />
                <View className="flex-1 gap-1.5">
                  <Text className="font-semibold text-brass-hi">Deletion requested</Text>
                  <Text className="text-sm leading-6 text-muted-foreground">
                    Your account and its relay data are scheduled for permanent deletion within
                    24 hours. Push notifications stop right away. Servers already paired on this
                    phone keep working over your local network.
                  </Text>
                  <Text className="text-sm leading-6 text-muted-foreground">
                    Changed your mind? Write to {LEGAL.supportEmail} before the 24 hours are up
                    and we will cancel it.
                  </Text>
                  <Button
                    variant="outline"
                    size="sm"
                    className="mt-2 self-start"
                    onPress={() => mailSupport('Cancel my Threadknot account deletion')}
                  >
                    <Text>Contact support</Text>
                  </Button>
                </View>
              </CardContent>
            </Card>
          ) : (
            <AlertDialog>
              <AlertDialogTrigger asChild>
                <Button variant="destructive" className="mt-1 self-start">
                  <Icon as={Trash2} className="size-4 text-white" />
                  <Text>Delete account</Text>
                </Button>
              </AlertDialogTrigger>
              <AlertDialogContent>
                <AlertDialogHeader>
                  <AlertDialogTitle>Delete your Threadknot account?</AlertDialogTitle>
                  <AlertDialogDescription>
                    Your account and everything the Threadknot relay holds for it — your paired
                    remote addresses and push tokens — will be permanently deleted within 24
                    hours. Push notifications stop immediately and this cannot be undone once
                    the 24 hours pass.
                    {'\n\n'}
                    The machines you run Threadknot on are not touched: projects, threads and
                    agent history stay on your own hardware.
                  </AlertDialogDescription>
                </AlertDialogHeader>
                <AlertDialogFooter>
                  <AlertDialogCancel>
                    <Text>Keep my account</Text>
                  </AlertDialogCancel>
                  <AlertDialogAction onPress={requestDeletion}>
                    <Text>Delete in 24 hours</Text>
                  </AlertDialogAction>
                </AlertDialogFooter>
              </AlertDialogContent>
            </AlertDialog>
          )}
        </Section>
      </ScrollView>
    </View>
  );
}
