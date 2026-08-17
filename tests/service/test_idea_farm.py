"""idea-farm harvest hygiene: focus adherence, near-duplicate detection, and
the corpus QA report.

The service lives outside the package (``services/idea-farm/``, a hyphenated
directory that is not importable), so both modules are loaded by path.
"""

from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path
from types import ModuleType, SimpleNamespace

import pytest

SERVICE_DIR = Path(__file__).resolve().parents[2] / "services" / "idea-farm"


def _load(name: str) -> ModuleType:
    spec = importlib.util.spec_from_file_location(name, SERVICE_DIR / f"{name}.py")
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module  # qa_corpus imports farm by name
    spec.loader.exec_module(module)
    return module


farm = _load("farm")
qa_corpus = _load("qa_corpus")


# ── tokenizing / stemming ────────────────────────────────────────────────

@pytest.mark.parametrize("a,b", [
    ("summaries", "summary"),
    ("meetings", "meeting"),
    ("responses", "responsing"),
    ("response", "responsing"),
    ("notes", "note"),
    ("reviewed", "reviewing"),
])
def test_stem_folds_word_variants_together(a: str, b: str) -> None:
    assert farm.stem(a) == farm.stem(b)


def test_stem_keeps_distinct_words_apart() -> None:
    assert farm.stem("invoice") != farm.stem("inventory")
    assert farm.stem("process") == "process"  # double-s is not a plural


def test_content_tokens_drops_stopwords_and_splits_hyphens() -> None:
    assert farm.content_tokens("Follow-ups for the Client") == frozenset(
        {"follow", "up", "client"}
    )


def test_title_of_takes_the_part_before_the_dash() -> None:
    assert farm.title_of("Recipe Scaler — resizes a recipe").strip() == "Recipe Scaler"
    assert farm.title_of("Recipe Scaler - resizes").strip() == "Recipe Scaler"


# ── near-duplicate detection ─────────────────────────────────────────────

# The pairs an owner review of runs 75-92 found banked side by side.
OBSERVED_DUP_PAIRS = [
    ("Drafting and tracking team meeting summaries — recaps each meeting.",
     "Drafting and Tracking Meeting Summaries — writes the recap."),
    ("Auto-responsing to Client Follow-ups — answers the chaser.",
     "Auto-Response to Client Follow-ups — answers the chaser."),
    ("Contract Clause Review Pipeline — flags risky clauses.",
     "Contract clause review pipeline — flags risky clauses."),
    ("Resume Screening Workflow — ranks applicants.",
     "Resume Screening Workflows — ranks the applicants."),
]


@pytest.mark.parametrize("first,second", OBSERVED_DUP_PAIRS)
def test_observed_near_dup_pairs_are_caught(first: str, second: str) -> None:
    index = farm.IdeaIndex()
    assert index.add(first) is True
    assert index.add(second) is False, "near-duplicate slipped past the index"
    assert len(index) == 1


@pytest.mark.parametrize("first,second", OBSERVED_DUP_PAIRS)
def test_old_exact_key_missed_the_variants(first: str, second: str) -> None:
    """Guards the premise: these pairs share no exact normalized key, which
    is why they were both banked before."""
    if first.lower() == second.lower():
        pytest.skip("casing-only pair — the exact key already caught it")
    assert farm.norm(first) != farm.norm(second)


@pytest.mark.parametrize("first,second", [
    ("Meeting Summary Drafting — recaps meetings.",
     "Invoice Reminder Chasing — nudges late payers."),
    ("Recipe Scaling Assistant — resizes recipes for a crowd.",
     "Recipe Nutrition Estimator — computes macros per serving."),
    ("Lesson Plan Builder — drafts weekly lessons.",
     "Lesson Feedback Summarizer — digests student feedback."),
])
def test_distinct_ideas_are_not_merged(first: str, second: str) -> None:
    index = farm.IdeaIndex()
    assert index.add(first) is True
    assert index.add(second) is True
    assert len(index) == 2


