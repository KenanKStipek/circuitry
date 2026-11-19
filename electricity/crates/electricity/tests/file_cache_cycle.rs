//! `electricity::run_orchestration`'s own `enter()` (#405): the whole
//! document-load-through-compile span, inside
//! `run_orchestration_with_config`, shares one cache -- the document is
//! read from disk exactly once per run, not once for the load and again
//! for the digest `post_state_checks` attaches to it afterward. Catches a
//! regression that removes this call's own `enter()` directly, not just a
//! change to `electricity_compiler::file_cache` itself (that module's own
//! unit tests, and `electricity_compiler::pipeline`'s own
//! `check_for_run_reads_the_document_from_disk_exactly_once`, exercise it
//! without going through `run_orchestration` at all).

use electricity::run::RunRequest;
use electricity_vm::CancellationToken;
use indexmap::IndexMap;
use std::fs;
use std::path::{Path, PathBuf};

/// A fresh, empty directory under the OS temp dir, removed again when
/// this guard drops -- same pattern as `tests/resume.rs`'s own.
struct TempDir(PathBuf);

impl std::ops::Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temp_dir(name: &str) -> TempDir {
    let dir = std::env::temp_dir().join(format!(
        "electricity-file-cache-cycle-test-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, contents).unwrap();
    path
}

fn request(config_path: PathBuf, orchestration_path: PathBuf) -> RunRequest {
    RunRequest {
        config_path: Some(config_path),
        orchestration_path,
        inputs: IndexMap::new(),
        out_path: None,
        pretty: false,
        live_state_path: None,
        events_path: None,
        profile_path: None,
        allow_capabilities: Vec::new(),
        config_warnings: Vec::new(),
        adapter_override: None,
        label_override: None,
        armed: true,
        state_path: None,
        resume: false,
        resume_default_out: None,
        force: false,
    }
}

async fn run(req: &RunRequest) -> electricity::RunResult {
    let token = CancellationToken::new();
    electricity::run_orchestration(req, &token).await
}

#[tokio::test]
async fn run_reads_the_document_from_disk_exactly_once() {
    let dir = temp_dir("run");
    let config = write(&dir, "config.json", "{}");
    // `provider: uuid`: no subprocess, no capability gate -- same reason
    // `tests/resume.rs`'s own documents use it.
    let doc = write(
        &dir,
        "doc.yml",
        "effects:\n  - type: tool\n    name: s\n    provider: uuid\n",
    );

    electricity_compiler::file_cache::reset_read_counts();
    let result = run(&request(config, doc.clone())).await;
    assert!(result.ok, "{:?}", result.error);
    assert_eq!(electricity_compiler::file_cache::read_count(&doc), 1);
}
