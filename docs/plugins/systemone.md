# systemone

Asks typed questions about a piece of state and returns typed, checked answers:
a probability (`noul`), a choice among options with probabilities (`choice`), or
a score on a rubric (`score`). It speaks the `POST /v1/systemone` wire format of
TypeSafe's hosted API ([API reference](https://docs.typesafe.ai/api)) and of the
local servers that copy it.

## When to use

A decision about a long piece of state (a document, a diff) where a later effect
branches on a number or a named option, not on prose. Calling the same endpoint
with the [`http`](../orchestration-reference.md) tool works, but it saves the
whole state again in `meta.params_rendered`, checks nothing about the answers,
and needs the key in the document's own params. `systemone` keeps the state out
of saved state (with `state_file`), fails an effect whose answers do not match
its questions, and reads the key from the environment only.

This is a **tool plugin** (`provider: systemone`). It needs no extra install.

## Host settings

The server is a host setting: it lives under `runtime.plugins.systemone` in
config (or in a document you run by path), never in an effect's params.

```json
{
  "runtime": {
    "plugins": {
      "systemone": {
        "base_url": "http://127.0.0.1:8080",
        "api_key_env": "SYSTEMONE_API_KEY",
        "allow_remote": false
      }
    }
  }
}
```

| Key | Default | Meaning |
| --- | --- | --- |
| `base_url` | — | Required. `http://` or `https://`; no credentials, query or fragment. The request goes to `<base_url>/v1/systemone`. |
| `api_key_env` | — | The **name** of an environment variable that holds the key. It is sent only as `Authorization: Bearer <key>`, and only when the variable exists and is not empty. |
| `allow_remote` | `false` | Unless `true`, only a loopback host is accepted. Any other host is refused before a request is made. |

- **Loopback** means the literal hosts `localhost`, `127.0.0.0/8` and `[::1]`.
  Every other host name counts as remote; no DNS lookup decides it. A loopback
  request never goes through a proxy.
- **A limited document cannot change these.** A document that reached `cof`
  indirectly (a library name, a fetched source, a REST or MCP caller) has its
  `runtime.plugins` block ignored with a warning, like every other host setting.
  An effect param named `base_url`, `api_key_env` or `allow_remote` is an error
  in `cof check` and at dispatch.
- Use `https://` for a remote server: with `http://` the key crosses the network
  in the clear.

## Params

| Param | Meaning |
| --- | --- |
| `model` | Required. The model name (the effect-level `model:` works too). |
| `questions` | Required. A map of question id → `{type, instructions, criteria}`. |
| `state` | Any JSON value, sent as written. |
| `state_file` | A file read when the effect dispatches. A `.json` file is parsed (a parse error fails the effect); any other file is sent as a JSON string of its UTF-8 text. |
| `within` | Required with `state_file`: an absolute path. After symlinks are resolved, the file must be inside it. |

Give exactly one of `state` and `state_file`.

### Questions

| `type` | `instructions` | `criteria` |
| --- | --- | --- |
| `noul` | string, object or array | optional `{"true": …, "false": …}` |
| `choice` | string, object or array | required: a map of option → string, object, array or `null`; at most 255 options |
| `score` | string, object or array | required: an array of 2 to 10 level descriptions |

`cof check` validates a literal `questions` map: a known type, `instructions`
present, `criteria` shaped for the type, at most 255 options, 2 to 10 levels. A
`{from: …}` value is accepted as it stands and checked when the effect runs.

**Quote the keys of `criteria` in YAML.** YAML reads an unquoted `true`, `false`,
`yes`, `no`, `on` or `off` key as a boolean and `1:` as a number. The request
carries the string JSON would write for it, and answers are checked against that
string: an unquoted `yes:` option is sent as `"true"` (and `no:` as `"false"`),
never `"yes"`. `true:` and `false:` in a `noul`'s `criteria`, and options `1:` to
`5:`, work as written; to offer the options `yes` and `no`, write `"yes":` and
`"no":`. Two keys that become the same string (`1:` and `"1":`) are an error.

### `state_file` and `within`

A document with no file of its own cannot use `state_file` or name a `within`:
a `use: inline` child, a reflector or decomposition plan, and a document read
from stdin or passed to the SDK as a string. Either is a compile error (`'file:'
cannot be used here`, naming `params.state_file` or `params.within`), the same as
for a `{file: …}` prompt source, so such a document can never read a file, even
if `params_json` supplies a `state_file`. Give it its text with `state`.

