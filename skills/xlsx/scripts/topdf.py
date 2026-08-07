#!/usr/bin/env python3
"""Render an Office document to PDF (and optionally to PNG page images) with
headless LibreOffice.

Two uses, both important:

  * The user asked for a PDF. Build the .docx/.xlsx/.pptx, then convert.
  * Verification. Inspecting structure cannot tell you a table ran off the page,
    an image is the wrong scale, or a style silently fell back. Rendering can —
    and with --png you can actually look at the result.

    topdf.py report.docx                  # -> report.pdf beside it
    topdf.py report.docx out.pdf
    topdf.py deck.pptx out.pdf --png      # also out-1.png, out-2.png, ...

Stdlib only. Requires `soffice` (LibreOffice) on PATH.
"""
import argparse
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


def find_soffice() -> str | None:
    for name in ("soffice", "libreoffice"):
        found = shutil.which(name)
        if found:
            return found
    # macOS bundle install.
    mac = Path("/Applications/LibreOffice.app/Contents/MacOS/soffice")
    return str(mac) if mac.exists() else None


def convert(src: Path, dest: Path, timeout: int = 180) -> int:
    soffice = find_soffice()
    if not soffice:
        print(
            "LibreOffice not found. Install it to render documents:\n"
            "  Debian/Ubuntu: sudo apt install libreoffice\n"
            "  macOS:         brew install --cask libreoffice\n"
            "  Fedora/Arch:   sudo dnf install libreoffice / sudo pacman -S libreoffice-fresh",
            file=sys.stderr,
        )
        return 2
    if not src.is_file():
        print(f"no such file: {src}", file=sys.stderr)
        return 1

    with tempfile.TemporaryDirectory(prefix="soffice-") as tmp:
        # A private user profile per run. Without this, a second soffice while
        # one is already running (including a desktop LibreOffice the user has
        # open) either blocks or exits silently having written nothing.
        cmd = [
            soffice,
            f"-env:UserInstallation=file://{tmp}/profile",
            "--headless",
            "--norestore",
            "--convert-to",
            "pdf",
            "--outdir",
            tmp,
            str(src.resolve()),
        ]
        try:
            proc = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        except subprocess.TimeoutExpired:
            print(f"LibreOffice timed out after {timeout}s converting {src}", file=sys.stderr)
            return 1

        produced = Path(tmp) / (src.stem + ".pdf")
        if not produced.is_file():
            print(f"conversion produced nothing for {src}", file=sys.stderr)
            if proc.stdout.strip():
                print(proc.stdout.strip(), file=sys.stderr)
            if proc.stderr.strip():
                print(proc.stderr.strip(), file=sys.stderr)
            return 1
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.move(str(produced), dest)

    print(f"wrote {dest} ({dest.stat().st_size:,} bytes)")
    return 0


def to_png(pdf: Path, dpi: int) -> int:
    """Rasterise each page so the result can actually be looked at."""
    stem = pdf.with_suffix("")
    if shutil.which("pdftoppm"):
        cmd = ["pdftoppm", "-png", "-r", str(dpi), str(pdf), str(stem)]
    elif shutil.which("magick"):
        cmd = ["magick", "-density", str(dpi), str(pdf), f"{stem}-%d.png"]
    elif shutil.which("convert"):
        cmd = ["convert", "-density", str(dpi), str(pdf), f"{stem}-%d.png"]
    else:
        print(
            "no rasteriser found — install poppler-utils (pdftoppm) or ImageMagick "
            "to produce page images",
            file=sys.stderr,
        )
        return 2
    proc = subprocess.run(cmd, capture_output=True, text=True)
    if proc.returncode != 0:
        print(proc.stderr.strip() or "rasterising failed", file=sys.stderr)
        return 1
    images = sorted(stem.parent.glob(f"{stem.name}-*.png"))
    for image in images:
        print(f"wrote {image}")
    if not images:
        print("rasteriser reported success but wrote no images", file=sys.stderr)
        return 1
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("src")
    ap.add_argument("dest", nargs="?", help="output .pdf (default: alongside the source)")
    ap.add_argument("--png", action="store_true", help="also rasterise each page to PNG")
    ap.add_argument("--dpi", type=int, default=110, help="PNG resolution (default 110)")
    ap.add_argument("--timeout", type=int, default=180)
    args = ap.parse_args()

    src = Path(args.src)
    dest = Path(args.dest) if args.dest else src.with_suffix(".pdf")

    code = convert(src, dest, args.timeout)
    if code != 0:
        return code
    if args.png:
        return to_png(dest, args.dpi)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
