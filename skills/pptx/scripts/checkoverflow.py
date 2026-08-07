#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["python-pptx>=1.0"]
# ///
"""Flag shapes that fall off the slide and text that probably overflows its box.

python-pptx does not lay text out, so nothing warns you when a bullet list is
twice the height of its placeholder — the deck saves happily and looks wrong
only when someone opens it. This is a cheap first pass that needs no rendering;
`topdf.py --png` remains the authoritative check.

    checkoverflow.py deck.pptx

Exits non-zero if anything was flagged, so it can gate a build.
"""
import argparse
import sys

from pptx import Presentation
from pptx.util import Emu, Pt

# Rough average glyph width as a fraction of font size, for a proportional
# sans face. Deliberately conservative: this exists to catch the badly wrong,
# not to replicate PowerPoint's line breaker.
CHAR_WIDTH_RATIO = 0.50
LINE_HEIGHT_RATIO = 1.22
DEFAULT_FONT_PT = 18


def para_font_pt(paragraph, fallback=DEFAULT_FONT_PT):
    if paragraph.font.size is not None:
        return paragraph.font.size.pt
    for run in paragraph.runs:
        if run.font.size is not None:
            return run.font.size.pt
    return fallback


def estimate_text_height(text_frame, width_emu):
    """Estimated rendered height in EMU, from wrapped line count."""
    width_in = Emu(int(width_emu)).inches
    if width_in <= 0:
        return 0
    total_in = 0.0
    for paragraph in text_frame.paragraphs:
        size_pt = para_font_pt(paragraph)
        text = paragraph.text
        char_in = (size_pt * CHAR_WIDTH_RATIO) / 72.0
        # Indent levels eat horizontal room.
        usable_in = max(width_in - 0.2 - 0.3 * (paragraph.level or 0), 0.5)
        chars_per_line = max(int(usable_in / char_in), 1) if char_in > 0 else 1
        lines = max(1, -(-len(text) // chars_per_line))  # ceil
        total_in += lines * (size_pt * LINE_HEIGHT_RATIO) / 72.0
    return Emu(int(total_in * 914400))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("path")
    ap.add_argument("--tolerance", type=float, default=0.05,
                    help="inches a shape may exceed the slide before it is flagged")
    args = ap.parse_args()

    try:
        prs = Presentation(args.path)
    except Exception as exc:  # noqa: BLE001
        print(f"could not open {args.path}: {exc}", file=sys.stderr)
        return 1

    slide_w = prs.slide_width
    slide_h = prs.slide_height
    tol = Emu(int(args.tolerance * 914400))
    problems = 0

    for i, slide in enumerate(prs.slides, start=1):
        for shape in slide.shapes:
            if shape.left is None or shape.top is None:
                continue
            name = f"slide {i} · {shape.name!r}"

            right = shape.left + (shape.width or 0)
            bottom = shape.top + (shape.height or 0)
            if shape.left < -tol or shape.top < -tol:
                print(f"OFF-SLIDE  {name}: starts at "
                      f"({Emu(int(shape.left)).inches:.2f}, {Emu(int(shape.top)).inches:.2f})in")
                problems += 1
            if right > slide_w + tol or bottom > slide_h + tol:
                print(f"OFF-SLIDE  {name}: extends to "
                      f"({Emu(int(right)).inches:.2f}, {Emu(int(bottom)).inches:.2f})in "
                      f"past {Emu(int(slide_w)).inches:.2f}x{Emu(int(slide_h)).inches:.2f}in")
                problems += 1

            if shape.has_text_frame and shape.text_frame.text.strip() and shape.height:
                needed = estimate_text_height(shape.text_frame, shape.width or 0)
                if needed > shape.height * 1.15:
                    print(f"OVERFLOW?  {name}: text needs about "
                          f"{Emu(int(needed)).inches:.2f}in in a "
                          f"{Emu(int(shape.height)).inches:.2f}in box")
                    problems += 1

            # `word_wrap` is tri-state: True (on), False (explicitly off), None
            # (inherit from the layout, which normally wraps). Only an explicit
            # False is a problem — treating None as off flags every placeholder
            # in a well-formed deck.
            if shape.has_text_frame and shape.text_frame.word_wrap is False:
                text = shape.text_frame.text.strip()
                if len(text) > 60:
                    print(f"NO-WRAP    {name}: word_wrap is off with {len(text)} characters")
                    problems += 1

    if problems:
        print(f"\n{problems} thing(s) to check — render with topdf.py --png and look.")
        return 1
    print("no obvious layout problems; render with topdf.py --png to be sure")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
