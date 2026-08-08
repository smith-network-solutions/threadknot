import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent } from '@/components/ui/card';
import { Icon } from '@/components/ui/icon';
import { Text } from '@/components/ui/text';
import { useServers } from '@/lib/servers';
import { useRouter } from 'expo-router';
import { ChevronLeft, ChevronRight, Plus, Server } from 'lucide-react-native';
import * as React from 'react';
import { Pressable, ScrollView, View } from 'react-native';
import { useSafeAreaInsets } from 'react-native-safe-area-context';

export default function ServersScreen() {
  const router = useRouter();
  const insets = useSafeAreaInsets();
  const { profiles, activeId, setActive } = useServers();

  return (
    <View className="flex-1 bg-background" style={{ paddingTop: insets.top + 8 }}>
      <View className="flex-row items-center gap-2 px-3 pb-3">
        <Pressable
          onPress={() => router.back()}
          className="h-9 w-9 items-center justify-center rounded-full active:bg-accent"
        >
          <Icon as={ChevronLeft} className="size-5 text-muted-foreground" />
        </Pressable>
        <Text className="flex-1 text-lg font-semibold">Servers</Text>
        <Button size="sm" variant="outline" onPress={() => router.push('/servers/add')}>
          <Icon as={Plus} className="size-4 text-foreground" />
          <Text>Add</Text>
        </Button>
      </View>

      <ScrollView contentContainerStyle={{ paddingBottom: insets.bottom + 24 }} className="px-3">
        <View className="gap-3">
          {profiles.map((p) => (
            <Pressable key={p.id} onPress={() => router.push(`/servers/${p.id}`)}>
              <Card className={p.id === activeId ? 'border-brass/50' : undefined}>
                <CardContent className="flex-row items-center gap-3 py-4">
                  <View className="h-10 w-10 items-center justify-center rounded-lg border border-border bg-muted">
                    <Icon as={Server} className="size-5 text-brass" />
                  </View>
                  <View className="flex-1 gap-0.5">
                    <View className="flex-row items-center gap-2">
                      <Text className="font-semibold">{p.name}</Text>
                      {p.id === activeId && (
                        <Badge variant="outline">
                          <Text>active</Text>
                        </Badge>
                      )}
                      {p.ingress === 'remote' && (
                        <Badge variant="outline">
                          <Text>relay</Text>
                        </Badge>
                      )}
                      {!p.notificationsEnabled && (
                        <Badge variant="secondary">
                          <Text>muted</Text>
                        </Badge>
                      )}
                    </View>
                    <Text className="text-xs text-muted-foreground" numberOfLines={1}>
                      {p.baseUrl}
                    </Text>
                  </View>
                  <Button
                    size="sm"
                    variant={p.id === activeId ? 'secondary' : 'default'}
                    onPress={() => {
                      setActive(p.id);
                      router.dismissTo('/');
                    }}
                  >
                    <Text>Open</Text>
                  </Button>
                  <Icon as={ChevronRight} className="size-4 text-muted-foreground" />
                </CardContent>
              </Card>
            </Pressable>
          ))}
          {profiles.length === 0 && (
            <Text className="pt-12 text-center text-muted-foreground">
              No servers yet — add your first one.
            </Text>
          )}
        </View>
      </ScrollView>
    </View>
  );
}
