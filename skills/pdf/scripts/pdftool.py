#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pypdf[crypto]>=5.0"]
# # [crypto] pulls in `cryptography`, without which AES encrypt/decrypt raises
# # a DependencyError at the point of use rather than at import.
# ///
"""Page-level PDF operations: inspect, merge, split, extract, rotate, delete,
metadata, encryption and AcroForm fields.

    pdftool.py info      report.pdf
    pdftool.py merge     out.pdf a.pdf b.pdf
    pdftool.py split     in.pdf outdir/
    pdftool.py extract   in.pdf out.pdf --pages 2-5,9
    pdftool.py rotate    in.pdf out.pdf --pages 1 --degrees 90
    pdftool.py delete    in.pdf out.pdf --pages 3
    pdftool.py meta      in.pdf out.pdf --title "Q1" --author "Acme"
    pdftool.py encrypt   in.pdf out.pdf --password s3cret
    pdftool.py decrypt   in.pdf out.pdf --password s3cret
    pdftool.py fields    form.pdf
    pdftool.py fill      form.pdf out.pdf --set name=Ada --set agree=Yes

Page numbers are 1-based and inclusive everywhere — what a person sees in a
viewer. pypdf's own API is 0-based; this converts.
"""
import argparse
import sys
from pathlib import Path

from pypdf import PdfReader, PdfWriter
from pypdf.generic import NameObject, BooleanObject

# A page with less text than this is treated as "probably a scan", which is the
# difference between "extract the text" and "you need OCR".
SCANNED_TEXT_THRESHOLD = 40


def parse_pages(spec: str, total: int) -> list[int]:
    """'2-5,9' -> [1,2,3,4,8] as 0-based indices, validated against `total`."""
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
        if hi > total:
            raise ValueError(f"page {hi} requested but the file has {total}")
        out.extend(range(lo - 1, hi))
    if not out:
        raise ValueError("no pages selected")
    return out


def open_reader(path: str, password: str | None = None) -> PdfReader:
    reader = PdfReader(path)
    if reader.is_encrypted:
        if reader.decrypt(password or "") == 0:
            raise SystemExit(f"{path} is encrypted — pass --password")
    return reader


def cmd_info(args) -> int:
    reader = open_reader(args.path, args.password)
    print(f"=== {args.path} ===")
    print(f"pages: {len(reader.pages)}")
    print(f"encrypted: {reader.is_encrypted}")

    meta = reader.metadata or {}
    if meta:
        print("\nmetadata:")
        for key, value in meta.items():
            print(f"  {str(key).lstrip('/'):<14} {value}")

    fields = reader.get_fields() or {}
    print(f"\nform fields: {len(fields)}" + (" (use `fields` to list them)" if fields else ""))

    print("\npages:")
    empty = 0
    for i, page in enumerate(reader.pages, start=1):
        box = page.mediabox
        w = float(box.width) / 72
        h = float(box.height) / 72
        rot = page.get("/Rotate", 0) or 0
        try:
            text = page.extract_text() or ""
        except Exception:
            text = ""
        chars = len(text.strip())
        if chars < SCANNED_TEXT_THRESHOLD:
            empty += 1
        if i <= 20:
            print(f"  [{i:>3}] {w:.2f}x{h:.2f}in rot={rot:<4} text={chars} chars")
    if len(reader.pages) > 20:
        print(f"  ... {len(reader.pages) - 20} more page(s)")

    if empty:
        print(
            f"\n{empty}/{len(reader.pages)} page(s) have almost no extractable text — "
            "this is probably a scan. Use `extract.py --ocr`, not a text extractor."
        )
    return 0


def carry_metadata(reader: PdfReader, writer: PdfWriter) -> None:
    """Copy the source's document metadata onto the output.

    A PdfWriter built by adding pages starts with EMPTY metadata, so every page
    operation — split, rotate, merge, even decrypt — silently strips the title,
    author and creation date unless they are copied across. Losing them is
    rarely noticed until someone looks at the file's properties.
    """
    meta = reader.metadata
    if meta:
        writer.add_metadata({str(k): str(v) for k, v in meta.items() if v is not None})


def write(writer: PdfWriter, dest: str) -> int:
    Path(dest).parent.mkdir(parents=True, exist_ok=True)
    with open(dest, "wb") as fh:
        writer.write(fh)
    print(f"wrote {dest} ({Path(dest).stat().st_size:,} bytes)")
    return 0


