import { P, Section } from '@/components/Prose';
import { ScreenHeader } from '@/components/ScreenHeader';
import { Button } from '@/components/ui/button';
import { Card, CardContent } from '@/components/ui/card';
import { Icon } from '@/components/ui/icon';
import { Text } from '@/components/ui/text';
import { LEGAL, mailSupport, openExternal } from '@/lib/legal';
import { useRouter } from 'expo-router';
import { ExternalLink, Mail } from 'lucide-react-native';
import * as React from 'react';
import { ScrollView, View } from 'react-native';
import { useSafeAreaInsets } from 'react-native-safe-area-context';

/** The three things that actually go wrong, and what to do about each. */
const FAQ: { q: string; a: string }[] = [
  {
    q: 'A server chip is stuck on “offline”.',
    a: 'The phone cannot reach that address. Check that Threadknot is running on the machine, that the phone is on the same network, and that the address in Settings → Servers still matches. A LAN address changes when the machine gets a new DHCP lease.',
  },
  {
    q: 'The QR code will not pair.',
    a: 'Pairing codes are short-lived. Show a fresh QR on the desktop under Settings → Phone & access → pair a phone, and scan it again. A remote (relay) address pairs with the code beneath the QR, never with a token URL.',
  },
  {
    q: 'Notifications are not arriving.',
    a: 'Check that notifications are enabled for that server on its own screen, that iOS has not turned them off for Threadknot in system settings, and that the machine can reach the internet — push is delivered through Apple, so a fully offline machine cannot send one.',
  },
  {
    q: 'The app asks for Face ID every time I switch away.',
    a: 'That is deliberate. Threadknot holds tokens that drive agents on your machines, so it re-locks whenever it leaves the foreground.',
  },
];

export default function SupportScreen() {
  const insets = useSafeAreaInsets();
  const router = useRouter();

  return (
    <View className="flex-1 bg-background" style={{ paddingTop: insets.top + 8 }}>
      <ScreenHeader title="Support" />

      <ScrollView
        contentContainerStyle={{ paddingBottom: insets.bottom + 40 }}
        contentContainerClassName="gap-7 px-4"
      >
        <Section title="Get in touch">
          <P>
            Write to {LEGAL.supportEmail} and a person answers — usually within one business day.
            Telling us the app version from Settings → About, and what the server chip was
            showing at the time, saves a round trip.
          </P>
          <View className="flex-row flex-wrap gap-3 pt-1">
            <Button onPress={() => mailSupport('Threadknot mobile support')}>
              <Icon as={Mail} className="size-4 text-primary-foreground" />
              <Text>Email support</Text>
            </Button>
            <Button variant="outline" onPress={() => void openExternal(LEGAL.support)}>
              <Icon as={ExternalLink} className="size-4 text-foreground" />
              <Text>Help centre</Text>
            </Button>
          </View>
        </Section>

        <Section title="Common problems">
          <View className="gap-3 pt-1">
            {FAQ.map((item) => (
              <Card key={item.q} className="gap-2 py-4">
                <CardContent className="gap-1.5">
                  <Text className="text-sm font-semibold text-foreground">{item.q}</Text>
                  <Text className="text-sm leading-6 text-muted-foreground">{item.a}</Text>
                </CardContent>
              </Card>
            ))}
          </View>
        </Section>

        <Section title="Still stuck?">
          <P>
            Removing a server and pairing it again is safe — it clears the stored token and
            nothing on the machine is affected.
          </P>
          <Button variant="outline" className="mt-1 self-start" onPress={() => router.push('/servers')}>
            <Text>Open server settings</Text>
          </Button>
        </Section>
      </ScrollView>
    </View>
  );
}
