#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["python-docx>=1.1"]
# ///
"""Substitute placeholders in a .docx without losing formatting.

Two problems this solves that a naive loop does not:

1. Word splits a placeholder across runs. `{{CLIENT}}` is frequently stored as
   `{{CLI`, `ENT`, `}}` in three runs — a spell-check or edit is enough to cause
   it — so a per-run `str.replace` silently matches nothing. This merges the
   runs of a paragraph before substituting, keeping the first run's formatting.
2. `doc.paragraphs` misses text in tables, headers and footers. This walks all
   of them.

    replace.py in.docx out.docx --set CLIENT="Acme Ltd" --set DATE=2026-04-01
    replace.py in.docx out.docx --set NAME=Ada --delimiters "{{,}}"

Exits non-zero if any placeholder was never found, so a template that quietly
did not fill is a failure rather than a surprise for the reader.
"""
import argparse
import sys
from collections import Counter

from docx import Document


def replace_in_paragraph(paragraph, mapping, counts):
    """Replace across run boundaries, preserving the paragraph's formatting.

    The full text is reassembled, substituted, then written back into the FIRST
    run with the remaining runs emptied. That keeps the first run's character
    formatting for the whole paragraph — correct for the overwhelmingly common
    case of a uniformly formatted placeholder line, and the only option that
    survives a placeholder split across differently formatted runs.
    """
    if not paragraph.runs:
        return
    original = "".join(run.text for run in paragraph.runs)
    updated = original
    for token, value in mapping.items():
        if token in updated:
            counts[token] += updated.count(token)
            updated = updated.replace(token, value)
    if updated == original:
        return

    paragraph.runs[0].text = updated
    for run in paragraph.runs[1:]:
        run.text = ""


def walk_tables(tables, mapping, counts):
    for table in tables:
        for row in table.rows:
            for cell in row.cells:
                for paragraph in cell.paragraphs:
                    replace_in_paragraph(paragraph, mapping, counts)
                # Tables nest.
                walk_tables(cell.tables, mapping, counts)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("src")
    ap.add_argument("dest")
    ap.add_argument(
        "--set",
        action="append",
        default=[],
        metavar="KEY=VALUE",
        help="placeholder name and its replacement (repeatable)",
    )
    ap.add_argument(
        "--delimiters",
        default="{{,}}",
        help="opening,closing delimiters around each KEY (default '{{,}}'). "
        "Pass ',' for bare keys with no delimiters.",
    )
    args = ap.parse_args()

    open_d, _, close_d = args.delimiters.partition(",")
    mapping = {}
    for item in args.set:
        key, sep, value = item.partition("=")
        if not sep:
            print(f"--set expects KEY=VALUE, got {item!r}", file=sys.stderr)
            return 2
        mapping[f"{open_d}{key}{close_d}"] = value
    if not mapping:
        print("nothing to do: pass at least one --set KEY=VALUE", file=sys.stderr)
        return 2

    doc = Document(args.src)
    counts = Counter()

    for paragraph in doc.paragraphs:
        replace_in_paragraph(paragraph, mapping, counts)
    walk_tables(doc.tables, mapping, counts)

    for section in doc.sections:
        for part in (section.header, section.footer, section.first_page_header,
                     section.first_page_footer, section.even_page_header,
                     section.even_page_footer):
            if part is None:
                continue
            for paragraph in part.paragraphs:
                replace_in_paragraph(paragraph, mapping, counts)
            walk_tables(part.tables, mapping, counts)

    doc.save(args.dest)

    missing = [token for token in mapping if not counts[token]]
    for token in mapping:
        print(f"{token}: {counts[token]} replacement(s)")
    print(f"wrote {args.dest}")
    if missing:
        print(f"NOT FOUND: {', '.join(missing)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
