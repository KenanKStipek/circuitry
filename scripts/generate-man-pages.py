#!/usr/bin/env python3
"""Write `man/man1/*.1` for `cof`, `electricity` and `osi` (issue #764).

    python scripts/generate-man-pages.py            # regenerate every page
    python scripts/generate-man-pages.py --check     # exit 1 if any page is stale

`cof`'s own pages come straight from `circuitry.cli.man_pages.render_page`,
the same code `cof man` itself calls -- there is nothing else to build.
`electricity`/`osi`'s own pages come from running their own binaries
(`electricity man [run] --roff`, `osi man [watch] --roff`); `cargo build`
runs first, every time, unless `--no-build` is given -- reusing whatever
already happened to sit at `target/debug/<bin>` could silently check a
stale binary against an option it was never rebuilt to carry. When cargo
isn't on PATH at all (or `--no-build` is given and nothing is built yet),
that tool's pages are skipped with a warning instead of failing the
whole run.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
MAN1 = REPO_ROOT / "man" / "man1"

sys.path.insert(0, str(REPO_ROOT / "src"))


def _pyproject_version() -> str:
    """`pyproject.toml`'s own `[project] version` -- never the installed
    package's: a stale editable install (bumped pyproject.toml, not yet
    reinstalled) would otherwise write every cof page with the wrong,
    pre-bump version."""
    text = (REPO_ROOT / "pyproject.toml").read_text(encoding="utf-8")
    match = re.search(r'(?m)^version\s*=\s*"([^"]+)"', text)
    if match is None:
        raise RuntimeError("pyproject.toml has no top-level version = \"...\"")
    return match.group(1)


def _cof_pages() -> dict[str, str]:
    from circuitry.cli.man_pages import all_page_names, render_page

    version = _pyproject_version()
    pages: dict[str, str] = {}
    for name in all_page_names():
        filename = "cof.1" if name == "cof" else f"cof-{name}.1"
        pages[filename] = render_page(name, version=version)
    return pages


def _cargo_binary(workspace: Path, bin_name: str, *, build: bool) -> Path | None:
    path = workspace / "target" / "debug" / bin_name
    if not build:
        return path if path.exists() else None
    try:
        result = subprocess.run(
            ["cargo", "build", "--bin", bin_name],
            cwd=workspace,
            env={**os.environ, "CARGO_BUILD_JOBS": "4"},
            check=False,
        )
    except FileNotFoundError:
        return None
    if result.returncode != 0 or not path.exists():
        return None
    return path


def _rust_tool_pages(
    workspace: Path, bin_name: str, pages: dict[str, tuple[str, ...]], *, build: bool
) -> dict[str, str] | None:
    """*pages* maps a committed filename to the `man <args>` tuple that
    produces it (e.g. `("run", "--roff")`)."""
    binary = _cargo_binary(workspace, bin_name, build=build)
    if binary is None:
        return None
    out: dict[str, str] = {}
    for filename, man_args in pages.items():
        result = subprocess.run(
            [str(binary), "man", *man_args, "--roff"],
            capture_output=True,
            text=True,
            check=True,
        )
        out[filename] = result.stdout
    return out


#: Every filename each Rust tool can produce, independent of whether its
#: binary happens to be built -- `main`'s own orphan check in `--check`
#: needs this regardless of a skipped/failed build, so a missing cargo
#: toolchain can't make it misreport electricity's/osi's own pages as
#: orphaned just because they weren't rebuilt this run.
ELECTRICITY_PAGE_ARGS: dict[str, tuple[str, ...]] = {
    "electricity.1": (),
    "electricity-run.1": ("run",),
}
OSI_PAGE_ARGS: dict[str, tuple[str, ...]] = {
    "osi.1": (),
    "osi-watch.1": ("watch",),
}


def _electricity_pages(*, build: bool) -> dict[str, str] | None:
    return _rust_tool_pages(
        REPO_ROOT / "electricity", "electricity", ELECTRICITY_PAGE_ARGS, build=build
    )


def _osi_pages(*, build: bool) -> dict[str, str] | None:
    return _rust_tool_pages(REPO_ROOT / "oscilloscope", "osi", OSI_PAGE_ARGS, build=build)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="exit 1 if any page would change, without writing it",
    )
    parser.add_argument(
        "--no-build",
        action="store_true",
        help="never run cargo build; skip a Rust tool's pages if its binary isn't already built",
    )
    args = parser.parse_args()

    build = not args.no_build
    pages = dict(_cof_pages())

    for loader in (
        lambda: _electricity_pages(build=build),
        lambda: _osi_pages(build=build),
    ):
        found = loader()
        if found is None:
            print(
                "warning: skipping a Rust tool's pages -- its binary isn't built "
                "(run cargo build first, or drop --no-build)",
                file=sys.stderr,
            )
            continue
        pages.update(found)

    stale: list[str] = []
    for filename, content in sorted(pages.items()):
        path = MAN1 / filename
        current = path.read_text(encoding="utf-8") if path.exists() else None
        if args.check:
            if current != content:
                stale.append(filename)
            continue
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    if args.check:
        from circuitry.cli.man_pages import all_page_names

        known = {
            "cof.1" if n == "cof" else f"cof-{n}.1" for n in all_page_names()
        } | set(ELECTRICITY_PAGE_ARGS) | set(OSI_PAGE_ARGS)
        if MAN1.is_dir():
            orphaned = sorted(p.name for p in MAN1.glob("*.1") if p.name not in known)
            stale.extend(f"{name} (orphaned -- no longer generated)" for name in orphaned)
        if stale:
            print(
                "stale man page(s) -- run scripts/generate-man-pages.py:\n  "
                + "\n  ".join(stale),
                file=sys.stderr,
            )
            return 1
        return 0

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
