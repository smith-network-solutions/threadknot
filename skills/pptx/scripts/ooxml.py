#!/usr/bin/env python3
"""Unpack / repack / validate an Office Open XML file (.docx, .xlsx, .pptx).

The escape hatch for anything the Python libraries do not model: tracked
changes, comments, footnotes, content controls, chart XML. Unpack to a tree,
edit the XML, pack it back.

    ooxml.py unpack contract.docx /tmp/contract/
    ooxml.py pack   /tmp/contract/ contract-v2.docx
    ooxml.py validate contract-v2.docx

Stdlib only — no dependencies, so it works anywhere Python does.

Packing rules that matter (Office rejects the file otherwise):
  * `[Content_Types].xml` must be the FIRST entry in the archive.
  * No directory entries.
  * Forward-slash paths.
"""
import argparse
import shutil
import sys
import xml.dom.minidom as minidom
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path

CONTENT_TYPES = "[Content_Types].xml"
# Pretty-printing these would corrupt them: they are not XML, or whitespace is
# significant enough that reformatting changes rendering.
NEVER_PRETTY = {".bin", ".png", ".jpeg", ".jpg", ".gif", ".emf", ".wmf", ".bmp", ".tiff"}


def unpack(src: Path, dest: Path, pretty: bool = True) -> int:
    if dest.exists() and any(dest.iterdir()):
        print(f"{dest} exists and is not empty — refusing to overwrite", file=sys.stderr)
        return 1
    dest.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(src) as zf:
        zf.extractall(dest)
        names = zf.namelist()

    if pretty:
        for name in names:
            path = dest / name
            if not path.is_file() or path.suffix.lower() in NEVER_PRETTY:
                continue
            if path.suffix.lower() not in {".xml", ".rels"}:
                continue
            try:
                raw = path.read_bytes()
                parsed = minidom.parseString(raw)
                # toprettyxml adds whitespace text nodes; harmless in OOXML
                # except inside <w:t>, which is why pack() re-collapses them.
                path.write_bytes(parsed.toprettyxml(indent="  ", encoding="UTF-8"))
            except Exception:
                # A part that will not parse is left exactly as it was; the
                # point of unpack is to let a human see it, not to fix it.
                pass

    print(f"unpacked {len(names)} part(s) into {dest}")
    print("main parts:")
    for name in sorted(names):
        if name.endswith((".xml", ".rels")) and name.count("/") <= 1:
            print(f"  {name}")
    return 0


def collapse_pretty_whitespace(data: bytes) -> bytes:
    """Undo pretty-printing inside text-bearing elements.

    `unpack --pretty` indents everything, which inserts whitespace into
    `<w:t>`/`<a:t>` runs and visibly changes the document. Rather than track
    which parts were prettified, strip the indentation that minidom added:
    remove whitespace-only text between tags.
    """
    out = bytearray()
    i = 0
    n = len(data)
    while i < n:
        start = data.find(b">", i)
        if start == -1:
            out += data[i:]
            break
        out += data[i:start + 1]
        end = data.find(b"<", start + 1)
        if end == -1:
            out += data[start + 1:]
            break
        chunk = data[start + 1:end]
        # Whitespace-only run between two tags: drop it. Anything else is real
        # content and is kept byte for byte.
        out += b"" if chunk.strip() == b"" else chunk
        i = end
    return bytes(out)


def pack(src: Path, dest: Path) -> int:
    ct = src / CONTENT_TYPES
    if not ct.is_file():
        print(f"{src} has no {CONTENT_TYPES} — is it an unpacked OOXML tree?", file=sys.stderr)
        return 1

    files = [p for p in sorted(src.rglob("*")) if p.is_file()]
    # [Content_Types].xml must come first.
    files.sort(key=lambda p: (p != ct, str(p.relative_to(src))))

    with zipfile.ZipFile(dest, "w", zipfile.ZIP_DEFLATED) as zf:
        for path in files:
            arcname = str(path.relative_to(src)).replace("\\", "/")
            data = path.read_bytes()
            if path.suffix.lower() in {".xml", ".rels"}:
                data = collapse_pretty_whitespace(data)
            # Content types is conventionally stored, not deflated.
            zf.writestr(arcname, data,
                        zipfile.ZIP_STORED if path == ct else zipfile.ZIP_DEFLATED)

    print(f"packed {len(files)} part(s) into {dest}")
    return validate(dest)


def validate(path: Path) -> int:
    if not zipfile.is_zipfile(path):
        print(f"{path} is not a ZIP archive", file=sys.stderr)
        return 1
    problems = []
    with zipfile.ZipFile(path) as zf:
        names = zf.namelist()
        if CONTENT_TYPES not in names:
            problems.append(f"missing {CONTENT_TYPES}")
        elif names[0] != CONTENT_TYPES:
            problems.append(f"{CONTENT_TYPES} must be the first entry (found {names[0]})")
        bad = zf.testzip()
        if bad:
            problems.append(f"corrupt entry: {bad}")
        for name in names:
            if name.endswith((".xml", ".rels")):
                try:
                    ET.fromstring(zf.read(name))
                except ET.ParseError as exc:
                    problems.append(f"{name}: {exc}")

    if problems:
        print(f"INVALID {path}", file=sys.stderr)
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)
        return 1
    print(f"OK {path}: {len(names)} part(s), all XML parses")
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)

    p_un = sub.add_parser("unpack", help="extract an OOXML file to a directory")
    p_un.add_argument("src")
    p_un.add_argument("dest")
    p_un.add_argument("--raw", action="store_true", help="do not pretty-print the XML parts")

    p_pk = sub.add_parser("pack", help="rebuild an OOXML file from a directory")
    p_pk.add_argument("src")
    p_pk.add_argument("dest")

    p_va = sub.add_parser("validate", help="check archive layout and XML well-formedness")
    p_va.add_argument("path")

    args = ap.parse_args()
    if args.cmd == "unpack":
        return unpack(Path(args.src), Path(args.dest), pretty=not args.raw)
    if args.cmd == "pack":
        return pack(Path(args.src), Path(args.dest))
    return validate(Path(args.path))


if __name__ == "__main__":
    raise SystemExit(main())
