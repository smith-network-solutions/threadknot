// Pixel-map rasterising, cached.
//
// The stages used to paint every sprite as a scatter of 1x1 fillRect calls,
// which meant a wall of thirty-two enemies cost roughly eight hundred canvas
// calls per frame before anything else was drawn, and the maze repainted three
// hundred static tiles sixty times a second. Both are now rendered once into an
// offscreen canvas and blitted with a single drawImage.
//
// Everything here degrades to the slow path when there is no DOM, because the
// stage logic is exercised headlessly and must not require a browser to run.

/** A pixel map is rows of characters; "." is transparent, anything else is a
 *  key into the palette. */
export type PixelMap = readonly string[];
export type Palette = Readonly<Record<string, string>>;

const hasDom = typeof document !== "undefined";
const cache = new Map<string, HTMLCanvasElement | null>();

function bake(map: PixelMap, palette: Palette): HTMLCanvasElement | null {
  if (!hasDom) return null;
  const h = map.length;
  const w = h > 0 ? map[0].length : 0;
  if (w === 0 || h === 0) return null;
  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  const g = canvas.getContext("2d");
  if (!g) return null;
  for (let y = 0; y < h; y++) {
    const row = map[y];
    for (let x = 0; x < row.length; x++) {
      const colour = palette[row[x]];
      if (!colour) continue;
      g.fillStyle = colour;
      g.fillRect(x, y, 1, 1);
    }
  }
  return canvas;
}

/** The baked sprite, or null when it has to be painted the slow way. */
export function sprite(key: string, map: PixelMap, palette: Palette): HTMLCanvasElement | null {
  const hit = cache.get(key);
  if (hit !== undefined) return hit;
  const made = bake(map, palette);
  cache.set(key, made);
  return made;
}

/** One sprite, by whichever route is available. `key` must be unique per
 *  (map, palette) pair: it is what the cache is keyed on. */
export function drawSprite(
  g: CanvasRenderingContext2D,
  key: string,
  map: PixelMap,
  palette: Palette,
  x: number,
  y: number,
): void {
  const baked = sprite(key, map, palette);
  const px = Math.round(x);
  const py = Math.round(y);
  if (baked) {
    g.drawImage(baked, px, py);
    return;
  }
  for (let r = 0; r < map.length; r++) {
    const row = map[r];
    for (let c = 0; c < row.length; c++) {
      const colour = palette[row[c]];
      if (!colour) continue;
      g.fillStyle = colour;
      g.fillRect(px + c, py + r, 1, 1);
    }
  }
}

/**
 * A full-size layer painted once and blitted every frame.
 *
 * Used for anything that never changes during a level: the back-of-tube grid,
 * the maze walls. `paint` runs at most once per instance.
 */
export class StaticLayer {
  private canvas: HTMLCanvasElement | null = null;
  private painted = false;

  constructor(
    private readonly w: number,
    private readonly h: number,
    private readonly paint: (g: CanvasRenderingContext2D) => void,
  ) {}

  draw(g: CanvasRenderingContext2D): void {
    if (!this.painted) {
      this.painted = true;
      if (hasDom) {
        const c = document.createElement("canvas");
        c.width = this.w;
        c.height = this.h;
        const cg = c.getContext("2d");
        if (cg) {
          this.paint(cg);
          this.canvas = c;
        }
      }
    }
    if (this.canvas) g.drawImage(this.canvas, 0, 0);
    else this.paint(g); // headless, or a context we could not get
  }
}
