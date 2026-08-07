#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["python-pptx>=1.0"]
# ///
"""Print the structure of a .pptx: slides, shapes with positions, placeholders,
tables, charts and notes — or the template's layouts.

    inspect_pptx.py deck.pptx              # every slide and shape
    inspect_pptx.py deck.pptx --text       # just the text, per slide
    inspect_pptx.py deck.pptx --layouts    # layouts + placeholder indices

`--layouts` is the one to run before authoring against a template: slides are
created from a layout and filled by placeholder index, and guessing those
indices is the usual cause of a deck with text in the wrong boxes.
"""
import argparse
import sys

from pptx import Presentation
from pptx.util import Emu


def inches(value) -> str:
    if value is None:
        return "?"
    return f"{Emu(int(value)).inches:.2f}"


def shape_line(shape, indent="  "):
    kind = str(shape.shape_type).split(" (")[0] if shape.shape_type is not None else "?"
    where = (
        f"@({inches(shape.left)},{inches(shape.top)}) "
        f"{inches(shape.width)}x{inches(shape.height)}in"
    )
    bits = [f"{indent}{shape.shape_id:>3} {shape.name!r} [{kind}] {where}"]
    if shape.is_placeholder:
        ph = shape.placeholder_format
        bits.append(f"{indent}    placeholder idx={ph.idx} type={ph.type}")
    if shape.has_text_frame:
        text = shape.text_frame.text.strip().replace("\n", " / ")
        if text:
            bits.append(f"{indent}    text: {text[:110]}{'…' if len(text) > 110 else ''}")
    if getattr(shape, "has_table", False) and shape.has_table:
        table = shape.table
        bits.append(f"{indent}    table {len(table.rows)}x{len(table.columns)}")
    if getattr(shape, "has_chart", False) and shape.has_chart:
        bits.append(f"{indent}    chart {shape.chart.chart_type}")
    return "\n".join(bits)


def walk_shapes(shapes, indent="  "):
    for shape in shapes:
        print(shape_line(shape, indent))
        # Group shapes carry their own children; a flat loop misses everything
        # inside them, which on a designed template is most of the content.
        if shape.shape_type is not None and str(shape.shape_type).startswith("GROUP"):
            try:
                walk_shapes(shape.shapes, indent + "    ")
            except AttributeError:
                pass


def dump_layouts(prs):
    print(f"slide size: {inches(prs.slide_width)} x {inches(prs.slide_height)} in\n")
    for mi, master in enumerate(prs.slide_masters):
        print(f"=== master {mi}: {master.name!r} ===")
        for li, layout in enumerate(master.slide_layouts):
            print(f"\n  layout[{li}] {layout.name!r}")
            if not layout.placeholders:
                print("    (no placeholders)")
            for ph in layout.placeholders:
                fmt = ph.placeholder_format
                print(
                    f"    idx={fmt.idx:<3} type={str(fmt.type):<24} {ph.name!r}"
                    f"  @({inches(ph.left)},{inches(ph.top)}) "
                    f"{inches(ph.width)}x{inches(ph.height)}in"
                )
    print(
        "\nUse: prs.slide_layouts[<layout index>] then slide.placeholders[<idx>]."
        "\nNote idx is the placeholder's own index, not its position in the list."
    )


def dump_text(prs):
    for i, slide in enumerate(prs.slides, start=1):
        print(f"--- slide {i} ---")
        for shape in slide.shapes:
            if shape.has_text_frame and shape.text_frame.text.strip():
                print(shape.text_frame.text)
        if slide.has_notes_slide:
            notes = slide.notes_slide.notes_text_frame.text.strip()
            if notes:
                print(f"[notes] {notes}")
        print()


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("path")
    ap.add_argument("--layouts", action="store_true", help="list the template's layouts and placeholders")
    ap.add_argument("--text", action="store_true", help="print slide text only")
    args = ap.parse_args()

    try:
        prs = Presentation(args.path)
    except Exception as exc:  # noqa: BLE001
        print(f"could not open {args.path}: {exc}", file=sys.stderr)
        return 1

    if args.layouts:
        dump_layouts(prs)
        return 0
    if args.text:
        dump_text(prs)
        return 0

    print(f"=== {args.path} ===")
    print(f"slide size: {inches(prs.slide_width)} x {inches(prs.slide_height)} in")
    print(f"slides: {len(prs.slides)}\n")

    for i, slide in enumerate(prs.slides, start=1):
        print(f"--- slide {i} (layout: {slide.slide_layout.name!r}) ---")
        walk_shapes(slide.shapes)
        if slide.has_notes_slide:
            notes = slide.notes_slide.notes_text_frame.text.strip()
            if notes:
                print(f"  notes: {notes[:150]}{'…' if len(notes) > 150 else ''}")
        print()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