def cmd_merge(args) -> int:
    writer = PdfWriter()
    total = 0
    for n, src in enumerate(args.sources):
        reader = open_reader(src, args.password)
        if n == 0:
            # A merged document inherits the first source's identity; there is
            # no defensible way to merge conflicting titles and authors.
            carry_metadata(reader, writer)
        for page in reader.pages:
            writer.add_page(page)
        total += len(reader.pages)
        print(f"  + {src}: {len(reader.pages)} page(s)")
    print(f"{total} page(s) total")
    return write(writer, args.dest)


def cmd_split(args) -> int:
    reader = open_reader(args.src, args.password)
    outdir = Path(args.outdir)
    outdir.mkdir(parents=True, exist_ok=True)
    stem = Path(args.src).stem
    width = len(str(len(reader.pages)))
    for i, page in enumerate(reader.pages, start=1):
        writer = PdfWriter()
        carry_metadata(reader, writer)
        writer.add_page(page)
        dest = outdir / f"{stem}-{i:0{width}d}.pdf"
        with open(dest, "wb") as fh:
            writer.write(fh)
    print(f"wrote {len(reader.pages)} file(s) into {outdir}")
    return 0


def cmd_extract(args) -> int:
    reader = open_reader(args.src, args.password)
    pages = parse_pages(args.pages, len(reader.pages))
    writer = PdfWriter()
    carry_metadata(reader, writer)
    for index in pages:
        writer.add_page(reader.pages[index])
    print(f"kept page(s): {', '.join(str(i + 1) for i in pages)}")
    return write(writer, args.dest)


def cmd_delete(args) -> int:
    reader = open_reader(args.src, args.password)
    drop = set(parse_pages(args.pages, len(reader.pages)))
    writer = PdfWriter()
    carry_metadata(reader, writer)
    for i, page in enumerate(reader.pages):
        if i not in drop:
            writer.add_page(page)
    print(f"removed page(s): {', '.join(str(i + 1) for i in sorted(drop))}")
    return write(writer, args.dest)


def cmd_rotate(args) -> int:
    if args.degrees % 90:
        print("--degrees must be a multiple of 90", file=sys.stderr)
        return 2
    reader = open_reader(args.src, args.password)
    targets = set(parse_pages(args.pages, len(reader.pages))) if args.pages else set(range(len(reader.pages)))
    writer = PdfWriter()
    carry_metadata(reader, writer)
    for i, page in enumerate(reader.pages):
        if i in targets:
            page.rotate(args.degrees)
        writer.add_page(page)
    print(f"rotated {len(targets)} page(s) by {args.degrees}°")
    return write(writer, args.dest)


def cmd_meta(args) -> int:
    reader = open_reader(args.src, args.password)
    writer = PdfWriter()
    for page in reader.pages:
        writer.add_page(page)
    existing = dict(reader.metadata or {})
    updates = {}
    for key, value in (("/Title", args.title), ("/Author", args.author),
                       ("/Subject", args.subject), ("/Keywords", args.keywords)):
        if value is not None:
            updates[key] = value
    if args.strip:
        writer.add_metadata(updates)          # only what was explicitly given
        print("stripped existing metadata")
    else:
        existing.update(updates)
        writer.add_metadata(existing)
    for key, value in updates.items():
        print(f"  {key.lstrip('/')} = {value}")
    return write(writer, args.dest)


def cmd_encrypt(args) -> int:
    reader = open_reader(args.src, args.source_password)
    writer = PdfWriter()
    carry_metadata(reader, writer)
    for page in reader.pages:
        writer.add_page(page)
    writer.encrypt(user_password=args.new_password, owner_password=args.owner or None,
                   algorithm="AES-256")
    print("encrypted with AES-256")
    return write(writer, args.dest)


def cmd_decrypt(args) -> int:
    reader = open_reader(args.src, args.password)
    writer = PdfWriter()
    carry_metadata(reader, writer)
    for page in reader.pages:
        writer.add_page(page)
    print("decrypted")
    return write(writer, args.dest)