def test_similarity_is_global_not_per_focus() -> None:
    """The same idea harvested under two different foci is still one idea."""
    index = farm.IdeaIndex()
    assert index.add("Contract Risk Heatmap — scores clauses.") is True
    assert index.add("Contract Risk Heatmaps — score the clauses.") is False


def test_exact_key_fast_path_still_applies() -> None:
    index = farm.IdeaIndex()
    index.add("Recipe Scaler — resizes a recipe.")
    assert index.match("Recipe Scaler — a totally different sentence.") == -1


def test_threshold_is_configurable() -> None:
    a = "Weekly Grant Report Drafting — drafts the funder update."
    b = "Weekly Grant Report Drafting and Review — drafts and checks it."
    strict = farm.IdeaIndex(threshold=0.95)
    strict.add(a)
    assert strict.add(b) is True  # 0.95 is too strict to merge these
    loose = farm.IdeaIndex(threshold=0.6)
    loose.add(a)
    assert loose.add(b) is False


def test_titleless_idea_is_not_banked() -> None:
    index = farm.IdeaIndex()
    assert index.add("—") is False
    assert len(index) == 0


# ── focus adherence ──────────────────────────────────────────────────────

@pytest.mark.parametrize("focus,idea", [
    ("resume and portfolio upkeep", "Contract Risk Heatmap — scores clauses."),
    ("world-building and fiction series", "Retirement Savings Forecast — projects income."),
    ("graphic design briefs and iteration", "Loan Application Review Pipeline — screens loans."),
])
def test_off_focus_ideas_are_rejected(focus: str, idea: str) -> None:
    assert farm.matches_focus(idea, farm.focus_tokens(focus)) is False


@pytest.mark.parametrize("focus,idea", [
    ("resume and portfolio upkeep", "Resume Bullet Rewriter — sharpens each bullet."),
    ("world-building and fiction series",
     "Continuity Bible Keeper — keeps a fiction series' canon consistent."),
    ("personal finance", "Financial Runway Planner — projects months of runway."),
    ("meeting notes and follow-ups",
     "Action Item Extractor — pulls owners and dates out of meeting notes."),
    ("code review and refactoring",
     "Refactoring Proposal Drafter — proposes and reviews code changes."),
])
def test_on_focus_ideas_are_kept(focus: str, idea: str) -> None:
    assert farm.matches_focus(idea, farm.focus_tokens(focus)) is True


def test_focus_match_reads_the_whole_line_not_just_the_title() -> None:
    wanted = farm.focus_tokens("gardening and homesteading")
    assert farm.matches_focus("Frost Date Planner — schedules the garden's sowing.",
                              wanted) is True


def test_blank_focus_accepts_everything() -> None:
    assert farm.matches_focus("Anything At All — really.", farm.focus_tokens("")) is True


# ── resume / state file ──────────────────────────────────────────────────

def _write_corpus(path: Path, records: list[dict]) -> None:
    path.write_text(
        "".join(json.dumps(rec) + "\n" for rec in records), encoding="utf-8"
    )


CORPUS = [
    {"idea": "Drafting and tracking team meeting summaries — recaps each meeting.",
     "focus": "meeting notes and follow-ups", "shape": "default", "run": 1,
     "complete_run": True, "ts": "2026-08-01T00:00:00+00:00"},
    {"idea": "Drafting and Tracking Meeting Summaries — writes the recap.",
     "focus": "meeting notes and follow-ups", "shape": "fanout", "run": 2,
     "complete_run": True, "ts": "2026-08-02T00:00:00+00:00"},
    {"idea": "Contract Risk Heatmap — scores clauses.",
     "focus": "world-building and fiction series", "shape": "default", "run": 3,
     "complete_run": False, "ts": "2026-08-03T00:00:00+00:00"},
    {"idea": "Frost Date Planner — schedules the garden's sowing.",
     "focus": "gardening and homesteading", "shape": "monitor", "run": 4,
     "complete_run": True, "ts": "2026-08-04T00:00:00+00:00"},
]


