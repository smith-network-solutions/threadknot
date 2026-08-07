import { Icon } from '@/components/ui/icon';
import { Text } from '@/components/ui/text';
import type { ServerProfile } from '@/lib/types';
import { cn } from '@/lib/utils';
import { useRouter } from 'expo-router';
import { Plus, Settings2 } from 'lucide-react-native';
import * as React from 'react';
import { Pressable, ScrollView, View } from 'react-native';

export type ConnState = 'connecting' | 'online' | 'offline';

interface Props {
  profiles: ServerProfile[];
  activeId: string | null;
  conn: Record<string, ConnState>;
  onSelect(id: string): void;
}

function dotClass(conn: ConnState | undefined): string {
  switch (conn) {
    case 'online':
      return 'bg-teal';
    case 'offline':
      return 'bg-destructive';
    default:
      return 'bg-muted-foreground';
  }
}

/** Compact chip strip above the WebView: one tap switches fleets. */
export function ServerSwitcher({ profiles, activeId, conn, onSelect }: Props) {
  const router = useRouter();
  return (
    <View className="flex-row items-center border-b border-border bg-background">
      <ScrollView
        horizontal
        showsHorizontalScrollIndicator={false}
        contentContainerClassName="flex-row items-center gap-2 px-3 py-2"
        className="flex-1"
      >
        {profiles.map((p) => {
          const active = p.id === activeId;
          return (
            <Pressable
              key={p.id}
              onPress={() => onSelect(p.id)}
              className={cn(
                'flex-row items-center gap-2 rounded-full border px-3 py-1.5',
                active ? 'border-brass/60 bg-brass/15' : 'border-border bg-card active:bg-accent'
              )}
            >
              <View className={cn('h-2 w-2 rounded-full', dotClass(conn[p.id]))} />
              <Text
                className={cn('text-sm', active ? 'font-semibold text-brass-hi' : 'text-foreground')}
                numberOfLines={1}
              >
                {p.name}
              </Text>
            </Pressable>
          );
        })}
        <Pressable
          onPress={() => router.push('/servers/add')}
          className="h-8 w-8 items-center justify-center rounded-full border border-dashed border-border active:bg-accent"
        >
          <Icon as={Plus} className="size-4 text-muted-foreground" />
        </Pressable>
      </ScrollView>
      <Pressable
        onPress={() => router.push('/servers')}
        className="mr-3 h-8 w-8 items-center justify-center rounded-full active:bg-accent"
      >
        <Icon as={Settings2} className="size-4 text-muted-foreground" />
      </Pressable>
    </View>
  );
}
