//! Spawning the engine (`process_group(0)`, `stdin` null, piped
//! `stdout`/`stderr`), signal forwarding, and mapping its exit status
//! back to `osp`'s own (DESIGN.md §4.1 and §6.1). POSIX only (design
//! Q10). O-1 work.
