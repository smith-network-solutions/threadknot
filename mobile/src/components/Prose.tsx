import { Text } from '@/components/ui/text';
import * as React from 'react';
import { View } from 'react-native';

/** A titled block of policy text. Used by the legal and support screens. */
export function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <View className="gap-2">
      <Text className="text-base font-semibold text-foreground">{title}</Text>
      {children}
    </View>
  );
}

/** One paragraph of body copy at the reading size the policy screens use. */
export function P({ children }: { children: React.ReactNode }) {
  return <Text className="text-sm leading-6 text-muted-foreground">{children}</Text>;
}

/** A bulleted point. `•` rather than a list view: these never get long enough
 * to need virtualising, and a Text row wraps correctly with the hanging indent. */
export function Bullet({ children }: { children: React.ReactNode }) {
  return (
    <View className="flex-row gap-2 pl-1">
      <Text className="text-sm leading-6 text-brass">•</Text>
      <Text className="flex-1 text-sm leading-6 text-muted-foreground">{children}</Text>
    </View>
  );
}

/** Small print under a policy screen: when it was last revised. */
export function Revised({ date }: { date: string }) {
  return <Text className="pt-2 text-xs text-muted-foreground">Last updated {date}</Text>;
}
