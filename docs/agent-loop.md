# The agent loop

Can an orchestration be the agent itself, instead of calling one? Yes. The
bundled `agents/agent_loop` orchestration is a small tool-using agent built
from four parts of the language: a `while` loop, a `prompt` with
`prompt_type: json`, `if` effects that dispatch to `tool` effects, and an `fs`
file that serves as the transcript. Nothing else drives it, and it has no memory
outside the transcript.

```bash
cof run agents/agent_loop -e task="Where is the retry backoff capped?" -e workdir=. -e max_steps=6
cof eject agents/agent_loop --out my_agent.yml   # a local copy to change
```

| Input | Default | Meaning |
|---|---|---|
| `task` | required | What the agent should find out. |
| `workdir` | `.` | The directory the tools may read. Every path the model names is resolved inside it. |
| `max_steps` | `8` | The most passes the loop runs. Each pass makes one model call. The loop also has a fixed ceiling of 50 (`max_iterations`). |
| `transcript` | `""` | The path of the transcript file. If you leave it empty, the file is `$TMPDIR/circuitry-agent-loop-<uuid>.md` (`/tmp` when `TMPDIR` is unset), never a path inside `workdir`. A file that already exists at the path you give is overwritten. |

| Output | Path | Meaning |
|---|---|---|
| `answer` | `prime.answer.value` | The model's final answer. If `max_steps` ran out first, a note that says so. |
| `transcript` | `prime.transcript_file.value` | The path of the transcript file. |

## Which model

The document names no `adapter:` and no `model:`, so it runs on whatever you
configured: your `default_adapter`/`default_model`, a profile, or
`--adapter`/`--model` on the command line (see
[Configuration](guidebook/04-configuration.md)). The model must reliably return
one JSON object for each request, so a very small local model can stop more
often on schema retries than a larger one.

