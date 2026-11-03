// Ported verbatim from the Alcove design prototype (C:/Projects/alcove):
// the frosted foreground surface. Shared by the Alcove skin; see src/styles/alcove/.
import { CSSProperties, FormHTMLAttributes, HTMLAttributes, ReactNode } from 'react';

type GlassExposure = 25 | 100 | 150;
type GlassProps = Omit<HTMLAttributes<HTMLElement>, 'onSubmit'> & FormHTMLAttributes<HTMLFormElement> & {
  as?: 'div' | 'form' | 'aside' | 'section' | 'header' | 'article';
  children: ReactNode;
  exposure?: GlassExposure;
  radius?: number;
  smoothing?: number;
};

/**
 * A reusable foreground surface. Exposure 150 is deliberately brighter than
 * CSS alpha can represent: it adds a refracted highlight and brightens only
 * the backdrop, leaving the content at its normal contrast.
 */
export function Glass({ as: Tag = 'div', children, className = '', exposure = 100, radius, smoothing, style, ...props }: GlassProps) {
  const glassStyle = {
    ...style,
    ...(radius === undefined ? {} : { borderRadius: radius }),
    ...(smoothing === undefined ? {} : { '--corner-smoothing': smoothing }),
  } as CSSProperties;

  const Element = Tag as any;
  const hasCustomGeometry = radius !== undefined || smoothing !== undefined;
  return <Element {...props} className={`glass glass--${exposure} ${hasCustomGeometry ? 'corner-smoothing' : ''} ${className}`.trim()} style={glassStyle}>{children}</Element>;
}
