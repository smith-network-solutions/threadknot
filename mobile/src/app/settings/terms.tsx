import { Bullet, P, Revised, Section } from '@/components/Prose';
import { ScreenHeader } from '@/components/ScreenHeader';
import { Button } from '@/components/ui/button';
import { Icon } from '@/components/ui/icon';
import { Text } from '@/components/ui/text';
import { LEGAL, openExternal } from '@/lib/legal';
import { ExternalLink } from 'lucide-react-native';
import * as React from 'react';
import { ScrollView, View } from 'react-native';
import { useSafeAreaInsets } from 'react-native-safe-area-context';

export default function TermsScreen() {
  const insets = useSafeAreaInsets();
  return (
    <View className="flex-1 bg-background" style={{ paddingTop: insets.top + 8 }}>
      <ScreenHeader title="Terms of Use" />

      <ScrollView
        contentContainerStyle={{ paddingBottom: insets.bottom + 40 }}
        contentContainerClassName="gap-7 px-4"
      >
        <Section title="What this app is">
          <P>
            Threadknot for iOS and Android is a client for Threadknot servers that you install
            and operate. Smith Network Solutions provides the app and, optionally, a relay
            service that lets your phone reach one of your machines from outside its network.
          </P>
        </Section>

        <Section title="Your responsibilities">
          <Bullet>
            You are responsible for the machines you connect to and for anything the agents on
            them do — including changes they make to your files and any spend on the agent
            accounts you have logged in.
          </Bullet>
          <Bullet>
            Keep pairing tokens and QR codes private. Anyone holding one can drive your agents.
          </Bullet>
          <Bullet>
            Do not use Threadknot to break the law, to reach machines you are not authorised to
            reach, or to abuse the relay.
          </Bullet>
        </Section>

        <Section title="Agent providers">
          <P>
            Threadknot drives the Claude Code, Codex and Kimi CLIs using the subscriptions you
            have already signed into on your own machine. Your use of those services is governed
            by their terms, not these. Threadknot does not resell them and adds no charge to
            them.
          </P>
        </Section>

        <Section title="Payment">
          <P>
            The app and local-network use are free. If a paid relay plan is offered, its price
            and renewal terms are shown before purchase; subscriptions bought through the App
            Store are billed by Apple and can be managed or cancelled in your App Store account
            settings.
          </P>
        </Section>

        <Section title="Availability and warranty">
          <P>
            The app is provided &quot;as is&quot;, without warranty of any kind. The relay is
            offered on a best-effort basis and may be interrupted for maintenance. To the extent
            the law allows, Smith Network Solutions is not liable for indirect or consequential
            loss, including lost work produced by an agent.
          </P>
        </Section>

        <Section title="Ending it">
          <P>
            You may stop using Threadknot at any time by deleting the app; that removes every
            token it stored. To remove what the relay holds, use Settings → Account &amp; data.
            We may suspend relay access that is being used abusively.
          </P>
        </Section>

        <Section title="Contact">
          <P>
            Smith Network Solutions — {LEGAL.supportEmail}. These terms are governed by the laws
            of the United States.
          </P>
        </Section>

        <View className="gap-3">
          <Button variant="outline" onPress={() => void openExternal(LEGAL.terms)}>
            <Icon as={ExternalLink} className="size-4 text-foreground" />
            <Text>Read the full terms</Text>
          </Button>
          <Revised date="8 August 2026" />
        </View>
      </ScrollView>
    </View>
  );
}
