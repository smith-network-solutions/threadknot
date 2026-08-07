---
name: pptx
description: Create, read and edit Microsoft PowerPoint .pptx presentations — slide decks with titles, bullets, images, tables, charts, speaker notes and consistent branding. Also covers building on a corporate template while keeping its theme, reading an existing deck's content, and rendering slides to images to check they actually look right. Use whenever a .pptx file must be produced, inspected or altered.
license: Apache-2.0
---

# PowerPoint decks (.pptx)

**python-pptx** for everything. The library is capable, but the deck you get is
only as good as your handling of two things: **layouts** and **units**.

## Layouts first — always

A slide is created *from a layout*, and the layout supplies the placeholders you
then fill. Guessing layout indices produces the classic broken deck: text in the
wrong place, a title that is really a body box, branding that silently
disappears.

    scripts/inspect_pptx.py template.pptx --layouts

Prints every layout in the template with its index, name, and each placeholder's
index, type and position. **Run this before authoring against any template**,
including the default one — index 1 is "Title and Content" in the stock template
but frequently something else in a corporate deck.

```python
from pptx import Presentation

prs = Presentation("template.pptx")   # or Presentation() for the stock template
layout = prs.slide_layouts[1]         # verify this index with --layouts first
slide = prs.slides.add_slide(layout)

slide.shapes.title.text = "Q1 results"
body = slide.placeholders[1]          # placeholder IDX, not position in a list
body.text = "Revenue up 14%"
for line in ["Costs flat", "Margin improved"]:
    para = body.text_frame.add_paragraph()
    para.text = line
    para.level = 1                     # indent level, 0-4
```

Building on the user's template is what preserves their fonts, colours and
master — always prefer it to `Presentation()` plus manual styling.

## Writing your own script

The scripts here declare their dependencies inline (PEP 723) and run under
`uv`, so they work with nothing installed. **Do the same for scripts you write**
rather than pip-installing or building a venv — on a modern distro the system
Python is PEP-668 managed and `pip install` fails outright:

```python
#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["python-pptx>=1.0"]
# ///
```

Then `chmod +x build.py && ./build.py`, or just
`uv run --with 'python-pptx>=1.0' python build.py` (quote the spec — `>` and `[` are shell
metacharacters).
If `uv` is genuinely unavailable, fall back to a venv — but check for `uv` first.

## Units are EMU

```python
from pptx.util import Inches, Pt, Emu
```

Every position and size is an English Metric Unit (914400 per inch). Passing a
bare number places a shape 1/914400th of an inch from the edge — it looks like
the shape vanished. Font sizes use `Pt`.

Default slide size is 13.333 x 7.5 in (16:9). `prs.slide_width` /
`prs.slide_height` give the real values; compute positions from those rather
than hard-coding, or the deck breaks on a 4:3 template.

## Free-floating content

When no placeholder fits:

```python
from pptx.util import Inches, Pt
from pptx.dml.color import RGBColor

box = slide.shapes.add_textbox(Inches(1), Inches(2), Inches(6), Inches(1.5))
tf = box.text_frame
tf.word_wrap = True                    # off by default: text runs off the slide
p = tf.paragraphs[0]
p.text = "Heading"
p.runs[0].font.size = Pt(28)
p.runs[0].font.bold = True
p.runs[0].font.color.rgb = RGBColor(0x1F, 0x4E, 0x79)

slide.shapes.add_picture("chart.png", Inches(1), Inches(3), width=Inches(6))

rows, cols = 3, 4
table = slide.shapes.add_table(rows, cols, Inches(1), Inches(2),
                               Inches(8), Inches(2)).table
table.cell(0, 0).text = "Region"
```

- `word_wrap` defaults to False on a new textbox. Set it.
- Give `add_picture` only a `width` **or** only a `height` and the aspect ratio
  is preserved; give both and the image is distorted.
- Text autofit is not computed by python-pptx. Long text overflows silently —
  size it yourself, or render and look (below).

## Speaker notes

```python
slide.notes_slide.notes_text_frame.text = "Mention the Q2 forecast here."
```

## Reading a deck

    scripts/inspect_pptx.py deck.pptx              # slides, shapes, positions
    scripts/inspect_pptx.py deck.pptx --text       # all text, per slide
    scripts/inspect_pptx.py deck.pptx --layouts    # the template's layouts

Text lives in `shape.text_frame` on shapes where `shape.has_text_frame` is true;
tables under `shape.has_table`; charts under `shape.has_chart`. A group shape
holds its own `.shapes` and needs recursion — `inspect_pptx.py` does this.

## Verify by looking at it

A deck is a visual artifact and structure inspection will not catch overflowing
text, overlapping shapes or an image covering the title.

    scripts/topdf.py deck.pptx deck.pdf --png     # one PNG per slide

**Look at the images.** This is the step that separates a deck that is correct
from one that merely contains the right words. It is also how you produce a PDF
when the user asked for one.

`scripts/checkoverflow.py deck.pptx` is a cheaper first pass: it flags shapes
extending past the slide edges and text boxes whose content is very likely to
overflow their frame, without needing LibreOffice.
