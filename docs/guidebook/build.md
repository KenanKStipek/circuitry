# Building the book

The guidebook is written as one markdown file per chapter so that it reads well on GitHub and so that parallel PRs never collide on a shared file. `scripts/build-guidebook.py` assembles those chapters into a single book and renders it:

```bash
python scripts/build-guidebook.py            # docs/guidebook/circuitry-guidebook.pdf and .epub
python scripts/build-guidebook.py --epub     # just the EPUB
python scripts/build-guidebook.py --pdf      # just the PDF
python scripts/build-guidebook.py --out DIR  # somewhere else
```

It needs [pandoc](https://pandoc.org/) 3.x. The PDF additionally needs `xelatex` with `fontspec`, `fvextra`, `newunicodechar`, and the DejaVu fonts — on Debian/Ubuntu, `texlive-xetex texlive-latex-extra texlive-fonts-recommended lmodern fonts-dejavu` covers it. The figure and the EPUB cover are rasterised from `docs/assets/shape-of-the-language.svg` with `rsvg-convert` or, failing that, a headless Chromium found on `PATH`; without either the build still succeeds, minus the picture.

What the assembly does, and nothing more: shifts every chapter heading down one level so the three acts become parts and the chapters become chapters; rewrites links between chapters into in-book anchors and links into the rest of the repository into GitHub URLs; turns the index page into an unnumbered introduction; and appends the [grammar](grammar.md) as an appendix. The chapters themselves are not modified.

Rebuild and commit both files whenever a chapter changes. `tests/docs/test_guidebook_examples.py` checks every YAML example in the chapters, so run `pytest tests/docs -q` first.