def cmd_fields(args) -> int:
    reader = open_reader(args.path, args.password)
    fields = reader.get_fields()
    if not fields:
        root = reader.trailer["/Root"]
        acro = root.get("/AcroForm")
        if acro is not None and "/XFA" in acro:
            print("This is an XFA form — pypdf cannot fill it. Such forms usually "
                  "require Adobe Acrobat.", file=sys.stderr)
            return 1
        print("no form fields in this PDF")
        return 0
    print(f"{len(fields)} field(s):")
    for name, spec in fields.items():
        ftype = spec.get("/FT", "?")
        value = spec.get("/V", "")
        states = spec.get("/_States_")
        line = f"  {name!r:<34} type={str(ftype).lstrip('/'):<10} value={value!r}"
        if states:
            line += f"  choices={list(states)}"
        print(line)
    return 0


def cmd_fill(args) -> int:
    reader = open_reader(args.src, args.password)
    known = reader.get_fields() or {}
    if not known:
        print("this PDF has no fillable AcroForm fields", file=sys.stderr)
        return 1

    values = {}
    for item in args.set:
        key, sep, value = item.partition("=")
        if not sep:
            print(f"--set expects KEY=VALUE, got {item!r}", file=sys.stderr)
            return 2
        values[key] = value

    unknown = [k for k in values if k not in known]
    if unknown:
        print(f"no such field(s): {', '.join(unknown)}", file=sys.stderr)
        print(f"available: {', '.join(known)}", file=sys.stderr)
        return 1

    writer = PdfWriter(clone_from=args.src)
    for page in writer.pages:
        writer.update_page_form_field_values(page, values)

    # Without NeedAppearances many viewers show a filled field as blank: the
    # value is set but no appearance stream was regenerated.
    #
    # `/AcroForm` is usually an IndirectObject (a reference), which does not
    # support item assignment — resolve it with .get_object() first.
    acro = writer._root_object.get("/AcroForm")
    if acro is not None:
        acro.get_object()[NameObject("/NeedAppearances")] = BooleanObject(True)

    for key, value in values.items():
        print(f"  {key} = {value}")
    return write(writer, args.dest)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--password", help="password for an encrypted source")
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("info"); p.add_argument("path"); p.set_defaults(fn=cmd_info)

    p = sub.add_parser("merge"); p.add_argument("dest"); p.add_argument("sources", nargs="+")
    p.set_defaults(fn=cmd_merge)

    p = sub.add_parser("split"); p.add_argument("src"); p.add_argument("outdir")
    p.set_defaults(fn=cmd_split)

    p = sub.add_parser("extract"); p.add_argument("src"); p.add_argument("dest")
    p.add_argument("--pages", required=True, help="1-based, inclusive: '2-5,9'")
    p.set_defaults(fn=cmd_extract)

    p = sub.add_parser("delete"); p.add_argument("src"); p.add_argument("dest")
    p.add_argument("--pages", required=True); p.set_defaults(fn=cmd_delete)

    p = sub.add_parser("rotate"); p.add_argument("src"); p.add_argument("dest")
    p.add_argument("--pages", help="default: every page")
    p.add_argument("--degrees", type=int, default=90); p.set_defaults(fn=cmd_rotate)

    p = sub.add_parser("meta"); p.add_argument("src"); p.add_argument("dest")
    for field in ("title", "author", "subject", "keywords"):
        p.add_argument(f"--{field}")
    p.add_argument("--strip", action="store_true", help="drop metadata not given here")
    p.set_defaults(fn=cmd_meta)

    p = sub.add_parser("encrypt"); p.add_argument("src"); p.add_argument("dest")
    # dest= keeps this distinct from the global --password (which unlocks the
    # INPUT); sharing the dest silently made one clobber the other.
    p.add_argument("--password", dest="new_password", required=True,
                   help="password to SET on the output")
    p.add_argument("--owner", help="separate owner password")
    p.add_argument("--source-password", help="password of the input, if encrypted")
    p.set_defaults(fn=cmd_encrypt)

    p = sub.add_parser("decrypt"); p.add_argument("src"); p.add_argument("dest")
    p.set_defaults(fn=cmd_decrypt)

    p = sub.add_parser("fields"); p.add_argument("path"); p.set_defaults(fn=cmd_fields)

    p = sub.add_parser("fill"); p.add_argument("src"); p.add_argument("dest")
    p.add_argument("--set", action="append", default=[], metavar="KEY=VALUE")
    p.set_defaults(fn=cmd_fill)

    args = ap.parse_args()
    try:
        return args.fn(args)
    except ValueError as exc:
        print(exc, file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