If your access to a model is a subscription that you use through a
coding-agent CLI's own login, and not an API key, the planned CLI-backed
adapters `pi` and `claude_code` ([#366](https://github.com/KenanKStipek/circuitry/issues/366))
send each `prompt` through that CLI with the CLI's own tools turned off. Once
they are in your installed version (`cof list --extensions` shows the adapters
that it has), they work here like any other adapter:
`cof run agents/agent_loop --adapter claude_code ...`.

## One pass

Before the loop, the run lists `workdir` once, without `on_error`. A `workdir`
that is not a directory fails the run there, before any model call.

Each pass of the `agent` loop runs four steps in order:

1. **`read_transcript`**: `fs` reads the transcript file. This is everything the
   agent did so far: the task, and each earlier step's decision and result.
2. **`decide`**: one model call. The prompt holds the task, the tool list and
   the whole transcript, and asks for strict JSON:
   `{thought, tool, args, done, answer}`. A JSON Schema checks the reply
   (`prompt_type: json` plus `schema:`). A reply that is not valid JSON, or
   that does not match the schema, is retried (`retries: {max_attempts: 3}`).
   If all three attempts fail, the run fails.
3. **dispatch**: a flat row of unnamed CEL `if` effects. At most one runs a
   tool, and each of them writes `prime.observation`:
   - `done: true` records that the run is finished and runs no tool;
   - a known tool with acceptable arguments runs that tool;
   - a `path` that is absolute or has a `..` segment, or a `search` with an
     empty pattern, is refused with a message;
   - any other tool name gets the message `Unknown tool. Use one of: ...`.

   Tool failures, such as a missing file or a bad revision, use
   `on_error: continue`, so the error is shown to the model and the run continues.
4. **`record`**: `fs` appends the step to the transcript: the thought, the
   tool, the arguments, and the observation (or `error: ...`). The next pass's
   `read_transcript` reads it back.

The `while` condition (`mode: cel`) continues while the model has not set
`done` and fewer than `max_steps` passes have run. `min_iterations: 1` makes
the first pass run without a check, because there is no decision to read yet.
After the loop, `answer` is `prime.agent.last.decide.value.answer` if the last
pass said `done`. If not, `answer` is a fixed note.

The transcript is plain markdown:

```text
# Agent transcript

Task: Why does parse_duration fail?
Workdir: ~/project

## Step 0
thought: Find where parse_duration is defined.
tool: search
args: {'pattern': 'def parse_duration', 'path': 'src'}
observation:
src/durations.py:1:def parse_duration(text):

## Step 1
...
```

An unknown tool, a refused path and a failed tool call all go into the
transcript in the same way. On the next pass, the model reads what went wrong
and can correct its call. Because none of these stop the run, a model that
keeps making the same mistake uses up its `max_steps`.

## The tools, and why they cannot write

| Tool | `args` | Runs |
|---|---|---|
| `list_dir` | `path` | `fs` `mode: list` on `<workdir>/<path>` |
| `read_file` | `path` | `fs` `mode: read` on `<workdir>/<path>` |
| `search` | `pattern`, `path` | `rg --no-config --line-number ... --regexp=<pattern> -- <path>` in `workdir` |
| `git_status` | none | `git --no-optional-locks -c core.fsmonitor=false status --short --branch` |
| `git_log` | `rev` | `git log --oneline -n 20 --end-of-options <rev>` (default `HEAD`) |
| `git_diff` | `rev` | `git diff --no-ext-diff --no-textconv --end-of-options <rev>` (default `HEAD`) |
| `git_show` | `rev` | `git show --no-ext-diff --no-textconv --end-of-options <rev>` (default `HEAD`) |

The model chooses a branch and supplies one argument value, and nothing else.
The document fixes everything else:

- **The operation.** The `fs` mode (`list`, `read`), the git subcommand and
  every flag are literals in the YAML. No branch uses a mode or a subcommand
  that writes. The model's text cannot become a mode or a subcommand.
- **Arguments that cannot become options.** The search pattern is one
  `--regexp=<pattern>` argument and the path comes after `--`. A revision comes
  after git's `--end-of-options`, so a value such as `--output=<file>` is read
  as a revision name (and fails) and is never used as the option that writes a
  file.
- **The tools' own configuration.** `--no-config` stops a ripgrep config file
  from adding flags such as `--pre`, which runs a command for each file.
  `core.fsmonitor=false` and `log.showSignature=false` stop the repository's own
  git config from starting a hook or a signature program, and `--no-ext-diff` and
  `--no-textconv` do the same for diff drivers. `--no-optional-locks` stops
  `git status` from refreshing the index.
- **Paths inside `workdir`.** A CEL check refuses an absolute `path` or one with
  a `..` segment before any tool runs.

The only file that the run writes is the transcript, outside `workdir`
by default.

**Circuitry's allowlists remain the boundary, but at plugin level.** The
dispatch above is the document's own allowlist, and it is the only thing that
keeps this agent read-only. The host's allowlists are a level above it:
`enabled_tools` in config.json limits the tool plugins that any run may
build. This document needs `env_vars`, `uuid`, `fs`, `json`, `ripgrep` and
`git`, so `"enabled_tools": ["env_vars", "uuid", "fs", "json", "ripgrep", "git"]`
lets this agent run and blocks every other plugin. It does not keep a changed
copy read-only: `fs` can also write and delete, and the `git` plugin runs any
subcommand it is given. A copy with a `write_file` branch or a `git commit`
branch passes the same `enabled_tools` list. See [Threat Model](threat-model.md)
and [Configuration](guidebook/04-configuration.md).

## Limits

- **Cost per step.** Each pass is one model call, and its prompt contains the
  whole transcript so far. Input tokens therefore increase with each pass: the
  total for a run is about the sum of the transcript's size at each step. One
  large `read_file` is in every prompt after it. `max_steps` limits the number
  of calls, not their size. Each `decide` effect's `meta` records its
  `tokens_sent`/`tokens_received`.
- **No memory beyond the transcript.** The model has no state between calls.
  If information is not in the transcript, the next pass does not have it:
  the file is the agent's whole history.
- **No truncation.** A file is read whole and a search is limited only by
  `--max-count=50` matches for each file and `--max-columns=300`. For a large
  repository, ask for narrow searches or add a limit of your own.
- **`max_steps` without `done` is not an error.** The run succeeds with the
  fixed note as `answer`. Read the transcript to see how far it got.
- **What the path check does not catch.** `fs` follows symbolic links, so a link
  inside `workdir` that points outside it is readable. ripgrep does not follow
  links while it walks a directory, but a `path` argument that is itself a link
  is searched at its target, the same exposure as `fs`. The check is for POSIX
  paths. git finds the repository that contains `workdir`, which can be a
  parent directory.
- **What the git flags do not stop.** A filter driver (`filter.<name>.clean` or
  `filter.<name>.process` in the repository's git config, selected by its
  `.gitattributes`) runs a program when `git status` or `git diff` compares
  working-tree files. No flag turns that off. Point the git tools only at a
  repository whose `.git/config` you trust.
- **The transcript holds what the agent read.** File contents go into the
  transcript, and the transcript goes to the model provider on every pass. The
  default location is your temporary directory, and the file is not removed
  after the run. Do not point the agent at secrets that you would not send to
  that provider.
- **The transcript's permissions are your umask's.** The default file has a
  random name directly in `$TMPDIR`, so another user cannot create it first or
  swap it for a link. It is created with your umask's permissions, though, and
  in a shared `/tmp` that usually means other users can read it. On a shared
  host, set `transcript` to a path in a directory only you can read. A
  `transcript` that already exists is overwritten.
- **Prompt injection.** A file that the agent reads can contain instructions
  for the model. With read-only tools the worst result is a wrong answer or a
  misdirected read inside `workdir`.

## Adding write tools

To add a tool, add a name, a dispatch branch and a line in the tool list. Eject
a copy (`cof eject agents/agent_loop --out my_agent.yml`), then:

1. add the tool to the list in `decide`'s template, with its `args`;
2. add an unnamed `if` whose `then` runs the tool as `observation`, with
   `on_error: continue`;
3. add the name to the unknown-tool check's list;
4. give it the same argument check the read tools use, if it takes a path.

For example, a `write_file` tool:

```yaml
- type: if
  if:
    mode: cel
    expr: "state.prime.decide.value.done != true && state.prime.args_ok.value == true && state.prime.decide.value.tool == 'write_file'"
  then:
    - type: tool
      name: observation
      provider: fs
      on_error: continue
      params:
        mode: write
        path: "{{{input.workdir}}}/{{{prime.decide.value.args.path}}}"
        content: "{{{prime.decide.value.args.content}}}"
```

(Add `content: {type: string}` to `args` in the schema, and `write_file` to the
`tool in [...]` list of the refusal branch so that its path is checked.) To run
commands, use the `shell` plugin with a literal `allowed_commands` list, such as
`allowed_commands: [pytest]`. `cof check` rejects a templated entry, and a host
can pin `runtime.plugins.shell.allowed_commands` as a limit that no document
can exceed.

What this changes about safety:

- **The model now changes your files.** It writes the content it chooses into
  any path that passes the check, and it can overwrite your work. Run the agent
  in a git checkout on a branch of its own, or in a container, so every change
  can be reviewed and undone.
- **Prompt injection can now cause changes.** Text in a file that the agent
  read can now cause a write or a command, and not only a wrong answer.
  Read tools and write tools in one loop let any file in `workdir` give
  instructions to the agent.
- **A command can do more than its name says.** A command in
  `allowed_commands` runs with your user's permissions, and many commands can
  start others: `pytest` runs the project's code, `git` runs hooks and config
  commands. Allow only the commands that you would run on untrusted input.
- **Capability consent does not tell the two apart.** Consent is per
  plugin capability (see the
  [orchestration reference](orchestration-reference.md#tool)), and the
  read-only document already needs all the ones a write tool adds: `fs-write`
  for the transcript, `shell` and `network` for `git` and `ripgrep`. A
  document that pulls your agent in with `use: ref:`, or a library copy run
  with `cof run-library`, asks the same question whether or not the agent can
  change your files. Only reading the dispatch tells you which it is.
- **Lower `max_steps`.** With write tools, each step can change a file, and
  is no longer only a cost.
