# electricity — design

electricity is an open-source (MIT) Rust runtime that executes Circuitry orchestrations in
production. Circuitry is a YAML/JSON orchestration language and Python reference implementation
(`cof`) for running multi-step LLM/tool pipelines: prompts, tool calls, loops, conditionals,
sub-orchestrations (`use`), and planner-style "reflector" effects, all writing into one JSON-
shaped state tree. electricity re-implements the language's runtime semantics in Rust so the
same document, config, and inputs produce **byte-for-byte the same final state** as `cof run
--out`. `cof` remains the authoring tool (schema checking, linting, a wizard, generation); this
document specifies the thing that runs orchestrations once they're written.

Every numbered citation below (e.g. "runtime-semantics §5.4", "tools-adapters-plugins §2.1") is
a cross-reference into `electricity/docs/spec/*` — the behavioral specification this design
implements — and `electricity/docs/research/rust-ecosystem.md`, which surveys the Rust crates
available for each area and the parity risk they carry.

---

## 1. Purpose, scope, and non-goals

**Decisions already made.** These are not open questions this design is re-litigating; they are
stated here once, in self-contained terms, so the rest of the document can refer back to "the
Decisions" instead of citing an external project file repeatedly:

- Circuitry's Python reference implementation (`cof`) remains the single source of truth for the
  orchestration language and keeps every authoring tool (schema checking, linting, a wizard,
  generation); electricity only runs orchestrations once they're written. Once electricity passes
  the full conformance suite, `cof run` itself delegates execution to electricity (§12, M4) — one
  engine, with Python kept as the authoring tool.
