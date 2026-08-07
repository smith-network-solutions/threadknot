---
name: xlsx
description: Create, read and edit Microsoft Excel .xlsx spreadsheets — data tables, formulas, multiple sheets, number formats, conditional formatting, charts, frozen panes and named ranges. Also covers reading an existing workbook to extract values or formulas, recalculating formulas so cached values are correct, converting to CSV or PDF, and the large-file streaming path. Use whenever an .xlsx file must be produced, inspected or altered.
license: Apache-2.0
---

# Excel workbooks (.xlsx)

**openpyxl** for anything that reads or edits. **XlsxWriter** only when writing a
very large file from scratch, where it is faster and lighter but cannot read.

The single fact that causes the most wasted time:

> **openpyxl does not calculate.** Writing `=SUM(B2:B10)` stores the formula
> string. The cached *value* stays empty until a real spreadsheet application
> opens and recalculates the file. `load_workbook(data_only=True)` returns
> `None` for every formula cell in a file that Excel has never opened.

So: if the user needs a workbook whose numbers are readable by other tools, run
`scripts/recalc.py` after writing it. If you need to read values from a file
someone sent you, `data_only=True` works — Excel already cached them.

## Before editing a workbook, look at it

    scripts/inspect_xlsx.py budget.xlsx

Prints each sheet with its dimensions, frozen panes, column widths, a preview
grid, where the formulas are, merged ranges, named ranges, conditional
formatting and charts. **Do this first on any file you did not create** — a
spreadsheet's structure is rarely what the file name suggests.

    scripts/inspect_xlsx.py budget.xlsx --sheet Q1 --range A1:F20   # focus
    scripts/inspect_xlsx.py budget.xlsx --formulas                  # every formula

## Creating a workbook

```python
from openpyxl import Workbook
from openpyxl.styles import Font, Alignment, PatternFill, Border, Side
from openpyxl.utils import get_column_letter

wb = Workbook()
ws = wb.active
ws.title = "Q1"                      # sheet names: <=31 chars, no : \ / ? * [ ]

ws.append(["Region", "Revenue", "Cost", "Margin"])
for cell in ws[1]:
    cell.font = Font(bold=True, color="FFFFFF")
    cell.fill = PatternFill("solid", fgColor="1F4E79")
    cell.alignment = Alignment(horizontal="center")

for row, (region, revenue, cost) in enumerate(data, start=2):
    ws.cell(row=row, column=1, value=region)
    ws.cell(row=row, column=2, value=revenue).number_format = '#,##0.00'
    ws.cell(row=row, column=3, value=cost).number_format = '#,##0.00'
    ws.cell(row=row, column=4, value=f"=B{row}-C{row}").number_format = '#,##0.00'

ws.freeze_panes = "A2"               # header stays visible
ws.auto_filter.ref = ws.dimensions
ws.column_dimensions["A"].width = 18 # openpyxl does NOT auto-size columns

wb.create_sheet("Notes")
wb.save("budget.xlsx")
```

Things that bite:

- **Rows and columns are 1-based.** `ws.cell(row=1, column=1)` is A1. Row 0 is an
  error, not the header.
- **Number formats are strings**, applied per cell: `'#,##0.00'`, `'0.0%'`,
  `'$#,##0'`, `'yyyy-mm-dd'`. Writing `0.15` with `'0.0%'` displays `15.0%` — do
  not also multiply by 100.
- **Columns are never auto-sized.** Set `column_dimensions[letter].width`
  yourself, or call `scripts/autofit.py` afterwards.
- Dates: write a real `datetime`/`date` object and give the cell a date
  `number_format`. A date written as a string sorts and filters as text.
- `ws.append()` is much faster than per-cell assignment for bulk rows.
- Sheet names longer than 31 characters, or containing `: \ / ? * [ ]`, make a
  file Excel refuses to open.

## Writing your own script

The scripts here declare their dependencies inline (PEP 723) and run under
`uv`, so they work with nothing installed. **Do the same for scripts you write**
rather than pip-installing or building a venv — on a modern distro the system
Python is PEP-668 managed and `pip install` fails outright:

```python
#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["openpyxl>=3.1"]
# ///
```

Then `chmod +x build.py && ./build.py`, or just
`uv run --with 'openpyxl>=3.1' python build.py` (quote the spec — `>` and `[` are shell
metacharacters).
If `uv` is genuinely unavailable, fall back to a venv — but check for `uv` first.

## Editing an existing workbook

```python
from openpyxl import load_workbook

wb = load_workbook("budget.xlsx")            # keeps formulas as formulas
ws = wb["Q1"]
ws["B2"] = 1234.5
wb.save("budget-v2.xlsx")
```

`load_workbook(path, data_only=True)` gives cached values instead of formulas —
**and saving from that workbook permanently replaces every formula with its last
cached value.** Load twice if you need both, and only ever save the
formula-preserving one.

Other load flags worth knowing: `read_only=True` (streams, huge files),
`keep_vba=True` (`.xlsm`).

openpyxl silently drops what it does not model when it round-trips a file:
pivot tables, slicers, some chart types and VBA. For a workbook with those,
prefer editing cells via the raw OOXML route (`scripts/ooxml.py`, same tool as
the docx skill) or tell the user what will be lost.

## Recalculating

    scripts/recalc.py budget.xlsx                # in place
    scripts/recalc.py budget.xlsx out.xlsx       # to a copy

Opens the workbook in headless LibreOffice, recalculates, and writes it back so
every formula cell carries a correct cached value. Run this whenever you wrote
formulas and anything downstream — another script, a PDF export, a person
reading it in Numbers — needs the numbers rather than the expressions.

> **Recalculate LAST.** Any openpyxl save after a recalc throws the cached
> values away again — openpyxl rewrites the file from its own model, which has
> formulas but no results. So `autofit.py` (or any other edit) then `recalc.py`,
> never the reverse. Getting this backwards produces a workbook that looks
> correct in the script's output and shows blanks to whoever opens it.

Verify it worked by reading values back:

    scripts/inspect_xlsx.py budget.xlsx --values

## Reading data out

    scripts/inspect_xlsx.py data.xlsx --csv --sheet Q1 > q1.csv
    scripts/inspect_xlsx.py data.xlsx --values --sheet Q1

For analysis rather than extraction, pandas reads openpyxl's output directly
(`pd.read_excel(path, sheet_name=None)` gives every sheet as a dict).

## Charts

```python
from openpyxl.chart import BarChart, LineChart, Reference

chart = BarChart()
chart.title = "Revenue by region"
data = Reference(ws, min_col=2, min_row=1, max_row=ws.max_row)   # include header
cats = Reference(ws, min_col=1, min_row=2, max_row=ws.max_row)
chart.add_data(data, titles_from_data=True)
chart.set_categories(cats)
ws.add_chart(chart, "F2")
```

The `Reference` must point at the sheet the data is on, and `min_row` must
include the header row when `titles_from_data=True` — otherwise the first data
point silently becomes the series name.

## Verify what you produced

    scripts/autofit.py out.xlsx                   # any openpyxl edits FIRST
    scripts/recalc.py out.xlsx                    # then numbers become real
    scripts/inspect_xlsx.py out.xlsx --values     # confirm they are there
    scripts/topdf.py out.xlsx out.pdf --png       # what it looks like printed

A spreadsheet that renders to twelve pages of split columns is a common failure
that only the PDF reveals. Set print areas and `ws.page_setup.fitToWidth = 1`
(with `ws.sheet_properties.pageSetUpPr.fitToPage = True`) if it matters.
