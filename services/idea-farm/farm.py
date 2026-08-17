"""idea-farm — run the idea_generator orchestration repeatedly against the
CyberDiner network until TARGET_IDEAS unique ideas have accumulated.

Design notes:
- Sharded dedupe: each run focuses on ONE domain from FOCI (rotating). The
  orchestration's existing_ideas input only receives that domain's prior
  ideas, so the prompt stays bounded no matter how large the farm grows.
  Duplicate detection is GLOBAL on write: an exact normalized-title key
  (fast path) plus a Jaccard token-set similarity check against every idea
  banked so far (IdeaIndex). Exact keys alone missed one-word/casing
  variants — "Drafting and tracking team meeting summaries" vs "Drafting
  and Tracking Meeting Summaries" both banked (owner corpus review,
  2026-08-16; effective uniqueness estimated 60-80% of nominal).
- FOCUS ADHERENCE: runs drifted off their assigned focus and regressed to
  generic business-process templates ("world-building and fiction series"
  producing "Retirement Savings Forecast"), which also defeats the
  sharded-dedupe premise — the same generic idea lands under several foci.
  The prompts now demand the focus's own artifacts and actors, and harvest
  rejects any idea line sharing no content word with the focus phrase
  (counted per run as focus_rejects).
- PARTIAL HARVEST: ideas are banked from whatever state a run produced —
  final_list, curate, gap_ideas, and every completed theme round — on
  success AND failure. A run that dies downstream still keeps its
  upstream work (lesson from the first harvest, where two days of
  successful rounds were discarded because a later step timed out).
- SHAPE BIAS: rotating hints steer WHAT KIND of process gets brainstormed.
  Precedence per run: fan-out (FANOUT_EVERY), connector (CONNECTOR_EVERY),
  composition (COMPOSITION_EVERY), threshold (THRESHOLD_EVERY), monitor
  (MONITOR_EVERY), else default. Downstream consumers need all shapes;
  a 469-idea crossover analysis (2026-08-16) showed unbiased crops are 89%
  draft→critique→revise with quality-bar loops at 13% and observe-and-branch
  at 11% — the two new variants target exactly those gaps.
  Records carry a "shape" field
  ("fanout" | "connector" | "composition" | "threshold" | "monitor" |
  "default") as provenance.
- Durable state: everything lives under DATA_DIR (mount a volume there).
  Restarts resume from the file — the count is derived, never trusted.
- Backoff caps at 120s: failures still bank salvage, so long sleeps only
  idle the fleet (a single bad job used to cost 10 minutes of dead air).

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
  CONNECTOR_EVERY       apply the connector/human-in-the-loop shape hint
                        every Nth run (when fan-out doesn't apply);
                        0 disables                     (default: 3)
  COMPOSITION_EVERY     apply the composition shape hint every Nth run
                        (when neither above applies); 0 disables
                                                       (default: 5)
  THRESHOLD_EVERY       apply the quality-bar/loop-until shape hint every
                        Nth run (when none above applies); 0 disables
                                                       (default: 7)
  MONITOR_EVERY         apply the observe-and-branch shape hint every Nth
                        run (when none above applies); 0 disables
                                                       (default: 11)
  DUP_THRESHOLD         Jaccard similarity at or above which two titles
                        count as the same idea         (default: 0.7)
  FOCUS_CHECK           1 = reject harvested ideas with no content-word
                        overlap with the run's focus; 0 = bank everything
                                                       (default: 1)
  QA_ON_BOOT            1 = print a qa_corpus report over the existing
                        corpus before the first run    (default: 0)
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
    # ── professional / organizational ─────────────────────────────────
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
    # ── everyday computer work ────────────────────────────────────────
    "email and inbox management", "meeting notes and follow-ups",
    "scheduling and calendar coordination", "document formatting and templates",
    "presentation building", "spreadsheet modeling and analysis",
    "data entry and cleanup", "file and folder organization",
    "personal knowledge management", "web research and fact-finding",
    "dashboarding and reporting", "form filling and paperwork",
    "digital archiving and backups", "password and account housekeeping",
    # ── communication / human-in-the-loop ─────────────────────────────
    "chasing responses and approvals", "recruiting outreach and screening",
    "vendor and procurement coordination", "client onboarding",
    "community moderation", "volunteer coordination",
    "customer follow-up and win-back", "appointment booking and reminders",
    "status reporting to stakeholders", "escalation handling",
    # ── computation / technical ───────────────────────────────────────
    "code review and refactoring", "API and data integration",
    "IT helpdesk and troubleshooting", "test writing and quality assurance",
    "bookkeeping reconciliation", "inventory tracking",
    "competitive analysis", "price tracking and deal hunting",
    "SEO and site auditing", "log and metric triage",
    # ── creative production ───────────────────────────────────────────
    "newsletter publishing", "podcasting", "video editing and production",
    "photo library organization", "streaming and content creation",
    "social media management", "UX writing and microcopy",
    "graphic design briefs and iteration", "world-building and fiction series",
    "translation and localization",
    # ── personal life operations ──────────────────────────────────────
    "subscription and bill management", "insurance claims and appeals",
    "moving and relocation", "wedding and party planning",
    "genealogy research", "study and exam preparation",
    "habit and goal tracking", "resume and portfolio upkeep",
]

# Passed as the orchestration's shape_hint on biased runs. Kept terse — it is
# injected into several prompts and must not blow the cook execution cap.
FANOUT_HINT = (
    "processes that operate over MANY items at once — every ticket, every "
    "clause, every product, every applicant — where the same treatment is "
    "applied to each item independently and the results are merged"
)

CONNECTOR_HINT = (
    "processes that reach OUT of the model to act and react — send an email "
    "and act on the reply, nudge a person and wait for their attention or "
    "approval, browse the web, read and write files or spreadsheets — with "
    "the LLM deciding each next step from what comes back"
)

COMPOSITION_HINT = (
    "processes COMPOSED from smaller reusable workflows — one step of the "
    "process is itself an existing workflow from a shared library (a "
    "critique pass, a summarizer, a research sweep, a whole sub-pipeline), "
    "invoked with its own inputs and its outputs wired into the next step"
)

THRESHOLD_HINT = (
    "processes with a MEASURABLE quality bar — produce something, then "
    "score or check it against an explicit threshold, rubric, or budget "
    "(readability grade, test pass rate, word limit, error count, cost cap) "
    "and loop until the bar is met or a retry budget runs out — the "
    "measurement, not vibes, decides when to stop"
)

MONITOR_HINT = (
    "processes that WATCH a stream of incoming things — new messages, "
    "submissions, tickets, readings, edits — and BRANCH on what they "
    "observe: classify each arrival into a regime (urgent/routine, "
    "compliant/violating, normal/anomalous) and take a genuinely different "
    "path per regime, escalating or acting only when warranted"
)

ORCH_PATH = Path(__file__).parent / "idea_generator.yml"


# Words carrying no topical signal. Kept deliberately small: every word
# dropped here is one the similarity and focus checks can no longer see.
STOPWORDS = frozenset("""
a an the and or but of for to in on at by with from into over under across
via per as is are be being been that this these those it its your their our
using use used automatically automated auto new each any all every some
""".split())

DEFAULT_DUP_THRESHOLD = 0.7

# Shortest token that may match a focus word by prefix. Below this, prefix
# matching pairs up unrelated stems ("art" / "article").
MIN_PREFIX_MATCH = 4


def stem(word: str) -> str:
    """Crude suffix stripper — enough to fold plural/gerund variants of the
    same word onto one token ("summaries"/"summary", "responsing"/"response").
    Deliberately not Porter: pure stdlib, no dependency, and the similarity
    check only needs both sides of a near-dup pair to land together."""
    w = word
    # Plural first, then gerund, then a bare trailing "e" — applied in
    # sequence rather than as alternatives so that every variant of a word
    # converges on one stem ("meetings" → "meeting" → "meet", matching the
    # stem of "meeting"; branching would have left the two apart).
    if len(w) > 4 and w.endswith("ies"):
        w = w[:-3] + "y"
    elif len(w) > 4 and w.endswith("es") and not w.endswith("ses"):
        w = w[:-2]
    elif len(w) > 2 and w.endswith("s") and not w.endswith("ss"):
        w = w[:-1]
    for suffix in ("ing", "ed"):
        if len(w) - len(suffix) >= 4 and w.endswith(suffix):
            w = w[: -len(suffix)]
            break
    if len(w) > 3 and w.endswith("e"):
        w = w[:-1]
    return w


def content_tokens(text: str) -> frozenset[str]:
    """Stemmed content words of a phrase — stopwords and punctuation gone.
    Hyphens split ("follow-ups" → follow, up) so hyphenation variants of the
    same phrase produce the same token set."""
    words = re.split(r"[^a-z0-9]+", (text or "").lower())
    return frozenset(stem(w) for w in words if w and w not in STOPWORDS and len(w) > 1)


def title_of(idea: str) -> str:
    """The title portion of an idea line — everything before the em-dash."""
    return idea.split("—")[0].split(" - ")[0]


def norm(title: str) -> str:
    """Normalized dedupe key for an idea line's title portion."""
    t = title_of(title)
    return re.sub(r"[^a-z0-9 ]", "", t.lower()).strip()


