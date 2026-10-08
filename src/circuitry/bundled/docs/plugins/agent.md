# agent

Runs a delegated coding-agent session — **pi** or **Claude Code** — as a
tool effect. Where a `prompt` effect is one model call, an `agent` effect is
a whole session with the agent's own tools on: it reads and edits files,
runs commands and tests, and iterates in a working directory for as long as
the effect's `timeout_ms` allows. The session goes through the CLI's own
login, the same way the `pi` and `claude_code` adapters do.

## It is not sandboxed

**The agent runs with your own permissions.** It can read, write and delete
anything your user can, run any program, and reach the network. Circuitry
does not sandbox it, and `cwd` is only where the session starts, not a
boundary. What narrows it is the engine's own tool lists. Both engines read
`tools` as an allowlist, but Claude Code also needs a permission mode for
the list to be the whole boundary:

- **pi:** `tools` (`--tools`) is an allowlist, the only tools the session
  has; `exclude_tools` (`--exclude-tools`) takes tools away from it.
- **Claude Code:** `tools` is an allowlist. `--tools` leaves the session
  exactly the built-in tools named in it, and `--allowedTools` runs those
  entries without a prompt. Under `dontAsk` a specifier narrows its tool:
  `Bash(pytest:*)` keeps `Bash` and allows only `pytest` calls. `mcp__`
  entries name MCP tools, not built-in ones. When `tools` is set and
  `permission_mode` is not, the session runs under `dontAsk`: the listed
  tools run, and every other call is denied, since nobody is there to
  approve it. When `tools` is absent, Claude Code's own default applies
  (`auto` in current versions, where Claude Code decides which calls to
  allow), so the session can edit files and run commands. An explicit
  `permission_mode` (`acceptEdits`, `auto`, `bypassPermissions`, ...)
  replaces `dontAsk`; under those modes a specifier no longer limits its
  tool, and Claude Code may approve calls the list does not name.
  `exclude_tools` (`--disallowedTools`) is a hard deny in every permission
  mode.

A Claude Code session with `tools` set can use only what is listed, so the
agent needs a tool that can write `result_file` (`Write`, `Edit` or `Bash`),
or it cannot write the result file.

Point `cwd` at the directory the work belongs in, and give an agent only
the tools its task needs:

```yaml
- type: tool
  name: review
  provider: agent
  params:
    engine: pi
    prompt: "Read the code under src/ and list the functions that lack tests."
    cwd: "{{input.repo}}"
    tools: [read, grep, find, ls]     # read-only: no bash, edit or write
```

Because of what it can do, `agent` is tagged with the `shell`, `fs-write`
and `network` capabilities: a document that did not come from your own disk
(a library asset, a `use: ref:` child) needs the same explicit consent
before its `agent` effects run as before its `shell` effects. The
`enabled_tools` allowlist in config can leave it out altogether.

## Repository settings

A Claude Code session would otherwise load the repository's own settings, and
some of them run code: hooks, an `apiKeyHelper` command, and the `.mcp.json`
servers all start in `-p` mode with no trust prompt. So by default a Claude
Code `agent` effect is isolated from the repository:

- **Settings:** the session runs with `--setting-sources user`. The
  repository's and the local project settings are ignored (their hooks,
  `apiKeyHelper`, `env` and permission rules). Your user-level settings
  (`~/.claude/settings.json`) still apply.
- **MCP servers:** none start. The session gets `--strict-mcp-config` with
  no `--mcp-config`; to give it a server, pass one explicitly:
  `extra_args: ["--mcp-config", "<file>"]`.
