#!/usr/bin/env python3
"""Regenerate every Threadknot mobile icon asset from the master artwork.

The master is a square render of the brand mark: a gold serif "T" bound by a
rope knot, sitting on a near-black plate with a thin gold rule around it. Two
families of asset come out of it:

  * plate assets  — the artwork as-is (iOS app icon, favicon). iOS applies its
    own superellipse mask, so the plate keeps a black margin outside the gold
    rule and the mask never bites into the rule.
  * mark assets   — the glyph alone on transparency (Android adaptive
    foreground, splash, and the in-app brand mark that replaced the stock
    anchor). The plate and its rule are keyed out; what is left is the T.

Keying is not a plain luminance threshold: the gold rule is exactly as bright
as the glyph, so a threshold keeps the ring too. Nor is the glyph one blob —
the rope's shadow separates each coil from the stem, so seeding a flood fill
anywhere reaches only part of the T. What actually distinguishes the rule is
its *extent*: it is the only lit shape that spans the whole frame. So the mask
is every lit component except the frame-spanning one.

Usage:  python3 scripts/make-icons.py [path/to/master.png]
"""

from __future__ import annotations

import os
import sys

import numpy as np
from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
ASSETS = os.path.join(HERE, "..", "assets", "images")
MASTER = os.path.join(ASSETS, "brand-source.png")

# The plate, sampled from the artwork's interior. Also app.json's
# backgroundColor / splash / adaptive-icon background, so they agree.
PLATE = (11, 13, 18)


def load_master(path: str) -> Image.Image:
    im = Image.open(path).convert("RGB")
    if im.width != im.height:
        raise SystemExit(f"master must be square, got {im.size}")
    return im


DOWNSCALE = 4
SPANS_FRAME = 0.85  # a component this wide *and* tall is the rule, not the T
MIN_COMPONENT = 20  # pooled pixels; below this it is JPEG speckle


def dilate(mask: np.ndarray, steps: int = 1) -> np.ndarray:
    for _ in range(steps):
        nxt = mask.copy()
        nxt[1:, :] |= mask[:-1, :]
        nxt[:-1, :] |= mask[1:, :]
        nxt[:, 1:] |= mask[:, :-1]
        nxt[:, :-1] |= mask[:, 1:]
        mask = nxt
    return mask


def pool_max(lum: np.ndarray, k: int) -> np.ndarray:
    """Downscale by max, not by sampling — sampling drops thin lit strands."""
    n = lum.shape[0] // k * k
    return lum[:n, :n].reshape(n // k, k, n // k, k).max(axis=(1, 3))


def component(mask: np.ndarray, seed: tuple[int, int]) -> np.ndarray:
    cur = np.zeros_like(mask)
    cur[seed] = True
    while True:
        nxt = dilate(cur) & mask
        if nxt.sum() == cur.sum():
            return cur
        cur = nxt


def glyph_gate(lum: np.ndarray) -> np.ndarray:
    """Full-res boolean gate covering the T and excluding the plate's rule."""
    pooled = pool_max(lum, DOWNSCALE) > 55
    span = pooled.shape[0] * SPANS_FRAME

    keep = np.zeros_like(pooled)
    unvisited = pooled.copy()
    for y, x in zip(*np.where(pooled)):
        if not unvisited[y, x]:
            continue
        comp = component(pooled, (y, x))
        unvisited &= ~comp
        ys, xs = np.where(comp)
        wide = xs.max() - xs.min() >= span and ys.max() - ys.min() >= span
        if not wide and comp.sum() >= MIN_COMPONENT:
            keep |= comp

    if not keep.any():
        raise SystemExit("no glyph found — is the master the right artwork?")

    up = np.repeat(np.repeat(keep, DOWNSCALE, axis=0), DOWNSCALE, axis=1)
    up = np.pad(up, ((0, lum.shape[0] - up.shape[0]), (0, lum.shape[1] - up.shape[1])))
    # Bleed outward so the glyph's antialiased shoulder survives the gate.
    return dilate(up, 6)


def extract_mark(im: Image.Image) -> Image.Image:
    """The glyph on transparency, trimmed to its own bounds."""
    a = np.asarray(im).astype(np.float32)
    lum = a.max(axis=2)

    # Alpha ramps over the glyph's antialiased shoulder rather than clipping,
    # so the mark keeps its bevel instead of gaining a hard cut edge.
    alpha = np.clip((lum - 18.0) / 55.0, 0.0, 1.0)
    alpha[~glyph_gate(lum)] = 0.0

    rgba = np.dstack([a, alpha * 255.0]).astype(np.uint8)
    out = Image.fromarray(rgba, "RGBA")
    box = out.getchannel("A").point(lambda v: 255 if v > 8 else 0).getbbox()
    return out.crop(box)


def square(mark: Image.Image, size: int, coverage: float) -> Image.Image:
    """Centre `mark` on a transparent square, scaled to `coverage` of the side."""
    target = int(size * coverage)
    scale = target / max(mark.size)
    m = mark.resize(
        (max(1, round(mark.width * scale)), max(1, round(mark.height * scale))),
        Image.LANCZOS,
    )
    canvas = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    canvas.paste(m, ((size - m.width) // 2, (size - m.height) // 2), m)
    return canvas


def flatten(img: Image.Image, bg: tuple[int, int, int]) -> Image.Image:
    plate = Image.new("RGB", img.size, bg)
    plate.paste(img, (0, 0), img)
    return plate


def write(img: Image.Image, name: str) -> None:
    path = os.path.join(ASSETS, name)
    img.save(path)
    print(f"  {name:34s} {img.size[0]}x{img.size[1]} {img.mode}")


def main() -> None:
    src = load_master(sys.argv[1] if len(sys.argv) > 1 else MASTER)
    mark = extract_mark(src)
    print(f"master {src.size}, glyph {mark.size}")

    # iOS/store icon: the full plate, no alpha channel (the App Store rejects
    # icons that carry one), 1024 square.
    write(src.resize((1024, 1024), Image.LANCZOS).convert("RGB"), "icon.png")

    # Android adaptive foreground: 1024 canvas, but Android crops to a mask and
    # animates within it, so the glyph must stay inside the 66% safe circle.
    write(square(mark, 1024, 0.52), "android-icon-foreground.png")
    write(Image.new("RGB", (1024, 1024), PLATE), "android-icon-background.png")

    # Themed (monochrome) Android icon: silhouette only, tinted by the system.
    mono = square(mark, 1024, 0.52)
    solid = Image.new("RGBA", mono.size, (255, 255, 255, 255))
    solid.putalpha(mono.getchannel("A"))
    write(solid, "android-icon-monochrome.png")

    # Splash: expo-splash-screen draws this over the plate colour at 76pt, so
    # ship the mark alone and let the background come from app.json.
    write(square(mark, 512, 0.86), "splash-icon.png")

    # Web favicon: the plate again, small.
    write(src.resize((96, 96), Image.LANCZOS).convert("RGB"), "favicon.png")

    # In-app mark, rendered by <BrandMark/> at up to 72pt — 256 covers @3x
    # with room to spare, and keeps 100KB out of the bundle.
    write(square(mark, 256, 0.92), "brand-mark.png")

    # Wordmark-adjacent glow tile used on dark cards.
    write(flatten(square(mark, 512, 0.72), PLATE), "logo-glow.png")


if __name__ == "__main__":
    main()
