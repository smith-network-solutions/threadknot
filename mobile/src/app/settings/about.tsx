import { BrandMark } from '@/components/BrandMark';
import { P, Section } from '@/components/Prose';
import { ScreenHeader } from '@/components/ScreenHeader';
import { Button } from '@/components/ui/button';
import { Card, CardContent } from '@/components/ui/card';
import { Icon } from '@/components/ui/icon';
import { Separator } from '@/components/ui/separator';
import { Text } from '@/components/ui/text';
import { LEGAL, openExternal } from '@/lib/legal';
import * as Application from 'expo-application';
import * as Clipboard from 'expo-clipboard';
import { Copy, ExternalLink } from 'lucide-react-native';
import * as React from 'react';
import { Platform, ScrollView, View } from 'react-native';
import { useSafeAreaInsets } from 'react-native-safe-area-context';

function Fact({ label, value }: { label: string; value: string }) {
  return (
    <View className="flex-row items-center justify-between gap-4 py-2">
      <Text className="text-sm text-muted-foreground">{label}</Text>
      <Text className="flex-1 text-right font-mono text-xs text-foreground" numberOfLines={1}>
        {value}
      </Text>
    </View>
  );
}

export default function AboutScreen() {
  const insets = useSafeAreaInsets();
  const [copied, setCopied] = React.useState(false);

  const version = Application.nativeApplicationVersion ?? 'dev';
  const build = Application.nativeBuildVersion ?? '—';
  const bundle = Application.applicationId ?? 'com.smithnetworksolutions.threadknot';
  const platform = `${Platform.OS} ${String(Platform.Version)}`;

  // Support asks for this verbatim; typing it off a screen is how it arrives wrong.
  function copyDiagnostics() {
    void Clipboard.setStringAsync(
      `Threadknot ${version} (${build})\n${bundle}\n${platform}`
    ).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
  }

  return (
    <View className="flex-1 bg-background" style={{ paddingTop: insets.top + 8 }}>
      <ScreenHeader title="About" />

      <ScrollView
        contentContainerStyle={{ paddingBottom: insets.bottom + 40 }}
        contentContainerClassName="gap-7 px-4"
      >
        <View className="items-center gap-2 pt-2">
          <BrandMark size={72} />
          <Text className="pt-1 text-xl font-bold tracking-tight">Threadknot</Text>
          <Text className="text-center text-sm text-muted-foreground">
            Every coding agent on one thread.
          </Text>
        </View>

        <Card className="py-4">
          <CardContent>
            <Fact label="Version" value={`${version} (${build})`} />
            <Separator />
            <Fact label="Bundle" value={bundle} />
            <Separator />
            <Fact label="Platform" value={platform} />
            <Button variant="outline" size="sm" className="mt-3 self-start" onPress={copyDiagnostics}>
              <Icon as={Copy} className="size-4 text-foreground" />
              <Text>{copied ? 'Copied' : 'Copy for support'}</Text>
            </Button>
          </CardContent>
        </Card>

        <Section title="Who makes it">
          <P>
            Threadknot is built by Smith Network Solutions. The desktop app it pairs with runs
            Claude Code, Codex, Kimi and remote Hermes gateways natively over their own wire
            protocols, and serves the same console to this phone.
          </P>
        </Section>

        <Section title="Open source">
          <P>
            This app is built on Expo, React Native and react-native-reusables, and uses Lucide
            icons. Their licences are reproduced on the website.
          </P>
        </Section>

        <View className="gap-3">
          <Button variant="outline" onPress={() => void openExternal(LEGAL.home)}>
            <Icon as={ExternalLink} className="size-4 text-foreground" />
            <Text>threadknot.ai</Text>
          </Button>
          <Text className="text-center text-xs text-muted-foreground">
            © 2026 Smith Network Solutions
          </Text>
        </View>
      </ScrollView>
    </View>
  );
}