def jaccard(a: frozenset[str], b: frozenset[str]) -> float:
    """Token-set overlap in [0, 1]. Empty on either side scores 0."""
    if not a or not b:
        return 0.0
    return len(a & b) / len(a | b)


def focus_tokens(focus: str) -> frozenset[str]:
    """Content words of a focus phrase, e.g. "resume and portfolio upkeep"
    → {resume, portfolio, upkeep}."""
    return content_tokens(focus)


def matches_focus(idea: str, wanted: frozenset[str]) -> bool:
    """True when the idea line shares a content word with the focus phrase.

    Cheap adherence net, not a precision instrument: it exists to catch gross
    drift (a retirement-savings idea harvested under "world-building and
    fiction series"), so matching is lenient — a prefix relation in either
    direction counts, which folds "financ(e)" onto "financial". A blank focus
    accepts everything."""
    if not wanted:
        return True
    for token in content_tokens(idea):
        for want in wanted:
            if token == want:
                return True
            shorter, longer = sorted((token, want), key=len)
            if len(shorter) >= MIN_PREFIX_MATCH and longer.startswith(shorter):
                return True
    return False


class IdeaIndex:
    """Global duplicate detector over banked idea titles.

    Two tiers, both pure-python: an exact normalized-title set (the original
    key, kept as the fast path) and a Jaccard similarity check over stemmed
    title token sets. Candidates for the similarity pass come from an
    inverted token→ids index, so a new idea is only compared against ideas
    sharing at least one content word rather than the whole corpus."""

    def __init__(self, threshold: float = DEFAULT_DUP_THRESHOLD) -> None:
        self.threshold = threshold
        self.keys: set[str] = set()
        self.token_sets: list[frozenset[str]] = []
        self.titles: list[str] = []
        self._postings: dict[str, list[int]] = {}

    def __len__(self) -> int:
        return len(self.token_sets)

    def match(self, idea: str) -> int | None:
        """Index of the banked idea this one duplicates, or None if new.
        Exact-key hits report -1 (no positional information needed)."""
        if norm(idea) in self.keys:
            return -1
        tokens = content_tokens(title_of(idea))
        if not tokens:
            return None
        best, best_score = None, 0.0
        candidates: set[int] = set()
        for token in tokens:
            candidates.update(self._postings.get(token, ()))
        for idx in candidates:
            score = jaccard(tokens, self.token_sets[idx])
            if score >= self.threshold and score > best_score:
                best, best_score = idx, score
        return best

    def add(self, idea: str) -> bool:
        """Bank the idea unless it duplicates one already held.
        Returns True when it was new."""
        if self.match(idea) is not None:
            return False
        key = norm(idea)
        if not key:
            return False
        self.keys.add(key)
        tokens = content_tokens(title_of(idea))
        idx = len(self.token_sets)
        self.token_sets.append(tokens)
        self.titles.append(title_of(idea).strip())
        for token in tokens:
            self._postings.setdefault(token, []).append(idx)
        return True


