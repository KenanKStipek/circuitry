//! Lane D: ports `core/prompt_files.py` — reading a `{file: ...}`
//! prompt source at compile time: literal/relative paths with no
//! `{{ }}`, confinement to the nearest `circuitry.config.json`/
//! `config.json` (checked after following symlinks, non-strict
//! resolve, before the existence check), missing/not-a-regular-file/
//! over-1-MiB/not-UTF-8/unreadable errors, universal-newline
//! translation, and the refusal for [`crate::DocumentOrigin::Generated`].
