# `service` tool plugin: start, own and stop background processes

Tool effects are synchronous calls. `service` is the one that leaves
something running: it starts a long-running process (a dev server for
browser tests or screenshots, a local API for an integration check) in the
background, waits until it is ready, and later stops exactly what it
started — never anything else on the machine.

One provider, three actions: `params.action` is `start`, `stop` or
`status`, and `params.name` names the service (letters, digits, `_`, `.`
and `-`). A name is unique per state directory, so a later `stop` or
`status` — in the same run or a later one — finds it by name.

```yaml
- type: tool
  name: start_web
  provider: service
  params:
    action: start
    name: web
    command: [npm, run, dev, --, --port, "5173"]
    cwd: ./site
    env: {BROWSER: none}
    ports: [5173]
    ready: "http://127.0.0.1:5173/"
    ready_timeout_ms: 60000
    log: ./logs/web.log
```

## `start`

| Param | Type | Default | Description |
|---|---|---|---|
| `command` | list of strings | — | Required. The argv to run (no shell). |
| `shell` | bool | `false` | Run `command`, then a single string, with `/bin/sh -c`. |
| `cwd` | string | the run's working directory | The process's working directory. |
| `env` | mapping | — | Environment variables merged over the inherited environment. |
| `ports` | list of ints | `[]` | TCP ports the service listens on. Checked before it starts, recorded, and waited on by `stop`. |
| `ready` | string or int | — | An `http(s)://` URL, ready on any response below 500, or a TCP port on localhost, ready once it accepts a connection. Without it, the service counts as ready as soon as it has started. |
| `ready_timeout_ms` | int | `60000` | How long to wait for `ready`; never longer than the effect's own timeout (`timeout_ms`). |
| `log` | string | `<state_dir>/<name>.log` | stdout and stderr are appended here. |
| `grace_ms` | int | `5000` | SIGTERM-to-SIGKILL grace when a failed or cancelled start stops the process again. |

**Certificate checks are off for an `https://` readiness URL.** A local dev
server usually has a self-signed or locally generated certificate, so the
readiness check does not verify it — use `ready` to learn "the server
answers", not to trust a remote host. The check also ignores any
`HTTP(S)_PROXY` in the environment.

What `start` does, in order:

