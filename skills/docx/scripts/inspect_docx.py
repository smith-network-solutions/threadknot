#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["python-docx>=1.1"]
# ///
"""Print the structure of a .docx: paragraphs with styles, tables, sections,
headers/footers and available styles.

Run this before editing any document you did not create — the paragraph indices
it prints are what the editing recipes address.

    inspect.py report.docx            # structure
    inspect.py report.docx --text     # body as plain text (tables as TSV)
"""
import argparse
import sys

from docx import Document
from docx.table import Table
from docx.text.paragraph import Paragraph


def iter_block_items(parent):
    """Yield paragraphs and tables in true document order.

    python-docx exposes `.paragraphs` and `.tables` as separate lists, which
    loses their interleaving — a report reads as "all the prose, then all the
    tables". Walking the body XML preserves the real order.
    """
    from docx.document import Document as _Document
    from docx.oxml.table import CT_Tbl
    from docx.oxml.text.paragraph import CT_P
    from docx.table import _Cell

    if isinstance(parent, _Document):
        parent_elm = parent.element.body
    elif isinstance(parent, _Cell):
        parent_elm = parent._tc
    else:
        parent_elm = parent._element

    for child in parent_elm.iterchildren():
        if isinstance(child, CT_P):
            yield Paragraph(child, parent)
        elif isinstance(child, CT_Tbl):
            yield Table(child, parent)


def table_rows(table):
    for row in table.rows:
        yield [cell.text.replace("\n", " ").strip() for cell in row.cells]


def dump_text(doc):
    for block in iter_block_items(doc):
        if isinstance(block, Paragraph):
            if block.text.strip():
                print(block.text)
        else:
            for cells in table_rows(block):
                print("\t".join(cells))
            print()


def dump_structure(doc, path):
    print(f"=== {path} ===\n")

    p_index = 0
    t_index = 0
    print("--- body ---")
    for block in iter_block_items(doc):
        if isinstance(block, Paragraph):
            style = block.style.name if block.style is not None else "?"
            text = block.text.strip()
            # Empty paragraphs are still addressable and often intentional
            # spacing, so they are listed rather than skipped.
            preview = text if len(text) <= 90 else text[:87] + "..."
            runs = len(block.runs)
            print(f"[p{p_index:>3}] ({style}, {runs} run{'s' if runs != 1 else ''}) {preview}")
            p_index += 1
        else:
            rows = len(block.rows)
            cols = len(block.columns)
            style = block.style.name if block.style is not None else "None (no borders)"
            print(f"[t{t_index:>3}] TABLE {rows}x{cols} style={style}")
            for cells in list(table_rows(block))[:3]:
                print(f"        | {' | '.join(cells)}")
            if rows > 3:
                print(f"        ... {rows - 3} more row(s)")
            t_index += 1

    print("\n--- sections ---")
    for i, section in enumerate(doc.sections):
        def inches(value):
            return f"{value.inches:.2f}in" if value is not None else "?"

        print(
            f"[{i}] page {inches(section.page_width)} x {inches(section.page_height)}"
            f"  margins L{inches(section.left_margin)} R{inches(section.right_margin)}"
            f" T{inches(section.top_margin)} B{inches(section.bottom_margin)}"
        )
        for label, part in (("header", section.header), ("footer", section.footer)):
            text = " / ".join(p.text.strip() for p in part.paragraphs if p.text.strip())
            print(f"    {label}: {text or '(empty)'}")

    print("\n--- inline shapes ---")
    shapes = doc.inline_shapes
    if not len(shapes):
        print("(none)")
    for i, shape in enumerate(shapes):
        print(f"[{i}] {shape.type} {shape.width.inches:.2f}in x {shape.height.inches:.2f}in")

    # A default Word document declares ~180 styles, almost all unused. Listing
    # them all buries the handful that matter, so lead with what the document
    # actually uses and keep the rest as a countable reference.
    print("\n--- styles in use ---")
    used = set()
    for block in iter_block_items(doc):
        if isinstance(block, Paragraph):
            if block.style is not None and block.style.name:
                used.add(block.style.name)
        elif block.style is not None and block.style.name:
            used.add(block.style.name)
    print(", ".join(sorted(used)) or "(none)")

    available = sorted(s.name for s in doc.styles if s.name)
    print(f"\n--- {len(available)} styles available (use any of these by name) ---")
    print(", ".join(available))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("path")
    ap.add_argument("--text", action="store_true", help="print body text instead of structure")
    args = ap.parse_args()

    try:
        doc = Document(args.path)
    except Exception as exc:  # noqa: BLE001 - the message is for a human
        print(f"could not open {args.path}: {exc}", file=sys.stderr)
        return 1

    if args.text:
        dump_text(doc)
    else:
        dump_structure(doc, args.path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
