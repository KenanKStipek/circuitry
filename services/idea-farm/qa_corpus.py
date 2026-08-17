"""qa_corpus — one-shot quality report over the idea farm's harvest.

Answers the questions an owner review of the corpus asked by hand
(2026-08-16): how many of the banked ideas are actually distinct, how many
stayed on their run's assigned focus, how the shape bias is landing, and
which near-duplicate clusters are the worst offenders.

READ-ONLY by default. `--dedupe` writes a de-duplicated copy to
`all_ideas.cleaned.jsonl` beside the master; the master itself is never
written, moved, or truncated by this script.

Usage:
    python qa_corpus.py [--data-dir DIR] [--threshold F] [--top N] [--dedupe]

Also runs at farm boot when QA_ON_BOOT=1.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from collections import Counter
from pathlib import Path

from farm import (
    DEFAULT_DUP_THRESHOLD,
    IdeaIndex,
    content_tokens,
    focus_tokens,
    matches_focus,
    norm,
    title_of,
)

CLEANED_NAME = "all_ideas.cleaned.jsonl"


def load_records(master: Path) -> list[dict]:
    """Every well-formed record in the master JSONL, in file order.
    Malformed lines are skipped, exactly as the farm's own resume does."""
    records: list[dict] = []
    if not master.exists():
        return records
    for line in master.read_text(encoding="utf-8").splitlines():
        try:
            rec = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(rec, dict) and isinstance(rec.get("idea"), str):
            records.append(rec)
    return records


def cluster(records: list[dict], threshold: float) -> list[list[int]]:
    """Group record positions into near-duplicate clusters.

    Each record is matched against an index built from the cluster
    representatives seen so far, so a cluster is "one surviving idea plus
    everything that would have been rejected by the new dedupe key". Returns
    one list of record positions per cluster, in first-seen order."""
    index = IdeaIndex(threshold)
    rep_of_id: list[int] = []              # IdeaIndex id → record position
    by_key: dict[str, int] = {}            # exact norm key → record position
    order: list[int] = []                  # representatives, first-seen order
    clusters: dict[int, list[int]] = {}    # representative → member positions
    for pos, rec in enumerate(records):
        idea = rec["idea"]
        key = norm(idea)
        if key and key in by_key:          # exact-title duplicate
            clusters[by_key[key]].append(pos)
            continue
        hit = index.match(idea)
        if hit is not None and hit >= 0:   # near-duplicate of a representative
            clusters[rep_of_id[hit]].append(pos)
            continue
        if index.add(idea):                # ids only advance on a real add
            rep_of_id.append(pos)
        if key:
            by_key[key] = pos
        order.append(pos)
        clusters[pos] = [pos]
    return [clusters[pos] for pos in order]


def focus_adherence(records: list[dict]) -> dict[str, tuple[int, int]]:
    """Per focus: (records on focus, records total)."""
    stats: dict[str, tuple[int, int]] = {}
    wanted_cache: dict[str, frozenset[str]] = {}
    for rec in records:
        focus = str(rec.get("focus", ""))
        wanted = wanted_cache.setdefault(focus, focus_tokens(focus))
        on, total = stats.get(focus, (0, 0))
        stats[focus] = (on + int(matches_focus(rec["idea"], wanted)), total + 1)
    return stats


def report(master: Path, threshold: float = DEFAULT_DUP_THRESHOLD,
           top: int = 20) -> int:
    """Print the QA report. Returns the number of records examined."""
    records = load_records(master)
    print(f"[qa] corpus: {master}", flush=True)
    if not records:
        print("[qa] no records — nothing to report", flush=True)
        return 0

    clusters = cluster(records, threshold)
    unique = len(clusters)
    total = len(records)
    print(f"[qa] records {total} | effective-unique {unique} "
          f"({unique / total:.1%}) | near-dup threshold {threshold}", flush=True)

    stats = focus_adherence(records)
    on_focus = sum(on for on, _ in stats.values())
    print(f"[qa] focus adherence overall: {on_focus}/{total} "
          f"({on_focus / total:.1%})", flush=True)
    worst = sorted(stats.items(), key=lambda kv: (kv[1][0] / kv[1][1], -kv[1][1]))
    print("[qa] weakest foci:", flush=True)
    for focus, (on, count) in worst[:10]:
        print(f"[qa]   {on / count:6.1%}  {on:4d}/{count:<4d}  {focus}", flush=True)

    shapes = Counter(str(rec.get("shape", "unknown")) for rec in records)
    print("[qa] shape distribution:", flush=True)
    for shape, count in shapes.most_common():
        print(f"[qa]   {count / total:6.1%}  {count:5d}  {shape}", flush=True)

    dupes = sorted((c for c in clusters if len(c) > 1), key=len, reverse=True)
    print(f"[qa] near-dup clusters: {len(dupes)} "
          f"(covering {sum(len(c) for c in dupes)} records); top {top}:", flush=True)
    for members in dupes[:top]:
        print(f"[qa]   ×{len(members)}", flush=True)
        for pos in members:
            rec = records[pos]
            print(f"[qa]     [{rec.get('focus', '?')}] "
                  f"{title_of(rec['idea']).strip()[:90]}", flush=True)

    thin = [r for r in records if len(content_tokens(title_of(r["idea"]))) < 2]
    if thin:
        print(f"[qa] titles with <2 content words: {len(thin)} "
              "(weak dedupe signal)", flush=True)
    return total


def write_cleaned(master: Path, threshold: float) -> Path:
    """Write the cluster representatives to all_ideas.cleaned.jsonl.
    The master is only ever read."""
    out = master.parent / CLEANED_NAME
    if out.resolve() == master.resolve():  # pragma: no cover - defensive
        raise ValueError("refusing to overwrite the master corpus")
    records = load_records(master)
    keep = sorted(members[0] for members in cluster(records, threshold))
    with out.open("w", encoding="utf-8") as fh:
        for pos in keep:
            fh.write(json.dumps(records[pos]) + "\n")
    print(f"[qa] wrote {len(keep)} de-duplicated records → {out} "
          f"({len(records) - len(keep)} dropped; master untouched)", flush=True)
    return out


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__ and __doc__.splitlines()[0])
    parser.add_argument("--data-dir", default=os.environ.get("DATA_DIR", "/data"),
                        help="directory holding all_ideas.jsonl (default: $DATA_DIR)")
    parser.add_argument("--threshold", type=float,
                        default=float(os.environ.get("DUP_THRESHOLD",
                                                     str(DEFAULT_DUP_THRESHOLD))),
                        help="Jaccard similarity counting as a duplicate")
    parser.add_argument("--top", type=int, default=20,
                        help="how many near-dup clusters to print")
    parser.add_argument("--dedupe", action="store_true",
                        help=f"also write {CLEANED_NAME} (never modifies the master)")
    args = parser.parse_args(argv)

    master = Path(args.data_dir) / "all_ideas.jsonl"
    if not master.exists():
        print(f"[qa] no corpus at {master}", flush=True)
        return 1
    report(master, threshold=args.threshold, top=args.top)
    if args.dedupe:
        write_cleaned(master, args.threshold)
    return 0


if __name__ == "__main__":
    sys.exit(main())