- CLI shape: `electricity <config.json> <orchestration.yml> -e key=value... --out state.json
  [--resume ...] [--pretty] [--profile <path>]`; the same `config.json` format and the same
  final-state JSON shape as `cof run --out`. Both files are operator-chosen on the command line
  and therefore fully trusted — no document-discovery mechanism, no document-trust
  classification, no project-config search path.
  Provider API keys come only from environment variables, never from config.json, state, or a CLI
  flag — with two named exceptions where the reference itself reads a credential from config as a
  fallback (the `cyberdiner` adapter's bearer token, §9.4/§9.6; the `surrealdb` runtime plugin's
  token/user/password, §10.2) and one place a credential lives in config by design, not as a
  fallback (an `mcp` server's own `headers`/`env`, §8.1/§14). The rule that holds everywhere,
  settled 2026-10-06: no credential of any kind ever reaches a subprocess's argv or unredacted
  state, regardless of which of these two paths supplied it.
- Profiles are supported, by explicit file path only: `--profile <path>`. There is no lookup by
  name and no search of the document's or the working directory's `profiles/` folder — the
  reference's own name-based discovery (`discover_profile_path`) is replaced by the operator
  naming the file directly, consistent with "explicit config only, both files operator-chosen"
  above. Everything else matches the reference exactly: profile schema and validation,
  precedence, model locking, and the `effective_settings.profile` record (§6.11). Needed in M1
  (§15), not deferred.
- `use` children resolve by filesystem path only; there is no library-name/remote-library
  resolution.
- There is no step cache; it was removed from the language itself (Circuitry issue #353), not
  just left out of electricity.
- Same security rules as the reference implementation: a `shell` effect's allowed commands are the
  **intersection** of the document's own list and a host-configured pin, never their union;
  `allowed_commands` is literal-only; no secrets are ever placed on a subprocess's argv; state is
  redacted before it is written anywhere.
- Mustache partials are removed from the orchestration language itself, in both engines — not an
  electricity-only omission.
- Error-message parity is explicitly two-tier: text Circuitry's own code writes (the dynamic-wrapped
  `"<path>: <message>"` format, `expect failed: ...`, `use` cycle messages, loop-bounds errors, and
  every other message quoted verbatim in the specification this design implements) must match
  **word for word**; text that originates in a third-party library (a CEL evaluation error, a YAML
  parse error, a JSON Schema message) only has to **fail in the same place** — the conformance
  suite normalizes that text rather than diffing it byte-for-byte.
- v1 must ship: run records with `--resume`, observability (OTel, live state, progress, totals), an
  embeddable Rust library crate, and release builds (static Linux x86_64/arm64, macOS arm64, a
  container image).
- Milestone order follows the production document sets this runtime is being built to run,
  starting with the two simplest and best-understood ones, then a larger and more varied one, then
  the remaining v1 features (§15).
- Bytecode is an internal implementation step only (§5): YAML/JSON in, results out — no
  user-facing bytecode format, no persisted file, no `electricity compile` subcommand.
- Process exit codes are `0` (success), `1` (ordinary run failure), `130` (SIGINT), `143`
  (SIGTERM), `129` (SIGHUP, e.g. the terminal closed), and `2` (a CLI usage error before a run
  starts — §6.9).
- electricity's v1 CLI has no `--dry-run`, `--validate-only`, or `--verbose` flag; the
  `runtime.last_run` fields of the same names (§6.10) are therefore always written `false`.
- The native tool set for v1 is `shell`, `json`, `fs`, `awk`, `imagemagick`, `ffmpeg`, `http`,
  `clock`, `hash`, `env_vars`, `regex`, `uuid`, `mcp`, `surrealdb`, `email_smtp` (§8.1).
- Models are reached through the OpenAI-compatible family (which also covers Ollama, §9.2) and
  a native `anthropic` adapter (§9.3); a third adapter — named `cyberdiner`, as in Circuitry's own
  public source (settled 2026-10-06) — is required for v1 by a production document set (§9.4).
- The external plugin bridge (§8.5) exists for *tool* plugins only — `python_eval` is its
  primary consumer — not a generic adapter or runtime-plugin bridge.
- **Repository and releases (settled 2026-10-07).** electricity lives in the Circuitry
  repository, in `electricity/` (its own Cargo workspace), not in a repository of its own. One
  version and one tag cover both: the Cargo workspace version always equals `pyproject.toml`'s,
  and the tag's release workflow attaches electricity's binaries and container image to the same
  GitHub release as the Python package, **from the next release on, marked as a preview** until
  v1 (§15, §17). Nothing is published to crates.io for now; an embedder depends on the git tag.
- Licence: MIT, the Circuitry repository's own (§17); SurrealDB support is electricity's own
  HTTP client rather than the official `surrealdb` crate specifically to keep that MIT licence
  intact (the official crate is BSL 1.1, §8.1, §10.2).

**Purpose.** Run a Circuitry orchestration — compile it, execute it against real tools and
models (or scripted fakes, for conformance testing), produce a final state identical to what
`cof run` would have produced for the same inputs.

**Scope (v1).** A CLI binary (`electricity <config.json> <orchestration.yml> [-e key=value]...
[--out state.json] [--state <file>] [--resume <id>|last] [--force] [--pretty] [--live-state <path>]
[--profile <path>] [--dump-ir]`) plus an embeddable library crate exposing the same run
semantics to a host Rust program (e.g. an application embedding the engine directly rather than shelling out to the
binary). Both surfaces share one workspace of language, VM, tool, adapter, and plugin crates.
`--resume last` reads the most recently written `--out` file for this orchestration path, tracked
in a small CLI-only stash file (`$XDG_STATE_HOME/electricity/last_run.json` on Linux, the platform
equivalent elsewhere — this location is an electricity-only convenience and carries no parity
requirement of its own, unlike the `--out` state format it points at).

**Non-goals — deliberately not built, and why:**

| Feature | Why it's out of scope |
|---|---|
| TUI | Authoring-time UX; `cof` keeps it. |
| Wizard (interactive document creation) | Authoring-time; `cof` keeps it. |
| `gen` (document generation from a prompt) | Authoring-time; belongs with the language's source of truth. |
| Library fetching (`cof`'s remote orchestration-library resolution for `use: {ref: ...}`) | electricity's `use` children resolve by filesystem `path` only (Decisions, above); no network library lookup, no pin-recording UX. |
| MCP **server** surface (`circuitry-mcp`, `host_claude` adapter) | electricity is a runner, not an interactive agent host; `host_claude`'s `generate()` has no transport of its own — it calls back into a live MCP session that only exists inside `cof`'s server (tools-adapters-plugins §3.9). electricity registers the adapter name only to reproduce `cof`'s own "cannot be built from config" error. |
| REST trigger / scheduler | Deployment-time concern layered over a runner, not the runner itself; nothing here precludes a caller from wrapping the CLI or library in one later. |
| The step cache (`cache:`, `cof cache`, `--no-cache`) | Removed from Circuitry itself (#353); electricity never had it to begin with. |
| `cof trust` / document discovery / untrusted-document `runtime:` filtering | Explicit config only, both files operator-chosen = trusted (Decisions, above). Every config key and every document `runtime:` key electricity reads is honored as-is; there is no discovery mechanism and no notion of an untrusted document to filter against (runtime-semantics §8.6's `ORCHESTRATION_RUNTIME_KEYS` trust split collapses to "always trusted"). |
| Mustache partials (`{{> name}}`) | Removed from both Circuitry and electricity (Decisions, above); chevron's partial loading reads an arbitrary file from the process CWD by name, which is both a parity-fragile feature (no production document uses it) and a minor file-disclosure surface for no real benefit. |

**Production-only surface.** electricity's only way to trust a file is for an operator to name
it on the command line or in the embedding API; there is no API-key discovery beyond environment
variables (Decisions, above).

---

## 2. Architecture

A Cargo workspace in `electricity/` at the root of the Circuitry repository (§17). Each crate is
`electricity-<name>`; the binary crate is `electricity`.

```
electricity-value        Python-semantics Value, py_str/py_repr, hashable keys
electricity-yaml         PyYAML-1.1-compatible loader on saphyr-parser
electricity-json         json.dumps-exact writer/reader over Value
electricity-template     chevron-port Mustache renderer over Value
electricity-cel          CEL evaluation (the `cel` crate) + Circuitry's strict/non-strict layer
electricity-schema       JSON Schema (Draft7) validation against the shared orchestration schema
electricity-compiler     load, structural checks, compile to bytecode (IR)
electricity-bytecode     the IR types themselves (shared by compiler and vm)
electricity-vm           the interpreter: frames, scheduler, concurrency limiter, state store
electricity-tools        native Rust tool plugins (shell, json, fs, awk, imagemagick, ffmpeg,
                          http, clock, hash, env_vars, regex, uuid, surrealdb, email_smtp, mcp)
electricity-adapters     model adapters (OpenAI-compatible family, anthropic, the vendor
                          adapter, host_claude's stub)
electricity-plugin-bridge  the NDJSON JSON-RPC client half of the external plugin protocol
electricity-runtime-plugins  persistence backends, OTel, live state, progress/totals
electricity-redaction    the shared deny-list redaction + capability tags (used by tools,
                          adapters, and runtime plugins alike)
electricity-config       config.json parsing, the merged runtime: view
electricity              the public library crate (`lib.rs`: `run_orchestration`, embedding API)
electricity-cli           the binary — argument parsing, signal handling, process exit codes
```

**Dependency direction** (lower crates do not depend on higher ones):

```
value → yaml, json, template, cel  (parity primitives, no knowledge of the language)
value, yaml, json, schema → compiler → bytecode
bytecode, template, cel, redaction → vm
tools, adapters, plugin-bridge → vm            (vm defines the trait boundary; these implement it)
runtime-plugins → vm                            (hooks into the vm's lifecycle events)
config → compiler, vm, tools, adapters, runtime-plugins  (everyone reads config, config reads nothing)
electricity (lib) → compiler, vm, tools, adapters, runtime-plugins, config
electricity-cli → electricity (lib)
```

The VM crate defines `ToolPlugin`, `Adapter`, and `RuntimePlugin` traits (mirroring
tools-adapters-plugins §1.1, §3.1, §4.3 field-for-field) and depends on **none** of the crates
that implement them — `electricity-tools`/`-adapters`/`-runtime-plugins`/`-plugin-bridge` depend
on `electricity-vm`, not the reverse. This is what makes the library crate embeddable with a
caller-supplied subset of tools/adapters (for example, an embedder that wants only `http` and
`anthropic` compiled in, or that supplies its own `ToolPlugin` for a provider electricity does
not ship).

The CLI crate is a thin shell: parse `config.json` + orchestration path + `-e`/`--out`/`--resume`
flags, wire up the full set of native tools/adapters/runtime-plugins, install signal handlers,
call into the library crate, write `--out`, set the process exit code (§6.9).

---

## 3. The Value model and the three owned parity components

### 3.1 `Value`

Circuitry's state is Python data, not JSON data: it can hold things JSON cannot represent, and
things `serde_json::Value` does not model at all (rust-ecosystem.md item 5). electricity defines
its own `Value` as the one type every other crate in the workspace speaks:

```rust
enum Value {
    None,
    Bool(bool),
    Int(IntValue),      // i64 fast path, BigInt fallback — Python ints are unbounded
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Dict(IndexMap<Value, Value>),   // insertion order preserved; Value must be Hash + Eq
    Date(chrono::NaiveDate),
    DateTime(chrono::NaiveDateTime, Option<chrono::FixedOffset>),  // offset None = naive (§3.2)
}
```

`Dict` keys are themselves `Value` (not `String`) because YAML/JSON can produce non-string keys
(`yes:` resolves to the bool key `true`; a document can use a bare integer key) — runtime-
semantics §1.1's `on`/`017`/`1:30:00` table only matters because the *value side* of a mapping
goes through the same resolver as scalar values generally, and a key can be any resolved scalar
too.

`Value` carries exactly the methods the three parity components below need:
- `py_str(&self) -> String` — chevron's rendering: `True`/`False`/empty-for-None/Python
  float-repr/`repr`-ish list and dict stringification (template §3.2 table).
- `py_repr(&self) -> String` — the `repr()` chevron's `_JsonAwareDict`-free path never actually
  needs on its own (it uses `py_str`), but is kept for parity testing against CPython's `repr()`
  directly in golden tests.
- `to_json_python(&self, ...) -> String` — the `json.dumps`-compatible writer (§3.4).
- CEL conversion is a separate, one-directional mapping owned by `electricity-cel` (§7.2), not a
  `Value` method, since the two type systems diverge (CEL has no `Date`, `Value` has no CEL
  `Duration`/`Timestamp` distinction from `DateTime`/an interval).

### 3.2 YAML: a PyYAML-1.1 composer on `saphyr-parser`

No Rust YAML library implements YAML 1.1's implicit-scalar resolution (rust-ecosystem.md items
6–11); every maintained one targets 1.2. electricity therefore owns the composer layer:
`saphyr-parser` for tokenizing/parsing (events, with scalar style, anchors, tags, and spans —
exactly what's needed, per the research), and a from-scratch resolver+constructor on top that
applies PyYAML's own regex table to every plain scalar (runtime-semantics §1.1):

- booleans: `on/off/yes/no/true/false` in three casings, `y`/`n` excluded;
- ints: `0x…` (hex), `0b[01_]+` (binary), legacy `0[0-7_]+` octal (**not** `0o`-prefixed — Q8),
  sexagesimal `H:MM:SS`, underscored literals;
- floats: must contain a literal `.`, exponent must carry an explicit sign (`1e3` is a *string*,
  `1.0e+3` is a float), plus `.inf`/`.nan` and sexagesimal float forms;
- nulls: `~`, `null`/`Null`/`NULL`, the empty scalar;
- timestamps: YAML timestamp grammar → `Value::Date`/`Value::DateTime`. PyYAML's timestamp
  resolver returns a **naive** `datetime.datetime` (no `tzinfo`) when the scalar carries no
  explicit offset — a plain `chrono::DateTime<FixedOffset>` cannot represent that (every variant
  is offset-aware). `Value::DateTime` therefore pairs a `chrono::NaiveDateTime` with a separate
  `Option<FixedOffset>` (`None` meaning "naive, as PyYAML produced it") rather than always
  defaulting a missing offset to UTC, since defaulting to UTC would make a naive and a UTC-aware
  timestamp compare equal where Python's own naive/aware comparison instead diverges by operator:
  `==`/`!=` between a naive and an aware datetime are well-defined and simply **return**
  `False`/`True` (Python's own "never equal across awareness" rule — this never raises), while an
  *ordering* comparison (`<`, `<=`, `>`, `>=`) between the two **raises** `TypeError`. `str()`
  never raises either way, since it only ever looks at one value. electricity's CEL layer (§7.2)
  must reproduce this split exactly — `_==_`/`_!=_` return `False`/`True`, the ordering operators
  raise — while `py_str` simply formats whichever value it's given and never needs to raise for
  this reason at all. **Settled 2026-10-06**: whether `cel`'s own ordering operators already
  raise on a naive/aware mismatch the way cel-python's do is decided empirically, by the
  differential corpus against cel-python (§12) — not a judgment call made in advance of running
  it. If the corpus finds a gap, electricity's CEL layer wraps the ordering operators so they
  raise on exactly the same inputs cel-python does;
- `<<` merge keys, including a list of maps to merge, with later/explicit keys winning
  (runtime-semantics §1.1's confirmed `{x:1,y:2}` + `{<<: *base, y: 3}` → `{x:1, y:3}`);
- `=` (the `tag:yaml.org,2002:value` tag) is a load error, matching PyYAML's SafeLoader having
  no constructor for it.

**Duplicate keys.** Circuitry's own `_UniqueKeyLoader` (runtime-semantics §1.1) is *not* stock
PyYAML behavior (stock `safe_load` silently keeps the last duplicate) — it's a Circuitry-layer
check electricity's composer reproduces directly: raise the moment the same mapping defines a
key twice, citing both locations, with the `<<:` merge key explicitly exempted. electricity's
composer implements this check itself rather than delegating to any crate's built-in duplicate-
key option (`serde-saphyr`'s `duplicate_keys` flag targets a different, serde-driven use case —
rust-ecosystem.md item 9) since the exact error message and the merge-key exemption are
Circuitry-specific, not generic YAML behavior.

**JSON documents** go through a structurally separate loader (`electricity-json`'s reader side,
not the YAML composer) that detects duplicate keys the same way `core/json_load.py` does:
bottom-up construction, then a top-down duplicate check naming the dotted path of the first
repeated key (runtime-semantics §1.2).

Every `.yml`/`.yaml` file electricity loads — the root document, a `use: {path: ...}` child, an
`inline:` child's rendered text — goes through this one composer (runtime-semantics §1.1).

### 3.3 Templates: a chevron port

`electricity-template` is a line-for-line port of chevron's tokenizer and renderer (~300–600
lines per the research, not a generic Mustache engine) over `Value`, because chevron deviates
from the Mustache spec in ways that are load-bearing for existing documents (rust-ecosystem.md
item 16, runtime-semantics §3):

- escaping: `{{name}}` escapes `& < > "` but **not** `'`; `{{{name}}}`/`{{&name}}` escape
  nothing;
- value rendering goes through `py_str`, not JSON: `True`/`False` literally, `None` as empty,
  a list/dict via Python `str()` (single-quoted, not valid JSON — `[1, 'a']`, `{'a': 1}`);
- dotted names walk nested dicts and render empty on any missing segment, never an error;
  `items.0` indexes a list;
- sections iterate a list (binding each element), render once for a truthy non-list, and inverted
  sections flip on falsy/empty/absent;
- a missing top-level key renders empty, never an error;
- no partials (§1, Decisions); `{{&name}}` is ported as the existing no-escape alias to
  `{{{name}}}` (already covered by the escaping bullet above — this is not a delimiter change,
  just a second spelling of the same tag). `{{=left= =right=}}` custom-delimiter tags **are**
  ported: chevron's tokenizer implements them in well under a hundred lines, no production
  document happens to use them today, and dropping a feature the reference implementation has
  would be an unrecorded, unapproved divergence rather than anything resembling an owner decision
  — unlike partials, this was never discussed with the owner and is not listed in the Decisions
  above.

A malformed template is a compile-time error (`template_syntax_error`, runtime-semantics §1.4);
a render-time failure against unrenderable data is the dispatching effect's own failure, handled
by that effect's `on_error` (runtime-semantics §3.3) — never silently sent through as raw,
unrendered text.

**The `params_json` asymmetry (Q2) is preserved exactly, not unified.** Every ordinary template
site renders a spliced list/dict via `py_str`. Only the context used for a tool's `params_json`
field is wrapped so the *same* splice syntax instead emits real `json.dumps` output
(runtime-semantics §3.2, Quirk Q2). electricity's template crate exposes two context wrapper
types for exactly this purpose — `PlainCtx` (default, `py_str` rendering) and `JsonAwareCtx`
(used only by the tool-effect compiler when rendering `params_json`) — rather than a single mode
flag, so the two call sites can never accidentally share behavior by a future refactor.

### 3.4 JSON: a `json.dumps`-exact writer (and reader)

`serde_json::Value` is not used as electricity's value type at all (§3.1); `electricity-json`
is a dedicated `Value ⇄ text` codec matching Python's `json` module, built on `serde_json`'s
low-level `Formatter` hook plus a custom float-repr layer (rust-ecosystem.md items 19–22):

- separators: `", "`/`": "` when not indented, bare `,`+newline when indented — not serde_json's
  defaults either way;
- `ensure_ascii` is a parameter, not a constant — **both values are load-bearing** (§3.4.1);
- non-finite floats write `NaN`/`Infinity`/`-Infinity` (Python), never `null` (serde_json's
  default);
- float formatting matches CPython's `repr` layout, not `ryu`'s: shortest round-trip digits,
  scientific notation below `1e-4` or at/above `1e16`, always a signed two-digit-minimum exponent
  (`1e-05`, `1e+16`) — golden-tested against CPython output (`rust-ecosystem.md` item 20 flags
  this as needing direct CPython verification, which the conformance suite performs, §12);
  for the plain `--out` round-trip, `electricity-json`'s own property tests also exercise
  round-tripping every float through write→read and comparing bit patterns;
  the unindented-`--out` vs `--pretty` separator/order difference is §3.4.1, below;
- big ints: arbitrary precision (serde_json's `arbitrary_precision` feature, or `Value::Int`'s
  own `BigInt` fallback written digit-for-digit);
- non-string dict keys follow Python's `json.dumps` key-stringification: `True`→`"true"`,
  `None`→`"null"`, `1`→`"1"`, a float key via its own repr rule;
  dict key **deletion** (needed for the `last`-alias compaction, §6.7) preserves the order of
  the remaining keys (`shift_remove`, not `swap_remove` — rust-ecosystem.md item 21);
- `Value`'s `Hash`/`Eq` for `Dict` keys follow **Python** equality, not Rust's derived
  discriminant-then-field comparison: `1` (`Int`), `1.0` (`Float`), and `True` (`Bool`) are the
  *same* dict key in Python (`hash(1) == hash(1.0) == hash(True)`, `{1: "a", 1.0: "b"} ==
  {1: "b"}`). `Value`'s key-side `Hash`/`Eq` impls therefore special-case the numeric-and-bool
  family to hash/compare across variants exactly like CEL's own heterogeneous-equality numeric
  family (§7.2) — this is a second, independent reason (besides CEL) `Value` cannot use a
  straightforward derived `PartialEq`/`Hash` for its `Dict` key type;
- `electricity-json`'s *writer* raises on any `Value` variant Python's `json.dumps` itself cannot
  serialize without a `default=`: `Date`/`DateTime` (Python's stdlib `json` has no native
  `date`/`datetime` encoding either) and `Bytes`. The one exception is the redacted-`raw`
  size-cap path (§8.2), which matches `cli/tool.py`'s own `json.dumps(raw, default=str)` call
  specifically — a non-JSON-native leaf inside `raw` is stringified via `py_str` rather than
  raising, but every other write site (`--out`, `--live-state`, persistence, the `params_json`
  parse-then-merge step) raises exactly where Python's plain `json.dumps(..., sort_keys=...)`
  would (including `TypeError: keys must be str, int, float, bool or None, not ...` for an
  unhashable or unsupported key type, and `TypeError` from `sort_keys=True` comparing two keys of
  incomparable types. CPython's own `sort_keys=True` path sorts the **original** `dict` items
  (`sorted(dct.items())`) *before* stringifying any key — not Python's own `sorted()` over
  already-stringified keys — so two int keys sort numerically (`{1: ..., 2: ..., 10: ...}` stays
  `1, 2, 10`, never the lexicographic `1, 10, 2`), and a dict mixing an int key with a str key
  raises `TypeError: '<' not supported between instances of 'str' and 'int'` (confirmed against
  CPython directly). `electricity-json`'s writer's sort step must therefore sort `Value` keys
  directly, using the same cross-type ordering rules Python's `<` uses — raising on an
  incomparable pair — and only stringify each key afterward; an earlier draft's "stringify, then
  sort" description is the wrong order and would both produce lexicographic instead of numeric
  ordering for same-typed numeric keys and silently accept a mixed int/str dict Python itself
  rejects).

**3.4.1 The two `--out` serializations are genuinely different, not whitespace-different**
(runtime-semantics §8.3, Quirk Q9): plain `--out` is `json.dumps(saved)` — insertion order, no
indent, `ensure_ascii=True` (default) — while `--out --pretty` is
`json.dumps(saved, indent=2, sort_keys=True)` — alphabetically sorted keys, 2-space indent, the
same `ensure_ascii=True`. `electricity-json`'s writer takes an explicit `WriteMode { indent:
Option<u8>, sort_keys: bool, ensure_ascii: bool }` rather than inferring sort order from whether
indentation is requested, specifically so a future `--out` variant can't accidentally couple the
two. Both forms always end with a trailing `\n` (matching `cli/app.py`'s `_write_state_json`).

---

## 4. Load-and-check pipeline

1. **Load.** YAML via §3.2's composer, or JSON via `electricity-json`'s duplicate-key-checked
   reader (runtime-semantics §1.1–1.2). The same loader is reused for every `use` child, inline
   or by path.
2. **Structural checks** (`structural_errors()`, runtime-semantics §1.3) — the single gate every
   run and every `use` child passes through, in order, any non-empty result blocking with
   Circuitry's exact `"Orchestration validation failed:\n  - ..."` prefix:
   1. unknown-key errors (near-miss/typo detection against the actual key set for that effect
      type, including the `_MISTAKEN_FOR` hardcoded confusables like `adapter`→`provider`);
   2. JSON-schema errors (Draft7, with the deepest `oneOf` sub-error appended — `electricity-schema`
      (§4 step 3, #380) cannot produce this suffix on `jsonschema` 0.26, whose
      `OneOfNotValid`/`AnyOf` variants carry no sub-error list to pick from; the compiler lane
      built here either re-validates the losing branches by hand or needs a `jsonschema` upgrade
      that exposes them, whichever lands first);
   3. `group:` placement errors (leaf effects only);
   4. `interface.inputs.<k>.type` must be one of the six recognized types;
   5. `interface.inputs.<k>.default` type-mismatch (already-typed data, not CLI text).

   Everything else (`unknown_key_warnings`, every `lint_orchestration()` advisory — deprecated
   aliases, loop-body reference footguns, `threshold:` on a built-in `if`, `min_iterations` on
   `each`) is a **warning**, printed to stderr, never blocking.
3. **Schema parity.** `electricity-schema` validates against the **same**
   `schema/orchestration.schema.json` Circuitry ships — not a hand-maintained re-derivation.
   electricity carries a copy of this schema file, synced from Circuitry's own
   `src/circuitry/schema/` by a script and checked byte-for-byte by CI on every commit (the same
   pattern as Circuitry's bundled docs, so the crate stays self-contained, §17), rather than
   hand-porting the Draft7 document into a Rust-native schema builder; the moment the two
   schemas drift, that check fails loudly instead of silently producing a different accept/reject
   verdict than `cof check`. Message *text* only needs to fail in the same place, not match
   word-for-word (Decisions, §1) — the conformance suite normalizes `jsonschema` crate text
   against Python `jsonschema` text (quote style, error ordering) rather than requiring an exact
   match.
4. **Compile-time checks beyond the schema** (runtime-semantics §1.4), each raising the Python
   reference's exact error-class semantics (message text may differ per the third-party-library
   carve-out, but Circuitry's *own* checks below must match word for word — Decisions, §1):
   - every template compiles (chevron tokenizer, walked over every string leaf recursively);
   - every `mode: cel` expression parses;
   - every `state.<key>` reference resolves to a namespace or an enclosing loop binding
     (inspecting the CEL parse tree, not a runtime check);
   - every `{from: path}` leaf's path is rooted at a namespace or loop binding;
   - no bare `{{name}}` template reference collides with a declared `interface.inputs` name;
   - effect-name validation (`^[A-Za-z_][A-Za-z0-9_]*$`, no `iter_\d+`, no reserved words, no
     duplicates within a scope — `finally:` and the body share one `seen_names` set);
   - `allowed_commands` is a literal list of strings everywhere it appears, never templated or
     referenced (§8's literal-only rule);
   - `group:` names exist in `runtime.concurrency_groups`;
   - exactly one of a bare CEL string or `{mode: cel|model, ...}` on an `expect:`;
   - a tree-flow `each` body never references `prime.<loop>.prev`;
   - exactly one of `ref`/`path`/`inline` on a `use` effect (`orchestration:` is accepted as a
     deprecated alias for `path`/library lookup, consistent with "`use` children by path"
     (Decisions, §1) — electricity does not implement the library-lookup half, so a document using
     `ref:` fails with a clear "library refs not supported; use `path:`" error rather than a
     generic "unknown key").
5. **Compile.** `electricity-compiler` turns the checked document into the bytecode IR (§5),
   rooted at `"prime"` for both a top-level run and a `use` child (each isolated in its own
   state, runtime-semantics §2.4).

---

## 5. The bytecode

Bytecode is an **internal implementation step** (Decisions, §1): YAML/JSON in, results out. There is
no user-facing bytecode format, no `electricity compile` subcommand, and no persisted `.ebc`
file — the IR exists only in memory for the duration of one compile+run, including for a plan
generated at runtime (a reflector's emitted YAML, or a decomposition planner's emitted plan),
which compiles through the exact same `compiler → IR` path a file on disk does. A `--dump-ir`
debug flag may print the IR as JSON for engineers debugging electricity itself; that
serialization is not a contract anything else reads.

### 5.1 Shape

A structured IR ("block bytecode") rather than a flat register/stack design (rust-ecosystem.md
item 35–37): each compiled effect is an `Op` carrying a **stable effect path ID**. For a non-loop
effect the path is simply the document path (`prime.handle`); for an effect nested inside a loop
body, the compiled path keeps the loop variable as a compile-time placeholder
(`prime.shots.<each>.handle`) — the body is compiled exactly once per loop, not once per pass —
and the VM concretizes the placeholder against the running pass index only when it actually
dispatches (`prime.shots.iter_3.handle`); that concretized path is what the state tree, OTel
spans, and `--resume` key off, never the placeholder form. Control flow is nested **regions**,
WASM-style, instead of arbitrary jumps:

```rust
enum Region {
    Block(Vec<Op>),                                    // a dynamic's or document root's effects:
    Loop { body: Box<Region>, mode: LoopMode },         // each/while; see LoopMode below
    If { then_: Box<Region>, else_: Option<Box<Region>> },
    TryFinally { body: Box<Region>, finally: Box<Region> },
    Parallel(Vec<Region>),                              // a *statically known* branch list only —
                                                          // `dynamic flow: tree`'s own effects, never
                                                          // a loop's runtime-sized pass set (below)
}

enum LoopMode {
    EachChain,  // each, sequential
    EachTree,   // each, flow: tree — one compiled `body`, replicated at run time once per element
                // of the resolved collection and dispatched as a single parallel batch using the
                // *same scheduling primitives* `Region::Parallel` uses (§6.2: shared snapshot,
                // semaphore, stop_on_error) — but never materialized as a `Region::Parallel` IR
                // node, since the branch count isn't known until the collection resolves. This is
                // the fix for an earlier draft's ambiguity between representing a tree-mode `each`
                // as `Loop{tree}` and as `Parallel`: it is always `Loop{mode: EachTree}` in the IR;
                // `Parallel` is reserved for `dynamic flow: tree`'s fixed, compile-time branch list.
    While,      // always sequential (runtime-semantics §5.5); no tree-mode `while` exists
}

// A leaf effect's own payload — prompt/tool/use/reflector never contain a nested Region of their
// own (a reflector's `inner` dynamic is a *sibling* field, compiled as an ordinary `Region`, not
// folded into this enum — see the `reflector` row below).
enum LeafKind {
    Prompt(PromptOp),
    Tool(ToolOp),
    Use(UseOp),
    Reflector(ReflectorOp),
}

// A compiled node is *either* a leaf effect *or* a control-flow region that owns its own path —
// `loop`/`dynamic`/`if` compile directly to `NodeKind::Control`, never to a `LeafKind` variant
// that then separately wraps a `Region`. This is the other half of the same earlier-draft
// ambiguity's fix: a `Region` and the `Op` that names it are the same node, not two.
enum NodeKind {
    Leaf(LeafKind),
    Control(Region),
}

struct Op {
    path: EffectPath,       // stable id (placeholder form inside a loop body, see above)
    name: Option<String>,   // None for an unnamed loop/conditional (transparent control)
    kind: NodeKind,
    on_error: OnErrorPolicy,
    labels: Option<Value>,
}
```

A `dynamic`/document-root `finally:` list compiles into a `TryFinally` region wrapping the
`Block`, not a sibling list — both land in the same state node and share one `seen_names` set
(§4, compile-time duplicate check), matching runtime-semantics §6.4.

### 5.2 Per-effect-type compilation

| Effect | Compiles to | Notes |
|---|---|---|
| `prompt` | `NodeKind::Leaf(LeafKind::Prompt)` | Carries the resolved `(adapter, model)` attempt chain *shape* (not resolved values — those are runtime, scoring/routing-dependent) and the pre-rendered template/messages AST. |
| `tool` | `NodeKind::Leaf(LeafKind::Tool)` | Carries the provider name, the params AST (template nodes + `{from:}` markers + the security-sensitive-key literal check already passed), and the `params_json` template (if any) tagged for `JsonAwareCtx` rendering (§3.3). |
| `use` | `NodeKind::Leaf(LeafKind::Use)` | Carries the resolution mode (`path`/`inline`), the inputs AST, and — for `inline` — the **uncompiled** template text (compiled to an IR subtree only at *run time*, after rendering produces YAML text, §5.3). A `path:` child resolves in a fixed order — absolute path first, then relative to the current working directory, then relative to the **parent orchestration's own directory** — and that last fallback is re-rooted per nesting level: a `use` nested inside an already-loaded child resolves its own `path:` relative to *that child's* directory, not the original root document's, so composition can descend through several directories of `use` children without every one needing a path relative to wherever the top-level document happened to live. |
| `loop` | `NodeKind::Control(Region::Loop { .. })` | `each`-chain/`each`-tree/`while` captured in `LoopMode` (above); an unnamed loop compiles identically but dispatches writes into the enclosing scope at run time rather than a child node. |
| `dynamic` | `NodeKind::Control(Region::Block(..))` (chain) or `NodeKind::Control(Region::Parallel(..))` (tree), wrapped in `Region::TryFinally` if `finally:` is present | The document root itself is a `dynamic` named `"prime"` with no scope-overlay semantics (runtime-semantics §2.4's "top-level root is not a scope-overlay container" — compiled into the IR as a `Block` whose child ops read `ctx` by reference, never through `scope_ctx`). |
| `if`/`conditional` | `NodeKind::Control(Region::If { .. })` | `mode: cel`'s expression and `mode: model`'s template are both pre-validated at compile time (§4); `threshold:` is carried through to `meta` but never consulted by the interpreter (runtime-semantics §5.6). |
| `reflector` | `NodeKind::Leaf(LeafKind::Reflector)`, carrying its own `inner: Region` field | Compiles the reflector's own `effects:` (the `inner` dynamic) as an ordinary nested `Region` field on `ReflectorOp`, but the **generated plan** it produces at run time is never part of this compiled IR — see §5.3. |

### 5.3 Run-time compilation of generated plans and inline `use` children

Two cases produce YAML **after** the document's own compile step has already finished, and both
compile through the identical `electricity-compiler` entry point a file-based document uses —
there is no separate "dynamic plan" code path:

- **`use: {inline: ...}`** — the `inline` template renders once, against the *parent's* context
  (Quirk Q3, §13 — this is a parent-scoped render, full stop, never re-rendered against the
  child's own inputs), producing YAML text; that text is loaded (§4 step 1), structurally checked
  if `validate: true` (the default), and compiled into a fresh IR subtree rooted at `"prime"`,
  executed in the child's isolated state. The resulting `Region` is held only for the duration of
  that one `use` effect's execution — it is discarded afterward, not cached, since the Decisions
  above remove the step cache and there is exactly one `inline` render per execution of that effect
  (per-pass, for an `inline` `use` inside a loop body).
- **Reflector-generated plans and decomposition plans** — a reflector's `plan_from_step` prompt
  output, or a decomposition planner's emitted plan, is extracted as YAML text (after stripping
  markdown fences and a non-schema `done:` key for the reflector case), schema-validated, and
  compiled the same way, then run as a `use`-child-shaped execution (`UseDefinition` semantics,
  `on_error: fail`, cycle-guarded through the same run-wide call-stack tracking a `path`/`inline`
  `use` uses). The capability *ceiling* on what a generated plan may invoke (tools-adapters-
  plugins §1.7) is applied identically whether the plan came from a human-written `inline:` or a
  model-generated reflector pass — the compiler does not distinguish the two once it has YAML
  text in hand. For electricity specifically that ceiling is always `None`/unrestricted (§14),
  since the condition that installs a non-`None` ceiling in the reference never arises for a
  path-run document; "applied identically" here means only that the compiler has one code path
  for both origins, not that electricity enforces a ceiling at all.

Because both cases are ordinary compiles of ordinary YAML, there is no separate "interpreter for
generated plans" — the same `electricity-vm` frame machinery (§6) runs a runtime-compiled
`Region` exactly as it runs the root document's.

---

## 6. The VM

### 6.1 Frames

An explicit frame stack, one frame per active `Region`/`Op`, rather than a native-call-stack
recursive walk — chosen so `parallel` can spawn each branch as an independent task with its own
frame, and so cancellation can unwind cleanly through nested `TryFinally` regions without relying
on Rust's own unwinding across an `await` boundary.

**electricity-vm runs on a single-threaded tokio runtime** (`tokio::task::LocalSet`, not the
default multi-worker runtime), and every branch/pass spawned for `parallel` or a tree-mode `each`
(§6.2) is a `tokio::task::spawn_local` task on that one OS thread, not a separate worker thread.
This is what makes `NodeRef`'s `Rc<RefCell<...>>` (§6.7) sound: an `Rc` is not `Send`, so it
cannot cross `tokio::spawn`'s default multi-worker scheduler, and a non-`Send` `NodeRef` captured
in a `spawn_local` future never needs to. "Parallel" here means *concurrent* (many in-flight
awaits interleaved on one thread — the right model for I/O-bound work: HTTP/model calls,
subprocess children, bridge calls), not OS-thread parallelism; true CPU-bound work (an
`imagemagick`/`ffmpeg` child process) is still a real, separately-scheduled OS process, so it does
not need the VM's own thread to be multi-threaded to run in parallel with other effects. This also
answers which side of the `Rc`-vs-`Arc` choice the embedding API (§15, M3-C) exposes: one run's
future is `!Send`, so a host embedding more than one concurrent run gives each its own OS thread
(or its own `LocalSet`) rather than scheduling multiple runs on one shared multi-threaded runtime. A frame holds: the `ctx` it executes
against (a `NodeRef` — a cloned handle onto a live store node, never an owned `Value::Dict`
snapshot, so aliasing is real pointer-sharing rather than a copy nobody can then write through;
§6.7 defines `NodeRef`), the region's local index/iteration state, and a handle to its parent
frame (for scope-overlay rebuilding, §6.3).

### 6.2 Scheduling: tokio tasks for parallel branches

Both a `dynamic flow: tree` (`Region::Parallel`, a statically-known branch list) and an `each
flow: tree` loop (`Region::Loop { mode: LoopMode::EachTree, .. }`, one compiled body replicated
once per resolved-collection element — §5.1) spawn one `tokio::task::spawn_local` task per
branch/pass, on the single-threaded `LocalSet` runtime (§6.1), through the same scheduling
primitive, against a **shared, deterministic snapshot taken at the parallel
dispatch's start** — a shallow copy of `ctx` at that instant (runtime-semantics §5.5) — never each
other's live writes; results merge back into the parent's node in **original index order** once
every task completes, not completion order (runtime-semantics §5.4's tree-`each` ordering
guarantee). `max_concurrency`, when set, bounds a `tokio::sync::Semaphore` sized to that value;
when unset, the semaphore falls back to the branch/pass count (i.e., effectively unbounded for
that one dispatch) — distinct either way from the run-wide limiter (§6.4). `stop_on_error: true`
cancels every not-yet-started task via the dispatch's own `CancellationToken` the moment one
branch/pass fails; already-running tasks are allowed to finish (best-effort, matching the
reference's `ThreadPoolExecutor` shutdown semantics, runtime-semantics §5.5).

### 6.3 Scope overlays

Reproduces runtime-semantics §2.4 exactly, including Quirk Q1:

- the document root (a `dynamic` with no name) and an ordinary named `dynamic` never build a
  scope overlay — a child op's `ctx` is `store.state` **by reference**, so the fully-qualified
  `prime.<sibling>.value` form resolves (reading the same live dict) but the **bare**
  `<sibling>.value` form does not, because nothing ever pushes `<sibling>` to the top of `ctx`;
- a loop body / `if` branch **does** build an overlay (`scope_ctx`): a shallow merge exposing
  each prior sibling in the same body/branch under both the bare top-level spelling and the
  `prime`-nested spelling, rebuilt from the original `ctx` after every sibling (never layered on
  the previous overlay);
- `local_writes()` shadowing: a name local to the current body/branch wins inside it even if the
  same name existed in the enclosing baseline; the enclosing name returns once the body/branch
  ends;
- `prime.<loop>.prev` (chain-flow named loops only) exposes the previous *completed* pass's own
  writes, absent (not an empty node) before the first completed pass;
- **a named `dynamic` nested inside a loop body or `if` branch** does not simply inherit the
  enclosing overlay unmodified. The reference builds that nested dynamic's own `ctx` as
  `{**ctx_override, **store.state}` where `ctx_override` is the enclosing body/branch's
  `scope_ctx` overlay (runtime-semantics §2.4) — i.e. `store.state`'s three real root namespaces
  are re-merged **on top of** the overlay. Since `scope_ctx` only ever adds bare top-level sibling
  names and merges its local writes *one level inside* `ctx["prime"]`, and `store.state` has no
  top-level keys other than `input`/`prime`/`runtime`, the practical effect is: the loop variable
  and any bare sibling name from the enclosing overlay **survive** (they aren't namespace keys,
  so `store.state` has nothing to overwrite them with), but `store.state`'s own unmodified `prime`
  key **overwrites** the overlay's `prime` (which had the enclosing body's local writes merged
  into it) — so a bare `{{sibling.value}}` reference still resolves inside the nested dynamic's
  own top-level effects, but `{{prime.sibling.value}}` inside it sees the *real*, un-overlaid
  `prime` tree, not the enclosing loop/if's locally-overlaid view of it. electricity's scope-overlay
  construction must reproduce this exact re-merge, not just "inherit the parent's `ctx`" — a naive
  port that has a nested dynamic simply reuse `ctx_override` as its own `ctx` would leave the
  overlay's `prime` in place instead of restoring the real one.

**Loop mechanics not otherwise covered above** (runtime-semantics §2.3/§5.4—§5.5, condensed —
filling in detail an earlier draft of this design left to a one-line bytecode-table row, §5.2):

- **`termination.reason`** is one of: `collection_exhausted` (`each`, every item ran);
  `max_iterations_reached` (hit the cap — a `while`/`each truncate: true` cap, or an `each` whose
  collection outran the cap under `on_error: break/continue`); `collection_unresolved` (`each.in`
  didn't resolve to a list at all — kept distinct from "exhausted" so a misspelled path isn't
  mistaken for zero items; `meta.each_in_error` names the failure); `condition_false` (`while`,
  genuinely false); `condition_error` (`while`, the condition itself raised, absorbed by
  `on_error: break/continue`); `error` (a body pass failed under `on_error: break`, or the
  each-loop bounds check failed under `on_error: break/continue` — `termination.detail` carries
  the message in that one case, since there is no failed per-pass node to carry it).
- **`truncate`/`unvisited`**: `each.truncate: true` processes only the first `max_iterations`
  elements and records the rest, by original index, under `meta.unvisited` — never silently
  dropped.
- **`min_iterations`** skips the *condition* check (not the body) for the first `min_iterations`
  passes of a `while` loop — the loop runs at least that many passes regardless of what the
  condition would have said on an earlier pass.
- **`while` always runs sequentially**, even when the orchestration otherwise sets
  `flow: tree` on it — there is no tree-mode `while` (reflected in `LoopMode::While` carrying no
  tree variant at all, §5.1); only `each` has a tree/chain choice.
- **`each.as`/the implicit `iter` bindings**: `each.as: <name>` binds the current element under
  that name, in addition to the implicit `iter.index`/`iter.count` carried in `ctx` (§6.3).
  **`as:` defaults to `item`, not to no binding at all** — an `each` loop with no `as:` still
  binds the current element as `{{item}}` inside the body; the element is never reachable only
  through `iter` (an earlier draft of this design stated the opposite, which would break every
  `{{item}}` reference in a loop body with no explicit `as:`).
- **`last` after zero completed passes**: a loop that never completes a single pass (an empty
  `each` collection, or a `while` whose condition is false before the first pass) writes **no**
  `last` key at all — not a `null`, not an absent-but-present key, simply never set.
- **`last` tracks the last pass that *completed without error***, not the last pass that merely
  ran — a `continue`/`break`-absorbed failing pass is skipped when picking which `iter_N` `last`
  aliases (§6.7's identity-aliasing mechanics apply to whichever `iter_N` this selects).

### 6.4 The concurrency limiter

One `RunConcurrencyLimiter` built once per run from `runtime.max_concurrency` (a global
semaphore) and `runtime.concurrency_groups` (named semaphores), threaded through the VM via the
shared run context — including into a `use` child's own context (same limiter instance, so the
cap is run-wide, not per-document) (runtime-semantics §7.2). Only leaf effects (`tool`, `prompt`)
ever acquire a slot; acquisition is always **group-first, then global**, the fixed order that
rules out the classic two-lock deadlock.

**The slot is held per attempt, not per retry loop**, and is explicitly released:
- before that attempt's backoff sleep runs, and
- before a tool's model-mode `expect:` check runs (runtime-semantics §7.3) —

both deliberate choices in the Python reference (cross-referenced against issues #333/#273/#281)
that electricity reproduces exactly: holding the slot across a whole retry loop would
under-parallelize relative to `cof` in a group-starved run, changing observable `waiting_for`
events and `--live-state` interleaving even when it happens not to change a deterministic
conformance case's final value.

### 6.5 Cancellation

A `CancellationToken` tree, one token per frame, parented along the frame stack. SIGINT/SIGTERM
install a top-level token cancellation (not a raw Rust panic/unwind); a `TryFinally` region's
`finally` always runs on cancellation (best-effort — matching the reference's
`except BaseException` around `finally:`, runtime-semantics §6.4) and the cancellation still
propagates afterward regardless of the enclosing `dynamic`'s own `on_error` (which only degrades
an *ordinary* failure, never a cancellation). This mirrors CPython 3.11's exception-table
unwinding and Lua's protected calls (rust-ecosystem.md item 37) without needing a VM-frame
snapshot — resume is state-based (§6.7), not snapshot-based, so cancellation only needs to unwind
cleanly, not serialize mid-flight frames.

**What the reference implementation currently does for already-dispatched work.** An earlier
draft of this section described the reference's own cancellation as "already-dispatched work
finishes and its result still lands in state." That is wrong for both of the reference's two
flows, in different directions, and the correction is what the agreed fix below starts from:

- **Chain flow.** Every leaf effect dispatches on the **main thread** — there are no worker
  threads to keep running anything. `KeyboardInterrupt`/the CLI's `SigTermInterrupt` is raised on
  that same main thread while it's blocked inside the leaf's own call (e.g. `subprocess.run`
  waiting on a `shell` child). CPython's `subprocess.run` catches exactly that interruption,
  **kills the child process**, and re-raises — so the in-flight leaf's result is **lost**, not
  recorded in state, the same as if it had never been dispatched (runtime-semantics §6.5).
- **Tree flow.** A `dynamic flow: tree`/tree-`each` dispatch runs its branches on a
  `ThreadPoolExecutor`. Leaving the `with ThreadPoolExecutor(...) as executor:` block on
  interrupt calls `executor.shutdown(wait=True)` with `cancel_futures` **not** set — which does
  **not** cancel any future already submitted to the pool, running or merely queued. Every
  branch that was ever submitted, not just the ones already running, finishes before `shutdown`
  returns. But the code that merges each branch's isolated store back into the parent's state
  sits **after** that `with` block, sequentially — so when the interrupt is raised while the main
  thread is blocked waiting inside the block, it propagates straight past the merge step. Every
  submitted branch still runs to completion; **none** of their results reach state.

Neither of these is a deliberate, coherent cancellation design — the chain-flow case throws away
an in-flight leaf's work, and the tree-flow case does the opposite (runs queued work it didn't
need to) while still discarding every result.

**The agreed behavior (settled 2026-10-06; in Circuitry since #357), on both engines: both runners
stop promptly.** On the first SIGINT, SIGTERM or SIGHUP:

- every **running** leaf effect is stopped immediately — its child's process group is killed
  (SIGKILL). As in Circuitry, while the CLI's signal handling is active every tool subprocess runs
  in its own session and process group (as plugin-bridge processes already do, §8.5), so the kill
  reaches any grandchild the tool spawned. Two consequences, documented by Circuitry too: the
  child never receives the terminal's own Ctrl-C (a tool's graceful shutdown, such as ffmpeg
  finalizing a partial file, does not run), and it cannot open the terminal (no interactive
  prompts). An embedded run (the library crate without the CLI's handlers) keeps children in the
  host's process group, and a timeout there kills only the child, never the host's own group.
  In-flight network calls electricity holds open are cancelled where the client supports it;
  Circuitry does not cancel an in-flight MCP call, and electricity may, since the call's result is
  discarded either way and state is unaffected;
- a branch that has **not yet started** — a `dynamic flow: tree` branch, a tree-mode `each`
  pass — is never started, full stop, never merely left queued and run anyway;
- a `TryFinally` region's `finally:` still runs, exactly as the unconditional-propagation rule
  above already states;
- the state an interrupted run leaves is **otherwise unchanged**: an interrupted leaf's or
  branch's result is not recorded and not merged, whether it was already running or never
  started — an interrupted parallel/tree step is not merged, `--resume` reruns it like any other
  incomplete effect (§6.8), and the run's state up to that point is persisted exactly as an
  ordinary interrupted run is today (§6.8, §10.1), with the container fix the next bullet states;
  exit code is `130`/`143` either way (§6.9). **A tree branch that had already finished before the
  signal arrived is not merged either** — today's reference merge step only ever runs once every
  submitted branch has returned, and the signal unwinds straight past it (above), so there is no
  already-finished-branch case for electricity's fix to carve out specially: the fix changes which
  branches ever *run* (none that hadn't started when the signal arrived), not which already-done
  ones get merged (none, on either engine, before or after the fix).
- **an interrupted `dynamic` must not look finished to `--resume`; a `loop`/`if`/`use` already
  don't.** Only `dynamic` catches a bare interrupt at all (`except BaseException`, so it can still
  run its own `finally:` and record the failure) — a `loop`, a conditional, and `use` each catch
  only `except Exception`, so a real interrupt passes straight through every one of those three
  untouched, leaving in place the `completed_at: None`/`error: None` pair each already wrote when
  it started. `--resume` already reruns an interrupted `loop`/`if`/`use` correctly today, on both
  engines, and that does not change. The fix below is scoped to the one container that does catch
  the interrupt: a named `dynamic` still on the call stack when the signal arrives gets its own
  `meta.completed_at` set, same as today, but its own `meta.error` must be set to the *same*
  literal interrupt text `RunResult.error` gets (§6.6 below: `"Interrupted (Ctrl-C/SIGINT)"`,
  `"Interrupted (SIGTERM)"` or `"Interrupted (SIGHUP)"`), never left empty. This is a deliberate fix, not a reproduction: the
  reference today sets a `dynamic`'s own `meta.error` to `str(body_exc)`, which is `""` for both
  `KeyboardInterrupt` and `SigTermInterrupt` — and `--resume`'s own completion check
  (`completed_at` truthy *and* `error` falsy, §6.8) then treats that empty string as "no error,"
  silently skipping the `dynamic`'s own unfinished children on resume. Circuitry fixed this
  together with the rest of this section (#356, merged as #357); electricity implements the
  corrected text, and its conformance case comes from that fix (§12).
- **a second SIGINT/SIGTERM received while this stop-and-unwind sequence is still in progress
  ends the run at once** (no `--out`, no traceback), with that second signal's own exit code
  (`130`/`143`), as in Circuitry — cleanup does not get a second grace period;
- **SIGHUP never acts as a second signal.** SIGHUP (the terminal closed) starts cancellation like
  SIGTERM, with exit code `129`, unless SIGHUP was already ignored when the run started (`nohup`),
  in which case it stays ignored throughout. Once a run is stopping, every further SIGHUP is
  ignored: closing a terminal delivers SIGHUP more than once (zsh forwards one to its foreground
  job and the kernel sends another when zsh exits, about 1 ms apart), and treating the repeat as a
  second signal would abort the cleanup the first one started. After a cancelled run SIGHUP stays
  ignored until `--out` is written; it is never set to ignored while steps can still start
  children, which would inherit it;
- **a write to a terminal that has gone away (EIO) or to a closed pipe (EPIPE) never stops
  cleanup, `--out` or the exit code**; any other write error (a full disk under a redirected
  stdout) still surfaces.

This is one rule applied uniformly to chain and tree flow alike — stop what's running, never
start what isn't, keep `finally:`, merge nothing that was still in flight — replacing both of the
reference's current, inconsistent behaviors above, not reproducing either one as-is. **Circuitry
has this fix since #357** (closing #356): electricity implements the same behavior, and its
conformance cases for cancellation (§12) come from that PR's tests, which drive real `cof run`
processes with SIGINT, SIGTERM and SIGHUP, including through a pseudo-terminal running an
interactive shell.

### 6.6 The error model

**`on_error` matrix** (runtime-semantics §6.1), reproduced exactly:

| Effect | Values | Non-default behavior |
|---|---|---|
| prompt, tool, use | `fail` (default), `skip`, `continue` | `skip`/`continue` clear `value` to `null`, keep `meta.error` |
| loop | `fail`, `break`, `continue` | `break` stops the loop (`termination.reason: "error"`); `continue` skips the pass |
| dynamic | `fail`, `skip`, `continue` | `skip`/`continue` degrade *this dynamic's own* failure one level, same as a leaf |
| if/conditional | `fail`, `skip`, `continue` | `skip` writes a null-result node, no branch runs; `continue` **forces the else branch** (not "no branch") |
| reflector | *(none — always propagates)* | — |

**Error text.** `meta.error` is always `str(exception)`-equivalent text, never a structured
object. Per the Decisions above: messages Circuitry itself writes (the dynamic-wrapped
`"<path>: <message>"` format, `expect failed: ...`, answer-parse failures, `use` cycle messages,
loop bounds errors — all cited verbatim in runtime-semantics §6.2) must match **word for word**;
text that originates in a third-party library (a CEL evaluation error, a YAML parse error, a
JSON Schema message) only needs to **fail in the same place**, with the conformance suite
normalizing that text rather than diffing it byte-for-byte.

`finally:` failure interaction (runtime-semantics §6.4): if the body failed and `finally:` also
fails, the body's error is what's reported at `meta.error`; the `finally:` failure rides along
as a non-overriding `meta.finally_error`. If the body succeeded but `finally:` failed, that
failure becomes this dynamic's own failure, subject to this dynamic's own `on_error`.

**Representative exact strings electricity must reproduce byte-for-byte** (runtime-semantics
§6.2 — a non-exhaustive sample of the word-for-word rule already stated above, called out
specifically because each has its own format template, not just its own wording): a tool
dispatch failure wrapped by the enclosing dynamic is `"<dotted-path-to-failing-effect>: <original
exception message>"` (e.g. `"prime.second: shell: args[1] contains forbidden char '\n'."`); a
`use` cycle is `"use '<name>': cycle detected — <path> → <path> → ..."`; a loop collection-bounds
failure is `"each loop '<name>' (<in_path>): collection has N items but max_iterations is M —
raise max_iterations, bound the collection, or set each.truncate: true to process the first M and
record the rest as unvisited"`; SIGINT/SIGTERM/SIGHUP set `RunResult.error`,
`runtime.persistence.error`, and the `on_run_failure` event's own `error` argument to the literal
text `"Interrupted (Ctrl-C/SIGINT)"` / `"Interrupted (SIGTERM)"` / `"Interrupted (SIGHUP)"`, never the raw signal
exception's own text — and, when a persistence backend is configured, the same literal text also
reaches that backend's own stored run-snapshot record (its `error` field, alongside the `state`
it snapshots — e.g. the jsonl-file backend's per-line `error` key), since `save_run_snapshot` is
called with this same `error_message` — so these three state fields are not the only things this
literal text reaches, only the only places *inside `state` itself* it reaches. A container's own
`meta.error` is a second, separate concern, fixed in §6.5 above, not something today's reference
already reuses this same formatting for.

**`meta.child_errors`** (a `use` effect's own meta field, §6.10's sibling for the `use` node
shape): a flat list of `{path, error}`, one entry for **every** node anywhere in the child's own
tree whose `meta.error` is non-empty, walked recursively regardless of whether that node's own
`on_error` already absorbed the exception — so a `use` effect that itself reports success
(`meta.error: null`, nothing propagated to the parent) can still carry `child_errors` entries for
swallowed grandchild failures (conformance case C18). `path` is relative to the child's own root,
not the parent's; `null` when there are none, never an empty list.

### 6.7 State store and write hooks

**Node identity.** Circuitry's state model relies on real aliasing in three places that an owned,
plain `Value::Dict` tree cannot express: a loop's `last` key must alias a sibling `iter_N` node
*by identity* (runtime-semantics §2.3), a plain `dynamic`'s children must read `ctx` as the *same live dict*
siblings already wrote into rather than a copy (§6.3), and `--live-state`/persistence need to
observe writes as they happen rather than re-walking a value tree. `Value` itself (§3.1) stays a
plain algebraic type with no reference semantics of its own — used for leaf values, for `Op`
payloads, and for materializing a point-in-time owned snapshot (a parallel dispatch's starting
copy, §6.2; the final `--out`/`--live-state` write). The *running* store instead represents every
container node (the three root namespaces, and every named `dynamic`/loop/`if`'s own node) as a
`NodeRef = Rc<RefCell<IndexMap<Value, Value>>>`. A `ctx` passed down a frame (§6.1) is a cloned
`NodeRef` (an `Rc` clone, not a copy of the contents), so a later sibling's fully-qualified read
sees the same live node earlier siblings wrote into, exactly reproducing "`ctx` is `store.state`
by reference" (§6.3) without needing `Value` itself to carry interior mutability. The store
exposes:
- `resolve(path) -> NodeRef` — walks dotted path segments by cloning the `NodeRef` at each level,
  creating an intermediate node if absent; this is `ensure_dict(path)`'s Rust shape;
- `fire_effect_start`/`fire_effect_complete` — called on **every** exit path (success, absorbed
  failure, re-raise) so the pair is always balanced; this is the hook point every runtime plugin
  (§10) and `--live-state` (§10.5) subscribe to, matching `core/runtime_plugins.py`'s
  `on_effect_start`/`on_effect_complete` contract (tools-adapters-plugins §4.3) field-for-field;
- `on_write` — a lower-level callback fired on any mutation, which `--live-state`'s coalesced
  mirror (§10.5) subscribes to directly rather than through the plugin hook list, matching the
  reference's own split between the `RuntimePlugin` protocol and the CLI-only `LiveStateMirror`.

**`last`-alias compaction.** A named loop's `last` key holds a second `NodeRef` pointing at the
*same* `Rc` as the sibling `iter_N` node — never copied — exactly reproducing the reference's
in-memory identity (runtime-semantics §2.3); checked via `Rc::ptr_eq`, which is the Rust shape of
Python's `is`. Serialization (`--out`, `--live-state`, persistence) **materializes** the live
`NodeRef` tree into an owned `Value::Dict` snapshot and, in the same pass, rewrites any `last`
that is `Rc::ptr_eq` to one of its own siblings into `{"$ref": "iter_N"}` before writing
(`compact_last_aliases`); loading reverses it, re-establishing the `Rc` sharing
(`link_last_refs`). A `last` that happens to hold an independent value (a document literally
naming an effect `last`) is left untouched, checked by pointer identity, not key-name pattern.

### 6.8 State-based positional `--resume`

No VM-frame snapshot is ever serialized (§6.5) — resume works exactly like the reference,
by re-running from the top and skipping whatever the loaded state says already finished
(runtime-semantics §8.5):

- an effect is resumable iff `meta.completed_at` is truthy **and** `meta.error` is falsy;
- within one chain-flow `dynamic`'s own `effects:` list, resume is **positional**: the moment one
  effect isn't resumed whole, every later sibling in that same chain reruns too, regardless of
  what its own node might (misleadingly) show from a stale prior run;
- a named loop resumes at the first gap in its own `meta.completed_passes` contiguous-from-0
  prefix — chain flow only; a tree-flow `each` or an unnamed loop always reruns whole;
  `finally:` is **never** skipped by resume;
- `document_sha256` guards against resuming against a changed document, refusing unless
  `--force` is passed (CLI) or the embedding caller explicitly opts in (library);
- for a **bare run-id** resume (neither `--state` nor `--resume last`, both of which already
  carry their own resolved `input.*`), every key the loaded state's `input` namespace has must be
  re-passed via `-e` on the new invocation — a single unrelated `-e` does not satisfy this check
  (runtime-semantics §8.5); electricity's CLI raises the same way, before compiling anything.

Two resume state sources, matching runtime-semantics §8.5: `--resume <run-id>` (via a configured
persistence backend, §10.1) and `--resume last`/`--state <file>` (an explicit `--out` file).
`--resume last`'s "most recent run" bookkeeping is a thin CLI-level stash; the library crate's
embedding API takes an explicit `initial_state: Value` instead and has no file-stash concept of
its own. **`--resume` with no explicit `--out` of its own writes back to the file it resumed
from** — `--state <file>` writes back to that same file, `--resume last` writes back to the
prior run's own `--out` file (the file the stash points at); in `effective_settings.sources`
(§6.10) this makes `out`'s value `"resume"`, ranked below an explicit `--out` but above writing
nothing. A bare `--resume <run-id>` has no single file of its own to default to (it resolves
through the persistence backend, not a file path), so it gets no such default — `--out` must be
given explicitly or nothing is written.

**Auto-load from persistence.** When `runtime.persistence` is configured and enabled, and the run
was started with **neither** `--state` **nor** an explicit `initial_state` (library) **nor any
`-e` flag at all** — i.e. a completely bare invocation, not merely a non-`--resume` one — the VM
seeds the run from `persistence.load_latest_state` before `interface.inputs` checking runs,
exactly as the reference does. **A single `-e` is enough to skip this path entirely**, even with
no `--state`/`--resume` given: the CLI layer builds a non-null `initial_state` straight from any
`-e` values before the run function ever checks whether persistence should auto-load, and that
check is `initial_state is None and state_path is None` — an earlier draft of this design said
auto-load applies to "every run... with no other state source," without naming `-e` as one of
those sources, which it is. On success, the loaded state (after `last`-alias relinking, above)
becomes the run's starting state and `runtime.last_run`/`_run_id`/`_timestamp` (§6.10) are still
assigned fresh, not carried over from the loaded snapshot. On failure, the run does not silently
start cold: it writes
`state.runtime.persistence = {"enabled": true, "status": "load_failed", "error": "<str(e)>"}` into
whatever state exists so far and raises `"Failed to load persisted state: <e>"`, matching the
reference's own message and failure point word for word (the Decisions above).

### 6.9 Signals and exit codes

| Condition | Exit code |
|---|---|
| Run succeeded | `0` |
| Run failed (ordinary error) | `1` |
| Interrupted by SIGINT | `130` |
| Interrupted by SIGTERM | `143` |
| Interrupted by SIGHUP (the terminal closed), unless SIGHUP was ignored at start (`nohup`) | `129` |
| A malformed `-e` value (`KEY=VALUE` parsing failure) — the one pre-run usage error the reference's own CLI never explicitly catches, left to Typer/Click's default `BadParameter` handling | `2` |
| Every other pre-run usage problem the reference's own CLI explicitly catches and re-raises as `typer.Exit(code=1)` — a `--resume` safety check failing without `--force`, an orchestration path that doesn't resolve, an unreadable config path | `1` |

All three interrupted cases still write `--out` exactly like an ordinary failure (runtime-semantics
§6.5, §8.4) — the exit code alone distinguishes them; resumability is identical across all
of them. `tokio::signal` installs the handlers; cancellation reaches the whole frame tree via
the top-level `CancellationToken` (§6.5), not a raw process-level abort, so `finally:` regions
still run before the process actually exits. A **second** SIGINT/SIGTERM delivered while that
stop-and-unwind sequence is still running (§6.5) ends the process at once, with that second
signal's own exit code (`130`/`143`), as in Circuitry — cleanup is given one chance, not an
unbounded one; a repeated SIGHUP never ends it (§6.5). The exit-code set `{0, 1, 2, 129, 130, 143}`
is settled (2026-10-06: "plus 2 for usage errors, as `cof` (click)"; `129` since Circuitry #357) — but *which* pre-run usage problem
gets `2` versus `1` follows the reference's own CLI case by case, not a blanket "any usage error
is 2" rule: the reference explicitly catches a failed `--resume` safety check's
`typer.BadParameter` and a few other pre-run problems (an orchestration path that doesn't
resolve) and re-raises each as `typer.Exit(code=1)`; only a malformed `-e` value's
`typer.BadParameter` is never caught anywhere in the reference's own call chain, so it falls
through to Typer/Click's own default `BadParameter` handling, which is what actually produces
exit code `2` — not a deliberate "usage errors are 2" design choice in the reference at all.
electricity reproduces that same case-by-case mapping rather than grouping every pre-run failure
under one code. None of these four write `--out`, since there is no state yet to write when any
of them fires.

### 6.10 Run-level state (`runtime.last_run`, `effective_settings`, `plugins`)

Every `--out` — not just a successful one — carries framework-level metadata next to the
orchestration's own `prime`/`input` trees, written once at run start and updated at every exit
path. electricity reproduces every key, since a diff against `cof run --out` otherwise fails on
these before it ever reaches the orchestration's own state:

- **`_run_id`** (a UUID v4 string) and **`_timestamp`** (`started_at`, formatted `%Y%m%d_%H%M%S`,
  UTC — **not** ISO-8601; this is the one `_timestamp`-shaped field the §12 normalization rules
  must know to parse with that format rather than `DateTime::parse_from_rfc3339`) — two
  root-level, underscore-prefixed keys, siblings of `input`/`prime`/`runtime`, not nested under
  `runtime`. `migrate_legacy_state`'s root-key sweep (§6.8's loader, runtime-semantics §2.1)
  treats any `_`-prefixed root key as framework metadata, never a candidate for migration into
  `input` — electricity's loader must exempt exactly these two keys the same way. **Both are
  assigned after `interface.inputs` checking runs, not before** (an earlier draft of this design
  had the order backwards): state is seeded (from `--state`, persistence auto-load, or a cold
  start), `interface.inputs` is checked/defaulted against that seeded state, and only then does
  the run get its fresh `_run_id`/`_timestamp` and its `runtime.last_run` record — so a run that
  fails during `interface.inputs` checking itself never reaches this point and writes no fresh
  `_run_id`/`_timestamp`/`runtime.last_run` record of its own. **That is not the same as writing
  nothing at all.** Every failure, this one included, goes through the one shared
  failure-handling path (§6.9's table — not one of its two pre-run-usage-error rows, covering
  four pre-run-usage-error cases between them, since state already exists by the time
  `interface.inputs` runs): it unconditionally `setdefault`s
  `runtime.last_run` into existence if it is not already there and fills in its `.completed_at`/
  `.totals` (updating an older `last_run` carried in from `--state` rather than replacing it), and
  does the matching `setdefault` for `runtime.plugins.{configured, loaded, events}`, appending the
  `on_run_failure` event for every plugin that loaded. `--out` is written with that partial
  record, the same as any other in-run failure (§6.9).
- **`runtime.last_run`**: `{run_id, orchestration_path, document_hash, dry_run, validate_only,
  verbose, started_at, completed_at}`. `document_hash` is the orchestration file's own SHA-256
  (`null` if it became unreadable between the earlier load and this hash — not treated as fatal
  here, since the load already succeeded once); it is what a later `--resume`'s safety check
  compares against (§6.8). `dry_run`/`validate_only`/`verbose` are always `false` for electricity's
  v1 CLI surface (none of the three flags exist yet — Decisions, above — so these fields are
  always written `false`, not omitted, to keep the state shape identical). `completed_at` starts
  `null` and is filled in on **every** exit path, success or failure alike, alongside `totals`.
- **`runtime.last_run.totals`**: `{wall_time_s, effects_run, tokens_sent, tokens_received,
  cost_usd}`, computed **live** from the same effect-complete event stream `--live-state`
  consumes (§10.5) — never by walking the finished state tree, since a `use` child in
  declared-outputs mode never leaves its effects in the final tree and an unnamed loop overwrites
  the same node every pass. **`cost_usd` is `null`, never `0`, until at least one effect reports a
  cost** — the reference's own accumulator starts at `None` and only becomes a number once
  something adds to it (an earlier draft of this design said it "sums to 0"; it does not,
  it stays absent-as-`null` the whole run if nothing ever reports a cost — see the §10.5 note on
  no adapter populating `meta.cost_usd` yet).
- **`runtime.effective_settings`**: `{model, adapter, out, plugins, runtime, sources}` plus an
  optional `profile` key, written exactly when `--profile` resolved a file for this run
  (§6.11 has the record's exact shape). `model`/`adapter` are the run's resolved defaults;
  `out` is the resolved `--out` path for this run — `--out` as given, a profile's own `out:`
  (§6.11), or the resume default (§6.8), in that order — or `null` when none of those apply and
  nothing is written; `plugins` is the configured runtime-plugin id list (§11); `runtime` is the
  redacted snapshot of the merged `runtime:` config tree (§8.2's deny-list, applied here too —
  this is a config object, not orchestration state, but it still reaches `--out`), **with the
  run-wide concurrency-limiter handle dropped before the snapshot is taken** (it isn't
  JSON-serializable at all) but every other private, non-document key the reference also carries
  on this tree — `_orchestration_dir` (the absolute directory a relative `use path:` falls back
  to, §5.2), `_allowlists`, and `_capability_allow` (always `[]` for electricity, since there is
  no `--allow-capabilities` flag) — left in place and redacted along with everything else, not
  stripped out, so the snapshot's key set matches `cof run --out`'s byte-for-byte.

  **`sources`** tracks which layer resolved each effective setting, using the reference's own
  fixed vocabulary of layer labels (`"cli"`, `"orchestration"`, `"config"`, `"default"`, plus
  `"router"` once routing wins, `"profile"` for anything a profile sets, and `"resume"` for
  `out`) — not a placeholder. Because electricity has no `--model`/`--adapter`/`--plugins`/
  `--scoring`/`--routing`/`--decompose` CLI flags, the one reference label that can simply never
  occur for electricity is `"cli"` for anything but `out` — `"profile"` occurs exactly as it
  does for the reference, since electricity's profile support (§6.11) is otherwise identical.
  Every label that *can* occur is computed exactly, not synthesized, for every one of the
  reference's own `sources`
  keys: `model`, `adapter`, `out`, `plugins`, `runtime`, `adapters.<adapter>.timeout_seconds`
  (only present once an adapter is resolved), `persistence` (present only once a persistence block
  resolves from somewhere), `complexity`, `complexity.{scoring,routing,decomposition}`,
  `complexity.routing.bands`, and `complexity.decomposition.{threshold,max_depth,on_failure}`.
  `model`/`adapter` resolve to `"orchestration"` (document sets it), `"config"` (config sets it,
  document doesn't), `"default"` (neither), or `"profile"` (a profile set it — §6.11) — `model`
  additionally becomes `"router"` once routing is enabled and wins (§9.7); `plugins`/`runtime`
  resolve to `"orchestration"`/`"config"`/`"default"` only, the same three, since a profile sets
  neither key (§6.11); `out` resolves to `"cli"` (`--out` given), `"profile"` (a profile's own
  `out:` — §6.11), `"resume"` (a resumed run with no `--out` of its own — §6.8), or `"default"`
  (no `--out` at all, nothing written); `persistence` has a `"profile"` layer **above**
  orchestration/config (§6.11's correction below) — a profile's own `persistence:` block replaces
  the merged value outright rather than contributing to it piecemeal, so this key only ever asks
  *which one layer* resolved the whole block, never which layer supplied which piece of it;
  `complexity`/its five sub-keys have no profile layer and follow the plain
  orchestration/config/default pattern `model`/`adapter` do, computed field-by-field exactly as
  the reference's own `_record_complexity_sources` does (a sub-block's own provenance, not just
  whichever layer supplied the `complexity` key as a whole).
- **`runtime.plugins`**: `{contract_version, configured, loaded, events}` — `configured` is the
  requested runtime-plugin id list as given; `loaded` is the subset that actually initialized
  (by name); **`events` records one entry per load attempt (`{plugin, hook: "load", ok, error}`,
  for every configured plugin, success or failure alike) plus one entry per *run-level*
  lifecycle-hook firing (`on_run_start`, and whichever of `on_run_success`/`on_run_failure` this
  run reaches) for every plugin that loaded.** Per-effect hooks (`on_effect_start`/
  `on_effect_complete`) are **not** recorded here: the reference's own dispatcher calls
  `invoke_plugins` for those two hooks but discards the list it returns, so no per-effect plugin
  event ever reaches `runtime.plugins.events` in the reference — an earlier draft of this design
  said per-effect firings land here too; they do not, and a run with a runtime plugin configured
  would otherwise diff against `cof` on every single effect. `contract_version` **must be the
  fixed string `"1"`**, matching the reference's own
  `PLUGIN_CONTRACT_VERSION` byte-for-byte (an earlier draft said this electricity-side identifier
  "need not match the reference's own version string"; it must, since the conformance suite
  diffs this field like any other).
- **`runtime.persistence`** (present only when a persistence backend is configured): `{enabled,
  status, error, loaded_from_persistence, persisted, run_id, ...describe()}` — `describe()` is
  backend-specific (§10.1); `loaded_from_persistence` and `persisted` track whether this run's
  starting state came from auto-load (§6.8) and whether this run's own ending state was
  successfully saved, respectively.

### 6.11 Profiles

A profile is a YAML file supplying run-level defaults (adapter/model/out), a base layer of
`input` values, and per-effect `model`/`provider`/`enabled`/`routing` overrides for a single run
— the reference's own format (`docs/profiles.md`, `cli/profiles.py`), reproduced exactly except
for how the file itself is located:

- **No discovery.** The reference resolves a bare `--profile <name>` by searching
  `<orchestration_dir>/profiles/<name>.yml` then `<cwd>/profiles/<name>.yml` (and the `.yaml`
  variant of each, `discover_profile_path`). electricity has no such search: `--profile <path>`
  names the file directly, consistent with "explicit config only, both files operator-chosen"
  (§1). A path that doesn't resolve to a readable file is **not** one of §6.9's enumerated
  pre-run usage errors: the reference's own `load_profile` runs inside the run's single
  top-level try block, *after* state/orchestration loading, the allowlist check, and capability
  consent, but still before `interface.inputs` checking (`runtime_shim.run`'s own call order) —
  so a profile the reference can't find, read, or parse fails exactly like any other in-run
  failure, not a pre-run one. electricity checks the profile path at that same point in its own
  pipeline (state/orchestration/allowlist/consent, then the profile, then `interface.inputs`),
  not earlier — matching where the reference's own failure happens, not just its outcome — so a
  profile that doesn't resolve writes the partial `--out` record §6.10's correction describes,
  and exits `1`, not nothing, exactly as the reference does. electricity matches
  that outcome (writes `--out`, exits `1`) for a profile path that doesn't resolve, rather than
  grouping it into the pre-run bucket the way an unreadable *config* path is (§6.9) — the two
  look similar but the reference's own failure point for each is different. Every other profile
  failure below — schema validation, an unknown effect path, an unresolvable routing pin — is an
  in-run failure for the same reason, writing `--out` the same way. **A profile failure's own
  `--out` is only ever the plain `--out` path given on this run, never the profile's own `out:`
  or the resume default** — the reference's `resolved_out` (what the failure handler actually
  writes to) starts as the caller's own `--out` and is only refined to include a profile's `out:`
  once `resolve_effective_settings` runs, which is *after* the profile has already loaded
  successfully; a profile that fails before that point never reaches its own refinement, so its
  failure writes wherever `--out` alone pointed (or nowhere, if `--out` was never given) rather
  than the profile's own `out:` or §6.8's resume default. electricity matches that: a profile
  failure's `--out` resolution uses only the plain `--out` value, not the profile's own `out:` or
  the resume default, the same narrower resolution the reference has at that point in its own
  run.
- **Schema and parsing.** electricity carries a synced copy of `schema/profile.schema.json`,
  checked the same way as the orchestration schema (§4 step 3);
  Draft7-validated the same way, word-for-word on Circuitry's own validation-failure message
  shape, third-party `jsonschema` text normalized the same way (Decisions, §1).
- **The loaded record** mirrors `ProfileSettings` field-for-field: `adapter`, `model`, `out`,
  `inputs` (a dict), `effects` (a map of dotted effect path → `{model?, provider?, enabled?,
  routing?}`, every other key dropped), `persistence`, and `raw` (the whole parsed document, used
  only for the `effective_settings.profile` record below). The one field the reference derives
  differently is `name`: the reference's own `name` is whatever the operator typed as
  `--profile <name>`, which — because discovery only ever finds a file called exactly
  `<name>.yml`/`<name>.yaml` — is always that file's own stem. electricity, given a path instead
  of a name, derives `name` the same way: the given file's basename with exactly one trailing
  `.yml` or `.yaml` suffix stripped. For the same physical file, this is the identical string the
  reference would have used had it discovered that file by name — so no conformance
  normalization rule is needed for `name` (or for `content`, below, which is a straight
  re-serialization of the same file's parsed YAML on both sides). `path` itself is never part of
  any state the run writes (confirmed from `runtime_shim.py`'s own two-field profile record,
  below), so there is nothing path-shaped to normalize either.
- **Effect-path and routing-pin validation**, reproduced word-for-word (Circuitry's own
  messages, Decisions §1): an `effects` key naming an unknown effect path, or naming a
  conditional's `if`/a loop's `while` (a *condition*, not an effect — containers only, never
  their condition), raises `_validate_effect_paths`'s/`condition_target_message`'s exact text;
  an `effects.<path>.routing` value naming a band the run's resolved `runtime.complexity.
  routing.bands` (§9.7) doesn't contain raises `validate_profile_routing_pins`'s exact text —
  checked once the run's full effective settings (config + orchestration + profile) are known,
  before anything compiles or dispatches, not mid-run on the one effect that would hit it. A
  profile's per-effect `provider` override is checked against `enabled_adapters` (§11) the same
  as any other adapter resolution. **Not-found/unreadable text is electricity's own**, not
  reproduced from the reference: the reference's own not-found message is built around a
  discovered *name* ("`... Searched: ...`", listing both search directories), which has no
  equivalent once a path is given directly — there is nothing to search, and the path itself is
  operator-supplied rather than assembled from a name. **Schema-failure text, by contrast, is
  shared word-for-word with the reference** (Circuitry's own `"... at {path} failed schema
  validation"` template, Decisions §1) — only the `{path}` segment itself differs, since
  electricity's path is the one the operator gave directly rather than one the reference
  assembled from a discovered name; §12's normalization rules compare that segment structurally
  (a valid path to the same file), not byte-for-byte, the same way every other
  path-dependent field is normalized. Malformed-YAML text already has no reference-side
  equivalent to reproduce for the same reason as not-found: `load_profile` raises its own
  `yaml.YAMLError`-wrapping text keyed to the name it discovered, which a direct path never has.
- **Inputs.** Profile `inputs:` merge into the `input` namespace as the lowest-priority layer:
  any `-e`/`--state`-sourced value already in `input` wins outright, and — if persistence
  auto-load (§6.8) hydrates this run's starting state from a prior snapshot — that snapshot's own
  `input` values also win, with a profile's inputs filling only the keys still missing
  afterward. A profile's own `inputs:` never themselves suppress persistence auto-load; only
  `-e`, `--state`, or an explicit `initial_state` (library) do (§6.8) — an empty-handed run with
  only `--profile` given still auto-loads.
- **Run-level model/adapter/out precedence.** The reference's own chain is
  `cli > profile > orchestration > config > default` for `model`/`adapter`, and
  `cli > profile > resume-default-out > default` for `out`. electricity has no `--model`,
  `--adapter`, `--scoring`, `--routing`, or `--decompose` CLI flags to rank above a profile
  (Decisions, §1), so its chain is the remainder: for `model`/`adapter`, **profile > orchestration
  default > config default**; for `out`, **profile > resume-default (§6.8) > default (nothing
  written)**. A profile's `model`/`adapter`/`out`, when set, therefore wins outright over the
  orchestration's own top-level `model:`/`adapter:` — exactly as it does for the reference once
  its own absent CLI overrides are factored out — and `runtime.effective_settings.sources`
  (§6.10) records `"profile"` for whichever of `model`/`adapter`/`out`/`persistence` a profile
  actually supplied, matching `resolve_effective_settings`'s own four `sources[...] = "profile"`
  assignments exactly.
- **A profile's `persistence:` block**, when present, **replaces** `runtime.persistence`
  outright rather than merging with whatever the orchestration or config already set — the two
  backends take disjoint config keys, so a partial overlay would produce one that is neither.
  `enabled` defaults to `true` on a profile-supplied block (naming a backend in a profile is
  itself the opt-in); `runtime.effective_settings.sources.persistence` (§6.10) becomes
  `"profile"`. This changes persistence auto-load (§6.8) and snapshot saving for the run exactly
  as it changes `runtime.persistence` itself — there is no separate persistence-specific CLI
  flag to rank above it.
- **Per-effect overrides.** Applied once, after the document's own structural checks and cycle
  detection and before any adapter/model is resolved — onto the compiled IR (§5) rather than onto
  an in-memory tree of definitions, since that's what electricity has at this point, but the
  same four keys and the same effect: a `model`/`provider` override replaces that one `Op`'s
  resolved model/provider outright, ranking above the router (§9.7's updated precedence chain,
  below); `enabled: false` compiles the targeted subtree to a disabled/skip node — the only way
  either engine ever disables an effect, since `disabled:` is not a document key in either one;
  `routing: <band-name>` pins that one effect's routing-band lookup, already validated above;
  `routing: false` is the per-effect opt-out instead — it suppresses the router for that one
  effect without naming a band, leaving its model resolution to whatever the chain above it
  already decided. Targeting a container disables its entire subtree, matching the reference
  exactly.
- **The `effective_settings.profile` record** — written only when `--profile` resolved
  successfully, omitted otherwise, exactly reproducing `runtime_shim.py`'s own two-field shape
  (around its `state["runtime"]["effective_settings"]["profile"] = {...}` assignment):
  `{"name": <the derived name, above>, "content": <the profile's whole raw parsed document,
  redacted (§8.2) the same way every other config-sourced state field is>}`. Nothing else —
  no `path`, no resolved `effects`/`inputs` — is part of this record on either engine.
- **Not implemented**: the reference's own `--profile-from-state` (reconstructing a
  `ProfileSettings` from a *previous* run's recorded `effective_settings.profile` content,
  `profile_from_record`, for a resume that no longer has the original file) has no electricity
  equivalent. A resumed run that wants its profile reapplied passes `--profile <path>` again,
  consistent with every other explicit-value resume rule (§6.8) — this is a smaller CLI surface,
  not a parity gap in the record a run writes.

---

## 7. Templates and CEL

### 7.1 Templates — every render site

Every string-bearing field below renders against the current `ctx` (root state plus whatever
scope overlay applies, §6.3) through `electricity-template` (runtime-semantics §3.4):

prompt `template`; every `messages[].content`; tool `prompt`; every string leaf of tool `params`
recursively (except a `{from: ...}` leaf, which is a typed reference, never rendered — §7.1.1);
tool `params_json` — rendered as **one** whole-string template through the `JsonAwareCtx`
wrapper (§3.3), then parsed as JSON and deep-merged onto the rendered `params` (`params_json`
keys win, nested dicts merge recursively); `use.inline` — rendered **once**, against the
*parent's* context, before the result is even parsed as YAML (Quirk Q3, §5.3); every string leaf
of `use.inputs` that is not a `{from:...}` reference; `if.template`/`while.template` (model-mode
conditions); `expect.template` (model-mode expect); asset `ref` (image paths/URLs).

**7.1.1 `{from: path}` references.** A leaf exactly `{"from": "<path>"}` (optionally with
`default:`) is a by-reference value: walked via the same path-resolution logic as a `use` input,
deep-copied, typed, **never** rendered as a template. Unresolvable without a `default:` raises
(a tool-specific message for tool params, a more specific "required input resolved to nothing"
message when the target is itself a required `interface.inputs` entry on a `use` child).

### 7.2 CEL

`electricity-cel` wraps the `cel` crate (chosen over `cel-core` for its larger user base and
active maintenance — rust-ecosystem.md item 15 — with `cel-core` kept as a documented fallback
if the differential corpus, §12, finds a gap `cel` cannot close) behind Circuitry's own
strict/non-strict layer, which is **not** generic CEL behavior and must be implemented
regardless of which underlying crate is used:

- **Type mapping** (runtime-semantics §4.2): `None→null`, `bool/int/float/str/bytes` map
  directly, `dict`→`MapType`, `list/tuple/set`→`ListType`, `datetime`→`TimestampType`,
  `timedelta`→`DurationType`; anything with no CEL counterpart maps to `null` — never the raw
  value, which is the sandbox boundary (no expression can reach an attribute/method/class
  through state).
- **Heterogeneous equality** (runtime-semantics §4.3): cross-type `==` between non-numeric types
  is `False`, never an error; `int`/`uint`/`double` are one numeric family and compare across
  subtype (`1.0 == 1` → `True`). This is the opposite of Rust's/Python's native `==` and must be
  implemented as custom `_==_`/`_!=_` overrides regardless of what the underlying crate's
  default equality does.
- **The absent-path convention** (runtime-semantics §4.4): reading an unset `state.` path is
  **not** an evaluation error inside `if`/`while` CEL — it is decided *structurally*, by walking
  the parse tree for every dotted `state.` read before evaluating, and makes the whole expression
  `False` if any such path is unresolved (with a warning naming the path). Two exemptions: any
  path inside a `has(...)` call anywhere in the expression, and `strict: true` on the definition
  (which turns the same condition back into a raised error).
- **`expect:` gets none of the above exemption** (Quirk Q6, runtime-semantics §4.4/§9) —
  `evaluate_cel_expect`'s absent-path behavior is genuinely different from `evaluate_cel`'s: an
  unresolved path on `value`/`meta` raises, which the caller treats as "expectation failed," not
  "vacuously true/false." electricity's CEL layer exposes these as two distinct entry points
  (`evaluate_condition` vs. `evaluate_expect`) specifically so a future change to one can never
  silently leak into the other.
- **Bindings**: `if`/`while` bind one root, `state`; `expect:` binds three — `value`, `meta`
  (this effect's own outcome, unprefixed), and `state` (the full run state).
- Max expression length 4096 chars; an empty/whitespace-only expression raises immediately.

### 7.3 Regex engines

Two engines, chosen for two different jobs (rust-ecosystem.md items 25–26):

- **`regex`** (RE2-like, linear-time, no lookaround/backreferences) for CEL's `matches()` — a
  good parity fit, since cel-python 0.5 (the version Circuitry's floor targets) itself uses RE2
  for `matches()`, having removed its earlier `re`-fallback.
- **`fancy-regex`** for the `regex` tool plugin and JSON Schema `pattern` validation — Python's
  `re` exposes lookahead/lookbehind/backreferences/named groups, none of which `regex` supports;
  `fancy-regex` covers most of them, behind a validator that rejects constructs Python itself
  would reject (fancy-regex is looser than Python `re` on variable-length lookbehind).

This is a **real, unclosed parity gap** for the `regex` tool specifically (§8's quirks table,
and §16): a document pattern using a true Python-`re`-only construct that `fancy-regex` also
rejects has no electricity-native path to full parity short of an embedded PCRE/Oniguruma
binding, which v1 does not add.

---

## 8. Native tools, the plugin protocol, and security rules

### 8.1 Native tool plugins (v1)

Rust-native implementations of the tools the owner's production documents use: `shell`, `json`,
`fs`, `awk`, `imagemagick`, `ffmpeg`, `http`, `clock`, `hash`, `env_vars`, `regex`, `uuid`,
`mcp`, `surrealdb`, `email_smtp` (Decisions, §1). Each implements the shared trait mirroring
`ToolPlugin` (tools-adapters-plugins §1.1):

```rust
trait ToolPlugin {
    fn name(&self) -> &str;
    async fn execute(&self, params: Value, timeout_seconds: u32) -> Result<ToolResult, ToolError>;
    fn check(&self) -> CheckResult;   // never panics; a misbehaving check() degrades to ok:false
}

struct ToolResult {
    value: Value,
    raw: Value,              // always a dict; redacted + size-capped before reaching state
    stdout: Option<String>,
    stderr: Option<String>,
    exit_code: Option<i32>,  // a real process exit code only; None for every non-process plugin
    ok: bool,
}
```

Per-tool behavior that must match exactly (tools-adapters-plugins §2, full detail there; the
parity-critical points, cited by section):

| Tool | Parity-critical behavior |
|---|---|
| `shell` (§2.1) | command checked bare-alphanumeric before PATH lookup; `allowed_commands` host pin **intersects**, never unions, with the effect's own list; literal-only enforcement (§8.3) runs before rendering. |
| `json` (§2.2) | `stringify`: `ensure_ascii=False`, `default=str`, `indent` only when set — electricity's `electricity-json` writer is reused here directly. |
| `fs` (§2.3) | no sandbox — parity means matching the *absence* of a jail, not inventing one; `delete` on a missing path is `value: false`, not an error. |
| `awk`/`imagemagick` (§2.4) | binary candidates searched in order (`awk, gawk, mawk`; `magick, convert`); `binary`/`env` only from `runtime.plugins.<name>`, never from params. |
| `ffmpeg` (§2.5) | two metacharacter tiers: `flags`/`input`/`output`/`extra_inputs` reject `|`/`;` too; `filter_complex` does not, since those are legitimate filter-graph syntax there. |
| `http` (§2.6) | a 4xx/5xx is a returned `ok=false` result (never a raised error); a connection failure *is* raised — this distinction drives retry classification; `http_error_excerpt`'s four-tier extraction (`error.message` → `error` → `message`/`detail` → redacted raw body) reproduced exactly. |
| `clock` (§2.7) | IANA timezone database required (`chrono-tz` or `tz`), not a bare UTC-offset model. |
| `hash`/`uuid`/`env_vars`/`regex` (§2.8–2.11) | `env_vars`'s own `_SECRET_PATTERN` is narrower than and independent of the central redaction deny-list — both must exist, not one standing in for the other. |
| `mcp` (§2.12) | `rmcp` 3.x client; one fresh connection per call, no session pooling, matching the reference's own choice; `parse: auto`'s structuredContent-then-JSON-then-text precedence, with a bare JSON scalar staying text. |
| `surrealdb` (§2.13) | own HTTP client (not the official `surrealdb` crate — BSL 1.1, incompatible with an MIT binary, rust-ecosystem.md item 28); a configured `url` using the `ws://`/`wss://` scheme (the reference's own default, and the scheme the `runtime.plugins.surrealdb`/`runtime.runtime_plugins.surrealdb` config both default to) is normalized to the equivalent `http://`/`https://` origin before electricity's client ever opens a connection, since electricity talks only SurrealDB's HTTP/`/sql` surface (§10.1) — never a real WebSocket. credentials **never** read from the config block, only from env vars — a stronger rule than the general redaction deny-list. |
| `email_smtp` (§2.14) | `lettre`; `value` is the accepted-recipient count, not a boolean. |

**`_as_bool` coercion** (tools-adapters-plugins §1.6) is implemented once, in
`electricity-redaction` or a shared `electricity-tools::coerce` module, and reused by every
boolean tool param: `None`→default, a real bool passes through, a string is false iff
(case-insensitively, trimmed) `"false"|"0"|"no"|"off"|"n"|""`, true otherwise. This exists
specifically because a templated `false` renders as the *string* `"False"`, and naive string
truthiness would get it backwards.

### 8.2 Redaction

One shared deny-list module (`electricity-redaction`), applied identically everywhere a tool's
`raw`/`params_rendered` or an adapter's wire traffic reaches state (tools-adapters-plugins
§1.5.1):
- a dict key whose final dotted/snake/kebab segment matches a fixed sensitive-key regex
  (`api_key`, `password`, `token`, `authorization`, `cookie`, ... ) → value replaced wholesale;
- a bare JWT-shaped string (`eyJ...\...\...`) → redacted regardless of key;
- key-prefix patterns (`sk-`, `xox[abposr]-`, `ghp_`, a bare `Bearer <16+ chars>`) → redacted;
- a string containing `://` and `@` → userinfo stripped (`scheme://***REDACTED***@host`);
- recurses through dicts/lists/tuples; every other scalar passes through unchanged.

This is a deny-list, explicitly not a secrets-management guarantee — electricity copies that
framing rather than overselling it. `raw` is additionally capped at 64 KiB after redaction,
replaced with a `{"_truncated": true, "_original_bytes", "_preview"}` marker over the limit
(tools-adapters-plugins §1.5).

### 8.3 The literal-only security rule for `allowed_commands`

Enforced **twice**, at two different points, both reproduced (not just one — Quirk Q5):
compile time (`_check_security_sensitive_param_leaf`-equivalent, §4 step 4) and dispatch time
(`_reject_templated_security_params`-equivalent, before any template rendering happens) — a
literal list of strings is the only legal form; a `{{` substring in any item, a `{from:...}`
reference as the whole value or any item, or a non-string item all raise. A rendered
`params_json` overlay is **never** allowed to set this key at all, checked as a third, separate
rule. This boundary exists so nothing computed at runtime — not state, not a model reply, not a
caller input — can widen what a `shell` effect is allowed to run; the host's own
`runtime.plugins.shell.allowed_commands` pin additionally **intersects** with (never unions
into) an effect's own list.

### 8.4 Host pin and allowlists

`runtime.plugins.shell.allowed_commands` is a host ceiling: effective allowlist is
`effect_list ∩ pinned_list`, never their union (tools-adapters-plugins §2.1). `shell`'s own
**default** allowlist, applied when an effect sets no `allowed_commands` of its own, is the fixed
set `ls, cat, head, tail, wc, echo, pwd, date` — not an empty set and not "every command on
PATH" — reproduced exactly rather than left to whatever feels like a reasonable default.
`enabled_tools` (config, §11) is `None`=default-open / `[]`=locked-out / a populated
list=allow-only, checked case-insensitively, resolved **once per run** and enforced before the
first attempt of every tool dispatch — not per retry, so a blocked tool never even starts its
backoff schedule (tools-adapters-plugins §1.8).

**Timeout defaults.** A tool's own `timeout_seconds` param, if unset, falls back to
`runtime.tools.timeout_seconds` (config), else a fixed default of **300 seconds** — both values
reproduced exactly, not treated as an implementation-chosen constant. A prompt's own retry count,
when the effect sets no `retries:`, falls back to `runtime.default_prompt_retries` (config), else
`1` (no retry) — there is no equivalent config fallback for a tool's own retry count (tools have
no `runtime.default_tool_retries`; an unset `retries:` on a tool effect is always exactly 1
attempt, runtime-semantics §5.2).

### 8.5 The external plugin protocol

NDJSON JSON-RPC 2.0 over stdio, MCP/LSP-precedented (tools-adapters-plugins §6, rust-ecosystem.md
item 34), used for `python_eval` and as a fallback for any tool plugin not yet ported to Rust.

**Process model: one process per distinct plugin *name* used in the run, not one process shared
across every plugin, and not one process per call.** An earlier draft of this design chose a
single long-lived process serving every bridged plugin for the whole run, which both contradicts
the project's own carried-forward research note ("one process per plugin, scrubbed env, own
process group, rlimits") and creates exactly the confused-deputy window tools-adapters-plugins
§6.5 item 4 warns about: a bug in one plugin's handling (a stray module-level global, an open
file handle) could leak into a different plugin's call in the same process. Per-plugin-name
processes close that window — a `python_eval` call and a bridged-fallback `wikipedia` call (say)
never share a process or its config slice — while still avoiding a fresh Python interpreter
startup on every single call, since each plugin's own process stays alive and is reused across
repeated calls to *that* plugin within the run. Each such process:
- runs with a **scrubbed environment** — a fixed base allowlist (`PATH`, `HOME`, `LANG`/`LC_*`,
  `TMPDIR`, `VIRTUAL_ENV`, every `PYTHON*`-prefixed variable) plus whatever `runtime.plugins.
  <name>.env` names explicitly for that one plugin — never a full inherited copy of the host's
  environment. This is **stricter than the reference**, which runs every plugin in-process and
  therefore with the host's full environment already available to it; the difference is recorded
  as a deliberate hardening divergence in §13, not a parity gap, since nothing a documented
  plugin legitimately needs is unreachable — it's one `runtime.plugins.<name>.env` entry away;
- runs in its **own process group**, so a SIGTERM/SIGKILL sent to the group reaches any
  grandchild the plugin itself spawned, not just the immediate Python process;
- has host-side **rlimits** applied at spawn (CPU time, address space) as a second line of
  defense around whatever limits the plugin's own sandboxing (e.g. `python_eval`'s
  RestrictedPython + forked-child + rlimit discipline) already applies internally;
- communicates over a **dedicated pipe pair** created for JSON-RPC traffic specifically — distinct
  from the process's inherited stdout, which the bridge's bootstrap redirects to stderr/a log file
  before any plugin code runs. No Python-side code may ever write to stdout except through the
  dedicated JSON-RPC pipe; a stray `print()` from a third-party plugin would otherwise corrupt
  line-delimited framing for every subsequent message on that plugin's process.

**Methods** — the project's own `describe`/`invoke`/`progress`/`cancel` set (settled 2026-10-06,
§16), not an electricity-invented naming:
`plugin/describe` (request: `{}` → `{name, version, tool_schemas}` plus an `ok`/`error` health
verdict — called once when the process starts, and re-callable at any later point the host wants
to re-verify the plugin is still alive, which is what covers the earlier, separate `check` round
trip an earlier draft of this section specified); `plugin/invoke` (request: `{plugin, params,
timeout_seconds, config}` → the `ToolResult` shape on success, a JSON-RPC **error object** — not
an `ok:false` result — on a raised exception, since the two map to different retry-classification
paths on the Rust side); `plugin/progress` (notification, Python→Rust, best-effort, during a
long-running `invoke` — no v1 plugin actually emits one yet, since `python_eval`'s own calls are
short-lived, but the method exists so a future bridged plugin that runs longer doesn't need a
protocol change); `plugin/cancel` (notification, Rust→Python, best-effort, maps onto
`python_eval`'s existing forked-child-kill path). Python-side logging is **not** a fifth RPC
method — it goes out over the plugin process's own stderr stream as plain structured log lines,
which the bridge reads directly into the host's log/OTel pipeline; this is also why the dedicated
JSON-RPC pipe pair below is stdout-only, never stderr.

**Host-side hard kill.** The Python side enforces `timeout_seconds` internally exactly as
`python_eval` already does (forked child + pipe + poll-with-deadline); independently, the Rust
host enforces its own hard kill of the whole bridge **call** at `timeout_seconds + grace` (a
fixed grace period, not configurable per v1) by sending the per-plugin process's process group a
SIGTERM followed by SIGKILL if it hasn't exited — so a bug in the Python side's own timeout logic
can never hang the Rust host indefinitely, the same belt-and-suspenders the reference
implementation already applies inside `python_eval` itself one level up.

**Config slicing**: only `runtime.plugins.<plugin>` (and, for `mcp`, only the specific servers a
call names) ever crosses the boundary — never the full config tree, never another plugin's
credentials; now additionally enforced by process separation itself, not just by what's placed on
the wire.

**Rendering and security checks happen on the Rust side, before the message is sent**: the Python
process never sees an unrendered template or a `{from:}` marker, and allowlist/capability checks
are never re-validated from the Python side's response — a bridged plugin cannot widen its own
permissions by lying in a reply.

**The Python side of the bridge is a change to Circuitry, not to electricity**: it imports
`circuitry.plugins.factory` directly and calls the existing `build_plugin()` (tools-adapters-
plugins §1.9) under a thin JSON-RPC server wrapper, so every registered Python plugin is
bridgeable without new Python authoring — not just `python_eval`. This server module lives in
Circuitry's Python package (`src/circuitry`), while electricity's crates own only the Rust client
half.

**No additional sandbox beyond what the Python plugin already provides.** `python_eval`'s own
RestrictedPython + forked-child + rlimit sandboxing is what bounds that one plugin; the bridge
adds process isolation (a crash doesn't take the Rust host down) and a kill switch, not a
security boundary of its own, and must not be described to users as one.

---

## 9. Model adapters

### 9.1 Interface

```rust
trait Adapter {
    fn name(&self) -> &str;
    async fn generate(&self, model: &str, options: GenerateOptions, timeout_seconds: u32)
        -> Result<GenerateResult, AdapterError>;
    fn check(&self) -> CheckResult;
    fn list_models(&self) -> Vec<String> { Vec::new() }   // best-effort; never raises
}

struct GenerateResult {
    text: String,
    raw: Value,
    tokens_sent: Option<u32>,
    tokens_received: Option<u32>,
    finish_reason: Option<String>,   // unnormalized ("stop", "length", "end_turn", ...)
    warnings: Vec<String>,
}
```

`GenerateOptions` carries the portable knobs every adapter maps to its own wire shape:
`temperature`, `max_tokens`, `stop`, a `params: Map` passthrough for every other prompt-effect
param (spread into the provider's own "extra options" slot last, so it can never override the
portable fields), `deterministic: bool` (→ `temperature=0` plus a seed, only for adapters whose
provider actually accepts one), `messages`, and `images` (tools-adapters-plugins §3.3).

### 9.2 The OpenAI-compatible family

One shared transport crate module covers every provider speaking `POST /chat/completions` with
Bearer auth and the `choices[0].message.content`/`usage.{prompt,completion}_tokens` shape
(tools-adapters-plugins §3.4) — this single adapter implementation, parameterized by
`base_url`/`api_key_env`/`default_model`/`chat_completions_path`, covers Ollama and essentially
every other OpenAI-shaped provider a document might name, which is what makes it the right unit
of native-adapter work rather than one adapter per provider name. Per-provider parameterization
(env var name, default base URL, special-cased header shape for `azure-openai`'s `api-key`
header) is config data, not separate Rust code. Request/response mapping detail — `role: "tool"`
turns relabeled to a user turn, images appended as `image_url` content parts after any text on
the last user turn, `max_completion_tokens` instead of `max_tokens` for `openai`'s own reasoning
models — is reproduced exactly per tools-adapters-plugins §3.4/§3.6/§3.7.

**Ollama via this transport is a named parity risk, not a design error.** The Decisions above
route Ollama through the OpenAI-compatible family rather than a native `ollama` adapter speaking
its own `/api/chat`/`/api/generate` surface; the reference's own `ollama` adapter uses that native
surface and supports an `options` sub-object plus request-level `format`/`keep_alive`/`think`
fields the OpenAI-compatible shape has no slot for (tools-adapters-plugins §3.7). `ollama` is
also the reference's own `default_adapter` when nothing else is configured — not a minor
provider. Any document setting one of those three fields, or relying on the native adapter's own
2-second `--head` reachability probe in `check()`, diverges under electricity. Carried to §13 as
an explicit parity-risk row rather than left implicit.

### 9.3 `anthropic`

A separate adapter (not built on the OpenAI-compatible transport — the wire shape genuinely
differs): `x-api-key` header, not `Authorization: Bearer`; system turns joined into a separate
`system` field, not a `messages` turn; images placed **before** text on the last user turn
(the opposite order from the OpenAI family); `ANTHROPIC_API_KEY` env var
(tools-adapters-plugins §3.5).

### 9.4 The `cyberdiner` adapter

A third adapter is required for v1 beyond OpenAI-compatible and Anthropic, because one of the
production document sets this runtime targets routes certain prompts through it. The adapter
name `cyberdiner` is settled (2026-10-06) and may appear here because it is already public in
Circuitry's own source; this section states the adapter's wire protocol at the shape level and
defers its exact endpoint paths, request/response field names, and config key names to
tools-adapters-plugins §3.8, which already cites all of them from Circuitry's own public source
— this section has no separate reason of its own to withhold them. electricity's implementation
must match the reference adapter's exact wire behavior byte-for-byte (verified by the
conformance suite, §12) — the description below is the *shape* of that protocol,
cross-referenced to §3.8 for the literal field names.

- **What it is**: a job-queue broker, not a direct completion API. `generate()` hides a
  submit-then-poll loop: a `POST` submits prompt/tier data and returns a job id; a `GET` on that
  job id is then polled at a configured interval until a terminal status, folding several status
  spellings onto the same outcomes — the exact endpoint paths and request/response field names
  are tools-adapters-plugins §3.8's detail, cited there from Circuitry's own public source, not
  repeated here; electricity's implementation uses them exactly, since this is wire behavior
  electricity is required to match byte-for-byte, not a protocol electricity designs itself.
- **Config** (`runtime.adapters.cyberdiner`): a required broker base URL, a required bearer token
  (read from config directly, matching the reference's own `CyberdinerAdapter` — there is no
  env-var form for this token in `cof` at all, which is why §9.6/§14 record this field as a named
  exception to "provider API keys from environment variables only"), a default tier name, an optional client-side tier allowlist (empty = pass-through, server
  validates), a poll interval in milliseconds, a per-HTTP-request timeout distinct from
  `generate()`'s own overall timeout (which bounds the whole submit+poll sequence), and an
  optional `max_in_flight` cap (0 = unbounded) enforced by a semaphore shared across the adapter
  instance — load-bearing specifically for a `flow: tree` loop that could otherwise submit more
  jobs than the broker's claim window can absorb, driving excess jobs into a server-side
  timeout.
- **Model-to-tier mapping**: the orchestration's `model:` field is reinterpreted entirely as a
  tier name, not a model string.
- **Terminal statuses**, normalized case/punctuation-insensitively (folding e.g. `TimedOut`/
  `timed_out`/`timedOut` onto one key): success (two accepted spellings, since the client and the
  server's own write path differ historically), a server-declared dead-job timeout (distinct from
  a client-side timeout), failure, cancellation.
- **Token accounting**: tokens-sent is always unknown (`None`); tokens-received carries the
  broker's own single combined counter as an approximation (it includes the prompt's tokens too).
- This adapter is implemented **natively** in Rust, not deferred to the plugin bridge — the
  protocol (submit/poll, tier mapping, status folding, semaphore backpressure) is small, and the
  bridge is scoped to *tool* plugins (Decisions, §1), not a generic adapter bridge.

### 9.5 `host_claude`: registered, never buildable

`host_claude` has no transport of its own in the reference implementation — it calls an injected
callback that only exists inside a live MCP-driven session, which is out of scope (§1). The
adapter name is registered in electricity's adapter registry solely so an orchestration naming
it produces the same clear "cannot be built from config" error the reference gives, rather than
a generic "unknown adapter" message (tools-adapters-plugins §3.9).

### 9.6 Retry classification, `Retry-After`, timeouts

**There is no single retryable-failure predicate shared uniformly by prompt, tool, and `use`** —
an earlier draft of this section stated one, which is wrong for most tool effects. The actual
rule set, reproduced exactly (tools-adapters-plugins §1.4/§3.10, runtime-semantics §5.1/§5.2/§6.1):

- **HTTP-status-classified retry** applies to every prompt/adapter call, and separately to the
  four HTTP-family tool providers (`http`, `web_fetch`, `webhook`, `linear`): HTTP 429, 408, or
  any 5xx is retryable; every other 4xx (400/401/403/404/422) is not; a connection failure with
  no HTTP response at all ("never got a reply") is retryable with no status attached. This is the
  **one** shared `classify_exception`-equivalent function, and it covers only these two leaf
  kinds, not every tool.
- **Every other tool provider** (`shell`, `ffmpeg`, `awk`, `imagemagick`, `mcp` tool calls, a
  bridged plugin, ...) has no HTTP status to classify by, so **any** failure on such a provider
  is retryable — "a process failing once (a transient GPU watchdog kill) is exactly the
  motivating case" (tools-adapters-plugins §1.4). A port that reuses the HTTP-status predicate
  for a `shell`/`ffmpeg` failure would never retry it at all, since a non-HTTP failure carries no
  status for that predicate to recognize.
- **A prompt's decode/schema-validation failure** (`AnswerParseError`/`SchemaValidationError`) is
  **always** retryable at the pass level, regardless of what HTTP status (if any) the underlying
  call returned — but it is tried first within the **fallback chain itself** (the next
  `provider_fallbacks` entry, without consuming a retry attempt) before the outer per-pass
  retry/backoff loop ever engages (Quirk Q10, runtime-semantics §5.1).
- **A tool's `expect:` check failing or being unreadable** is **always** retried — it carries no
  status classification of its own (it isn't an HTTP response), and a false/unreadable
  expectation always counts as a retryable attempt failure (runtime-semantics §5.2).
- **A `use` effect's config-shaped failure** (a cycle, a bad `path`/`ref`, a bad interface input)
  is **never** retried, regardless of `retries:` or `on_error` — retrying would just reproduce the
  same structural error (runtime-semantics §6.1).

A provider's `Retry-After` response header, when present, **overrides** the computed
exponential-backoff delay outright, still capped at 60s. Backoff without a header is full-jitter
exponential. Per-adapter timeouts (`runtime.adapters.<name>.timeout_seconds`) are independent of
a tool's own timeout budget — a tool on the no-op adapter must never inherit a model's socket
budget (tools-adapters-plugins §1.3) — and a prompt effect's own `timeout_ms` can only
**shorten** the adapter's configured timeout for that attempt's specific adapter, never lengthen
it, since a `provider:`/fallback can dispatch to a different adapter than the run default.

**Provider API keys come from environment variables only** (Decisions, §1) — never from
config.json, never from state, never from a CLI flag — matching every adapter's own
`*_API_KEY`/`*_TOKEN` env-var convention in the reference implementation, **with one named
exception, settled rather than open**: the `cyberdiner` adapter's bearer token (§9.4) is read
from `runtime.adapters.cyberdiner.token` in config, matching the reference's own
`CyberdinerAdapter` exactly — `cof` has no env-var form for this token at all. This is a
deliberate, settled divergence from the blanket provider-API-key rule above, not a silent
contradiction and not something electricity should "fix" by inventing an env-var path the
reference itself doesn't have; §14 states the rule this exception actually falls under.

### 9.7 Complexity scoring, routing, and decomposition

An earlier draft of this design deferred all three to a single citation in M2-F; they belong in
M1 instead (§15), because one of the two first production document sets this runtime targets
turns on complexity scoring and routing (via its own config, not an orchestration-level
`runtime:` override — §11) and reads `meta.complexity.{score,band}` out of the final state for
documents using `provider: ollama` with no explicit `model:` — skipping this feature does not
just skip a capability, it changes every byte of `meta` on any prompt effect that document set
scores. (The other first-milestone document set has no complexity config at all; M1 still gates
on scoring/routing because the first set needs it, not because both do — an earlier draft of
this paragraph implied both did.)

**All three default to `enabled: false`** and are gated: `routing`/`decomposition` each require
`scoring` to also be on (config resolution rejects any other combination). A run turns them on
via `runtime.complexity.{scoring,routing,decomposition}.enabled: true` in config or an
orchestration's own `runtime:` block (the `complexity` key, one of the two an orchestration's
`runtime:` block may set without being otherwise trusted — §11).

- **Scoring** (`electricity-compiler`'s prompt-compile step computes the *shape*; `electricity-vm`
  computes the actual score at dispatch time, since it depends on the fully-rendered prompt text):
  a weighted-mean score over seven named signals (`prompt_size`, `state_references`,
  `prompt_type`, `output_schema`, `output_size`, `structural_position`, `keywords`), each
  normalized to `[0, 1]` and combined as `MAX_SCORE * Σ(weight_i × normalized_i) / Σ(weight_i)`
  with `MAX_SCORE = 100.0` — matching `core/complexity.py` exactly, not a one-line "weighted
  score" sketch, which includes **where** the rounding happens: every weight, every normalized
  value, every per-signal contribution, the weight total, and the final total are each
  independently rounded to 6 decimal places using Python's own `round(x, 6)` (banker's rounding,
  not truncation), not just the final score once at the end — electricity's scorer must round at
  every one of those same intermediate steps, in the same order, and must reproduce `round()`'s
  banker's-rounding behavior exactly (e.g. via a decimal- or scaled-integer-based
  round-half-to-even implementation), not an `(x * 1e6).round() / 1e6` float-multiply shortcut,
  which disagrees with Python's `round()` on tie cases and on values floating-point
  representation error nudges differently. The default weights (`prompt_size: 0.28,
  state_references: 0.18, prompt_type: 0.14, output_schema: 0.16, output_size: 0.10,
  structural_position: 0.08, keywords: 0.06`) and the ~20-phrase keyword-weight table
  (whole-word, case-insensitive match; matched weights sum and clamp to 1.0) are ported
  verbatim, byte-for-byte including every phrase and weight, not re-derived from the general
  idea of "keyword matching" — both tables are a runtime-overridable config shape
  (`runtime.complexity.scoring.weights`/`keyword_weights`) as well as a compiled-in default.
  Size-based signals use the reference's own saturating curve `v / (v + k)` (bounded, monotone,
  never exceeding 1.0) with its own fixed half-point constant per signal (e.g. 800 tokens for
  `prompt_size`, a flat 4-characters-per-token estimate rather than a real tokenizer, so the
  measurement stays deterministic across models) — these half-points and the schema
  depth/field-count measurement for `output_schema` are ported exactly, not approximated, and are
  added to the differential corpus (§12) alongside CEL/template/regex, since the score itself (not
  just its presence/absence) is observable state once scoring is on. The result is written to
  `meta.complexity` **only when scoring is enabled** — the key is entirely absent, not `null`,
  when off, so toggling the feature must not otherwise change a single byte of state (conformance
  case C26).
- **Routing**: a band table (`runtime.complexity.routing.bands`, each `{name, model, threshold}`,
  with a mandatory catch-all) maps a score to a model. The reference's full precedence chain is
  `--model` (CLI) > per-effect `model:` > profile effect override > profile routing pin > router >
  orchestration default > config default (runtime-semantics §5.8, confirmed against
  `core/router.py`'s own module docstring); electricity has no `--model` CLI flag (Decisions, §1)
  but does support profiles (§6.11), so its chain is the remainder of the reference's own:
  **per-effect `model:` > profile effect override > profile run-level `model:` (locked) >
  profile routing pin > router > orchestration default > config default — by default.**
  A profile's own run-level `model:` (§6.11) doesn't occupy its own link in the reference's own
  chain at all — it is folded into the same boolean the first two layers already collapse into.
  Setting a run-level `model:` (`--model` on the reference, a profile's bare `model:` for
  electricity) flips a single run-wide `model_locked` flag, and that flag — not a position in
  the chain — is what makes *every* scored prompt effect's own `explicit` check true, the same
  `explicit` the router already defers to by default. Concretely: a profile that sets a bare
  `model:` with no per-effect overrides suppresses routing across **every** prompt in the
  document once `routing.respect_explicit` is at its default `true`, not only on the one effect
  that happens to name that model itself — `model_locked` is evaluated once per run, not once
  per effect, so it protects every prompt dispatch the run makes, not just one. Unlike a genuine
  per-effect `model:`, this lock does **not** set `meta.model_reason` to `"explicit"` on the
  effects it protects from the router — the reference reserves `"explicit"` for an effect naming
  its own `model:` specifically — so such an effect still reports `model_reason: "default"` even
  though the router, deferring to the lock, never touched it. `routing.respect_explicit: false`
  still overrides the lock the same way it overrides a per-effect `model:` (below): the lock is
  one more thing that opt-out deliberately overrules, not an exception to it. **A locked run
  ignores a profile's own per-effect `routing` pin/opt-out outright, regardless of
  `respect_explicit`** — the reference settles `routing_override` only `if not explicit`, so
  once the run-level lock makes an effect `explicit`, its own pin or opt-out is never even
  consulted; with `respect_explicit: false` the router then decides that effect by score exactly
  as it would an unpinned one, not by falling through to whatever the (unconsulted) pin named.
  The router is
  **not** merely a fallback that only
  fires when nothing above it in the run-level chain has an opinion — once routing is enabled
  with a non-empty band table, the router's result replaces the *run-level default*
  `sources.model` entry with `"router"` even when an orchestration-level `model:` had already
  resolved one. **The run-level `model` value itself stays whatever the profile/orchestration/
  config default already was** (§6.11's own precedence chain), even once `sources["model"]` says
  `"router"` — the router's real answer is per-effect, and the run-level `model` is only what a
  prompt with no score to route on (or a non-prompt effect) falls back to. **`sources["model"]`
  itself says `"router"` only when the router actually won** — on a locked run with
  `respect_explicit` at its default `true`, the router never wins (above), so `sources["model"]`
  stays whatever the lock itself set (`"profile"` when a profile supplied the model, on both
  engines; the reference's `"cli"` comes only from its `--model` flag, which electricity lacks) and
  never becomes `"router"` at all; it is only the `respect_explicit: false` opt-out — which lets
  the router act despite the lock — that can make a locked run's `sources["model"]` say
  `"router"` in the first place. The **one** case where
  the run-level value itself becomes the band table's catch-all model is a run that configures a
  band table but no profile/orchestration/config default `model` at all — there the catch-all's
  whole job (the model for anything not otherwise matched) makes it the run default by necessity,
  not because the router generally overwrites a profile's or an orchestration's own default (an
  earlier draft of this paragraph had this backwards). An
  explicit *per-effect* `model:` wins over the router **by default**
  (`routing.respect_explicit: true`, matching the reference's own default) — but
  `routing.respect_explicit: false` is a documented, deliberate opt-out that makes the router
  decide for every scored prompt effect, including ones naming their own `model:`; "always wins
  outright" is therefore wrong as a blanket statement (an earlier draft of this paragraph said
  it), and electricity's router must check `respect_explicit` before deferring to a per-effect
  `model:`, not treat deferral as unconditional. When the router acts, `meta.model_reason` becomes
  `"router"` and `meta.complexity.band = {name, model}` records which row; the band is recorded
  even when the router doesn't act (an explicit per-effect model was already named, or
  `respect_explicit` suppressed the router), since `band` alone never implies the router's choice
  was *applied* — `model_reason` is the field that says that.
- **Decomposition**: when a scored prompt's score strictly exceeds `threshold`, the single
  over-complex call is replaced by plan → validate → execute: a bundled planner orchestration
  (a synced copy of Circuitry's own, checked the same way as the shared JSON Schema, §4 step 3,
  since this is Circuitry-authored YAML, not Rust code) runs isolated (decomposition and
  scoring switched off for *this one* inner run, to avoid infinite recursion on its own
  deliberately large planning prompt), emits a YAML plan that must pass schema validation, stay
  within `[2, max_chunks]` fan-out, and honor a merge contract (a top-level effect writing the
  planner-reported `result_path`); the emitted plan then runs as a `use`-child-shaped,
  state-isolated execution (same cycle guard, same namespaced observability as an ordinary `use`)
  and the value at `result_path` is written back as if it were the original prompt's own `value`.
  Recursion is bounded by `max_depth`; at the ceiling the effect "routes up" to the routing
  table's catch-all model (if routing is on) or just runs as-is (if routing is off). Failure
  semantics (`on_failure: route_up` default, or `fail`) are recorded in
  `meta.decomposition.{decomposed, outcome, reason, score, threshold, depth, max_depth, plan?,
  chunk_count?, yaml?, result_path?, fallback_model?, error?}` — present only when actually
  triggered, absent otherwise, the same all-or-nothing-key rule as scoring.
- **The reflector effect's own planning prompt** runs with decomposition/scoring/routing forced
  off for itself specifically (its prompt is deliberately huge and would otherwise trigger the
  very feature it's generating); this is the one documented exception, reproduced exactly.

---

## 10. Runtime plugins for v1

Two subsystems share the word "persistence" in the reference implementation and must stay
distinct in electricity, because only one of them is read by `--resume` (runtime-semantics §8.1,
tools-adapters-plugins §4.1–§4.2):

### 10.1 State-snapshot persistence (`runtime.persistence`) — what `--resume` reads

**SurrealDB is not a state-snapshot backend** — an earlier draft of this design put it here;
the reference implementation only ever accepts `backend: jsonl-file|mongodb|postgres|sqlite` for
`runtime.persistence` (tools-adapters-plugins §4.1). SurrealDB support exists only as an
*observability* runtime plugin (§10.2) and, separately, as a tool plugin (§8.1); neither of those
is ever read by `--resume`. Fixed here, not just noted, since a `backend: surrealdb` value in a
config file would otherwise be accepted by electricity and rejected by `cof` — a config
compatibility break in exactly the direction the Decisions above rule out.

A `PersistenceBackend` trait: `describe()`, `load_latest_state(path)`, `load_run(path, run_id)`,
`save_run_snapshot(path, run_id, ok, error, state)`. Each backend writes the **full
JSON-serialized run state**, not a relational row:

- **`jsonl-file`** — appends one line per `save_run_snapshot` call to a configured path:
  `{run_id, orchestration_path, created_at, ok, error, state: <full state dict>}`.
  `load_latest_state` scans the file **backwards**, returning the first record matching the
  orchestration path with `ok: true`; `load_run` scans **forwards**, matching both `run_id` and
  the orchestration path (never across documents). A malformed line is skipped with a warning,
  not fatal — an append log can be truncated mid-write.
- **`sqlite`**/**`postgres`** (via `sqlx`) — one row per run, `state_json` as a text column
  holding the full serialized state, looked up by `(run_id, orchestration_path)`; postgres
  defaults `sslmode: require`, rejecting plaintext unless `allow_insecure: true` is explicitly
  set.
- **`mongodb`** — not cited by any document in the first milestones' sets; its existence is noted
  for completeness and its spec deferred until a document actually needs it (matching
  tools-adapters-plugins §4.1's own deferral), not designed here.

`--resume <run-id>` requires `runtime.persistence` to be configured on the orchestration; there
is no fallback to the observability-only plugins in §10.2. `document_sha256` guards against
resuming a changed document (§6.8).

### 10.2 Observability runtime plugins (`runtime.runtime_plugins.*`) — never resumed from

A `RuntimePlugin` trait with three required lifecycle hooks (`on_run_start`, and the mutually-
exclusive pair `on_run_success`/`on_run_failure`) and two optional ones (`on_effect_start`,
`on_effect_complete`), matching tools-adapters-plugins §4.3 exactly. A plugin's own failure is
caught and logged, never propagated — one misbehaving plugin must never abort the run or block
another plugin from firing. v1 ships:

- **SQL-family** (sqlite/postgres via `sqlx`) write two relational tables — `runs` and
  `effect_results`, one row per effect, with `effect_type` inferred from the meta shape the same
  heuristic way the reference does (prompt-shaped meta → `"prompt"`, tool-shaped → `"tool"`,
  etc.) and `state_path` decomposed into `(effect_name, parent_path, iteration_index)` rather
  than stored as an opaque string.
- **`surrealdb`** (distinct from the `surrealdb` *tool* plugin, §8.1) — the same `runs`/`effects`
  shape as the SQL tables, but as native SurrealDB objects over electricity's own HTTP client
  (not the official `surrealdb` crate — BSL 1.1, incompatible with an MIT binary,
  rust-ecosystem.md item 28). **Credential precedence is env-var-first, config-fallback-second,
  matching the reference exactly — not "config has no credential fields at all," which an earlier
  draft of this design claimed**: `SURREAL_TOKEN`/`token` (token takes priority over user/pass
  when both are present), `SURREAL_USER`+`SURREAL_PASS`/`user`+`password` — each pair checks the
  env var first, then the matching `runtime.runtime_plugins.surrealdb.*` config key. This is a
  settled, named exception to the *provider*-API-key rule stated in §1's Decisions (§9.6/§14
  state the exact rule this exception falls under, alongside the `cyberdiner` adapter's token,
  §9.4) — not an open contradiction needing a security-posture call. The default
  `url` is `ws://localhost:8000/rpc` (the reference's own default), normalized to its `http(s)://`
  equivalent before electricity's client connects (§8.1); the default `namespace`/`database` are
  both `"circuitry"` — **different defaults from the tool plugin**, which requires an explicit
  namespace. Auth tries signin scopes most-specific-first (`{namespace, database}` →
  `{namespace}` → `{}`/ROOT).
- **`jsonl-file`** (a **third, independent** JSONL writer — not the same file or format as
  §10.1's `jsonl-file` persistence backend; the two can be enabled at the same time) — an
  **event stream**, one line per lifecycle hook firing (`run_start`, `effect_complete`,
  `run_success`, `run_failure`), suited to `grep`/`jq`-style tailing, not resume. An
  `include_effects` config bool (default `true`) drops the per-effect lines when `false`,
  keeping only run-level events.

A `store_raw` cascade, shared by every SQL-family plugin and `surrealdb`, controls whether the
largest/most sensitive payload (full provider responses, full tool `raw`) is persisted at all: an
env var override, then a config bool, then an environment-based default (`true` only when
`environment: "dev"`).

### 10.3 `--resume` by run id

Reads `runtime.persistence.load_run(orchestration_path, run_id)`; combined with §6.8's engine-
level skip logic, this is what makes `--resume <id>` work end-to-end.

### 10.4 OpenTelemetry

OTLP over HTTP (matching the reference's exporter choice, pinned `opentelemetry`/`-sdk`/`-otlp`
0.33.x per rust-ecosystem.md item 30, since the tracing API is still pre-stable upstream). One
root span per run, one child span per effect, parented by nested **state path** (walking dotted
prefixes for the nearest open ancestor span), not call order — so a loop's own span correctly
parents its body's effects even though intermediate pass segments never get their own span.
Span timing comes from the effect's own `meta.created_at`/`completed_at`, not from when the
plugin happens to be invoked. Concurrent branches sharing an unnamed path (an unnamed `tree`
loop/dynamic) are disambiguated by task identity, not a single span slot, since two branches can
report the identical effect path simultaneously (tools-adapters-plugins §4.4). When no OTLP
endpoint is configured, the OTel plugin falls back to a stdout console exporter rather than
silently disabling tracing — matching the reference's own fallback — so a run without observability
infrastructure configured still gets spans visible somewhere.

### 10.5 Live state, loop progress, run totals

- **Live state**: a `Store::on_write`-driven mirror (not a `RuntimePlugin`), writing the full
  current state to a path atomically (temp file + rename, never a predictable sibling path),
  coalesced to at most one write per fixed interval regardless of write volume, with the first
  write always synchronous (so an unwritable path fails the run immediately) and the final write
  always synchronous too (so the mirror ends exactly equal to `--out`).
- **Loop progress**: `meta.progress = {done, total, elapsed_s, eta_s}`, recomputed after every
  pass including a failed one, so ETA never freezes/overestimates under `on_error: continue`.
  `total` is `None` for an uncapped `while`; for `each` it is always the uncapped collection
  length, even under `truncate: true`.
- **Run totals**: `runtime.last_run.totals = {wall_time_s, effects_run, tokens_sent,
  tokens_received, cost_usd}`, computed **live** from the same effect-complete event stream
  `--live-state` consumes — never by walking the finished state tree after the fact, since a
  `use` child in declared-outputs mode never leaves its effects in the final tree, and an unnamed
  loop overwrites the same node every pass; only the live event stream sees every leaf that
  actually ran. `cost_usd` sums each prompt effect's own `meta.cost_usd` — a field no adapter in
  §9 is specified to actually populate (none of the OpenAI-compatible family, `anthropic`, or
  `cyberdiner` has a documented per-call cost figure in its response shape); until an adapter
  computes it from a price table, `cost_usd` **stays `null`, never `0`** (§6.10's correction —
  the reference's own accumulator starts at `None` and only becomes a number once something adds
  to it), and this gap is carried to §16 rather than silently assumed solved by §9.6's retry
  classification work.

---

## 11. Configuration

Explicit config file only — no discovery, no `cof trust` (Decisions, §1). Both the config file
and the orchestration file are operator-chosen and therefore trusted; electricity never
evaluates any notion of document trust at run time.

**Keys that change run behavior** (runtime-semantics §5, condensed):

| Key | Effect |
|---|---|
| `default_model` / `default_adapter` | Used when a prompt effect sets neither `model:` nor `provider:`. |
| `enabled_adapters` / `enabled_plugins` / `enabled_tools` | Allowlists; `None` = default-open, `[]` = locked out, a populated list = allow-only. Adapter/tool matching is case-insensitive; runtime-plugin matching is exact-id, case-sensitive. |
| `environment` | `"dev"` / `"prod"` / `"test"`; drives the `store_raw` default for every observability-persistence plugin (`true` only in `dev`); an invalid value falls back to `"dev"` with a warning. |
| `plugins` | Ids of **runtime plugins** to load — distinct from tool plugins, which are referenced directly by `provider:` and need no such list. The reference's own `plugins:` list holds Python dotted module paths or `module:attr` pairs (`core/runtime_plugins.py`'s `importlib`-based loader) — there is no short-id form in the reference itself. electricity therefore accepts the reference's **own bundled dotted ids** (e.g. `circuitry.runtime_plugins.sqlite`) and maps each one to its native Rust equivalent (§10), so a config file written for `cof` loads unchanged. A dotted id electricity doesn't recognize is **not** a hard config error: it fails exactly the way an unimportable module does in the reference — a single `{"plugin": id, "hook": "load", "ok": false, "error": "..."}` entry in `runtime.plugins.events` (§6.10), the plugin simply absent from `loaded`, and the run continues. |
| `runtime.adapters.<name>` | Per-adapter config: `base_url`, `default_model`, `timeout_seconds`, provider-specific keys (the vendor adapter's broker URL/token/tier settings, §9.4). |
| `runtime.plugins.<name>` | Per-**tool**-plugin config: `binary`/`env` overrides, `shell`'s `allowed_commands` host pin, `surrealdb`'s `url`/`namespace`/`database`, `mcp`'s `servers`. |
| `runtime.runtime_plugins.<name>` | Per-**runtime**-plugin config: `db_path`/`dsn`/`url`/`store_raw`/`path`/`include_effects`. |
| `runtime.persistence` | State-snapshot backend selection for `--resume`: `enabled`, `backend`, backend-specific keys. |
| `runtime.max_concurrency` / `runtime.concurrency_groups` | The run-wide limiter (§6.4). |

Environment-variable overlays win over all of the above when set: per-adapter API key vars, the
SurrealDB credential vars, `CIRCUITRY_<NAME>_STORE_RAW`, persistence path/DSN vars, and a handful
of `CIRCUITRY_*` overrides for model/adapter/allowlist/environment selection (tools-adapters-
plugins §5's env-var overlay table, cross-referenced against runtime-semantics §8.6's
config-keys-that-affect-a-run list), reproduced with the same names for config-file compatibility
with existing deployments.

**A document's own `runtime:` block.** The reference restricts an orchestration's own `runtime:`
block to `ORCHESTRATION_RUNTIME_KEYS = {"complexity", "state"}` unless the whole document is
trusted (run by path, not a bare library name) — every other `runtime.<key>` an untrusted
document sets is dropped with a warning. electricity has no untrusted-document case at all (both
files are operator-chosen and trusted, §1), so this restriction collapses to "always trusted":
electricity honors **every** `runtime.<key>` a document sets, not only `complexity`/`state` (§4
step 4's `use`-note, runtime-semantics §8.6). In practice, every production document inspected so
far that sets a `runtime:` block at all sets exactly `runtime.state.record_children: true`; the
second key the reference's own allowlist names, `complexity`, is what a document uses to turn on
scoring/routing/decomposition for itself (§9.7) without needing a config-file change.

electricity's config-merge layer accepts a document-level `runtime:` block and merges it onto the
config file's own `runtime:` tree **one top-level key at a time**, matching the reference's own
`_merge_runtime`: `plugins` and `adapters` deep-merge recursively (a document setting
`plugins.sqlite.*` must not drop the config's own `plugins.shell.*`; a document setting
`adapters.ollama.*` must not drop another adapter's config), while every other top-level key
(`complexity`, `persistence`, `state`, ...) is **replaced wholesale** by the document's own value
when it sets one — not merged key-by-key within that block. The one exception to "whichever side
wins, wins outright": `runtime.plugins.shell.allowed_commands` is re-intersected against the
host's own pin **after** the merge regardless of which side supplied it, so a document can only
ever narrow that one list, never replace or widen it (§8.3, §8.4).

**`-e key=value` parsing.** Each `-e` value is JSON-sniffed first (`json.loads`), falling back to
the literal string on any decode error — so `-e start=06` (leading zero, invalid JSON) becomes
the string `"06"`, while `-e x=1.50` becomes the float `1.5`, silently dropping the trailing
zero. When the document's own `interface.inputs.<key>.type` is declared `string`, electricity
substitutes the original, unparsed `-e` text back in for that key after sniffing — best-effort,
never blocking the run if the document can't be peeked for some reason — so a declared-string
input doesn't lose its exact original text to JSON's own numeric coercion. Parsed `-e` values
land under `input.` as ordinary state before `interface.inputs` type/required checking runs; that
check (shared by every run, not just `-e`-seeded ones) fills in declared `default:` values and
coerces an already-parsed value to its declared type.

**Legacy state migration.** A loaded state (`--state`, persistence auto-load, §6.8) whose root
has no `input`/`prime`/`runtime` keys at all is migrated once, idempotently: every root key that
is neither one of the three namespaces nor `_`-prefixed (§6.10's `_run_id`/`_timestamp`) is moved
under `input`. This runs at every state-hydration point, not just `--state` — including an
`-e`-only run with no `--state` file at all, where the seeded `-e` values still go through the
same migration path as a side effect of how they're seeded.

---

## 12. The conformance suite

**Where it lives.** In the Circuitry repository's own test tree, next to the Python suite —
Python `cof` is the source of truth for the language (Decisions, §1), so the suite's expected
outputs are generated by running `cof run` itself, and the suite format is shared infrastructure
both engines are checked against. Because electricity lives in the same repository, both engines
run the suite on the same commit: there is no pinned reference checkout, and a change to the
language cannot merge while the two engines disagree (a deliberate, temporary divergence has to
be marked in the case itself).

**Case format**, one directory per case: a minimal orchestration document; a scripted
model-adapter reply list (fixed text/token counts, no real network calls); scripted tool fakes
(fixed stdout/exit codes for process-backed tools, fixed response bodies for HTTP-family tools);
the exact expected final state (or a named sub-path of it) as JSON, generated by running that
same document through `cof run --out` on the same commit.

**How "scripted" is actually configured on both engines — the gap this design previously left
unspecified, and previously misdescribed.** The reference implementation has **no built-in
"scripted" adapter shipped as part of `cof` itself** — an earlier draft of this design claimed
there was one, citing a `meta.adapter: "scripted"` example node in runtime-semantics §5.1; that
node comes from a scratch orchestration run against one of the reference's own **test-local**
fixture adapters (`tests/tui/test_run_execution.py`, `tests/orchestrations/test_decompose_agent.py`),
not a shipped, documented, production adapter name a real document could rely on. There is
therefore no existing shared contract to port — electricity needs a **new**, matching test-only
adapter on the Rust side, and Circuitry needs its own equivalent formalized as shared,
documented infrastructure (not left as scattered test fixtures) before both engines can be
scripted from the same case file.

**Resolved (#362): Circuitry's own `scripted` adapter ships as a documented, registered adapter**
(`circuitry.adapters.scripted.ScriptedAdapter`, `runtime.adapters.scripted.replies_file`), and the
fixture-config format is specified in `electricity/docs/spec/scripted-replies.md` so electricity's
own test-only adapter (still to be built, on the Rust side) can read the identical file. Replies
are **keyed by the calling effect's stable state path** (`prime.review`,
`prime.shots.iter_2.describe`), never by call sequence — call sequence is nondeterministic for a
`flow: tree` loop or dynamic, whose branches dispatch in whatever order their worker threads
happen to run. Circuitry threads this path through every model-call site via a context variable
(`core.effect_identity`), set for the duration of one call and read by the adapter, not derived
from a global counter. Replies queued for one path are consumed in order, which is what makes a
retry or an `expect:` re-ask against the same effect deterministic too. Tool fakes split by kind:
a process-backed tool (`shell`, `ffmpeg`, `awk`, `imagemagick`) is faked by a recorded-stdout/
exit-code wrapper script the case substitutes for the real binary (both engines invoke whatever
`binary`/`PATH` resolves to, so pointing that resolution at a fixture script needs no
engine-specific fake-tool code at all); an HTTP-family tool (`http`, `web_fetch`, `webhook`, `mcp`
over HTTP) is faked by a loopback-bound mock HTTP server the suite starts per case, with the
document's `base_url`/equivalent config pointed at it — again requiring no fake-mode branch
inside either engine's real `http` client.

**Normalization rules** applied before diffing, not before generating (the raw `cof` output is
the ground truth; normalization only narrows what's compared):
- timestamps (`created_at`/`completed_at`/ISO-8601 fields, **and** the root-level `_timestamp`
  key, whose format is `%Y%m%d_%H%M%S`, not ISO-8601 — §6.10) — compared for presence/shape/
  ordering, not exact value, each parsed with the format that field actually uses;
- durations (`elapsed_s`/`eta_s`/wall-clock totals) — compared for presence and plausibility
  bounds, not exact value;
- run ids (`run_id`, UUID v4 fields) — compared for well-formedness, not exact value;
- third-party-library error text (CEL evaluation errors, YAML/JSON parse errors, JSON Schema
  messages) — compared for "failed in the same place, with a non-empty message," per the
  Decisions above — never byte-for-byte; Circuitry's **own** error text (everything cited
  verbatim in runtime-semantics §6.2) is compared byte-for-byte, with **no** normalization;
- the `{path}` segment inside a profile's schema-failure message (§6.11) — compared
  structurally (both sides name a path to the same physical file), not byte-for-byte, since
  electricity's path is the operator-given one rather than the reference's own discovered name.

**Differential corpora** (rust-ecosystem.md's "next steps," owner-decision-driven): every
`mode: cel` expression, every Mustache template, and every regex pattern actually used in the
owner's production documents is extracted and run through both engines' underlying library
(cel-python vs. the `cel` crate layer; chevron vs. electricity's port; Python `re` vs.
`fancy-regex`/`regex`) on the same recorded inputs, diffed independently of the end-to-end
conformance cases — this is what decides, empirically, whether `cel-core` is needed as a
fallback (§7.2) and which documents' regex patterns fall into the unclosed parity gap (§7.3).
A YAML differential corpus similarly runs every document's actual scalar literals through
PyYAML's resolver and electricity's composer. A **complexity-scoring differential corpus**
(§9.7) runs every scored prompt's actually-rendered text through `core/complexity.py` and
electricity's ported scorer, diffing the full per-signal breakdown (not just the final score),
since scoring enabled changes observable state (`meta.complexity`) byte-for-byte, not just a
pass/fail verdict.

**Normalizing the YAML/JSON-Schema *library* text** does not relax Circuitry's own structural-
check messages (§4 step 2's unknown-key/near-miss errors, `group:` placement errors, interface
type-mismatch errors) — those originate in Circuitry's own code and are held to word-for-word
parity like every other Circuitry-authored message.

**The handoff.** Once electricity passes the full suite for a milestone's document set
(Decisions, §1), `cof run` itself is changed to shell out to (or link) electricity for execution,
making electricity the one engine and leaving Python `cof` as the authoring tool only (check,
wizard, TUI). This is milestone M4 (§15) and is explicitly **not** a v1 requirement — it is the
point at which the two-engine period ends.

---

## 13. Quirks and parity risks

Every quirk/apparent-bug identified in runtime-semantics §9 and every parity risk flagged in
tools-adapters-plugins and rust-ecosystem.md, with a recommendation. "Copy" means electricity
reproduces the behavior exactly, including when it looks like a bug. "Fix upstream" means change
Circuitry first, so both engines agree on the corrected behavior — never a silent electricity-only
divergence.

| # | Quirk / risk | Recommendation | Owner decision needed? |
|---|---|---|---|
| Q1 | Bare sibling references (`{{first.value}}`) resolve inside a loop body/`if` branch but **not** at plain top level or inside a plain `dynamic` — only the `prime.`-prefixed form works there. | **Copy.** This is the single most load-bearing parity assertion in the whole suite (conformance case C27) — silently "fixing" it would change which existing documents even compile the same way. | **No — settled 2026-10-06**: confirmed no production document relies on the bare-form failing silently in a way a fix would surface as a new error; copied exactly, no owner question remains. |
| Q2 | `params_json` renders spliced lists/dicts as real JSON; every other template site (including `{{{...}}}` elsewhere) renders them via Python `str()` (single-quoted, not valid JSON). | **Copy**, via the two-context-type design (§3.3) rather than a mode flag, so the asymmetry can't be accidentally unified by a later refactor. | No — this is a clean two-path implementation, not a judgment call. |
| Q3 | `use.inline`'s template renders once, against the **parent's** context, before the child orchestration exists — `use.inputs` has no effect on what the inline template text itself can reference. | **Copy.** "Fixing" this (rendering `inline` twice, once per scope) would be a new, undocumented behavior change, not a bugfix — out of scope for electricity to invent unilaterally. | **No — moot**: no document in the first production document sets uses `use: {inline: ...}` at all, so this quirk's behavior has nothing to apply to yet. |
| Q4 | `while`'s condition sees `iter.index = count - 1` (post-pass, −1 before any pass); the body sees `iter.index = count` (uncompensated) — a deliberate one-line asymmetry. | **Copy**, with an explicit conformance case asserting both sides (C10). | No — documented as deliberate in the reference's own source commentary. |
| Q5 | `allowed_commands`'s literal-only rule is enforced **twice** (compile time and dispatch time) by two separate checks. | **Copy both checks** — the duplication is intentional defense-in-depth (dispatch time catches a `use`-/reflector-generated document the static compile pass never statically checked). | No. |
| Q6 | `expect:`'s absent-path-is-false exemption applies to `if`/`while` CEL but **not** to `expect:` itself — an unresolved path there raises, treated as "expectation failed." | **Copy**, via two distinct CEL entry points (§7.2) so the exemption can never leak across. | No. |
| Q7 | A loop's `collect:` aggregation silently drops a pass whose target step itself produced a value, if a *later* step in that same pass then failed — it only checks "did this pass complete end-to-end," never "did the target step succeed independently." | **Copy.** | No — a plausible footgun, but changing it would change which values survive into `collected` for any existing document using `collect:` inside a loop with later-failing steps. |
| Q8 | YAML octal is `017` (legacy, no prefix) per PyYAML/YAML 1.1 — not `0o17` (Python/YAML-1.2 octal), which loads as the *string* `"0o17"`. | **Copy** — implemented directly in the composer's resolver table (§3.2). | No — this is the YAML-1.1-vs-1.2 divergence the whole composer exists to bridge, not a judgment call. |
| Q9 | `--out` (no flag) is insertion-ordered; `--out --pretty` is alphabetically sorted — two genuinely different serializations, not whitespace variants. | **Copy**, with both forms covered by conformance case C23 specifically (a suite that only ever diffs `--pretty` output would never catch a plain-form key-ordering regression). | No. |
| Q10 | A decode/schema-validation failure on a prompt retries through the **fallback chain** first; only once every fallback is exhausted does the outer per-pass retry/backoff loop engage. | **Copy** — changes both the order of what's tried and the `fallback_attempts` log shape even when the final `value` happens to match a naive implementation. | No. |
| — | **Regex engine gap**: `fancy-regex`/`regex` do not cover the full Python `re` grammar (true backreference-plus-lookaround combinations, some lookbehind forms). | **Accept the gap for v1**, documented via tools-adapters-plugins §7 case 8 rather than hidden; revisit with an embedded PCRE/Oniguruma binding only if a production document's pattern actually needs it. | **No**: no production regex pattern uses lookaround or backreferences; re-scan if a future document adds one. |
| — | **CEL crate gap**: the `cel` crate has ~820 skipped conformance cases upstream (mostly proto/extension-related) and is not independently verified against cel-python's own (non-spec-exact) behavior. | **Use `cel` behind the strict/non-strict layer, validated by a differential corpus** (§12) against cel-python on every expression the production documents actually use; keep `cel-core` as a documented fallback if a gap surfaces. | No — this is the research's own recommended mitigation, not an open call. |
| — | **Error-string parity scope**: how much of `meta.error`/schema/YAML error text must match byte-for-byte. | **Resolved** (Decisions, §1): Circuitry's own messages word-for-word, third-party library text only "fails in the same place." Listed here for completeness, not as open. | No — already decided. |
| — | **`trust_orchestration_runtime` / untrusted-document `runtime:` filtering.** | **Collapse to "always trusted"** per the explicit-config decision (§11) — but confirm no production document's `runtime:` block relies on being *filtered* (i.e., assumes an untrusted key is silently dropped rather than applied). | **No**: no production document's `runtime:` block sets anything beyond `state.record_children`. |
| — | **A `use` document naming `ref:`** (library lookup) rather than `path:`. | **Reject with a clear "library refs not supported; use `path:`" error** rather than attempting a partial library-resolution feature (§4 step 4) — electricity's `use` scope is filesystem-path children only (Decisions, §1). | **No**: no `use` child in the production document sets names a library `ref:` at all. |
| — | **`host_claude` / MCP server surface.** | **Not implemented**; registered only to produce the matching "cannot build from config" error (§9.5). | No — explicitly out of scope (Decisions, §1). |
| — | **Plugin-bridge process-sharing confused-deputy risk.** An earlier draft of §8.5 shared one process across every bridged plugin for a run; fixed to one process per plugin name (§8.5), which removes the risk at the design level. | **Already fixed** — listed here as the record of the finding, not as a remaining gap. | No. |
| — | **Plugin-bridge environment scrubbing.** The reference runs every plugin in-process with the host's full environment; electricity's bridge process gets a fixed base allowlist plus `runtime.plugins.<name>.env` only (§8.5) — strictly narrower. | **Keep the narrower allowlist** — this only restricts a plugin's own process, never the orchestration's observable behavior, as long as a plugin's actually-needed env vars are named in its own `runtime.plugins.<name>.env`. | No — a hardening choice that can only cause a new "env var not found" failure for a plugin that needs one outside the allowlist and isn't configured for it, not a silent divergence in a document's output. |
| — | **Plugin-bridge method naming**: an earlier draft's elaborated method set (`plugin/execute`/`check`/`cancel`/`log`) did not match the project's own sketch (`describe`/`invoke`/`progress`/`cancel`) term-for-term. | **Settled 2026-10-06 in favor of the sketch** (§8.5): `describe`/`invoke`/`progress`/`cancel`, with the elaborated design's health-check and logging behavior folded into `describe` (re-callable) and the plugin's own stderr stream, respectively, rather than kept as separate RPCs. | No — settled; listed here as the record of the decision. |
| — | **Cancellation granularity**: the reference's own current behavior is coarser than an earlier draft of this design described, and inconsistent between its two flows — chain flow loses an in-flight leaf's result (the child process is killed), tree flow runs every already-submitted branch to completion but then discards all of their results, having skipped the merge step (§6.5). | **Settled 2026-10-06: fix upstream, don't reproduce as-is.** Both runners stop promptly on the first signal — every running leaf/branch is stopped (child process groups terminated, in-flight network/MCP calls cancelled where possible), a not-yet-started branch is never started, `finally:` still runs, nothing in flight is merged, a second SIGINT/SIGTERM during cleanup ends the run at once with its own exit code, SIGHUP is handled like SIGTERM (exit `129`) and never acts as a second signal (§6.5 has the full rule). Circuitry has this fix since #357 (closing #356); electricity implements the same behavior, with conformance cases from that PR's tests. | No — settled; listed here as the record of the decision. |
| — | **Ollama via the OpenAI-compatible adapter family** (§9.2): no native `/api/chat`/`/api/generate` support, so `options`/`format`/`keep_alive`/`think` have no slot. | **Accept** — already an explicit decision (Decisions, §1); listed here as the parity-risk record, since Ollama is the reference's own default adapter and the first milestones' documents use it. | No — the routing choice itself is decided; only the resulting gap is tracked. |
| — | **`uuid` v1 nondeterminism**: a v1 UUID is time/host-dependent by construction. | **The conformance harness does not compare v1 output byte-for-byte** — it asserts both engines produce a well-formed v1 UUID with a monotonically increasing timestamp component (tools-adapters-plugins §7 case 9); v4/v5 *are* compared byte-for-byte given a scripted seed/fixed namespace. | No. |
| — | **`HTTP_PROXY`/`NO_PROXY` handling**: whether electricity's native HTTP client honors these by default. | **Settled 2026-10-06**: honored the same way `cof` honors them — Python's own `urllib` environment-proxy convention (`HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY`, case-insensitive on most platforms) — for every native HTTP-backed surface: the `http` tool (§8.1) and every HTTP-transport adapter (§9). Not left to whatever `reqwest` defaults to on its own. | No — settled; listed here as the record of the decision. |
| — | **Credentials in config.json**: the reference reads the `cyberdiner` adapter's token (§9.4) from config only (no env-var form exists for it at all), the `surrealdb` runtime plugin's token/user/password (§10.2) from config as a fallback after the matching env var, and an `mcp` server's `headers`/`env` (§8.1) from config by design. | **Settled 2026-10-06**: follow the reference exactly for all three — config credentials are redacted in state and config.json is already operator-chosen/trusted, so the real rule is "provider API keys from env only; every other service credential from wherever `cof` reads it; no credential on argv or in state" (§1, §14). | No — settled; listed here as the record of the decision. |
| — | **`--resume <run-id>` input re-pass rule**: every key the loaded state's `input` namespace has must be re-supplied via `-e`, not just one. | **Copy** — implemented in §6.8's resume-safety checks. | No. |
| — | **CLI usage-error exit code `2`**: not one of the four codes in an earlier draft's CLI-surface assumption. | **Settled 2026-10-06** (`0`/`1`/`130`/`143` plus `2` for usage errors, as `cof`/click; `129` for SIGHUP since Circuitry #357) — §6.9 now also maps which specific pre-run failures get `1` versus `2`, matching the reference's own case-by-case `BadParameter` handling rather than grouping every usage error under one code. | No — settled; listed here as the record of the decision. |

---

## 14. Security model

electricity's security posture mirrors Circuitry's (Decisions, §1: "same security rules as
`cof`"):

- **Explicit trust, no discovery.** Both the config file and the orchestration file are
  operator-chosen and therefore fully trusted — electricity performs no document-trust
  classification, no `cof trust`-equivalent prompt, no implicit config search path.
- **Shell allowlists + host pin, intersection semantics.** A `shell` effect's commands are
  bounded by the intersection of the document's own `allowed_commands` and the host's
  `runtime.plugins.shell.allowed_commands` pin — never their union (§8.4).
- **`allowed_commands` is literal-only**, checked twice (§8.3, Quirk Q5) — nothing computed at
  runtime can widen a shell effect's permitted commands.
- **No secrets on argv.** electricity's native HTTP/process clients never construct a shell
  command line carrying a credential; headers and bodies go directly into the HTTP library's
  request object, never through a spawned shell. No logging/tracing path (including OTel span
  attributes) may echo a header value or request body verbatim.
- **Redacted state.** The shared deny-list redaction (§8.2) runs on every tool `raw`/
  `params_rendered` and on any adapter wire traffic that reaches state, before `--out`,
  `--live-state`, or any persistence write — never after.
- **Provider API keys from environment only, with credentials settled case by case elsewhere**
  (§9.6, §1): the reference reads the `cyberdiner` adapter's bearer token (§9.4) **from config
  only — there is no env-var form for this token in `cof` at all** — and the `surrealdb` runtime
  plugin's token/user/password (§10.2) **from config as a fallback after the matching env var**;
  electricity follows the reference exactly in both cases — settled 2026-10-06, not an open
  contradiction with this design's own Decisions (§1), which state the *provider* API-key rule
  specifically, not a blanket "no credential of any kind in config.json" rule. An `mcp` server's
  own `headers`/`env` (§8.1) are a third case, config-held by design rather than as any kind of
  fallback — a stdio MCP server gets its credential through its own `env` map, an HTTP MCP server
  through its own `headers` map, exactly as `cof` configures them. The rule that holds across all
  three cases without exception: no credential ever reaches a subprocess's argv, and every one
  of these three config fields is redacted before any state write (§8.2).
- **SurrealDB has two separate credential rules, not one** — an earlier draft of this bullet
  conflated them. The **observability runtime plugin** (§10.2) reads credentials env-var-first,
  config-second (`SURREAL_TOKEN`/`token`, etc.) — the correction an earlier draft of this document
  made, replacing a still-earlier claim that config has no credential fields for SurrealDB at
  all. The **`surrealdb` tool plugin** (§8.1) is stricter and has no config fallback at all: its
  credentials come from environment variables only, full stop, matching the reference's own tool
  plugin exactly — §8.1's own table row already states this correctly; this bullet previously
  read as if §8.1 shared §10.2's config-fallback rule, which it does not.
- **The plugin bridge is process isolation, not a sandbox** (§8.5) — `python_eval`'s own
  RestrictedPython + forked-child + rlimit sandboxing is what bounds that specific plugin; the
  bridge's value is a crash boundary and a kill switch, and this distinction is stated plainly to
  users, not implied to be stronger than it is.
- **Capability ceiling on generated plans — always unrestricted for electricity, not a gate it
  enforces.** The reference's own capability ceiling is installed only for a *library-sourced*
  document (a shared-library fetch, a remote library source, or a path inside a library cache
  directory) or a `use: ref:` child reached from one — **never** for a document run directly by
  path, which is electricity's only mode (Decisions, §1). Since electricity also rejects `ref:`
  outright (§4 step 4, §13), neither condition that installs a ceiling in the reference can ever
  apply to electricity: the ceiling is **always `None`/unrestricted** for every electricity run,
  reflector- and decomposition-generated plans included. This is worth stating plainly rather than
  describing electricity as "enforcing a capability ceiling" (an earlier draft implied it does) —
  the risk is someone building a *stricter* gate that then diverges from what `cof run` itself
  does for the exact same path-run document, not someone building a gate that's too loose.

---

## 15. Milestones and parallel lanes

Each milestone's acceptance criteria are conformance-suite passes (§12) for the document set
named, plus any explicitly listed non-conformance deliverable (release artifacts, embedding API
surface). Lanes within a milestone are designed to be worked in parallel once that milestone's
shared foundation (the lane marked **(gate)**) lands.

**How this maps onto the Decisions-above milestone order.** The Decisions (§1) commit to an
ordering by *document set* (the two simplest production document sets first, then a larger and
more varied one, then the remaining v1 features) — not by technical layer. The milestones below
are still organized by technical layer, because the actual build dependencies run that way (no
document set runs end-to-end without the VM, and the VM doesn't need every tool built first); what
changed from an earlier draft is that each milestone now states **which document set it actually
unlocks**, so the layer-based lane structure doesn't silently reorder the owner's own priority:
M0 unlocks nothing end-to-end on its own (foundation only); M1 — including the complexity/
routing/decomposition lanes moved up into it, §9.7 — is what's needed to run the first two
document sets: one of them turns on complexity scoring/routing and reads `meta.complexity.*`
for documents using `provider: ollama` with no explicit model (§9.7's correction — only one of
the two sets does this, not both, but M1 still has to gate on it); M2 is needed for the third,
larger document set (it is the one that uses the
extended protocol surface — `python_eval`, the vendor adapter, SurrealDB); M3 and M4 are v1
features layered on top of a document set that already runs, not gates on any one document set.

### M0 — Foundation

| Lane | Deliverable | Depends on |
|---|---|---|
| M0-0 **(gate)** | Repository setup (§17): the `electricity/` Cargo workspace skeleton (an `electricity` binary that prints its version and a preview notice), the CI workflow, the version-sync check, and the preview release jobs, so the next Circuitry release already carries electricity. | — |
| M0-A **(gate)** | `electricity-value`: the `Value` type, `py_str`/`py_repr`, hashable `Dict` keys. | — |
| M0-B | `electricity-yaml`: the PyYAML-1.1 composer on `saphyr-parser`, duplicate-key detection, merge keys. | M0-A |
| M0-C | `electricity-json`: the `json.dumps`-exact writer/reader, both `--out` serializations. | M0-A |
| M0-D | `electricity-template`: the chevron port, both context-wrapper types (§3.3). | M0-A |
| M0-E | `electricity-cel`: the `cel` crate wrapped in the strict/non-strict layer, the two binding entry points (§7.2). | M0-A |
| M0-F | `electricity-schema`: Draft7 validation against the synced schema copy, offline, plus the sync script and its CI check. | M0-A |
| M0-G | `electricity-bytecode` + `electricity-compiler`: IR types, the load-and-check pipeline (§4), compiling a document with no effects that need runtime support yet. | M0-B, M0-C, M0-D, M0-E, M0-F (the compile-time checks in §4 step 4 walk every template and every `mode: cel` expression, so the compiler needs the tokenizer and the CEL parser, not just YAML/JSON/schema) |
| M0-H | `electricity-vm` skeleton: frames, the state store (`NodeRef`-based, §6.7), `fire_effect_start`/`complete`, the concurrency limiter, cancellation tree — runnable against a document with only `tool`/`dynamic`/`if` effects and an in-process fake tool. | M0-G |
| M0-I | The conformance harness itself: running `cof run` from the same checkout to generate expected state, diffing with normalization rules (§12). | — (parallel with all of the above) |
| M0-J **(gate, Python side)** | The `scripted` fixture adapter and the shared fixture-config format (§12) — a change to Circuitry's Python package in the same repository; blocks any conformance case that needs a scripted model reply, which is most of them. | — (parallel with M0-A..I, but gates M0-I actually producing usable fixtures) |

**Acceptance:** conformance cases C2, C3, C6, C23, C27 pass against a hand-written smoke-test
document set exercising every M0 crate, plus **the `if` half only of C7** (the `while` half
needs loop support, which doesn't exist until M1-E — see below). **C2 is exercised through
electricity's own load-and-check pipeline directly, not through a CLI `--validate-only` flag**,
since v1 has none (§1, Decisions): the case's YAML-value-type and duplicate-key assertions hold
regardless, but `runtime.last_run.{dry_run,validate_only,verbose}` are compared loosely for this
one case, not byte-for-byte, since electricity always writes `false` there (§6.10) while the
reference's own `validate_only` run writes `true`. **C9 (tree loop), C24 (named loop), and the
`while` half of C7 move to M1's acceptance** — an earlier draft claimed C9 and C24 here, but loop
support doesn't exist until M1-E; M0-H's VM skeleton only covers `tool`/`dynamic`/`if`.

### M1 — Run the production tool/adapter set (unlocks the first two document sets)

| Lane | Deliverable | Depends on |
|---|---|---|
| M1-A | `electricity-tools`: `shell`, `fs`, `json`, `env_vars`, `hash`, `uuid`, `clock`, `regex` (the `fancy-regex`-backed plugin, not the CEL path). | M0 |
| M1-B | `electricity-tools`: `awk`, `imagemagick`, `ffmpeg` (process-backed, shared `GenericSubprocessTool`-equivalent). | M0 |
| M1-C | `electricity-tools`: `http` (native client, the four-tier error-excerpt extraction, Retry-After capture). | M0 |
| M1-D | `electricity-adapters`: the OpenAI-compatible family (one parameterized adapter, covering Ollama) + `anthropic`, shared retry classification (§9.6), **and the `host_claude` registration stub (§9.5)** — folded in here since it's a registry entry producing one error message, not real adapter work. | M0 |
| M1-E | Full effect-type support in `electricity-vm`: `prompt` (retries, fallback chains, decode/schema-retry-through-fallback-first ordering — Quirk Q10), `loop` (each/while, chain/tree, `collect:`, `prev`), `use` (path + inline, the three output-mapping modes), scope overlays (Quirk Q1, and the nested-named-dynamic re-merge, §6.3) end-to-end. | M0 only — built and tested against the `scripted` adapter and fake tools (M0-I/J), **not** gated on M1-A–D; the real tool/adapter lanes can run in parallel with this one, not after it. |
| M1-F | `electricity-redaction` fully wired into the real tools/adapters from M1-A–D; the literal-only `allowed_commands` rule at both checkpoints (Quirk Q5). | M1-A, M1-D (needs real tools/adapters to wire into, unlike M1-E) |
| M1-G | Complexity scoring and routing (§9.7): the scorer, the band table, the precedence chain. | M1-E (needs prompt dispatch to attach `meta.complexity`/`model_reason` to) |
| M1-H | The reflector effect (sugar over `use: inline`, §5.3) and decomposition (§9.7) — **decoupled from the plugin bridge**, since neither needs `python_eval`; decomposition additionally needs the synced copy of the planner orchestration (§9.7). | M1-E, M1-G |
| M1-I | Profiles (§6.11): `--profile <path>` loading/schema validation, the model/adapter/out precedence layer, per-effect overrides applied onto the compiled IR, and the `effective_settings.profile` record. | M1-E (per-effect overrides need the compiled IR to apply onto), M1-G (a profile's `routing:` pin needs the resolved band table to validate against) |

**Acceptance:** conformance cases C1, C4, C5, C9, C10–C22, C24, C25, C26 pass, plus the `while`
half of C7 (C7's `if` half and C27 already passed in M0; neither retested here), plus
tools-adapters-plugins §7 cases 1–6, 7 (the `http` half
only — the `mcp` half of case 7 moves to M2's acceptance, since `mcp` itself is M2-A), 9, 12,
14, 15, 17 pass against real documents using this tool/adapter set. **Case 13 is dropped from
this milestone's required-pass list**: it asserts the native `ollama` adapter's own three
hint strings, which electricity's OpenAI-compatible transport (§9.2) has no code path to
produce — an accepted, named gap (§13's Ollama parity-risk row), not a target to hit. **C8** runs
in full, including the half that disables a step via a profile override (§6.11) — reproduced by
running electricity with an equivalent `--profile <path>` fixture in place of the reference's
`--profile <name>`, not skipped.

### M2 — Extended protocol surface (unlocks the third, larger document set)

| Lane | Deliverable | Depends on |
|---|---|---|
| M2-A | `electricity-tools`: `mcp` (`rmcp` client). | M1 |
| M2-B | `electricity-tools`: `surrealdb` (own HTTP client) + `email_smtp` (`lettre`). | M1 |
| M2-C | `electricity-adapters`: the `cyberdiner` adapter (§9.4). | M1 |
| M2-D | `electricity-plugin-bridge`: the Rust NDJSON JSON-RPC client half, per-plugin-name process lifecycle (§8.5), stdout-redirect enforcement, the host-side hard-kill timer. | M1 |
| M2-E | **Python-side** change: the bridge server module in Circuitry's Python package (§8.5), a hard dependency of M2-D's conformance cases. | — |
| M2-F | `python_eval` end-to-end through the bridge (M2-D + M2-E only — no longer bundled with the reflector/decomposition/scoring work, which moved to M1-G/H since it has no real dependency on the bridge). | M2-D, M2-E |
| M2-G | `electricity-runtime-plugins`: state-snapshot persistence backends (`jsonl-file`, sqlite/postgres via `sqlx`; mongodb deferred, §10.1), `--resume` by run id (§6.8). | M1 |
| M2-H | `electricity-runtime-plugins`: observability plugins — SQL-family (sqlite/postgres), `surrealdb` (distinct from M2-B's tool plugin, §10.2), `jsonl-file` event stream, the `store_raw` cascade. | M1 |

**Acceptance:** tools-adapters-plugins §7 cases 7 (the `mcp` half deferred from M1), 8, 10, 11,
16, 18, 19 pass (19 needs M2-H, added here since no earlier draft's lane built it); a document
using `python_eval` produces identical
state to `cof run`.

### M3 — Observability, embedding, release

| Lane | Deliverable | Depends on |
|---|---|---|
| M3-A | OpenTelemetry runtime plugin (OTLP/http, console-exporter fallback when unconfigured — §10.4), including the thread/task-identity span disambiguation under unnamed `tree` concurrency. | M1 |
| M3-B | Live state mirror, loop progress, run totals (§10.5) — the live-event-stream-based totals accumulator, not a post-hoc state walk. | M1 |
| M3-C | The public embedding API (`electricity` lib crate): a stable `run_orchestration()`-shaped entry point, caller-supplied tool/adapter sets, `initial_state` for resume without a file stash. | M1 (crate boundary already exists from §2; this lane is the API-stability pass) |
| M3-D | v1 release hardening: the preview release jobs from M0-0 lose their preview label once M1–M3 pass; `mimalloc` on the musl targets; the container image hardened (scratch/distroless, CA roots); crates.io publishing stays out of v1 (Decisions, §1). | M1 (needs a stable, buildable binary; does not need M2) |

**Acceptance:** tools-adapters-plugins §7 cases 20, 21 pass; **case 22 (the blanket redaction
check) is run as a cross-cutting post-check against every case in M1 onward's acceptance runs,
starting the first milestone that has real tools/adapters to redact — not deferred to M3**, since
an earlier draft placed it in no milestone's acceptance at all; a built release binary (each
target platform) runs the M1 conformance set; the embedding API lane has its own integration test
embedding electricity in a minimal host binary.

### M4 — `cof run` delegation

| Lane | Deliverable | Depends on |
|---|---|---|
| M4-A | Full conformance parity across every document class in M0–M3 — the gate for this milestone to even start. | M0–M3 |
| M4-B (Python side) | `cof run` hands execution to the electricity binary (Decisions, §1: "one engine"), found on `PATH` or configured explicitly. | M4-A |

**Acceptance:** `cof run` and `electricity` produce identical output for the full owner document
set, confirmed by running the *same* conformance suite against both as backends, not just
electricity against a frozen `cof` snapshot.

---

## 16. Risks and open questions

**Review findings carried here instead of fixed elsewhere, with the reason:**

- **Plugin-bridge method naming** (§8.5, §13): an earlier draft of this design kept its own
  elaborated `execute`/`check`/`cancel`/`log` set rather than the project's own
  `describe`/`invoke`/`progress`/`cancel` sketch. **Settled 2026-10-06 in favor of the sketch**:
  §8.5 is rewritten to the `describe`/`invoke`/`progress`/`cancel` method set, with the
  elaborated design's extra behavior (a plugin's own health check, Python-side logging) folded
  into that set rather than kept as separate RPCs — `describe` is re-callable at any time, not
  just at process start, so it also serves the health-check role `check` played; Python-side
  logging goes out over the plugin process's own stderr stream, which the bridge already reads
  as a log/OTel source, rather than over a dedicated `log` notification.
- **Regex parity gap** (§7.3, §13) — no closure plan beyond "accept and document" until a
  concrete production pattern is shown to need true Python-`re`-only syntax.
- **CEL crate maturity** — the `cel` crate's own conformance gaps (mostly proto/extension cases)
  are believed irrelevant to the owner's documents, but this is only confirmed by the
  differential corpus (§12), not by inspection.
- **PyYAML syntax edge cases beyond scalar resolution** — tabs, `%YAML 1.1` directives, `\/`
  escapes, NEL line breaks, and flow-context plain-scalar edge cases may be accepted by one
  parser and rejected by the other even after the resolver/constructor layer matches; this needs
  its own differential corpus pass over real document syntax, not just scalar values.
- **Float formatting** — the Python-`repr`-matching layout rule (§3.4) is implemented from
  documented CPython behavior, not verified against a fetched CPython source at design time;
  it must be golden-tested against real CPython output before M0 is considered done, not assumed
  correct from the rule as written here.
- **`jsonschema` crate error-message shape** — verdicts (valid/invalid) are high-confidence
  parity; message *text* differences are accepted per the third-party-library carve-out
  (Decisions, §1), but the exact set of keywords the production schema actually uses should be
  checked against known `jsonschema`-crate message gaps before assuming "fails in the same
  place" holds for all of them.
- **`cost_usd`** (§10.5, §6.10): no adapter in §9 has a documented per-call cost figure to
  populate `meta.cost_usd` from, so `runtime.last_run.totals.cost_usd` stays `null` for every
  electricity run until one does — matching the reference's own behavior when nothing reports a
  cost (it is `null`, not `0`, per §6.10's correction), but still an unclosed gap in the sense
  that electricity has no adapter that *ever* populates it. Needs a price-table-driven cost
  calculation in at least one adapter before this field carries real information.
- **`--resume last` and stashed `-e` values — a review claim not adopted.** A re-review of an
  earlier draft asserted `--resume last` replays the `-e` values stashed from the run it resumes.
  Reading the reference's own `_resolve_resume_state` shows the opposite for that specific flag:
  its own docstring states plainly that, unlike the separate, out-of-scope bare `--last` flag
  (which replays an entire prior invocation, not just its state), `--resume last` does **not**
  replay anything from the stash — the loaded `--out` file already carries its own resolved
  `input.*`, and `_resolve_resume_state`'s `replay_env_vars` return value is unconditionally empty
  for both the `--state <file>` and `--resume last` branches. Not adopted here, since it does not
  match the cited source; electricity's `--resume last`/`--state <file>` only apply `-e` values
  given on the current invocation (§6.8), consistent with the reference.
- **MongoDB and the remaining SQL-family dialects** (mysql/duckdb/mssql/cockroachdb/clickhouse)
  from tools-adapters-plugins §4.2 are not in the owner's current document set and are not
  scheduled in M0–M4; if a future document needs one, it is additive work within the existing
  `SqlDialect`-equivalent abstraction, not a design change.

---

## 17. Repository setup

- **Where.** electricity lives in the Circuitry repository (Decisions, §1), in `electricity/` at
  the repository root, next to the Python package (`src/circuitry`) and the other non-Python
  components (`editor/`, `services/`): the Cargo workspace (`electricity/Cargo.toml`,
  `electricity/Cargo.lock`, `electricity/crates/*`), this design (`electricity/DESIGN.md`) and the
  specification and survey (`electricity/docs/`). The Python package is unaffected: setuptools
  packages only `src/circuitry`, so neither the wheel nor the sdist carries Rust sources.
- **Licence.** MIT, the repository's own.
- **Versioning.** One version for both: the workspace's `version` always equals
  `pyproject.toml`'s, enforced by a check in the Python test suite and by the release workflow's
  tag check; `RELEASING.md` bumps both.
- **Crate names.** The binary and the top-level library crate are both named `electricity`;
  every internal crate is `electricity-<area>` as listed in §2. Nothing is published to
  crates.io for now (Decisions, §1): every crate is `publish = false`, and an embedder depends on
  the repository's git tag.
- **CI.** A workflow of its own runs `cargo fmt --check`, `cargo clippy --all-targets -- -D
  warnings` and `cargo test --workspace` on Linux and macOS whenever `electricity/**` changes,
  plus the MSRV check below. The conformance suite (§12), with its differential corpora, runs as
  a separate, longer job whenever either engine, the shared schema or the suite changes: it
  generates expected state with the Python engine and checks electricity against it on the same
  commit. The schema and planner copies (§4 step 3, §9.7) are checked against
  `src/circuitry/` in the same workflow. Python-only changes keep their existing workflow and
  never wait for a Rust build.
- **MSRV.** Pinned explicitly in `Cargo.toml` (`rust-version`) and checked in CI via a dedicated
  `cargo +<msrv> check --workspace` job, rather than left to drift with whatever the CI image
  happens to ship — chosen once the full dependency graph (§2, rust-ecosystem.md's crate list) is
  locked, since several candidate crates (the `cel` crate, `jsonschema`, `reqwest` 0.13) are
  recent enough that the MSRV floor is realistically "whatever those require," not an
  independently chosen target.
- **Releases.** The repository's existing tag-driven release workflow (`vX.Y.Z`: tests, version
  check, PyPI, GitHub release) gains electricity jobs, from the next release on (Decisions, §1):
  static Linux binaries (x86_64, arm64) built against musl with `cargo-zigbuild`; a dynamically
  linked macOS arm64 binary (fully static is not possible against `libSystem`); each as a
  `.tar.gz` with a SHA-256 file; and a multi-arch container image on the GitHub Container
  Registry, built from the musl binaries (scratch/distroless with CA roots). All of them are
  attached to the same GitHub release as the Python distributions and marked as a **preview** in
  the release notes until v1 (§15 M3-D, which also brings `mimalloc` on the musl targets, since
  musl's default allocator is slow under multithreaded load). The electricity builds run in
  parallel with the Python build, and nothing is published (neither PyPI nor the GitHub release)
  unless every artifact built. The same build jobs also run, without publishing anything, on pull
  requests that change electricity's build or release configuration, so a broken cross-compile
  is caught before a tag. cargo-dist is not used: it expects to own the release workflow, which
  here is shared with the Python package.
