"""Capability tags for tool plugins — what a document's use of one can do
to the host, independent of the plugin's own per-call parameters.

Read by the capability-consent gate (:mod:`circuitry.cli.document_consent`)
to work out, statically, which capabilities a document's tool effects need
before any of it runs. The tag lives here, next to
:data:`circuitry.plugins.factory.PLUGIN_REGISTRY`, keyed by the same plugin
name — one registry, so the two can never drift apart.

Four capabilities, matching the ones #275 names and the ones a consent
prompt can meaningfully explain to a user:

* ``shell`` — runs an external binary with effect-supplied arguments
  (covers the dedicated ``shell`` plugin and every "subprocess wrapper"
  plugin in :mod:`circuitry.plugins.factory` — a fixed binary with
  structured args is still a general-purpose execution surface, and most of
  these can write arbitrary files as a side effect of running).
* ``python_eval`` — evaluates Python source the document supplies.
* ``fs-write`` — writes or deletes arbitrary local paths without shelling
  out (the stdlib-only archive/filesystem plugins; a *read* of a path the
  document names is not gated — only a plugin that can create, overwrite or
  remove one is).
* ``network`` — reaches a remote host: an HTTP(S) API, a cloud/SaaS SDK, a
  DNS/whois/ping-style probe, or a download.

Many plugins carry more than one tag (``git`` writes to the working tree
*and* can push over the network; ``ffmpeg`` shells out *and* writes its
output file) — the consent prompt lists the union for the whole document.
A plugin absent from :data:`PLUGIN_CAPABILITIES` needs none: it only reads
its own parameters and returns a value (``math``, ``regex``, ``json``, ...).
"""

from __future__ import annotations

SHELL = "shell"
PYTHON_EVAL = "python_eval"
FS_WRITE = "fs-write"
NETWORK = "network"

#: Every capability the consent gate understands, in the order a prompt
#: lists them.
CAPABILITIES: tuple[str, ...] = (SHELL, PYTHON_EVAL, FS_WRITE, NETWORK)

#: Tool-plugin registry name -> the capabilities using it requires. Keyed
#: the same as :data:`circuitry.plugins.factory.PLUGIN_REGISTRY`; a name
#: absent here needs none (see module docstring).
PLUGIN_CAPABILITIES: dict[str, frozenset[str]] = {
    # The dedicated shell-execution and Python-evaluation plugins.
    "shell": frozenset({SHELL}),
    "python_eval": frozenset({PYTHON_EVAL}),
    # Subprocess wrappers: a binary on PATH, run with effect-supplied args.
    "docker": frozenset({SHELL, NETWORK}),
    "kubectl": frozenset({SHELL, NETWORK}),
    "gh": frozenset({SHELL, NETWORK}),
    "git": frozenset({SHELL, NETWORK}),
    "yt_dlp": frozenset({SHELL, NETWORK}),
    "ripgrep": frozenset({SHELL}),
    "pytest": frozenset({SHELL}),
    "awk": frozenset({SHELL}),
    "sed": frozenset({SHELL}),
    "pandoc": frozenset({SHELL}),
    "mediainfo": frozenset({SHELL}),
    "imagemagick": frozenset({SHELL}),
    "exiftool": frozenset({SHELL}),
    "7z": frozenset({SHELL}),
    "ping": frozenset({SHELL, NETWORK}),
    "traceroute": frozenset({SHELL, NETWORK}),
    "linter": frozenset({SHELL}),
    "ocr": frozenset({SHELL}),
    "gpg": frozenset({SHELL}),
    "diff_patch": frozenset({SHELL}),
    "pdf_render": frozenset({SHELL}),
    "ffmpeg": frozenset({SHELL}),
    # Starts a long-running process; an http(s) readiness check reaches a URL.
    "service": frozenset({SHELL, NETWORK}),
    # Stdlib-only filesystem writers (no subprocess).
    "fs": frozenset({FS_WRITE}),
    "tar": frozenset({FS_WRITE}),
    "zip": frozenset({FS_WRITE}),
    "gzip": frozenset({FS_WRITE}),
    "vector_search": frozenset({FS_WRITE}),
    # Network-reaching plugins: HTTP family, cloud/SaaS SDKs, probes.
    "comfyui": frozenset({NETWORK}),
    "http": frozenset({NETWORK}),
    "email_smtp": frozenset({NETWORK}),
    "port_check": frozenset({NETWORK}),
    "dns": frozenset({NETWORK}),
    "whois": frozenset({NETWORK}),
    "rss": frozenset({NETWORK}),
    "wikipedia": frozenset({NETWORK}),
    "webhook": frozenset({NETWORK}),
    "web_fetch": frozenset({NETWORK}),
    "web_search": frozenset({NETWORK}),
    "weather": frozenset({NETWORK}),
    "s3": frozenset({NETWORK}),
    "surrealdb": frozenset({NETWORK}),
    "mcp": frozenset({NETWORK}),
    "linear": frozenset({NETWORK}),
    "slack": frozenset({NETWORK}),
    "discord": frozenset({NETWORK}),
    "github": frozenset({NETWORK}),
    "jira": frozenset({NETWORK}),
    "notion": frozenset({NETWORK}),
    "gcalendar": frozenset({NETWORK}),
    "gdrive": frozenset({NETWORK, FS_WRITE}),
    "playwright": frozenset({NETWORK}),
    "screenshot": frozenset({NETWORK, FS_WRITE}),
}

#: Every :data:`circuitry.plugins.factory.PLUGIN_REGISTRY` name *not* in
#: :data:`PLUGIN_CAPABILITIES` — a deliberate "needs nothing" classification,
#: not the absence of one. ``test_every_plugin_registry_entry_is_classified``
#: (``tests/plugins/test_capabilities.py``) asserts every registry key is in
#: exactly one of this set and :data:`PLUGIN_CAPABILITIES`, so a newly added
#: plugin fails CI until someone puts it in one or the other — the tag table
#: can drift by omission (a plugin nobody classified) as easily as by a typo,
#: and only this closes that half.
NO_CAPABILITIES: frozenset[str] = frozenset(
    {
        "clock",
        "math",
        "regex",
        "json",
        "csv",
        "env_vars",
        "hash",
        "base64",
        "hex",
        "uuid",
        "validate_yaml",
        "pdf_extract",
        "xml",
        "html_extract",
        "system_info",
        "process_list",
        # Debatable (#275 review): both can download a model file over the
        # network on first use. Left unclassified pending an explicit
        # product decision rather than guessed at here.
        "embed",
        "rerank",
    }
)


def capabilities_of(plugin_name: str) -> frozenset[str]:
    """The capabilities referencing tool plugin *plugin_name* requires.

    An unrecognized or untagged name needs none — the empty set, never an
    error; the consent gate cares only about the four it names.
    """
    return PLUGIN_CAPABILITIES.get((plugin_name or "").strip().lower(), frozenset())
