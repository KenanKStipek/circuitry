"""One run state, shaped like a real one, shared by the inspector tests.

Covers every structure the tree has to render: a completed prompt with a
long value, a JSON-typed effect holding a list, a loop with two iterations
(one of which failed), a ``use`` in full-namespace mode (the child's
effects hanging off the use node), a conditional that recorded its branch,
and the ``runtime`` bookkeeping — including ``effective_settings`` with an
already-redacted credential.

Timestamps are fixed so anything derived from them (elapsed, snapshots) is
the same on every run.
"""

from __future__ import annotations

from typing import Any

T0 = "2024-05-01T10:00:00+00:00"
T1 = "2024-05-01T10:00:04+00:00"
T2 = "2024-05-01T10:00:09+00:00"

#: Long enough to be truncated on a tree row, so "expand" has something to do.
LONG_VALUE = "Cybernetics is the science of control and communication. " * 6

#: The marker `circuitry.cli.redaction` leaves behind.
REDACTED = "***REDACTED***"


def fixture_state() -> dict[str, Any]:
    """A fresh copy of the fixture (tests mutate freely)."""
    return {
        "topic": "cybernetics",
        "prime": {
            "meta": {"created_at": T0, "completed_at": T2, "flow": "chain"},
            "draft": {
                "value": LONG_VALUE,
                "meta": {
                    "adapter": "openai",
                    "model": "gpt-4o-mini",
                    "created_at": T0,
                    "completed_at": T1,
                    "tokens_sent": 120,
                    "tokens_received": 340,
                },
            },
            "items": {
                "value": ["alpha", "beta"],
                "meta": {
                    "adapter": "openai",
                    "model": "gpt-4o-mini",
                    "created_at": T0,
                    "completed_at": T1,
                    "tokens_sent": 12,
                    "tokens_received": 8,
                },
            },
            "over_items": {
                "meta": {"created_at": T1, "completed_at": T2, "mode": "each"},
                "iter_0": {
                    "handle": {
                        "value": "handled alpha",
                        "meta": {
                            "adapter": "openai",
                            "model": "gpt-4o-mini",
                            "created_at": T1,
                            "completed_at": T2,
                            "tokens_sent": 7,
                            "tokens_received": 13,
                        },
                    }
                },
                "iter_1": {
                    "handle": {
                        "value": None,
                        "meta": {
                            "adapter": "openai",
                            "model": "gpt-4o-mini",
                            "created_at": T1,
                            "completed_at": T2,
                            "error": "adapter timed out",
                        },
                    }
                },
            },
            "helper": {
                # A ``use`` in full-namespace mode: the child orchestration's
                # own effects hang directly off the use node.
                "meta": {"created_at": T1, "completed_at": T2, "orchestration": "summarise.yml"},
                "summarise": {
                    "value": {"headline": "Control and communication", "words": 7},
                    "meta": {
                        "adapter": "ollama",
                        "model": "llama3.1:8b",
                        "created_at": T1,
                        "completed_at": T2,
                        "tokens_sent": 40,
                        "tokens_received": 11,
                    },
                },
            },
            "gate": {
                "value": True,
                "meta": {"created_at": T2, "completed_at": T2, "branch": "then"},
            },
        },
        "runtime": {
            "last_run": {
                "orchestration": "demo.yml",
                "started_at": T0,
                "completed_at": T2,
            },
            "effective_settings": {
                "adapter": "openai",
                "model": "gpt-4o-mini",
                "api_key": REDACTED,
            },
        },
    }


def every_path(state: Any, prefix: str = "") -> set[str]:
    """Every addressable path in ``state``, walked independently of the model."""
    found: set[str] = set()
    if isinstance(state, dict):
        for key, value in state.items():
            path = f"{prefix}.{key}" if prefix else str(key)
            found.add(path)
            found |= every_path(value, path)
    elif isinstance(state, list):
        for index, value in enumerate(state):
            path = f"{prefix}[{index}]"
            found.add(path)
            found |= every_path(value, path)
    return found