`within` is a literal absolute path or `{from: input.<name>}` (exactly one name,
as for a prompt's `file: {from: …}`); a `{{ … }}` template, a path read from
another part of state and a `params_json` value are refused, because each can
carry generated text. `cof check` checks the shape of `within` whenever it is
set, also when `state_file` arrives through `params_json`. A relative `state_file` is
resolved against the working directory.

A **limited** document (one not run by path) must also name a `within` inside its own project: the
directory of the nearest `config.json` at or above the document, or the
document's own directory (the same project a `{file: …}` prompt is confined to).
A document you run by path may name any `within`. A limited effect that has no
project to stay inside reads no file at all, so a missing project is never the
same as no limit.

`cof check` (and MCP's validate) checks a literal `within` against the project
when it knows the document is limited, for example a document inside a library
source's cache. It cannot know that for a `use: ref:` child (that is only settled
when the run reaches it) or for a `within` taken from `input`, so those are
checked when the effect runs. A document run by path is trusted, so `cof check`
on it does not apply the project rule.

### Trust and `use`

These are the same rules as a prompt's `file: {from: …}` source, and one
mechanism decides them: a document's trust is fixed when it compiles.

- A document run by path is trusted. A child run by `use: {path: …}` has its
  parent's trust and its own project, so a limited parent's path child is
  confined to that child's project.
- A child found through the library (`use: {ref: …}`) is limited whatever its
  parent is, also under a trusted root.
- A `use: inline` child is limited, so a `use: {path: …}` grandchild inside it
  is limited too. A reflector or decomposition plan is limited the same way.
- A `use: {path: …}` that points into a library cache inherits the parent's
  trust: the path names the file, not the library. Name library documents with
  `ref:` to get them limited.
- With `trust_orchestration_runtime: true`, a document run by library name as
  the root is unlimited, while the same document as a `use: {ref: …}` child is
  limited.
- In a `use` child, `input.<name>` is whatever the parent maps to it, which can
  be another effect's output. A limited child's `within` is still held to its own
  project, but a trusted child that takes `within` from `input` takes it from the
  parent's mapping.

```yaml
- type: tool
  name: verdict
  provider: systemone
  params:
    model: typesafe-1
    state_file: {from: input.report}
    within: {from: input.reports_dir}
    questions:
      risky:
        type: noul
        instructions: Does this change risk data loss?
      verdict:
        type: choice
        instructions: What should the reviewer do?
        criteria:
          approve: Nothing blocks the change.
          revise: Something must change first.
          reject: The change should not land.
```

## Output

- `value` is the response's `answers` map, as returned:
  - `noul`: `{type: noul, noul: 0..1}`
  - `choice`: `{type: choice, choice, probabilities, confidence}`
  - `score`: `{type: score, score, legend, probabilities, confidence}`
- `meta.raw`: `{model, usage, status, state_sha256, state_bytes, question_ids}`.
  `state_sha256` and `state_bytes` describe the state as it was sent: its compact
  UTF-8 JSON encoding, so a `state_file` and the same content passed as `state`
  hash alike. **The state text itself is never stored**, and a `state_file`
  keeps it out of `meta.params_rendered` too.
- `meta.tokens_sent` and `meta.tokens_received` come from `usage`
  (`input_tokens`, `output_tokens`).

## Validation

Each failure fails the effect (`meta.raw.error_kind` is `invalid_answer`) and
names the question id:

- a question with no answer, and an answer to a question that was not asked;
- an answer whose `type` differs from its question's;
- a `choice` that is not one of the question's options;
- `probabilities` (required for `choice` and `score`) that are not finite, not in
  [0, 1], name an option or level the question does not have, or do not sum to
  1 ± 0.01;
- a `noul` outside [0, 1];
- a `score` outside `[0, levels − 1]`;
- a `confidence`, when present, outside [0, 1].

## Limits

A request is UTF-8 JSON with no BOM, finite numbers only, at most 64 nesting
levels and 32 MiB. A body over 32 MiB is refused before it is sent
(`error_kind: too_large`); so is a state nested too deeply or holding `NaN`.
Nothing is compressed.

## Errors and retries

| Outcome | Retried | `meta.raw.error_kind` |
| --- | --- | --- |
| Connection refused or DNS failure | yes — `systemone server not reachable at <base_url>` | — |
| Timeout | yes — `systemone request to <base_url> timed out after Ns` | — |
| 408, 429, 529 and 5xx (honouring `Retry-After`) | yes | `timeout`, `rate_limited`, `overloaded`, `server_error` |
| 400 with `max_tokens_exceeded` in the body | no | `context_exceeded` |
| 400, 422 | no | `invalid` |
| 401 | no | `unauthorized` |
| 413 | no | `too_large` |
| 415 | no | `unsupported_media_type` |
| 403, 404, other 4xx | no | `forbidden`, `not_found`, `http_error` |
| Any 3xx | no | `redirect` |
| Answers that do not match, or an unreadable response | no | `invalid_answer`, `invalid_response` |

`meta.status_code` holds the HTTP status. Retries follow the effect's `retries:`
(see the [orchestration reference](../orchestration-reference.md)).

**Redirects are never followed.** Any 3xx fails the effect, naming the status, so
the key is only ever sent to `base_url`.

## Security

- The key is read from the variable named by `api_key_env` at dispatch. It
  appears nowhere in params, `meta`, events, logs or errors. Anything the server
  sends back has the key replaced by `[redacted]` if it echoes it: an error body,
  `value`, `meta.raw.model`, `meta.raw.usage` and `Retry-After`. `api_key_env` must be a variable name; a key pasted there is
  refused without being printed.
- `systemone` is tagged `network` for [capability consent](../threat-model.md).
- A `state_file` is read only after `within` is proven: symlinks are resolved
  first, and only a regular file is read.
- Preflight and `cof doctor` check the configuration only; they never contact the server.

## electricity

`systemone` is a cof-only plugin. `electricity` refuses a document that names it
before the run starts, with its usual preview message pointing at `cof run`.