def load_state(
    data_dir: Path, threshold: float = DEFAULT_DUP_THRESHOLD
) -> tuple[IdeaIndex, dict[str, list[str]]]:
    """Rebuild (dedupe index, per-focus idea lines) from the master JSONL.

    Read-only: the master file's format and layout are untouched, so a farm
    that resumes on an older corpus picks up exactly where it left off — it
    simply holds a stricter duplicate key from here on."""
    index = IdeaIndex(threshold)
    by_focus: dict[str, list[str]] = {}
    master = data_dir / "all_ideas.jsonl"
    if master.exists():
        for line in master.read_text(encoding="utf-8").splitlines():
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                continue
            index.add(rec["idea"])
            by_focus.setdefault(rec["focus"], []).append(rec["idea"])
    return index, by_focus


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


def pick_shape(run_no: int, fanout_every: int, connector_every: int,
               composition_every: int, threshold_every: int,
               monitor_every: int) -> tuple[str, str]:
    """Rotating shape bias. Precedence: fan-out, then connector, then
    composition, then threshold, then monitor, else default.
    Returns (shape_name, shape_hint)."""
    if fanout_every > 0 and run_no % fanout_every == 0:
        return "fanout", FANOUT_HINT
    if connector_every > 0 and run_no % connector_every == 0:
        return "connector", CONNECTOR_HINT
    if composition_every > 0 and run_no % composition_every == 0:
        return "composition", COMPOSITION_HINT
    if threshold_every > 0 and run_no % threshold_every == 0:
        return "threshold", THRESHOLD_HINT
    if monitor_every > 0 and run_no % monitor_every == 0:
        return "monitor", MONITOR_HINT
    return "default", ""


