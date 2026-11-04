// Ported verbatim from the Alcove design prototype (C:/Projects/alcove):
// the iOS-style continuous corner. Shared by the Alcove skin; see src/styles/alcove/.
import { CSSProperties, ComponentPropsWithoutRef, ElementType, ReactNode } from 'react';

type CornerSmoothingProps<T extends ElementType> = {
  /** The conventional circular radius, in CSS units. */
  radius?: CSSProperties['borderRadius'];
  /**
   * 0 keeps a conventional rounded corner. 100 produces the fullest
   * superellipse curve supported by the browser.
   */
  smoothing?: number;
  as?: T;
  children?: ReactNode;
  className?: string;
  style?: CSSProperties;
} & Omit<ComponentPropsWithoutRef<T>, 'as' | 'children' | 'className' | 'style'>;

/**
 * Applies an iOS-style continuous corner where `corner-shape` is available,
 * with a normal `border-radius` fallback for older webviews.
 */
export function CornerSmoothing<T extends ElementType = 'div'>({
  as,
  radius,
  smoothing = 60,
  className,
  style,
  children,
  ...props
}: CornerSmoothingProps<T>) {
  const Component = as ?? 'div';
  const amount = Math.min(100, Math.max(0, smoothing));
  const cornerShape = `superellipse(${(1 + amount / 100).toFixed(2)})`;

  return (
    <Component
      {...props}
      className={['corner-smoothing', className].filter(Boolean).join(' ')}
      style={{
        ...style,
        ...(radius === undefined ? {} : { borderRadius: radius }),
        '--corner-smoothing': amount,
        // `cornerShape` has not reached React's CSS typings yet.
        cornerShape,
      } as CSSProperties}
    >
      {children}
    </Component>
  );
}
