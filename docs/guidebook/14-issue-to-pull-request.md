# Issue to pull request

Every part of the language, in one document: the agent that takes a bug report and returns a pull request. It starts where `triage.yml` ends, with an issue that is a bug, and it works in a checkout of the repository on a fresh branch. Two files: the agent, and the test-running circuit it calls twice.

```yaml
# run_tests.yml — the child: a typed interface, two tools, and a recorded decision
interface:
  inputs:
    repo: {type: string, required: true, description: The checkout to test.}
    command: {type: array, required: true, description: "The pytest arguments, such as [-q, tests/]."}
  outputs:
    passed: {type: boolean, path: prime.verdict.value.result}
    failures: {type: array, path: prime.failures.value}

effects:
  - type: tool
    name: test_run
    provider: pytest
    params: {cwd: "{{input.repo}}", allow_nonzero: true}
    params_json: '{"args": {{{input.command}}}}'
  - type: tool
    name: failures
    provider: regex
    params: {pattern: "^FAILED (\\S+)", input: "{{{prime.test_run.value}}}", flags: [MULTILINE]}
  - type: if
    name: verdict
    if: {mode: cel, expr: "state.prime.test_run.meta.exit_code == 0"}
    then: []
```

```yaml
# issue_to_pr.yml
# Inputs: issue and repo (required). Output: prime.ready.pr.value
interface:
  inputs:
    issue: {type: string, required: true}
    repo: {type: string, required: true}
  outputs:
    pr: {type: string, path: prime.ready.pr.value}

effects:
  # prompt — read the error out of the issue
  - type: prompt
    name: error
    template: "Copy the error message out of this issue, without the values in it. Reply with the message only. Issue: {{input.issue}}"

  # dynamic (tree) + tools — look around in parallel; no gh login, still a fix
  - type: dynamic
    name: context
    flow: tree
    effects:
      - type: tool
        name: search
        provider: ripgrep
        params: {args: [--line-number, --fixed-strings, "{{{prime.error.value}}}", "."], cwd: "{{input.repo}}", allow_nonzero: true}
      - type: tool
        name: history
        provider: git
        params: {args: [log, --oneline, "-5"], cwd: "{{input.repo}}"}
      - type: tool
        name: related
        provider: gh
        params: {args: [issue, list, --search, "{{{prime.error.value}}}"], cwd: "{{input.repo}}"}
        on_error: skip

  # use — run the tests through the child; its interface supplies the outputs
  - type: use
    name: tests
    path: ./run_tests.yml
    inputs: {repo: "{{input.repo}}", command: [-q]}

  # loop (each) — diagnose every failing test in parallel, collect the diagnoses
  - type: loop
    name: diagnoses
    flow: tree
    each: {in: prime.tests.value.failures, as: test}
    collect: diagnose
    body:
      - type: prompt
        name: diagnose
        template: "Explain in two sentences why this test fails: {{test}}\nWhere the error is raised:\n{{{prime.context.search.value}}}"

  # prompt — plan the fix from everything gathered so far
  - type: prompt
    name: plan
    template: |
      Plan the fix for this issue in at most three steps.
      Issue: {{{input.issue}}}
      Where the error is raised: {{{prime.context.search.value}}}
      Recent commits: {{{prime.context.history.value}}}
      Related issues, if any: {{{prime.context.related.value}}}
      Why the tests fail: {{{prime.diagnoses.collected.value}}}

  # loop (while) — patch, apply, rerun; unnamed, so each patch reads the last test run
  - type: loop
    while: {mode: cel, expr: "!state.prime.tests.value.passed"}
    max_iterations: 3
    body:
      - type: prompt
        name: patch
        template: |
          Write a unified diff for this repository that carries out the plan. Output the diff only.
          Plan: {{{prime.plan.value}}}
          Failing tests: {{prime.tests.value.failures}}
      - type: tool
        name: apply
        provider: git
        params: {args: [apply, "-"], cwd: "{{input.repo}}", stdin: "{{{prime.patch.value}}}\n"}
        on_error: skip        # a patch that does not apply costs a pass, not the run
      - type: use
        name: tests
        path: ./run_tests.yml
        inputs: {repo: "{{input.repo}}", command: [-q]}

  # reflector — plan the checks before review; the planner reads only its own template
  - type: reflector
    name: pre_review
    max_effects: 3
    effects:
      - type: prompt
        name: propose_steps
        template: |
          An agent has patched a bug in a small Python library. A pull request opens next if the tests pass.
          Plan the checks a maintainer expects before review. Output must follow the OUTPUT CONTRACT exactly.

  # if — open the pull request only when the tests pass; both branches write `pr`
  - type: if
    name: ready
    if: {mode: cel, expr: "state.prime.tests.value.passed", strict: true}
    then:
      - type: prompt
        name: pr
        template: |
          Write the pull request description for this fix: what was wrong, what changed, how it was tested.
          Issue: {{{input.issue}}}
          Plan: {{{prime.plan.value}}}
      - type: tool
        name: commit
        provider: git
        params: {args: [commit, --all, --message, "Fix: {{{prime.error.value}}}"], cwd: "{{input.repo}}"}
      - type: tool
        name: push
        provider: git
        params: {args: [push, --set-upstream, origin, HEAD], cwd: "{{input.repo}}"}
      - type: tool
        name: open_pr
        provider: gh
        params: {args: [pr, create, --draft, --title, "Fix: {{{prime.error.value}}}", --body, "{{{prime.ready.pr.value}}}"], cwd: "{{input.repo}}"}
    else:
      - type: prompt
        name: pr
        template: |
          The tests still fail after three patches. Write a comment for the issue: what was tried, and what still fails.
          Plan: {{{prime.plan.value}}}
          Failing tests: {{prime.tests.value.failures}}
```

