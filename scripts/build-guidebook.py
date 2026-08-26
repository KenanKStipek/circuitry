#!/usr/bin/env python3
"""Build the Guidebook (docs/guidebook/) as one PDF and one EPUB.

    python scripts/build-guidebook.py            # writes docs/guidebook/circuitry-guidebook.{pdf,epub}
    python scripts/build-guidebook.py --epub     # just the EPUB
    python scripts/build-guidebook.py --pdf      # just the PDF
    python scripts/build-guidebook.py --out DIR  # somewhere else

Requires pandoc (3.x) and, for the PDF, xelatex with the DejaVu fonts. The
EPUB cover is rendered from the shape-of-the-language figure with ImageMagick
when it is available and skipped otherwise.

The chapters stay ordinary GitHub-flavoured markdown; this script only
assembles them: it shifts every chapter heading down one level so the three
acts become parts, rewrites cross-chapter links into in-book anchors and
repo-relative links into GitHub URLs, and prepends a title block.
"""

from __future__ import annotations

import argparse
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GUIDEBOOK = ROOT / "docs" / "guidebook"
FIGURE = ROOT / "docs" / "assets" / "shape-of-the-language.svg"
REPO_URL = "https://github.com/KenanKStipek/circuitry/blob/main/"

TITLE = "The Circuitry Guidebook"
SUBTITLE = "Cybernetic orchestration, one primitive at a time"

PARTS: list[tuple[str, list[str]]] = [
    (
        "Basics — the machine",
        [
            "01-prompt.md",
            "02-dynamic.md",
            "03-state.md",
            "04-configuration.md",
            "05-errors.md",
        ],
    ),
    (
        "Cybernetics — the machine steering itself",
        ["06-if.md", "07-loop.md", "08-reflector.md"],
    ),
    (
        "The machine in the world",
        [
            "09-composition.md",
            "10-complexity.md",
            "11-decomposition.md",
            "12-surfaces.md",
            "13-tools-and-persistence.md",
            "14-the-whole-meal.md",
        ],
    ),
]
APPENDICES = ["grammar.md"]

HEADING = re.compile(r"^(#{1,5})\s", re.MULTILINE)
LINK = re.compile(r"\]\(([^)\s]+)\)")


def _slug(text: str) -> str:
    """GitHub-style anchor for a heading."""
    text = re.sub(r"[^\w\- ]", "", text.strip().lower())
    return re.sub(r"\s", "-", text)


def _chapter_title(md: str) -> str:
    for line in md.splitlines():
        if line.startswith("# "):
            return line[2:].strip()
    raise ValueError("chapter has no H1")


def _rewrite_links(md: str, chapter_anchors: dict[str, str]) -> str:
    def fix(match: re.Match[str]) -> str:
        target = match.group(1)
        if target.startswith(("http://", "https://", "mailto:")):
            return match.group(0)
        path, _, fragment = target.partition("#")
        if path in chapter_anchors:  # another chapter
            return f"](#{fragment or chapter_anchors[path]})"
        if path == "" and fragment:  # same-chapter anchor
            return match.group(0)
        # a repo-relative file: resolve against docs/guidebook/ and point at GitHub
        resolved = (GUIDEBOOK / path).resolve()
        try:
            rel = resolved.relative_to(ROOT)
        except ValueError:
            return match.group(0)
        url = REPO_URL + rel.as_posix() + (f"#{fragment}" if fragment else "")
        return f"]({url})"

    return LINK.sub(fix, md)


