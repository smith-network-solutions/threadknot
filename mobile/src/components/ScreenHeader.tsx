import { Icon } from '@/components/ui/icon';
import { Text } from '@/components/ui/text';
import { useRouter } from 'expo-router';
import { ChevronLeft } from 'lucide-react-native';
import * as React from 'react';
import { Pressable, View } from 'react-native';

interface Props {
  title: string;
  /** Rendered at the trailing edge — an Add button, a badge, anything. */
  right?: React.ReactNode;
}

/** Back chevron + title, sized to the same 9pt hit target the app uses. */
export function ScreenHeader({ title, right }: Props) {
  const router = useRouter();
  return (
    <View className="flex-row items-center gap-2 px-3 pb-3">
      <Pressable
        onPress={() => router.back()}
        accessibilityRole="button"
        accessibilityLabel="Back"
        className="h-9 w-9 items-center justify-center rounded-full active:bg-accent"
      >
        <Icon as={ChevronLeft} className="size-5 text-muted-foreground" />
      </Pressable>
      <Text className="flex-1 text-lg font-semibold">{title}</Text>
      {right}
    </View>
  );
}
