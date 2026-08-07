#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pdfplumber>=0.11"]
# ///
"""Get content out of a PDF: text, tables, or page images.

    extract.py report.pdf                    # text, in content order
    extract.py report.pdf --layout           # position-aware (fixes columns)
    extract.py report.pdf --pages 1-3        # 1-based, inclusive
    extract.py report.pdf --tables           # tables as CSV
    extract.py report.pdf --png              # rasterise pages to look at them
    extract.py scan.pdf   --ocr              # scanned pages via tesseract

`--layout` matters more than it sounds: a PDF stores text in drawing order, not
reading order, so a two-column paper extracts as interleaved nonsense without
it.
"""
import argparse
import csv
import io
import shutil
import subprocess
import sys
from pathlib import Path

import pdfplumber


def parse_pages(spec: str | None, total: int) -> list[int]:
    """1-based inclusive spec to 0-based indices; None means every page."""
    if not spec:
        return list(range(total))
    out: list[int] = []
    for part in spec.split(","):
        part = part.strip()
        if not part:
            continue
        if "-" in part:
            lo_s, _, hi_s = part.partition("-")
            lo, hi = int(lo_s), int(hi_s)
        else:
            lo = hi = int(part)
        if lo < 1 or hi < lo:
            raise ValueError(f"bad page range: {part!r}")
        out.extend(range(lo - 1, min(hi, total)))
    return out


def dump_text(pdf, pages, layout: bool) -> int:
    empty = 0
    for index in pages:
        page = pdf.pages[index]
        # layout=True reconstructs spatial arrangement, at the cost of padding
        # whitespace — worth it whenever columns or tables-as-whitespace matter.
        text = page.extract_text(layout=layout) or ""
        print(f"--- page {index + 1} ---")
        print(text)
        print()
        if len(text.strip()) < 40:
            empty += 1
    if empty == len(pages) and pages:
        print(
            "No extractable text on any requested page — this looks like a scan. "
            "Re-run with --ocr.",
            file=sys.stderr,
        )
        return 1
    return 0


def dump_tables(pdf, pages) -> int:
    writer = csv.writer(sys.stdout)
    found = 0
    for index in pages:
        page = pdf.pages[index]
        for t, table in enumerate(page.extract_tables(), start=1):
            found += 1
            print(f"# page {index + 1} table {t}")
            for row in table:
                writer.writerow(["" if cell is None else cell.replace("\n", " ") for cell in row])
            print()
    if not found:
        print(
            "No tables detected. pdfplumber finds tables by their ruling lines; a "
            "table laid out with whitespace alone will not be found — use --layout "
            "and parse the text instead.",
            file=sys.stderr,
        )
        return 1
    return 0


def dump_png(path: Path, pages, dpi: int) -> int:
    """Rasterise via pdftoppm/ImageMagick — no AGPL renderer required."""
    stem = path.with_suffix("")
    first, last = min(pages) + 1, max(pages) + 1
    if shutil.which("pdftoppm"):
        cmd = ["pdftoppm", "-png", "-r", str(dpi), "-f", str(first), "-l", str(last),
               str(path), str(stem)]
    elif shutil.which("magick") or shutil.which("convert"):
        binary = shutil.which("magick") or shutil.which("convert")
        cmd = [binary, "-density", str(dpi), f"{path}[{first - 1}-{last - 1}]",
               f"{stem}-%d.png"]
    else:
        print("install poppler-utils (pdftoppm) or ImageMagick to rasterise pages",
              file=sys.stderr)
        return 2
    proc = subprocess.run(cmd, capture_output=True, text=True)
    if proc.returncode != 0:
        print(proc.stderr.strip() or "rasterising failed", file=sys.stderr)
        return 1
    images = sorted(stem.parent.glob(f"{stem.name}-*.png"))
    for image in images:
        print(f"wrote {image}")
    return 0 if images else 1


def dump_ocr(path: Path, pages, dpi: int) -> int:
    if not shutil.which("tesseract"):
        print(
            "tesseract not found — it is what reads scanned pages:\n"
            "  Debian/Ubuntu: sudo apt install tesseract-ocr\n"
            "  macOS:         brew install tesseract\n"
            "  Fedora/Arch:   sudo dnf install tesseract / sudo pacman -S tesseract",
            file=sys.stderr,
        )
        return 2
    if not shutil.which("pdftoppm"):
        print("pdftoppm (poppler-utils) is needed to rasterise before OCR", file=sys.stderr)
        return 2

    import tempfile

    with tempfile.TemporaryDirectory(prefix="ocr-") as tmp:
        prefix = Path(tmp) / "page"
        for index in pages:
            page_no = index + 1
            subprocess.run(
                ["pdftoppm", "-png", "-r", str(dpi), "-f", str(page_no), "-l", str(page_no),
                 str(path), str(prefix)],
                capture_output=True, check=False,
            )
            images = sorted(Path(tmp).glob("page-*.png"))
            if not images:
                continue
            image = images[-1]
            proc = subprocess.run(["tesseract", str(image), "stdout"],
                                  capture_output=True, text=True)
            print(f"--- page {page_no} (OCR) ---")
            print(proc.stdout.strip())
            print()
            image.unlink(missing_ok=True)
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("path")
    ap.add_argument("--pages", help="1-based, inclusive: '2-5,9' (default: all)")
    ap.add_argument("--layout", action="store_true", help="position-aware text")
    ap.add_argument("--tables", action="store_true", help="emit detected tables as CSV")
    ap.add_argument("--png", action="store_true", help="rasterise pages to PNG")
    ap.add_argument("--ocr", action="store_true", help="OCR the pages (needs tesseract)")
    ap.add_argument("--dpi", type=int, default=150)
    args = ap.parse_args()

    path = Path(args.path)
    if not path.is_file():
        print(f"no such file: {path}", file=sys.stderr)
        return 1

    try:
        with pdfplumber.open(path) as pdf:
            pages = parse_pages(args.pages, len(pdf.pages))
            if not pages:
                print("no pages selected", file=sys.stderr)
                return 2
            if args.png:
                return dump_png(path, pages, args.dpi)
            if args.ocr:
                return dump_ocr(path, pages, args.dpi)
            if args.tables:
                return dump_tables(pdf, pages)
            return dump_text(pdf, pages, args.layout)
    except ValueError as exc:
        print(exc, file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
