---
name: pdf
description: Read, create and manipulate PDF files — extract text and tables, merge, split, rotate, reorder and delete pages, read and fill AcroForm fields, add or strip metadata, encrypt and decrypt, and generate new PDFs from HTML or from scratch. Also covers rasterising pages to images so a PDF can actually be looked at, and OCR for scanned documents. Use whenever a .pdf must be produced, inspected or altered.
license: Apache-2.0
---

# PDF files

Which tool depends on the verb:

| Task | Tool |
| --- | --- |
| Rearrange pages, merge, split, rotate, encrypt, metadata, forms | **pypdf** (`scripts/pdftool.py`) |
| Extract text, and especially **tables** | **pdfplumber** (`scripts/extract.py`) |
| Make a PDF from HTML/CSS | **WeasyPrint**, or LibreOffice for an Office source |
| Make a PDF programmatically (precise placement) | **reportlab** |
| Look at a page | rasterise — `scripts/extract.py --png` |

Avoid **PyMuPDF/fitz**. It is widely recommended and technically excellent, but
it is AGPL-3.0 or paid-commercial, which quietly infects whatever it touches.
Everything above is MIT or BSD.

## Inspect before you act

    scripts/pdftool.py info report.pdf

Page count, per-page size and rotation, metadata, encryption status, whether it
has form fields, and whether the pages carry extractable text or are scanned
images. **The last one decides your whole approach** — no text extractor will
get anything out of a scan, and the answer is OCR, not a different library.

## Extracting text and tables

    scripts/extract.py report.pdf                     # all text
    scripts/extract.py report.pdf --pages 1-3         # a range
    scripts/extract.py report.pdf --tables            # tables as CSV
    scripts/extract.py report.pdf --layout            # preserve visual columns

`--tables` uses pdfplumber's ruling-line detection, which works well on tables
that have visible borders and poorly on those laid out with whitespace alone.
For the whitespace kind, `--layout` plus your own parsing is usually faster than
fighting the table detector.

Text comes out in the PDF's internal content order, which is **not always
reading order** — multi-column academic papers commonly interleave. `--layout`
reconstructs position-aware text and is the fix.

## Scanned documents

If `info` reports little or no extractable text, the pages are images:

    scripts/extract.py scan.pdf --ocr --pages 1-5

Needs `tesseract` on PATH (`apt install tesseract-ocr`, `brew install
tesseract`). It rasterises each page and OCRs it. Slow — always scope with
`--pages` while iterating.

## Writing your own script

The scripts here declare their dependencies inline (PEP 723) and run under
`uv`, so they work with nothing installed. **Do the same for scripts you write**
rather than pip-installing or building a venv — on a modern distro the system
Python is PEP-668 managed and `pip install` fails outright:

```python
#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pypdf[crypto]>=5.0"]
# ///
```

Then `chmod +x build.py && ./build.py`, or just
`uv run --with 'pypdf[crypto]>=5.0' python build.py` (quote the spec — `>` and `[` are shell
metacharacters).
If `uv` is genuinely unavailable, fall back to a venv — but check for `uv` first.

## Page operations

```bash
scripts/pdftool.py merge out.pdf a.pdf b.pdf c.pdf
scripts/pdftool.py split in.pdf outdir/            # one file per page
scripts/pdftool.py extract in.pdf out.pdf --pages 2-5,9
scripts/pdftool.py rotate in.pdf out.pdf --pages 1 --degrees 90
scripts/pdftool.py delete in.pdf out.pdf --pages 3
scripts/pdftool.py meta in.pdf out.pdf --title "Q1 Report" --author "Acme"
scripts/pdftool.py encrypt in.pdf out.pdf --password s3cret
scripts/pdftool.py decrypt in.pdf out.pdf --password s3cret
```

Page numbers in every command are **1-based and inclusive**, matching what a
person sees in a viewer. (pypdf's own API is 0-based — the script converts.)

## Forms

    scripts/pdftool.py fields form.pdf                          # list fields + current values
    scripts/pdftool.py fill form.pdf out.pdf --set name=Ada --set agree=Yes

Only AcroForm fields can be filled this way; XFA forms (mostly government
LiveCycle documents) cannot, and `fields` will say so rather than silently
filling nothing.

Filled values may not display until a viewer regenerates appearances. `fill`
sets `NeedAppearances`, which handles most viewers; verify with
`scripts/extract.py out.pdf --png` and look.

## Creating a PDF

**From HTML** — the most controllable route for anything document-shaped, since
you get real CSS for layout, page breaks and headers:

```python
from weasyprint import HTML, CSS
HTML(string=html).write_pdf("out.pdf", stylesheets=[CSS(string="@page { size: A4; margin: 2cm }")])
```

`uv run --with weasyprint python …` if it is not installed.

**From an Office document** — build the `.docx`/`.xlsx`/`.pptx` first, then
convert with `scripts/topdf.py`. Usually the right answer when the user's
content is a report, spreadsheet or deck.

**Programmatically** with reportlab, when you need exact placement:

```python
from reportlab.lib.pagesizes import A4
from reportlab.pdfgen import canvas

c = canvas.Canvas("out.pdf", pagesize=A4)
width, height = A4
c.setFont("Helvetica-Bold", 18)
c.drawString(72, height - 72, "Quarterly Report")   # origin is BOTTOM-left
c.showPage()                                         # ends the page
c.save()
```

reportlab's origin is the **bottom** left and y grows upward — the opposite of
every screen coordinate system, and the reason first attempts draw off the top
of the page. Units are points (72/inch).

## Verify what you produced

    scripts/pdftool.py info out.pdf
    scripts/extract.py out.pdf --png --pages 1-2

**Look at the images.** A PDF is the format people receive and cannot easily
fix, so it is the one most worth actually viewing before handing over.