1. **Already running?** If a service with this `name` is running and still
   provably ours (see [Ownership](#ownership)), it is left as it is —
   `start` is idempotent per name — and `value.already_running` is `true`.
2. **Ports.** Every port in `ports`, plus the `ready` port (or the port of
   a `ready` URL on `localhost`/`127.0.0.1`/`::1`), must be free: not
   claimed by another service this plugin is running, and not held by any
   other process. Otherwise `start` fails without starting anything and
   reports each holder's pid, command and working directory (via `lsof`
   when it is installed; without `lsof`, a connect check still detects the
   port as taken, without those details). **A holder is never signalled.**
3. **Start.** The process runs in a new session and process group, with
   stdin from `/dev/null` and its output appended to `log`. It is not part
   of the run's own process group, so a Ctrl-C in the terminal does not
   reach it.
4. **Record.** The ownership record is written before the readiness wait.
5. **Ready.** It waits for `ready`. If the process exits first, `start`
   fails with its exit code and the tail of its log; if it is not ready
   within `ready_timeout_ms`, `start` fails too. Either way the process
   group it started is stopped again and the record removed. If the run is
   cancelled (Ctrl-C, SIGTERM) during the wait, the group is stopped before
   the cancellation goes on. This cleanup only stops the group this `start`
   started: if another run stopped and restarted the service meanwhile, the
   new one is left alone.

`value`: `{name, running, ready, already_running, pgid, ports, log,
started_at}`. A failed start returns `ok: false` (so `on_error` applies)
with `meta.raw.port_holders` or `meta.raw.log_tail`.

## `stop`

Stops the named service only if its record still proves it is ours:
SIGTERM to the whole process group, up to `grace_ms` (default `5000`) for
it to exit, then SIGKILL to whatever is left of the group — a child that
ignores SIGTERM does not survive. It then waits (up to a few seconds) for
the recorded ports to be free.

`stop` is idempotent: no record, or a stale one, is `ok` with
`value.stopped: false`. `value`: `{name, stopped, running, pgid, signal,
ports_free, port_holders, stale_record}` — `signal` is the last one sent
(`SIGTERM`, or `SIGKILL` when the grace period ran out); `ports_free` is
`false`, with `port_holders`, if something else took a port over once the
group was gone. Only a group that survives SIGKILL fails `stop`.

## `status`

`value`: `{name, running, ready, pgid, ports, log, started_at,
port_holders, stale_record}`. `ready` re-runs the start's own readiness
check (`null` when it had none); each entry of `port_holders` carries
`ours: true` when it belongs to this service's process group. A stale
record is dropped and reported in `stale_record`, never an error.

## Ownership

Every running service has a record, `<state_dir>/<name>.json`, holding its
process group id, the group leader's start time, the machine's boot time,
its ports, its log path and its command. Records are claimed and written
under an exclusive lock (`flock` on `<state_dir>/.lock`), so two runs can
neither both start the same service nor take over each other's.

Before any signal, the record must prove the group is still the one this
plugin started:

- **Boot time differs** — the machine has rebooted: the record is stale.
- **The leader is alive with the recorded start time** — ours.
- **The leader is alive with a different start time** — its pid now
  belongs to an unrelated process. Not ours: never signalled, the record
  is dropped.
- **The leader is gone but its process group still has members** — still
  ours: a pid cannot be reused while a process group with that id exists.
- **The group is gone** — the record is stale and dropped. A group whose
  only members are zombies (exited, but not yet reaped by the run that
  started them, which may still be running) counts as gone.

A stale record is dropped silently by `start`, `stop` and `status` alike.

The state directory is `runtime.plugins.service.state_dir` in config,
default `~/.config/circuitry/services` (beside the global config):

```json
{"runtime": {"plugins": {"service": {"state_dir": "~/.local/state/circuitry/services"}}}}
```

## Stopping it when the run ends

A service outlives the run on purpose — a later run can use it, or stop it.
To stop it when the run ends, win or lose, put the `stop` in a
[`finally:`](../guidebook/05-errors.md#finally--cleanup-that-always-runs)
block. `finally:` runs on success, on failure and on Ctrl-C/SIGTERM. A
second Ctrl-C while `finally:` runs exits at once: the service then keeps
running with its record, and a later `stop` still finds it.

```yaml
- type: dynamic
  name: with_web
  effects:
    - type: tool
      name: start_web
      provider: service
      params:
        action: start
        name: web
        command: [python3, -m, http.server, "8000", --bind, 127.0.0.1]
        ports: [8000]
        ready: "http://127.0.0.1:8000/"
    - type: tool
      name: page
      provider: http
      params: {url: "http://127.0.0.1:8000/"}
  finally:
    - type: tool
      name: stop_web
      provider: service
      params: {action: stop, name: web}
```

## Capabilities and platforms

`service` needs the `shell` and `network` capabilities (it runs a command,
and an `http(s)` readiness check reaches a URL): a fetched or referenced
document asks before using it (see the capability-consent table in the
[orchestration reference](../orchestration-reference.md#tool)).

`service` runs any command it is given — an argv list, or a `/bin/sh -c`
string with `shell: true`. The host pin `runtime.plugins.shell.allowed_commands`
applies to the `shell` plugin only, not to `service`: where that pin is
what you rely on, leave `service` out of the `enabled_tools` allowlist.

POSIX only (macOS, Linux). The leader's start time comes from
`/proc/<pid>/stat` on Linux and `ps -o lstart=` elsewhere; the boot time
from `/proc/stat` or `sysctl -n kern.boottime`. `lsof` is optional.
