# Curation Library

This library is curated for both humans and LLMs reading it as exemplars. Each file declares its inputs, outputs, and intent in **machine-readable** form (`manifest.json`) and **human-readable** form (header comment block + `interface:` declaration). When using these as few-shot examples or RAG context, prefer the `manifest.json` entry as the index and load the file body for full context.

The library is organised by category — what role the file plays in your orchestration, not what topic it covers.

## When to feed which file to an agent

| Category | Use when… | Files |
|----------|-----------|-------|
| **`learn/`** | Teaching the agent ONE primitive (or one adapter) in isolation. Each file demonstrates a single concept. | `prompt`, `dynamic_chain`, `dynamic_tree`, `conditional`, `loop`, `reflector`, `hello`, `cyberdiner_hello` |
| **`utilities/`** | The agent needs a small composable building block to call from a recipe via `use:`. Each utility has a single declared output and is individually runnable. | `summarize`, `critique`, `refine`, `judge`, `classify`, `decompose`, `extract`, `route` |
| **`patterns/`** | Showing the agent how primitives compose. These are runnable templates that exercise multi-step shapes. | `all_primitives`, `critique_refine_loop`, `parallel_then_judge`, `classify_then_route` |
| **`recipes/`** | Real end-to-end workflows. These are what you'd ship to a user — full prompts, structured outputs, composed utilities. | `article_summarizer`, `comic_strip`, `research_brief`, `code_review`, `meeting_notes` |
| **`agents/`** | Orchestrations that build or improve other orchestrations. These are the meta layer — feed them when the agent's job is to author or refactor YAML, not run domain logic. | `meta_orchestrator`, `improver`, `improver_judge` |

## Composition contract

Every utility in `utilities/` declares an `interface:` block with typed `inputs` (with `required: true` where applicable) and typed `outputs` (with `path:` for auto-mapping). When a caller invokes a utility via `use:` and omits the `outputs:` field, the utility's interface is honoured automatically — the caller gets a flat dict at `prime.<use_name>.value` keyed by the utility's declared output names.

Utilities never reach into ambient parent state — every input arrives via the explicit `inputs:` map. This is the boundary the runtime enforces.

## Running

```bash
cof list                                    # browse the library by category
cof run learn/prompt --dry-run              # any entry by slash-delimited name
cof info recipes/research_brief             # show inputs / outputs / when-to-use
cof eject utilities/critique --out my.yml   # copy a curation entry locally for editing
```

## Manifest

`manifest.json` is the single source of truth for `cof list/run/info/eject` AND per-entry documentation. Every YAML file in this directory must have exactly one manifest entry, and every manifest entry must resolve to an existing file (enforced by `tests/orchestrations/test_curation_metadata.py`).

The manifest schema lives at `src/circuitry/schema/curation-manifest.schema.json` (Draft-07). Each entry validates against the schema's `Entry` definition.