def assemble(include_figure: Path | None) -> str:
    chapters = [name for _, names in PARTS for name in names] + APPENDICES
    anchors = {
        name: _slug(_chapter_title((GUIDEBOOK / name).read_text(encoding="utf-8")))
        for name in chapters
    }

    out: list[str] = []
    out.append(
        "---\n"
        f'title: "{TITLE}"\n'
        f'subtitle: "{SUBTITLE}"\n'
        'author: "Kenan Stipek"\n'
        "lang: en-US\n"
        "---\n\n"
    )
    intro = (GUIDEBOOK / "README.md").read_text(encoding="utf-8")
    # The index page becomes an unnumbered front-matter chapter: keep the
    # framing, drop the chapter list and the format notes (the book's own
    # table of contents replaces them).
    intro = intro.split("## Chapters", 1)[0]
    intro = intro.replace("# The Circuitry Guidebook\n", "## Introduction {-}\n", 1)
    intro = intro.replace(
        "the [README](../../README.md)",
        "the [README](https://github.com/KenanKStipek/circuitry#readme)",
    )
    out.append(intro.strip() + "\n\n")
    if include_figure is not None:
        out.append(f"![Circuitry — the shape of the language]({include_figure})\n\n")
    how = (
        (GUIDEBOOK / "README.md")
        .read_text(encoding="utf-8")
        .split("## How to read the examples", 1)[1]
    )
    how = how.split("## Other formats", 1)[0]
    out.append("### How to read the examples {-}\n" + how.strip() + "\n\n")

    for part_title, names in PARTS:
        out.append(f"# {part_title}\n\n")
        for name in names:
            md = (GUIDEBOOK / name).read_text(encoding="utf-8")
            md = HEADING.sub(
                lambda m: "#" + m.group(1) + " ", md
            )  # shift down one level
            out.append(_rewrite_links(md, anchors).strip() + "\n\n")
    # Appendices: LaTeX letters them (A, B, …); other writers ignore the raw block.
    out.append("```{=latex}\n\\appendix\n```\n\n")
    for name in APPENDICES:
        md = (GUIDEBOOK / name).read_text(encoding="utf-8")
        md = HEADING.sub(lambda m: "#" + m.group(1) + " ", md)
        out.append(_rewrite_links(md, anchors).strip() + "\n\n")
    return "".join(out)


LATEX_HEADER = r"""
\usepackage{fvextra}
\DefineVerbatimEnvironment{Highlighting}{Verbatim}{breaklines,breakanywhere,commandchars=\\\{\},fontsize=\small}
\usepackage{etoolbox}
\AtBeginEnvironment{verbatim}{\small}
\usepackage{microtype}
\setlength{\emergencystretch}{3em}
% The serif and the oblique mono lack a few symbols the guidebook leans on;
% borrow them from DejaVu Sans, which has the full Dingbats/Misc Symbols blocks.
\usepackage{newunicodechar}
\newfontfamily\symbolfont{DejaVu Sans}
\newunicodechar{✗}{{\symbolfont ✗}}
\newunicodechar{⚠}{{\symbolfont ⚠}}
\newunicodechar{⊕}{{\symbolfont ⊕}}
"""

EPUB_CSS = """
body { font-family: Georgia, serif; line-height: 1.5; }
code, pre { font-family: "DejaVu Sans Mono", Menlo, Consolas, monospace; font-size: 0.85em; }
pre { white-space: pre-wrap; word-wrap: break-word; padding: 0.6em; border: 1px solid #ccc; }
blockquote { color: #555; border-left: 3px solid #ccc; padding-left: 1em; margin-left: 0; }
table { border-collapse: collapse; font-size: 0.9em; }
th, td { border: 1px solid #ccc; padding: 0.3em 0.5em; vertical-align: top; }
h1 { page-break-before: always; }
"""


def _find_chrome() -> str | None:
    for name in ("chromium", "chromium-browser", "google-chrome", "chrome"):
        found = shutil.which(name)
        if found:
            return found
    for candidate in sorted(
        Path("/opt/pw-browsers").glob("chromium-*/chrome-linux/chrome")
    ):
        return str(candidate)
    return None


def _rasterise(svg: Path, png: Path) -> bool:
    """SVG → PNG via rsvg-convert, else headless Chromium (trimmed), else give up."""
    rsvg = shutil.which("rsvg-convert")
    if rsvg:
        subprocess.run(
            [rsvg, "-z", "2", "-o", str(png), str(svg)],
            check=False,
            capture_output=True,
        )
        return png.exists()
    chrome = _find_chrome()
    if chrome is None:
        return False
    width, height = 1548, 729
    match = re.search(
        r'viewBox="([-\d.]+) ([-\d.]+) ([\d.]+) ([\d.]+)"',
        svg.read_text(encoding="utf-8"),
    )
    if match:
        width, height = int(float(match.group(3))), int(float(match.group(4)))
    subprocess.run(
        [
            chrome,
            "--headless",
            "--no-sandbox",
            "--disable-gpu",
            "--hide-scrollbars",
            "--force-device-scale-factor=2",
            f"--window-size={width + 40},{height + 40}",
            f"--screenshot={png}",
            svg.resolve().as_uri(),
        ],
        check=False,
        capture_output=True,
    )
    convert = shutil.which("magick") or shutil.which("convert")
    if png.exists() and convert:
        subprocess.run(
            [convert, str(png), "-trim", "+repage", str(png)],
            check=False,
            capture_output=True,
        )
    return png.exists()