- **`CLAUDE.md`:** the repository root's `CLAUDE.md` is appended to the first
  prompt, under a heading that says Claude Code did not load it, because
  `--setting-sources user` also stops Claude Code from reading it. The repair
  turn does not repeat it. The root is the first directory, walking up from
  `cwd`, that holds a `.git` entry (a directory, or a file in a git worktree);
  with none, it is `cwd`. Only that one file is read: not `CLAUDE.md` in a
  subdirectory, not `CLAUDE.local.md`, not `.claude/CLAUDE.md`, and not files
  it imports with `@`. A `CLAUDE.md` that is a link to a file outside the
  repository is not read, and an empty one is skipped. `meta.raw.project_instructions`
  records the path that was appended.

To let Claude Code load the repository's settings the normal way, set
`trust_project_settings: true`. Then the repository's hooks and `apiKeyHelper`
command run, and its MCP servers start, without asking, and Claude Code reads
`CLAUDE.md` itself, so nothing is appended:

> **Only for a repository you trust.** `trust_project_settings: true` runs the
> repository's hooks and its `apiKeyHelper` command, and starts its MCP
> servers, without asking.

`trust_project_settings` applies to `claude_code` only; with `engine: pi` it
is an error.

pi is isolated differently: `pi` always runs with `--no-approve`, so
project-local pi files (its project settings directory, extensions and prompts) are ignored.
pi still reads the repository's `AGENTS.md` itself, as it does in any session.

## Params

| Param | Type | Default | Meaning |
|---|---|---|---|
| `prompt` | string | required | The task. Mustache-rendered, multi-line. Written to a temporary file that pi attaches with `@<file>`, or sent to Claude Code on stdin — never put in the command line. |
| `engine` | `pi` \| `claude_code` | `runtime.plugins.agent.engine`, else `pi` | Which CLI runs the session. |
| `cwd` | string | the current directory | Where the session runs. |
| `model` | string | the CLI's default | pi: `provider/id`; Claude Code: a model name or alias. |
| `thinking` | string | pi's default | pi only: `off`, `minimal`, `low`, `medium`, `high`, ... |
| `permission_mode` | string | `dontAsk` when `tools` is set; otherwise Claude Code's own default (`auto`) | Claude Code only: `--permission-mode` (`acceptEdits`, `plan`, ...). An explicit value replaces `dontAsk`. |
| `trust_project_settings` | bool | `false` | Claude Code only. `false` isolates the repository's settings and appends its root `CLAUDE.md`; `true` lets Claude Code load them, MCP servers included, without asking. See [Repository settings](#repository-settings). |
| `tools` | list of strings | the engine's default; must not be empty | pi: `--tools` (comma-joined), the only tools the session has. Claude Code: `--tools` with the built-in names in the list, and `--allowedTools` with every entry; `mcp__` entries are not built-in tools. |
| `exclude_tools` | list of strings | none | pi: `--exclude-tools`; Claude Code: `--disallowedTools`, a hard deny in every permission mode. |
| `session` | string | a new session | Resume this session id (pi `--session`, Claude Code `--resume`). |
| `extra_args` | list of strings | none | More CLI flags, passed as they are. |
| `env` | mapping | none | Variables added to the session's environment. |
| `unset_env` | list of strings | `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN` | Variables removed from it; replaces the default list. |
| `result_file` | string | none | A JSON file the agent must write, relative to `cwd` (an absolute path, or one with `..`, may point anywhere). |
| `result_schema` | JSON Schema object | none | Validates `result_file`'s contents; needs `result_file`. |

The session's environment is yours, minus the variables that would make the
CLI think it runs inside a parent pi or Claude Code session (`PI_SESSION_ID`,
`CLAUDECODE`, ... — always removed), minus `unset_env`, plus `env`. Removing
the API-key variables by default makes the CLI use its own login rather than
bill a key that happens to be exported.

Both engines save the session (pi in its session directory, Claude Code in
its project history), so a later effect can resume it with `session`.

## The result contract

Without `result_file`, `value` is the agent's final reply text.

