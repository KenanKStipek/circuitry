"""idea-farm — run the idea_generator orchestration repeatedly against the
CyberDiner network until TARGET_IDEAS unique ideas have accumulated.

Design notes:
- Sharded dedupe: each run focuses on ONE domain from FOCI (rotating). The
  orchestration's existing_ideas input only receives that domain's prior
  ideas, so the prompt stays bounded no matter how large the farm grows.
  A normalized-title check on write catches exact/near-exact duplicates
  globally.
- PARTIAL HARVEST: ideas are banked from whatever state a run produced —
  final_list, curate, gap_ideas, and every completed theme round — on
  success AND failure. A run that dies downstream still keeps its
  upstream work (lesson from the first harvest, where two days of
  successful rounds were discarded because a later step timed out).
- SHAPE BIAS: every FANOUT_EVERY-th run passes a shape_hint steering the
  brainstorm toward fan-out/per-item processes (work applied across many
  items with a merge step) — downstream consumers need tree/each-shaped
  ideas, and unbiased crops skew heavily critique→revise. Records carry a
  "shape" field ("fanout" | "default") as provenance.
- Durable state: everything lives under DATA_DIR (mount a volume there).
  Restarts resume from the file — the count is derived, never trusted.

Env:
  CYBERDINER_EXPO_URL   expo root URL (project-internal http://expo:3000)
  CYBERDINER_TOKEN      ck_... API key  (Northflank secret)
  TIER                  network tier per run           (default: cheap)
  TARGET_IDEAS          stop after this many uniques   (default: 10000)
  IDEAS_PER_RUN         target_count passed per run    (default: 15)
  SLEEP_BETWEEN_RUNS    seconds between runs           (default: 20)
  JOB_TIMEOUT_SECONDS   per-job adapter timeout        (default: 600)
  FANOUT_EVERY          apply the fan-out shape hint every Nth run;
                        0 disables                     (default: 2)
  DATA_DIR              state directory                (default: /data)
"""

from __future__ import annotations

import json
import os
import re
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

from circuitry import run_orchestration
from circuitry.cli.config import CircuitryConfig

FOCI = [
    "small business operations", "job hunting and careers", "academic research",
    "software engineering practice", "legal work", "healthcare administration",
    "teaching and curriculum", "parenting and family logistics", "personal finance",
    "sales and outreach", "customer support", "marketing and content",
    "real estate", "event planning", "nonprofits and grants", "journalism",
    "creative writing", "game design", "music and audio production",
    "cooking and meal planning", "fitness and coaching", "travel planning",
    "home improvement", "gardening and homesteading", "local government and civics",
    "scientific data analysis", "product management", "human resources",
    "manufacturing and logistics", "agriculture", "insurance", "accounting and tax",
    "therapy and self-reflection", "language learning", "religious and community groups",
    "elder care", "hobby communities", "open source maintenance",
    "film and video production", "architecture and construction planning",
]

# Passed as the orchestration's shape_hint on biased runs. Kept terse — it is
# injected into several prompts and must not blow the cook execution cap.
FANOUT_HINT = (
    "processes that operate over MANY items at once — every ticket, every "
    "clause, every product, every applicant — where the same treatment is "
    "applied to each item independently and the results are merged"
)

ORCH_PATH = Path(__file__).parent / "idea_generator.yml"


def norm(title: str) -> str:
    """Normalized dedupe key for an idea line's title portion."""
    t = title.split("—")[0].split(" - ")[0]
    return re.sub(r"[^a-z0-9 ]", "", t.lower()).strip()


def load_state(data_dir: Path) -> tuple[set[str], dict[str, list[str]]]:
    """Rebuild (seen_keys, per-focus idea lines) from the master JSONL."""
    seen: set[str] = set()
    by_focus: dict[str, list[str]] = {}
    master = data_dir / "all_ideas.jsonl"
    if master.exists():
        for line in master.read_text(encoding="utf-8").splitlines():
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                continue
            seen.add(norm(rec["idea"]))
            by_focus.setdefault(rec["focus"], []).append(rec["idea"])
    return seen, by_focus


def extract_ideas(text: str) -> list[str]:
    """Pull idea lines (numbered or bulleted) out of a text blob."""
    out = []
    for line in (text or "").splitlines():
        line = line.strip()
        m = re.match(r"^(?:\d+[.)]\s*|-\s*)(.+)$", line)
        if m and len(m.group(1)) > 10:
            out.append(m.group(1).strip())
    return out


