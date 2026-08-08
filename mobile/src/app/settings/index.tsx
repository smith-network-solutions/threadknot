import { BrandMark } from '@/components/BrandMark';
import { ScreenHeader } from '@/components/ScreenHeader';
import { Card } from '@/components/ui/card';
import { Icon } from '@/components/ui/icon';
import { Text } from '@/components/ui/text';
import { LEGAL, openExternal } from '@/lib/legal';
import { useServers } from '@/lib/servers';
import { type Href, useRouter } from 'expo-router';
import {
  BellRing,
  ChevronRight,
  ExternalLink,
  FileText,
  Info,
  LifeBuoy,
  type LucideIcon,
  Server,
  ShieldCheck,
  UserCog,
} from 'lucide-react-native';
import * as React from 'react';
import { Pressable, ScrollView, View } from 'react-native';
import { useSafeAreaInsets } from 'react-native-safe-area-context';

interface RowProps {
  icon: LucideIcon;
  label: string;
  detail?: string;
  onPress(): void;
  /** Shows the leaves-the-app glyph instead of the drill-in chevron. */
  external?: boolean;
}

function Row({ icon, label, detail, onPress, external }: RowProps) {
  return (
    <Pressable
      onPress={onPress}
      accessibilityRole="button"
      className="flex-row items-center gap-3 px-4 py-3.5 active:bg-accent"
    >
      <Icon as={icon} className="size-5 text-brass" />
      <View className="flex-1 gap-0.5">
        <Text className="text-foreground">{label}</Text>
        {detail ? (
          <Text className="text-xs text-muted-foreground" numberOfLines={1}>
            {detail}
          </Text>
        ) : null}
      </View>
      <Icon
        as={external ? ExternalLink : ChevronRight}
        className="size-4 text-muted-foreground"
      />
    </Pressable>
  );
}

function Group({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <View className="gap-2">
      <Text className="px-1 text-xs uppercase tracking-widest text-muted-foreground">{title}</Text>
      <Card className="overflow-hidden py-0">{children}</Card>
    </View>
  );
}

export default function SettingsScreen() {
  const router = useRouter();
  const insets = useSafeAreaInsets();
  const { profiles } = useServers();

  const muted = profiles.filter((p) => !p.notificationsEnabled).length;
  const go = (href: Href) => () => router.push(href);

  return (
    <View className="flex-1 bg-background" style={{ paddingTop: insets.top + 8 }}>
      <ScreenHeader title="Settings" />

      <ScrollView
        contentContainerStyle={{ paddingBottom: insets.bottom + 32 }}
        contentContainerClassName="gap-6 px-3"
      >
        <View className="items-center gap-2 pb-2 pt-4">
          <BrandMark size={56} />
          <Text className="text-lg font-semibold tracking-tight">Threadknot</Text>
          <Text className="text-center text-xs text-muted-foreground">
            Every coding agent on one thread.
          </Text>
        </View>

        <Group title="connection">
          <Row
            icon={Server}
            label="Servers"
            detail={
              profiles.length === 0
                ? 'No servers paired yet'
                : `${profiles.length} paired${muted > 0 ? ` · ${muted} muted` : ''}`
            }
            onPress={go('/servers')}
          />
          <Row
            icon={BellRing}
            label="Notifications"
            detail="Per-server, on the server's own screen"
            onPress={go('/servers')}
          />
        </Group>

        <Group title="account">
          <Row
            icon={UserCog}
            label="Account & data"
            detail="What is stored, and how to delete it"
            onPress={go('/settings/account')}
          />
        </Group>

        <Group title="legal">
          <Row icon={ShieldCheck} label="Privacy Policy" onPress={go('/settings/privacy')} />
          <Row icon={FileText} label="Terms of Use" onPress={go('/settings/terms')} />
        </Group>

        <Group title="help">
          <Row icon={LifeBuoy} label="Support" onPress={go('/settings/support')} />
          <Row icon={Info} label="About" onPress={go('/settings/about')} />
          <Row
            icon={ExternalLink}
            label="threadknot.ai"
            detail={LEGAL.home}
            external
            onPress={() => void openExternal(LEGAL.home)}
          />
        </Group>
      </ScrollView>
    </View>
  );
}
