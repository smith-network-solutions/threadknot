---
name: docx
description: Create, read and edit Microsoft Word .docx documents — reports, letters, contracts, formatted multi-section documents with headings, tables, images, headers/footers and styles. Also covers reading an existing .docx to extract or modify its content, filling a .docx template, converting to PDF, and the raw OOXML route for things python-docx cannot reach (tracked changes, comments, footnotes). Use whenever a .docx file must be produced, inspected or altered.
license: Apache-2.0
---

# Word documents (.docx)

A `.docx` is a ZIP archive of XML parts. Two ways in, and picking the right one
is most of the job:

| Need | Route |
| --- | --- |
| Write a document, edit text, add tables/images/styles | **python-docx** (`scripts/` here) |
| Tracked changes, comments, footnotes, content controls, anything python-docx has no API for | **raw OOXML** — `scripts/ooxml.py unpack`, edit the XML, `pack` |

Never hand-write a whole `.docx` as XML from nothing. Start from a real file —
either one python-docx produced or one the user supplied — and modify it.

## Before editing an existing file, look at it

    scripts/inspect_docx.py report.docx

Prints every paragraph with its index and style, every table with its
dimensions, section/page setup, headers and footers, and the document's declared
styles. **Do this first on any file you did not create.** Paragraph indices from
this listing are what the editing recipes below address.

## Creating a document

Write a short Python script using python-docx. The API worth remembering:

```python
from docx import Document
from docx.shared import Pt, Inches, RGBColor
from docx.enum.text import WD_ALIGN_PARAGRAPH

doc = Document()                      # or Document("template.docx") to build on one

doc.add_heading("Quarterly Report", level=1)
p = doc.add_paragraph("Ordinary body text. ")
run = p.add_run("Bold tail.")         # runs carry the character formatting
run.bold = True
p.alignment = WD_ALIGN_PARAGRAPH.JUSTIFY

table = doc.add_table(rows=1, cols=3)
table.style = "Table Grid"            # without a style the table has NO borders
hdr = table.rows[0].cells
hdr[0].text, hdr[1].text, hdr[2].text = "Region", "Q1", "Q2"
for region, q1, q2 in data:
    cells = table.add_row().cells
    cells[0].text = region
    cells[1].text = f"{q1:,.0f}"

doc.add_picture("chart.png", width=Inches(6))
doc.add_page_break()
doc.save("report.docx")
```

Points that cost time if you learn them the hard way:

- **All measurements are typed.** `Pt`, `Inches`, `Cm`, `Emu` — never a bare
  number. Internally everything is EMU (914400 per inch).
- **Formatting lives on runs, not paragraphs.** `paragraph.text = "x"` destroys
  the paragraph's runs and their formatting. To change text while keeping
  formatting, assign to `run.text` on the existing runs.
- **A table with no `.style` has no visible borders.** `"Table Grid"` is the
  usual intent.
- Styles must already exist in the document. Building on a user's template
  inherits their styles; `Document()` gives you the default set. Check with
  `inspect_docx.py` before referencing a style by name.
- `add_heading(level=0)` produces a Title, not a heading.

## Writing your own script

The scripts here declare their dependencies inline (PEP 723) and run under
`uv`, so they work with nothing installed. **Do the same for scripts you write**
rather than pip-installing or building a venv — on a modern distro the system
Python is PEP-668 managed and `pip install` fails outright:

```python
#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["python-docx>=1.1"]
# ///
```

Then `chmod +x build.py && ./build.py`, or just
`uv run --with 'python-docx>=1.1' python build.py` (quote the spec — `>` and `[` are shell
metacharacters).
If `uv` is genuinely unavailable, fall back to a venv — but check for `uv` first.

## Editing an existing document

```python
doc = Document("in.docx")

# Replace text without losing formatting: edit runs, not the paragraph.
for para in doc.paragraphs:
    for run in para.runs:
        if "{{CLIENT}}" in run.text:
            run.text = run.text.replace("{{CLIENT}}", "Acme Ltd")

doc.save("out.docx")
```

A placeholder split across runs (Word does this often, especially after
editing) will not match. `scripts/replace.py` handles that case — it merges runs
within a paragraph before substituting, and reports anything it could not find:

    scripts/replace.py in.docx out.docx --set CLIENT="Acme Ltd" --set DATE=2026-04-01

Tables, headers and footers hold paragraphs too, and are missed by a naive walk
of `doc.paragraphs`. `replace.py` covers all of them.

## The raw OOXML route

For features python-docx does not model:

    scripts/ooxml.py unpack contract.docx /tmp/contract/    # ZIP -> XML tree
    # edit /tmp/contract/word/document.xml
    scripts/ooxml.py pack /tmp/contract/ contract-v2.docx   # back to .docx
    scripts/ooxml.py validate contract-v2.docx              # parse check

`unpack` pretty-prints each XML part so it is diffable and editable; `pack`
rewrites it as a valid archive (`[Content_Types].xml` first, stored, no
directory entries). Body text lives in `word/document.xml`; styles in
`word/styles.xml`; the relationship targets for images and links in
`word/_rels/document.xml.rels`.

Word's namespaces are verbose but consistent — the main one is
`w = http://schemas.openxmlformats.org/wordprocessingml/2006/main`. A paragraph
is `<w:p>`, containing runs `<w:r>`, containing text `<w:t>`. Whitespace-bearing
text needs `xml:space="preserve"` or Word silently trims it.

**Always `validate` after a raw edit.** Word refuses to open a malformed
document with an unhelpful error, and a stray unclosed tag is invisible until
then.

## Verify what you produced

Do not hand over a document you have not looked at. Two levels:

    scripts/inspect_docx.py out.docx        # structure is what you intended
    scripts/topdf.py out.docx out.pdf --png # renders, and gives you pages to look at

`topdf.py` is also the answer when the user asked for a PDF: build the `.docx`,
then convert. Rendering catches what inspection cannot — a table running off the
page, an image at the wrong scale, a style that did not exist and silently fell
back.

## Reading content out

For extracting text rather than editing, `inspect_docx.py --text` prints the document
body as plain text with tables rendered as TSV.
