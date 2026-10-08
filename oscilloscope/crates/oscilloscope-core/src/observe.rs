//! The live-state poller and the `--events` tailer (DESIGN.md §3 and
//! §6.1): polling every 100 ms rather than `notify`, since the live
//! file is replaced by rename on every write, and reading a partial
//! JSONL line safely at EOF while a write is in progress. O-1 work.