With `result_file`, the prompt gets a short closing section naming the
file's absolute path (and the schema, when given), and `value` is the
file's parsed JSON. A file left over from an earlier run is deleted before
the session starts, so only this session's result counts — wherever the
path points, so do not aim it at a file you want to keep. When the
session ends and the file is missing, is not JSON, or does not match
`result_schema`, the **same session** gets exactly one repair turn: it is
resumed with a short prompt that quotes the errors. If the file is still
invalid after that, the effect fails with those errors (`on_error`
applies); `meta.raw` still records the session, so you can inspect or
resume it.

```yaml
- type: tool
  name: fix_issue
  provider: agent
  timeout_ms: 3600000          # one hour for the whole session, repair turn included
  params:
    engine: claude_code
    cwd: "{{input.repo}}"
    prompt: |
      Fix the failing test described below, then run the test suite.

      {{input.issue}}
    tools: [Read, Grep, Glob, Edit, Write, "Bash(pytest:*)"]   # Bash only for pytest
    exclude_tools: [WebFetch, WebSearch]
    result_file: .agent/result.json
    result_schema:
      type: object
      properties:
        summary: {type: string}
        tests_pass: {type: boolean}
      required: [summary, tests_pass]
```

`tools` gives the session reading, searching, editing and writing. `Bash` is
limited to `pytest` commands. Because `permission_mode` is left out, the
session runs under `dontAsk`: any other call is denied, since nobody is there
to approve it. `Write` or `Edit` is what lets it write the result file.
`exclude_tools` keeps `WebFetch` and `WebSearch` out even if they are later
added to `tools`.

`prime.fix_issue.value.tests_pass` is then a real boolean a later `if` can
branch on.

## What lands in state

`value` is the parsed result (or the final text). `meta.raw` holds:

| Key | Meaning |
|---|---|
| `engine` | `pi` or `claude_code` |
| `session_id` | The session's id — pass it to a later effect's `session` to continue it |
| `turns` | Model turns, summed over the session and its repair turn |
| `tool_calls` | Tool calls the agent made |
| `tokens` | `{sent, received}`: input tokens (cache reads and writes included) and output tokens; `null` when the CLI reported none |
| `cost` | The CLI's own cost figure in USD — present only when it reports one |
| `repair_turn` | Whether the repair turn ran |
| `transcript` | Path of a compact log: one line per tool call and per reply, a header per turn |
| `permission_mode` | Claude Code only: the `--permission-mode` passed (`dontAsk` by default when `tools` is set), or `null` when none was passed |
| `project_settings` | Claude Code only: `isolated` (the default) or `trusted` (`trust_project_settings: true`) |
| `project_instructions` | Claude Code only: the path of the repository's `CLAUDE.md` appended to the prompt, or `null` |

The transcript stays a file in the system's temporary directory; it never
goes into state itself, and nothing removes it.

## Timeouts and cancellation

The session runs in its own process group. The effect's `timeout_ms`
bounds the whole session, the repair turn included — set it generously,
since `runtime.tools.timeout_seconds` (300s unless configured) applies
otherwise. On a timeout, or when the run is cancelled (Ctrl-C, SIGTERM,
SIGHUP), the whole group is stopped: the CLI and everything the agent
started.

When a turn times out or the CLI fails after the session has an id (a
`session` param, or a timed-out repair turn), the error names the session
id, so a later effect can resume it, and the transcript's path, which is
kept.

## Configuration

`runtime.plugins.agent` in your config file:

```json
{
  "runtime": {
    "plugins": {
      "agent": {
        "engine": "claude_code",
        "pi": {"binary": "~/.local/bin/pi"},
        "claude_code": {"binary": "/opt/claude/bin/claude"}
      }
    }
  }
}
```

- `engine` — the engine an effect gets when its params name none (`pi`
  when unset).
- `pi.binary` / `claude_code.binary` — an absolute path (`~` is expanded)
  that replaces the `PATH` search for `pi` / `claude`.

`cof doctor` reports the plugin ready when at least one engine's CLI is
found, and names each engine's state. A missing CLI for the engine an
effect asks for fails that effect with a message naming the setting.
