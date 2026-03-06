---
name: pdf-generator
description: "Generate PDFs — reports, dossiers, documents, invoices, memos, and chaos reports. Uses fpdf2 (Python) for programmatic PDF creation with text, tables, images, colors, and multi-page layouts. Use when someone asks for a PDF, report, document, or formal-looking output."
metadata:
  {
    "openclaw":
      {
        "emoji": "📄",
        "requires": { "bins": ["python3", "gs"] },
        "install":
          [
            {
              "id": "brew-gs",
              "kind": "brew",
              "formula": "ghostscript",
              "bins": ["gs"],
              "label": "Install Ghostscript via Homebrew",
            },
          ],
      },
  }
---

# PDF Generator

Generate professional (or chaotic) PDFs using `fpdf2` in Python. Output to `~/Documents/PDFs/` and send to Discord.

**Library:** `fpdf2` (installed via pip)
**Ghostscript:** `gs` (installed, used for PostScript rendering and ImageMagick font support)
**Output:** `~/Documents/PDFs/`

## Quick Start

```python
#!/usr/bin/env python3
from fpdf import FPDF
import os

pdf = FPDF()
pdf.add_page()
pdf.set_font("Helvetica", "B", 24)
pdf.cell(0, 20, "Title Here", new_x="LMARGIN", new_y="NEXT", align="C")
pdf.set_font("Helvetica", "", 12)
pdf.multi_cell(0, 8, "Body text goes here. This wraps automatically.")

output = os.path.expanduser("~/Documents/PDFs/output.pdf")
pdf.output(output)
print(f"Saved: {output}")
```

Then send via Discord message tool with `filePath: ~/Documents/PDFs/output.pdf`.

## Page Setup

```python
pdf = FPDF()
pdf.set_auto_page_break(auto=True, margin=15)  # Auto page breaks

# Standard page
pdf.add_page()  # Portrait A4

# Landscape
pdf.add_page(orientation="L")

# Set margins
pdf.set_margins(left=15, top=15, right=15)
```

## Text

### Fonts & Styles

```python
pdf.set_font("Helvetica", "", 12)      # Normal
pdf.set_font("Helvetica", "B", 14)     # Bold
pdf.set_font("Helvetica", "I", 12)     # Italic
pdf.set_font("Helvetica", "BI", 12)    # Bold Italic
pdf.set_font("Courier", "", 10)        # Monospace (for code)
pdf.set_font("Times", "", 12)          # Serif
```

Built-in fonts: `Helvetica`, `Times`, `Courier`, `Symbol`, `ZapfDingbats`.

### Text Output

```python
# Single line cell
pdf.cell(0, 10, "Single line text", new_x="LMARGIN", new_y="NEXT")

# Centered
pdf.cell(0, 10, "Centered", new_x="LMARGIN", new_y="NEXT", align="C")

# Right-aligned
pdf.cell(0, 10, "Right", new_x="LMARGIN", new_y="NEXT", align="R")

# Multi-line (auto-wraps)
pdf.multi_cell(0, 8, "Long text that wraps across multiple lines automatically.")

# Line break
pdf.ln(10)  # 10mm vertical space
```

## Colors

```python
# Text color (RGB 0-255)
pdf.set_text_color(255, 0, 0)      # Red text
pdf.set_text_color(0, 128, 0)      # Green text
pdf.set_text_color(255, 255, 255)  # White text

# Background fill
pdf.set_fill_color(0, 0, 0)       # Black background
pdf.cell(0, 10, "White on black", new_x="LMARGIN", new_y="NEXT", fill=True)

# Draw color (borders, lines)
pdf.set_draw_color(200, 200, 200)  # Light gray borders
```

## Tables

```python
# Simple table
headers = ["Name", "Role", "Status"]
data = [
    ["Jamie", "CEO", "Active"],
    ["Jack", "COO", "Active"],
    ["Conrad", "Crawfish", "CONTAINED"],
]

# Header row
pdf.set_font("Helvetica", "B", 12)
pdf.set_fill_color(40, 40, 40)
pdf.set_text_color(255, 255, 255)
col_widths = [60, 60, 60]
for i, header in enumerate(headers):
    pdf.cell(col_widths[i], 10, header, border=1, fill=True)
pdf.ln()

# Data rows
pdf.set_font("Helvetica", "", 11)
pdf.set_text_color(0, 0, 0)
for row in data:
    for i, cell in enumerate(row):
        pdf.cell(col_widths[i], 8, cell, border=1)
    pdf.ln()
```

## Images

```python
# Insert image (auto-scales)
pdf.image("~/Pictures/AI_Generated/image.png", x=10, y=None, w=80)

# Full-width image
pdf.image("image.png", x=10, w=190)

# Specific size
pdf.image("image.png", x=10, y=50, w=100, h=75)
```