```bash
git switch -c fix/parse-duration    # the agent commits and pushes the branch you are on
cof check issue_to_pr.yml
cof run issue_to_pr.yml -e issue="parse_duration fails on 1h30m with ValueError: invalid duration. 90m works." -e repo=. --live-state agent.live.json
```

The `then` branch commits, pushes the current branch and opens a draft pull request with your own `git` and `gh` logins, so create a branch first, as above, and run the agent in a checkout you mean it to change.

## Reading the run

`agent.live.json` is the state graph filling in as the agent works. When it is done, the shape is exactly what the names dictate:

```
input.issue                                     "parse_duration fails on 1h30m …"
input.repo                                      "."
prime.error.value                               "invalid duration"
prime.context.search.value                      "./src/durations.py:9: raise ValueError(…)"
prime.context.history.value                     the last five commits
prime.context.related.value                     the related issues — or null, skipped, with meta.error saying why
prime.diagnoses.iter_0.diagnose.value … iter_1  one pass per failing test, run in parallel
prime.diagnoses.collected.value                 the diagnoses, in test order
prime.diagnoses.value.termination.reason        "collection_exhausted"
prime.diagnoses.last                            {"$ref": "iter_<N>"} — the final pass, stored once
prime.plan.value                                the plan
prime.patch.value                               the last patch the loop wrote
prime.apply.meta.exit_code                      0 — it applied
prime.tests.value                               {"passed": true, "failures": []} — the last test run
prime.pre_review.generated.iter_0.…             whatever the planner decided to run
prime.ready.value.branch                        "then"
prime.ready.pr.value                            the pull request description
prime.ready.open_pr.value                       the pull request URL `gh` printed
runtime.effective_settings.sources              where every setting came from
```

Every effect's `meta` sits beside its value — which model, why, the rendered prompt, tokens, timing, any fallback that answered. Because the patch loop is unnamed, `patch`, `apply` and `tests` hold the last pass only: the first `tests` run, the one the diagnoses read, was overwritten by the loop, which is what let each patch read the latest failures. The file stores `diagnoses`' final pass once and points `last` at it, and `--state` or the TUI's Runs view link the reference back when they load the file.

