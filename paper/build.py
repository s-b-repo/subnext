#!/usr/bin/env python3
"""Build the DCR paper: one source fragment, two outputs.

    python3 paper/build.py            # -> paper/dcr-bounded-attention.pdf
    python3 paper/build.py --html-only

The print PDF and the web version share `paper.frag.html`, so the prose can
never drift between them.
"""

from __future__ import annotations

import argparse
import re
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent

TITLE = "Dynamic Context Runtime: Bounded Attention over Unbounded History"
SUBTITLE = (
    "Separating unbounded history from bounded attention with a representation "
    "ladder, a provenance graph, and a budgeted context planner"
)
AUTHOR = "Stephan Botes"
AFFIL = "Cyber Sec &middot; cybersec.org.za"
DATE = "20 August 2026"
ISO_DATE = "2026-08-20"
REPORT_ID = "DCR-TR-2026-01"
SLUG = "research-dcr-bounded-attention"
PDF_NAME = "dcr-bounded-attention.pdf"


def fragment() -> str:
    text = (HERE / "paper.frag.html").read_text(encoding="utf-8")
    for marker, svg in (
        ("FIGURE_ARCH", "fig-arch.svg"),
        ("FIGURE_SCALE", "fig-scale.svg"),
    ):
        body = (HERE / svg).read_text(encoding="utf-8")
        body = re.sub(r"<\?xml[^>]*\?>\s*", "", body)
        text = text.replace(marker, body.strip())
    return text


def masthead(for_print: bool) -> str:
    if not for_print:
        return ""
    return f"""<header class="masthead">
  <p class="venue"><span>Dynamic Context Runtime &middot; Technical Report</span><span><b>DCR-TR-2026-01</b> &middot; {DATE}</span></p>
  <h1>{TITLE}</h1>
  <p class="subtitle">{SUBTITLE}</p>
  <p class="byline"><b>{AUTHOR}</b> &nbsp;<span>&middot;&nbsp; {AFFIL}</span></p>
  <p class="affil">Specification, implementation and experiments: github.com/s-b-repo/subnext</p>
</header>"""


def build_print() -> Path:
    css = (HERE / "print.css").read_text(encoding="utf-8")
    fonts = ROOT.parent / "site" / "assets" / "fonts"
    if not fonts.is_dir():
        fonts = HERE / "fonts"
    css = css.replace("FONT_DIR", fonts.as_uri())
    html = f"""<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<title>{TITLE}</title>
<meta name="author" content="{AUTHOR}">
<style>{css}</style></head>
<body><div class="paper">
{masthead(True)}
{fragment()}
<div class="colophon">Typeset from the project's own source. Every table and figure in this
report is reproducible offline from the repository with the commands listed under Availability.</div>
</div></body></html>"""
    out = HERE / "print.html"
    out.write_text(html, encoding="utf-8")
    return out


def to_pdf(source: Path, pdf: Path) -> bool:
    chrome = shutil.which("chromium") or shutil.which("chromium-browser") or shutil.which("google-chrome")
    if not chrome:
        print("no chromium found; wrote HTML only", file=sys.stderr)
        return False
    subprocess.run(
        [
            chrome, "--headless", "--disable-gpu", "--no-sandbox",
            "--no-pdf-header-footer", "--virtual-time-budget=12000",
            f"--print-to-pdf={pdf}", source.as_uri(),
        ],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    return pdf.exists()


SITE_DESCRIPTION = (
    "A runtime that separates unbounded history from bounded attention: a "
    "representation ladder, a provenance graph, and a budgeted context planner. "
    "Measured at 467 tokens per query over a 27,362-token transcript, flat across "
    "33x history growth."
)


def build_site(site_dir: Path) -> Path:
    """Render the same fragment into the site's chrome."""
    template = (HERE / "site-template.html").read_text(encoding="utf-8")
    paper_css = (HERE / "site-paper.css").read_text(encoding="utf-8")
    body = fragment()

    # The abstract gets the site's panel treatment; tables need a scroll box on
    # narrow screens, which print does not.
    body = body.replace('<section class="abstract">', '<section class="paper-abstract">')
    body = re.sub(
        r"(<table[^>]*>.*?</table>)",
        lambda m: f'<div class="table-scroll">{m.group(1)}</div>',
        body,
        flags=re.S,
    )

    main = f"""
  <article>
  <section class="page-hero">
    <div class="wrap">
      <p class="kicker reveal">// research &middot; technical report</p>
      <h1 class="reveal">{TITLE}</h1>
      <p class="page-sub reveal">{SUBTITLE}</p>
      <p class="article-meta reveal">{REPORT_ID} &nbsp;&middot;&nbsp; By <b>{AUTHOR}</b> &nbsp;&middot;&nbsp; {DATE}</p>
      <div class="paper-actions reveal">
        <a class="paper-dl" href="papers/{PDF_NAME}" download>Download PDF <span aria-hidden="true">&darr;</span></a>
        <a class="paper-alt" href="https://github.com/s-b-repo/subnext">Source &amp; experiments <span aria-hidden="true">&rarr;</span></a>
        <a class="paper-alt" href="research.html">All research <span aria-hidden="true">&rarr;</span></a>
      </div>
      <div class="paper-facts reveal">
        <span>Working set<b>467 tokens</b></span>
        <span>vs full history<b>59x less</b></span>
        <span>Probes answered<b>7 of 7</b></span>
        <span>Implementation<b>Rust, 0 deps</b></span>
      </div>
    </div>
  </section>

  <section class="content-section">
    <div class="wrap">
      <div class="paper-body">
{body}
      </div>
    </div>
  </section>
  </article>
"""
    page = (
        template.replace("{{TITLE}}", TITLE)
        .replace("{{DESCRIPTION}}", SITE_DESCRIPTION)
        .replace("{{SLUG}}", SLUG)
        .replace("{{ISODATE}}", ISO_DATE)
        .replace("{{REPORTID}}", REPORT_ID)
        .replace("{{PDF}}", PDF_NAME)
        .replace("{{PAPERCSS}}", paper_css)
        .replace("{{MAIN}}", main)
    )
    out = site_dir / f"{SLUG}.html"
    out.write_text(page, encoding="utf-8")
    return out


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--html-only", action="store_true")
    parser.add_argument("--site", type=Path, help="site repo root; also writes the web version")
    args = parser.parse_args()
    source = build_print()
    print(f"wrote {source.relative_to(ROOT)}")
    if args.html_only:
        return 0
    pdf = HERE / PDF_NAME
    if to_pdf(source, pdf):
        print(f"wrote {pdf.relative_to(ROOT)}  ({pdf.stat().st_size // 1024} KB)")
    if args.site:
        page = build_site(args.site)
        target = args.site / "papers" / PDF_NAME
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(pdf, target)
        print(f"wrote {page}")
        print(f"wrote {target}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