def test_load_state_resumes_from_an_unchanged_state_file(tmp_path: Path) -> None:
    _write_corpus(tmp_path / "all_ideas.jsonl", CORPUS)
    before = (tmp_path / "all_ideas.jsonl").read_bytes()

    index, by_focus = farm.load_state(tmp_path)

    assert len(index) == 3  # the two meeting-summary lines collapse to one
    assert by_focus["meeting notes and follow-ups"] == [
        CORPUS[0]["idea"], CORPUS[1]["idea"],
    ]
    assert by_focus["gardening and homesteading"] == [CORPUS[3]["idea"]]
    # Resume is read-only — the master must be byte-identical afterwards.
    assert (tmp_path / "all_ideas.jsonl").read_bytes() == before


def test_load_state_skips_malformed_lines(tmp_path: Path) -> None:
    master = tmp_path / "all_ideas.jsonl"
    _write_corpus(master, CORPUS[:1])
    with master.open("a", encoding="utf-8") as fh:
        fh.write("{not json\n")
    index, _ = farm.load_state(tmp_path)
    assert len(index) == 1


def test_load_state_without_a_corpus_starts_empty(tmp_path: Path) -> None:
    index, by_focus = farm.load_state(tmp_path)
    assert len(index) == 0 and by_focus == {}


def test_already_banked_ideas_are_rejected_on_the_next_run(tmp_path: Path) -> None:
    _write_corpus(tmp_path / "all_ideas.jsonl", CORPUS)
    index, _ = farm.load_state(tmp_path)
    assert index.add("Frost date planners — schedule the garden sowing.") is False


# ── qa_corpus ────────────────────────────────────────────────────────────

def test_report_leaves_the_corpus_untouched(tmp_path: Path, capsys) -> None:
    master = tmp_path / "all_ideas.jsonl"
    _write_corpus(master, CORPUS)
    before = master.read_bytes()

    assert qa_corpus.report(master) == 4

    assert master.read_bytes() == before
    out = capsys.readouterr().out
    assert "records 4 | effective-unique 3" in out
    assert "focus adherence overall: 3/4" in out
    assert "world-building and fiction series" in out  # weakest focus
    assert "monitor" in out and "fanout" in out        # shape distribution
    assert "×2" in out                                 # the near-dup cluster


def test_report_on_an_empty_corpus(tmp_path: Path, capsys) -> None:
    master = tmp_path / "all_ideas.jsonl"
    master.write_text("", encoding="utf-8")
    assert qa_corpus.report(master) == 0
    assert "nothing to report" in capsys.readouterr().out


def test_dedupe_writes_a_copy_and_never_the_master(tmp_path: Path) -> None:
    master = tmp_path / "all_ideas.jsonl"
    _write_corpus(master, CORPUS)
    before = master.read_bytes()

    out = qa_corpus.write_cleaned(master, farm.DEFAULT_DUP_THRESHOLD)

    assert out == tmp_path / "all_ideas.cleaned.jsonl"
    assert master.read_bytes() == before
    cleaned = [json.loads(line) for line in out.read_text(encoding="utf-8").splitlines()]
    assert [rec["idea"] for rec in cleaned] == [
        CORPUS[0]["idea"], CORPUS[2]["idea"], CORPUS[3]["idea"],
    ]
    # Records survive verbatim — same schema, same fields, same order.
    assert cleaned[0] == CORPUS[0]


def test_cli_reports_without_dedupe_by_default(tmp_path: Path) -> None:
    _write_corpus(tmp_path / "all_ideas.jsonl", CORPUS)
    assert qa_corpus.main(["--data-dir", str(tmp_path)]) == 0
    assert not (tmp_path / "all_ideas.cleaned.jsonl").exists()


def test_cli_dedupe_flag_writes_the_cleaned_file(tmp_path: Path) -> None:
    _write_corpus(tmp_path / "all_ideas.jsonl", CORPUS)
    assert qa_corpus.main(["--data-dir", str(tmp_path), "--dedupe"]) == 0
    assert (tmp_path / "all_ideas.cleaned.jsonl").exists()