## Turning on the complexity layer

Nothing in the document changes. In config:

```json
{
  "runtime": {
    "complexity": {
      "scoring": {"enabled": true},
      "routing": {
        "enabled": true,
        "bands": [
          {"name": "light", "max": 15, "model": "phi3:mini"},
          {"name": "mid",   "max": 40, "model": "qwen2.5:7b-instruct"},
          {"name": "heavy",            "model": "gpt-oss:20b"}
        ]
      },
      "decomposition": {"enabled": true, "threshold": 45, "max_chunks": 5, "max_depth": 1}
    }
  }
}
```

```bash
cof score issue_to_pr.yml
cof run issue_to_pr.yml -e issue="parse_duration fails on 1h30m" -e repo=. --explain-routing --decompose-out ./plans
```

The short prompts — `error`, `diagnose` — route to the small model; `plan`, reading across everything the agent gathered, scores higher and routes up; and if `plan` ever grows past the threshold, it decomposes into parts of its own and merges back at `prime.plan.value`, where the patch loop reads it unchanged. A patch is only as long as the plan it carries out, so it can score low; to send every `patch` to the capable model whatever it scores, pin it in a [profile](04-configuration.md): `effects: {patch: {routing: heavy}}`.

## When the model chooses the next tool

This agent's plan is in the document: search, then read the history, then diagnose, patch and test. The model fills in each step, but the document decides which step comes next. The other kind of agent lets the model choose the next tool on each turn, and Circuitry can express that too, with the same parts. The bundled `agents/agent_loop` is one `while` loop. Each pass reads a transcript file, asks for one `prompt_type: json` decision (`{thought, tool, args, done, answer}`), dispatches it through CEL `if` effects to one read-only tool inside a working directory (`fs` list and read, `ripgrep`, `git status`/`log`/`diff`/`show`), and appends the step to the transcript:

```bash
cof run agents/agent_loop -e task="Why does parse_duration reject 1h30m?" -e workdir=. -e max_steps=6
```

The transcript is the agent's only memory, every pass costs one model call whose prompt holds the whole transcript, and the dispatch branches are the only tools it can call. [The agent loop](../agent-loop.md) walks through a pass, its limits, and what adding write tools changes.

## What the agent demonstrates

| Chapter | In the document |
| --- | --- |
| [Prompt](01-prompt.md) | every prompt an instruction to the agent; triple-stache for code |
| [Dynamic](02-dynamic.md) | `context` fans out with `flow: tree`; the document itself is a chain |
| [Shadow state](03-state.md) | every path derived from names; `input.` / `prime.` / `runtime.` |
| [Configuration](04-configuration.md) | no `adapter:` or `model:` anywhere in the document |
| [Errors](05-errors.md) | `on_error: skip` on the `gh` lookup and on `apply`; a `plan` template written for an empty value |
| [If](06-if.md) | `ready`, a `strict` CEL gate on the test result, same name `pr` in both branches |
| [Loop](07-loop.md) | `each` with `flow: tree` and `collect`; an unnamed `while` whose body overwrites what it reads |
| [Reflector](08-reflector.md) | a bounded planner whose template states the situation |
| [Composition](09-composition.md) | `run_tests.yml` with an interface, called twice; typed outputs (`passed`, `failures`) feed CEL and a loop |
| [Complexity](10-complexity.md), [Decomposition](11-decomposition.md) | switched on in config, invisible to the document |
| [Surfaces](12-surfaces.md) | `--live-state`, `--explain-routing`, `--decompose-out` |
| [Tools](13-tools-and-persistence.md) | `ripgrep`, `git`, `gh`, `pytest` and `regex` as effects like any other |

An orchestration is a declared control mechanism which, through reflectors and decomposition, lays its own plans. The pull request is open; a maintainer reviews it.
