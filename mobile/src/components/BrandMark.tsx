import { cn } from '@/lib/utils';
import { Image } from 'expo-image';
import * as React from 'react';
import { View } from 'react-native';

// The keyed-out glyph from the app icon — see scripts/make-icons.py. Kept as a
// raster rather than an SVG because the mark's gold is a rendered bevel, not a
// flat fill: a traced outline of it reads as a different logo.
const MARK = require('@/assets/images/brand-mark.png');

interface Props {
  /** Side of the plate, in points. The glyph is inset within it. */
  size?: number;
  className?: string;
}

/**
 * The Threadknot mark on the same rounded plate the app icon uses.
 *
 * Every launch-shaped screen (lock, pair, first-run) opens with this, so the
 * phone shows the same thing the home screen icon did a second earlier.
 */
export function BrandMark({ size = 64, className }: Props) {
  return (
    <View
      style={{ height: size, width: size }}
      className={cn(
        'items-center justify-center rounded-2xl border border-border bg-card',
        className
      )}
    >
      <Image
        source={MARK}
        style={{ height: size * 0.64, width: size * 0.64 }}
        contentFit="contain"
        // Bundled asset: no placeholder flicker wanted on the lock screen.
        transition={0}
        accessibilityLabel="Threadknot"
      />
    </View>
  );
}