Supports PNG, JPEG, and GIF (first frame).

## Headers & Footers

```python
class MyPDF(FPDF):
    def header(self):
        self.set_font("Helvetica", "B", 16)
        self.cell(0, 10, "REPORT TITLE", new_x="LMARGIN", new_y="NEXT", align="C")
        self.ln(5)

    def footer(self):
        self.set_y(-15)
        self.set_font("Helvetica", "I", 8)
        self.cell(0, 10, f"Page {self.page_no()}/{{nb}}", align="C")

pdf = MyPDF()
pdf.alias_nb_pages()  # Enable {nb} placeholder for total pages
pdf.add_page()
```

## Sections & Structure

```python
def add_section(pdf, title, items):
    """Reusable section with title and bullet points."""
    pdf.set_font("Helvetica", "B", 14)
    pdf.set_text_color(0, 100, 200)
    pdf.cell(0, 10, title, new_x="LMARGIN", new_y="NEXT")
    pdf.set_font("Helvetica", "", 11)
    pdf.set_text_color(0, 0, 0)
    for item in items:
        pdf.cell(10)  # indent
        pdf.multi_cell(0, 7, f"• {item}")
    pdf.ln(5)
```

## Lines & Shapes

```python
# Horizontal rule
pdf.set_draw_color(200, 200, 200)
pdf.line(10, pdf.get_y(), 200, pdf.get_y())
pdf.ln(5)

# Rectangle
pdf.rect(10, 50, 190, 100)  # x, y, w, h

# Filled rectangle (background block)
pdf.set_fill_color(240, 240, 240)
pdf.rect(10, 50, 190, 30, style="F")
```

## Common Patterns

### Standup Report

```python
pdf = FPDF()
pdf.add_page()
pdf.set_font("Helvetica", "B", 20)
pdf.cell(0, 15, "DAILY STANDUP", new_x="LMARGIN", new_y="NEXT", align="C")
pdf.set_font("Helvetica", "I", 10)
pdf.cell(0, 8, datetime.now().strftime("%A, %B %d, %Y - %I:%M %p"), new_x="LMARGIN", new_y="NEXT", align="C")
pdf.ln(10)

add_section(pdf, "Yesterday", ["Completed task 1", "Fixed bug in thing"])
add_section(pdf, "Today", ["Work on feature X", "Review PR #42"])
add_section(pdf, "Blockers", ["Waiting on API access", "Need design review"])
```

### Dossier / Profile Page

```python
pdf.set_fill_color(20, 20, 20)
pdf.set_text_color(255, 255, 255)
pdf.rect(0, 0, 210, 40, style="F")
pdf.set_font("Helvetica", "B", 28)
pdf.cell(0, 30, "CLASSIFIED DOSSIER", new_x="LMARGIN", new_y="NEXT", align="C")
pdf.set_text_color(0, 0, 0)
pdf.ln(10)

# Photo + info side by side
pdf.image("subject.png", x=15, w=50)
pdf.set_xy(70, 55)
pdf.set_font("Helvetica", "B", 16)
pdf.cell(0, 10, "Subject: Name Here")
```

### Color-Coded Sections (Chaos Reports)

```python
colors = [(255, 50, 50), (255, 200, 0), (50, 150, 255), (200, 50, 200)]
sections = ["CRITICAL", "WARNING", "INFO", "CHAOS"]

for i, (title, color) in enumerate(zip(sections, colors)):
    pdf.set_fill_color(*color)
    pdf.set_text_color(255, 255, 255)
    pdf.cell(0, 12, f"  {title}", new_x="LMARGIN", new_y="NEXT", fill=True)
    pdf.set_text_color(0, 0, 0)
    pdf.multi_cell(0, 7, "Section content here...")
    pdf.ln(3)
```

## Discord Workflow

```bash
# 1. Generate the PDF (write and run a Python script)
python3 /tmp/make_report.py

# 2. Send to channel via message tool
# filePath: ~/Documents/PDFs/report.pdf
```

## Tips

- **Always save to `~/Documents/PDFs/`** — that's where PDFs belong.
- `multi_cell` wraps text; `cell` does not. Use `multi_cell` for body text.
- `new_x="LMARGIN", new_y="NEXT"` moves cursor to next line after a cell.
- For embedded images in GIF format, only the first frame is used.
- Use `pdf.set_auto_page_break(auto=True, margin=15)` to avoid text running off the page.
- Write the generation script to `/tmp/` — it's disposable. The PDF output is what matters.
- For very long documents, use the header/footer class pattern for consistent page layouts.
- **Use ASCII characters only** with built-in fonts (no em dashes, curly quotes, etc.) or use a Unicode font.
