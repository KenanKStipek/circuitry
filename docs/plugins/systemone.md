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

### `state_file` and `within`

`within` is a literal absolute path or `{from: input.<name>}`; a `{{ … }}`
template, a path read from another part of state and a `params_json` value are
refused, because each can carry generated text. A relative `state_file` is
resolved against the working directory.

A **limited** document must also name a `within` inside its own project: the
directory of the nearest `config.json` at or above the document, or the
document's own directory (the same project a `{file: …}` prompt is confined to).
A `use` child found through the library is limited whatever its parent is, and a
child run by path has its own project. A document you run by path may name any
`within`.

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
| Connection refused, DNS failure, timeout | yes — `systemone server not reachable at <base_url>` | — |
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
  appears nowhere in params, `meta`, events, logs or errors: an error body the
  server sends back is redacted, and the key itself is replaced if the server
  echoes it. `api_key_env` must be a variable name; a key pasted there is
  refused without being printed.
- `systemone` is tagged `network` for [capability consent](../threat-model.md).
- A `state_file` is read only after `within` is proven: symlinks are resolved
  first, and only a regular file is read.
- Preflight and `cof doctor` check the configuration only; they never contact the server.

## electricity

`systemone` is a cof-only plugin. `electricity` refuses a document that names it
before the run starts, with its usual preview message pointing at `cof run`.