def test_cli_reports_a_missing_corpus(tmp_path: Path, capsys) -> None:
    assert qa_corpus.main(["--data-dir", str(tmp_path)]) == 1
    assert "no corpus at" in capsys.readouterr().out


def test_cluster_groups_every_record_exactly_once() -> None:
    clusters = qa_corpus.cluster(CORPUS, farm.DEFAULT_DUP_THRESHOLD)
    flat = sorted(pos for members in clusters for pos in members)
    assert flat == list(range(len(CORPUS)))


class _Idled(Exception):
    """Raised in place of the farm's post-completion idle sleep."""


# One fabricated run's output: an on-focus idea for FOCI[0] ("small business
# operations"), an off-focus drifter, and a near-duplicate of the first.
RUN_OUTPUT = "\n".join([
    "1. Invoice Chasing Ladder for Small Business Cash Flow — nudges late payers.",
    "2. Retirement Savings Forecast — projects post-career income.",
    "3. Invoice chasing ladders for small business cash flow — nudge late payers.",
])


def _harvest_one_run(tmp_path: Path, monkeypatch, target: str) -> list[dict]:
    """Drive farm.main() through exactly one fabricated run and return the
    records it banked."""
    monkeypatch.setattr(farm, "build_config", lambda: None)
    monkeypatch.setattr(
        farm, "run_orchestration",
        lambda **kwargs: SimpleNamespace(
            ok=True, state={"prime": {"final_list": {"value": RUN_OUTPUT}}}, error=None,
        ),
    )

    def fake_sleep(seconds: float) -> None:
        if seconds >= 3600:  # the "DONE — idling" loop
            raise _Idled

    monkeypatch.setattr(farm.time, "sleep", fake_sleep)
    monkeypatch.setenv("DATA_DIR", str(tmp_path))
    monkeypatch.setenv("TARGET_IDEAS", target)
    monkeypatch.setenv("SLEEP_BETWEEN_RUNS", "0")

    with pytest.raises(_Idled):
        farm.main()
    return [json.loads(line) for line in
            (tmp_path / "all_ideas.jsonl").read_text(encoding="utf-8").splitlines()]


def test_one_harvest_run_filters_and_logs(tmp_path: Path, monkeypatch, capsys) -> None:
    """The off-focus line and the near-duplicate are dropped, the survivor is
    banked in the unchanged record format, and both reject counts are logged."""
    banked = _harvest_one_run(tmp_path, monkeypatch, target="1")

    assert len(banked) == 1
    assert banked[0]["idea"].startswith("Invoice Chasing Ladder")
    assert set(banked[0]) == {"idea", "focus", "shape", "run", "complete_run", "ts"}
    assert banked[0]["focus"] == farm.FOCI[0]
    assert banked[0]["run"] == 1 and banked[0]["complete_run"] is True

    out = capsys.readouterr().out
    assert "focus_rejects=1" in out
    assert "near_dups=1" in out
    assert "[focus-reject]" in out and "Retirement Savings Forecast" in out
    assert "[near-dup]" in out


def test_focus_check_can_be_disabled(tmp_path: Path, monkeypatch) -> None:
    """FOCUS_CHECK=0 banks the drifter again; dedupe still applies."""
    monkeypatch.setenv("FOCUS_CHECK", "0")
    banked = _harvest_one_run(tmp_path, monkeypatch, target="2")

    assert [rec["idea"].split(" —")[0] for rec in banked] == [
        "Invoice Chasing Ladder for Small Business Cash Flow",
        "Retirement Savings Forecast",
    ]


def test_focus_adherence_counts_per_focus() -> None:
    stats = qa_corpus.focus_adherence(CORPUS)
    assert stats["meeting notes and follow-ups"] == (2, 2)
    assert stats["world-building and fiction series"] == (0, 1)
