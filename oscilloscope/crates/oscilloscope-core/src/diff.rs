//! Turns a live-state snapshot diff, or an `--events` line, into log
//! lines (DESIGN.md §2.4), sorted by `meta.created_at`/`completed_at`
//! rather than observation order. O-1 work.