def main() -> int:
    target = int(os.environ.get("TARGET_IDEAS", "10000"))
    per_run = int(os.environ.get("IDEAS_PER_RUN", "15"))
    sleep_s = float(os.environ.get("SLEEP_BETWEEN_RUNS", "20"))
    fanout_every = int(os.environ.get("FANOUT_EVERY", "2"))
    connector_every = int(os.environ.get("CONNECTOR_EVERY", "3"))
    composition_every = int(os.environ.get("COMPOSITION_EVERY", "5"))
    threshold_every = int(os.environ.get("THRESHOLD_EVERY", "7"))
    monitor_every = int(os.environ.get("MONITOR_EVERY", "11"))
    threshold = float(os.environ.get("DUP_THRESHOLD", str(DEFAULT_DUP_THRESHOLD)))
    focus_check = os.environ.get("FOCUS_CHECK", "1") != "0"
    data_dir = Path(os.environ.get("DATA_DIR", "/data"))
    data_dir.mkdir(parents=True, exist_ok=True)
    master = data_dir / "all_ideas.jsonl"

    config = build_config()
    index, by_focus = load_state(data_dir, threshold)

    if os.environ.get("QA_ON_BOOT") == "1":
        from qa_corpus import report  # local module; import kept lazy

        report(master, threshold=threshold)

    print(f"[farm] resuming with {len(index)} unique ideas; target {target}; "
          f"{len(FOCI)} foci; fan-out bias every {fanout_every or '∅'}; "
          f"connector bias every {connector_every or '∅'}; "
          f"composition bias every {composition_every or '∅'}; "
          f"threshold bias every {threshold_every or '∅'}; "
          f"monitor bias every {monitor_every or '∅'} runs; "
          f"dup threshold {threshold}; "
          f"focus check {'on' if focus_check else 'off'}", flush=True)

    run_no = 0
    backoff = 30.0
    while len(index) < target:
        focus = FOCI[run_no % len(FOCI)]
        run_no += 1
        shape, hint = pick_shape(run_no, fanout_every, connector_every,
                                 composition_every, threshold_every,
                                 monitor_every)
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
                "shape_hint": hint,
                "target_count": per_run,
            },
            raise_on_error=False,
        )

        # PARTIAL HARVEST: bank ideas from whatever state exists, ok or not.
        fresh = 0
        focus_rejects = 0
        wanted = focus_tokens(focus) if focus_check else frozenset()
        with master.open("a", encoding="utf-8") as fh:
            for text in harvest_texts(result.state or {}):
                for idea in extract_ideas(text):
                    if not matches_focus(idea, wanted):
                        focus_rejects += 1
                        print(f"[focus-reject] {focus} :: {idea}", flush=True)
                        continue
                    if not index.add(idea):
                        continue
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
                  f"(salvaged +{fresh}, focus_rejects={focus_rejects} → {len(index)}/{target}): "
                  f"{str(result.error)[:160]} — backing off {backoff:.0f}s", flush=True)
            time.sleep(backoff)
            backoff = min(backoff * 2, 120)
            continue

        backoff = 30.0
        print(f"[farm] run {run_no} ({focus}, {shape}): +{fresh} new, "
              f"focus_rejects={focus_rejects}, "
              f"{len(index)}/{target} total, {time.time()-started:.0f}s", flush=True)
        time.sleep(sleep_s)

    print(f"[farm] DONE — {len(index)} unique ideas in {master}. Idling.", flush=True)
    while True:  # deployment services restart on exit; idle instead
        time.sleep(3600)


if __name__ == "__main__":
    sys.exit(main())