def _render_cover(workdir: Path, figure_png: Path | None) -> Path | None:
    """Compose a portrait cover from the figure; None when ImageMagick is missing."""
    convert = shutil.which("magick") or shutil.which("convert")
    if convert is None:
        return None
    cover = workdir / "cover.png"
    cmd = [
        convert,
        "-size",
        "1600x2400",
        "xc:#121212",
        "-fill",
        "#e8e8e8",
        "-gravity",
        "north",
        "-font",
        "DejaVu-Sans",
        "-pointsize",
        "110",
        "-annotate",
        "+0+300",
        "The Circuitry",
        "-pointsize",
        "110",
        "-annotate",
        "+0+430",
        "Guidebook",
        "-pointsize",
        "40",
        "-fill",
        "#9a9a9a",
        "-annotate",
        "+0+620",
        SUBTITLE,
    ]
    if figure_png is not None and figure_png.exists():
        cmd += [
            "(",
            str(figure_png),
            "-resize",
            "1440x",
            ")",
            "-gravity",
            "center",
            "-geometry",
            "+0+150",
            "-composite",
        ]
    cmd += [
        "-fill",
        "#9a9a9a",
        "-gravity",
        "south",
        "-font",
        "DejaVu-Sans",
        "-pointsize",
        "40",
        "-annotate",
        "+0+220",
        "Kenan Stipek",
        str(cover),
    ]
    try:
        subprocess.run(cmd, check=True, capture_output=True)
    except (subprocess.CalledProcessError, OSError) as exc:
        print(f"cover: skipped ({exc})", file=sys.stderr)
        return None
    return cover


def build(out_dir: Path, *, pdf: bool, epub: bool) -> None:
    if shutil.which("pandoc") is None:
        sys.exit("pandoc is required: https://pandoc.org/installing.html")
    out_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        figure_png: Path | None = work / "shape-of-the-language.png"
        if not (
            FIGURE.exists()
            and figure_png is not None
            and _rasterise(FIGURE, figure_png)
        ):
            figure_png = None
            print(
                "figure: skipped (no rsvg-convert or Chromium found)", file=sys.stderr
            )
        book = work / "book.md"
        book.write_text(assemble(figure_png), encoding="utf-8")

        common = [
            "pandoc",
            str(book),
            "--from",
            "markdown+smart",
            "--toc",
            "--toc-depth=2",
            "--top-level-division=part",
            "--number-sections",
            "--resource-path",
            str(work),
        ]
        if epub:
            css = work / "epub.css"
            css.write_text(EPUB_CSS, encoding="utf-8")
            target = out_dir / "circuitry-guidebook.epub"
            cmd = [
                *common,
                "--to",
                "epub3",
                "--epub-chapter-level=2",
                "--css",
                str(css),
                "-o",
                str(target),
            ]
            cover = _render_cover(work, figure_png)
            if cover:
                cmd += ["--epub-cover-image", str(cover)]
            subprocess.run(cmd, check=True)
            print(f"wrote {target}")
        if pdf:
            if shutil.which("xelatex") is None:
                sys.exit("xelatex is required for the PDF (TeX Live with fontspec)")
            header = work / "header.tex"
            header.write_text(LATEX_HEADER, encoding="utf-8")
            target = out_dir / "circuitry-guidebook.pdf"
            cmd = [
                *common,
                "--to",
                "pdf",
                "--pdf-engine=xelatex",
                "-H",
                str(header),
                "-V",
                "documentclass=book",
                "-V",
                "classoption=oneside",
                "-V",
                "geometry:margin=1in",
                "-V",
                "mainfont=DejaVu Serif",
                "-V",
                "sansfont=DejaVu Sans",
                "-V",
                "monofont=DejaVu Sans Mono",
                "-V",
                "fontsize=10pt",
                "-V",
                "colorlinks=true",
                "-V",
                "linkcolor=NavyBlue",
                "-V",
                "urlcolor=NavyBlue",
                "--highlight-style=tango",
                "-o",
                str(target),
            ]
            subprocess.run(cmd, check=True)
            print(f"wrote {target}")


def main() -> None:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=GUIDEBOOK,
        help="output directory (default: docs/guidebook)",
    )
    parser.add_argument("--pdf", action="store_true", help="build only the PDF")
    parser.add_argument("--epub", action="store_true", help="build only the EPUB")
    args = parser.parse_args()
    both = not (args.pdf or args.epub)
    build(args.out, pdf=args.pdf or both, epub=args.epub or both)


if __name__ == "__main__":
    main()
