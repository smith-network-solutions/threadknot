#!/usr/bin/env python3
"""Recalculate an .xlsx so its formula cells carry correct cached values.

openpyxl writes formulas as text and never evaluates them. Until a real
spreadsheet application opens the file, every formula cell's cached value is
empty — so `load_workbook(data_only=True)`, pandas, a PDF export and anyone
opening it in a viewer all see blanks where the numbers should be.

This opens the workbook in headless LibreOffice, which recalculates on load, and
writes it back.

    recalc.py budget.xlsx                 # in place
    recalc.py budget.xlsx out.xlsx        # to a copy

Stdlib only. Requires `soffice` (LibreOffice) on PATH.
"""
import argparse
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
    mac = Path("/Applications/LibreOffice.app/Contents/MacOS/soffice")
    return str(mac) if mac.exists() else None


def recalc(src: Path, dest: Path, timeout: int = 240) -> int:
    soffice = find_soffice()
    if not soffice:
        print(
            "LibreOffice not found — it is what does the calculating. Install it:\n"
            "  Debian/Ubuntu: sudo apt install libreoffice-calc\n"
            "  macOS:         brew install --cask libreoffice\n"
            "  Fedora/Arch:   sudo dnf install libreoffice-calc / sudo pacman -S libreoffice-fresh",
            file=sys.stderr,
        )
        return 2
    if not src.is_file():
        print(f"no such file: {src}", file=sys.stderr)
        return 1

    with tempfile.TemporaryDirectory(prefix="soffice-recalc-") as tmp:
        # Private profile: a second soffice against a shared profile (including a
        # desktop LibreOffice the user has open) silently does nothing.
        cmd = [
            soffice,
            f"-env:UserInstallation=file://{tmp}/profile",
            "--headless",
            "--norestore",
            "--convert-to",
            "xlsx:Calc MS Excel 2007 XML",
            "--outdir",
            tmp,
            str(src.resolve()),
        ]
        try:
            proc = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        except subprocess.TimeoutExpired:
            print(f"LibreOffice timed out after {timeout}s", file=sys.stderr)
            return 1

        produced = Path(tmp) / (src.stem + ".xlsx")
        if not produced.is_file():
            print("recalculation produced no output", file=sys.stderr)
            for stream in (proc.stdout, proc.stderr):
                if stream.strip():
                    print(stream.strip(), file=sys.stderr)
            return 1
        # Move only after success, so a failed run never destroys the input.
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.move(str(produced), dest)

    print(f"recalculated -> {dest}")
    print("check the numbers landed:  inspect_xlsx.py "
          f"{dest} --values")
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("src")
    ap.add_argument("dest", nargs="?", help="output path (default: overwrite the source)")
    ap.add_argument("--timeout", type=int, default=240)
    args = ap.parse_args()

    src = Path(args.src)
    dest = Path(args.dest) if args.dest else src
    return recalc(src, dest, args.timeout)


if __name__ == "__main__":
    raise SystemExit(main())