def harvest_texts(state: dict) -> list[str]:
    """Collect every idea-bearing text from a run's (possibly partial) state:
    final_list, curate, gap_ideas, and each completed theme round."""
    prime = state.get("prime") or {}
    texts: list[str] = []

    def value_of(name: str) -> str | None:
        node = prime.get(name)
        if isinstance(node, dict):
            v = node.get("value")
            if isinstance(v, str) and v.strip():
                return v
        return None

    for name in ("final_list", "curate", "gap_ideas"):
        v = value_of(name)
        if v:
            texts.append(v)

    rounds = prime.get("theme_rounds")
    if isinstance(rounds, dict):
        for key, node in rounds.items():
            if key.startswith("iter_") and isinstance(node, dict):
                ideas_node = node.get("ideas")
                if isinstance(ideas_node, dict):
                    v = ideas_node.get("value")
                    if isinstance(v, str) and v.strip():
                        texts.append(v)
    return texts


def build_config() -> CircuitryConfig:
    expo_url = os.environ["CYBERDINER_EXPO_URL"]
    token = os.environ["CYBERDINER_TOKEN"]
    tier = os.environ.get("TIER", "cheap")
    return CircuitryConfig(
        default_adapter="cyberdiner",
        default_model=tier,
        enabled_adapters=["cyberdiner"],
        runtime={
            "adapters": {
                "cyberdiner": {
                    "expo_url": expo_url,
                    "token": token,
                    "default_tier": tier,
                    "poll_interval_ms": 500,
                    "timeout_seconds": int(os.environ.get("JOB_TIMEOUT_SECONDS", "600")),
                }
            }
        },
    )


def main() -> int:
    target = int(os.environ.get("TARGET_IDEAS", "10000"))
    per_run = int(os.environ.get("IDEAS_PER_RUN", "15"))
    sleep_s = float(os.environ.get("SLEEP_BETWEEN_RUNS", "20"))
    fanout_every = int(os.environ.get("FANOUT_EVERY", "2"))
    data_dir = Path(os.environ.get("DATA_DIR", "/data"))
    data_dir.mkdir(parents=True, exist_ok=True)
    master = data_dir / "all_ideas.jsonl"

    config = build_config()
    seen, by_focus = load_state(data_dir)
    print(f"[farm] resuming with {len(seen)} unique ideas; target {target}; "
          f"fan-out bias every {fanout_every or '∅'} runs", flush=True)

    run_no = 0
    backoff = 30.0
    while len(seen) < target:
        focus = FOCI[run_no % len(FOCI)]
        run_no += 1
        fanout = fanout_every > 0 and run_no % fanout_every == 0
        shape = "fanout" if fanout else "default"
        # Bounded dedupe context — keeps the curate prompt under the cook
        # fleet's per-job execution cap.
        existing = "\n".join(by_focus.get(focus, [])[-40:])

        started = time.time()
        result = run_orchestration(
            orchestration_path=ORCH_PATH,
            config=config,
            state={
                "focus": focus,
                "existing_ideas": existing,
                "shape_hint": FANOUT_HINT if fanout else "",
                "target_count": per_run,
            },
            raise_on_error=False,
        )

        # PARTIAL HARVEST: bank ideas from whatever state exists, ok or not.
        fresh = 0
        with master.open("a", encoding="utf-8") as fh:
            for text in harvest_texts(result.state or {}):
                for idea in extract_ideas(text):
                    key = norm(idea)
                    if not key or key in seen:
                        continue
                    seen.add(key)
                    by_focus.setdefault(focus, []).append(idea)
                    fh.write(json.dumps({
                        "idea": idea,
                        "focus": focus,
                        "shape": shape,
                        "run": run_no,
                        "complete_run": bool(result.ok),
                        "ts": datetime.now(timezone.utc).isoformat(),
                    }) + "\n")
                    print(f"[idea] {focus} :: {idea}", flush=True)
                    fresh += 1

        if not result.ok:
            print(f"[farm] run {run_no} ({focus}, {shape}) FAILED after {time.time()-started:.0f}s "
                  f"(salvaged +{fresh} → {len(seen)}/{target}): {str(result.error)[:160]} — "
                  f"backing off {backoff:.0f}s", flush=True)
            time.sleep(backoff)
            backoff = min(backoff * 2, 600)
            continue

        backoff = 30.0
        print(f"[farm] run {run_no} ({focus}, {shape}): +{fresh} new, "
              f"{len(seen)}/{target} total, {time.time()-started:.0f}s", flush=True)
        time.sleep(sleep_s)

    print(f"[farm] DONE — {len(seen)} unique ideas in {master}. Idling.", flush=True)
    while True:  # deployment services restart on exit; idle instead
        time.sleep(3600)


if __name__ == "__main__":
    sys.exit(main())
