//! `electricity/docs/spec/scripted-replies.md` §7's export, replayed
//! against real [`ScriptedAdapter`] instances -- a dedicated
//! integration test binary (its own process), never a unit test
//! inside `src/lib.rs`'s own test module: both
//! [`electricity_adapters::LEFTOVER_REPLIES_EXPORT_ENV_VAR`] and the
//! crate's own export registry are process-global state, and this
//! file's own tests mutate both, so sharing a process with any other
//! test that touches either would race (the same reason
//! `electricity-tools`' own `env_vars` corpus replay runs its cases
//! inside one `#[test]` function rather than several).

use electricity_value::{CancellationToken, Value};
use electricity_vm::adapter::{Adapter, CheckResult, GenerateOptions};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "electricity-adapters-export-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn join(&self, name: &str) -> std::path::PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write(dir: &TempDir, name: &str, contents: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, contents).unwrap();
    path.to_string_lossy().into_owned()
}

/// SAFETY: this whole test file runs as a single test (never two
/// `#[test]` functions touching this variable in the same process),
/// and no other binary shares this process -- see the module doc
/// comment.
unsafe fn set_export_var(value: &str) {
    unsafe { std::env::set_var("CIRCUITRY_TEST_SCRIPTED_LEFTOVER_REPLIES_FILE", value) };
}

unsafe fn unset_export_var() {
    unsafe { std::env::remove_var("CIRCUITRY_TEST_SCRIPTED_LEFTOVER_REPLIES_FILE") };
}

fn read_json_object(path: &std::path::Path) -> std::collections::BTreeMap<String, i64> {
    let text = std::fs::read_to_string(path).expect("export file should exist");
    let value = electricity_json::loads(&text).expect("export file should be valid JSON");
    let dict = value
        .as_dict()
        .expect("export file should be a JSON object");
    dict.iter()
        .map(|(k, v)| {
            (
                k.as_str().unwrap().to_string(),
                v.as_int().unwrap().to_bigint().try_into().unwrap(),
            )
        })
        .collect()
}

async fn load_via_check(adapter: &dyn Adapter) -> CheckResult {
    adapter.check().await
}

async fn load_via_generate_one(adapter: &dyn Adapter, path_str: &str) {
    let token = CancellationToken::new();
    let path = electricity_bytecode::EffectPath::root().push_name(
        path_str
            .strip_prefix("prime.")
            .expect("test path must start with prime."),
    );
    let call = electricity_vm::adapter::CallContext {
        path: &path,
        timeout_seconds: 30,
        token: &token,
    };
    adapter
        .generate("m", "p", &GenerateOptions::default(), &call)
        .await
        .expect("generate should succeed in this test");
}

#[tokio::test]
async fn the_leftover_export_behaves_per_scripted_replies_md_7() {
    // 1. Unset: `write_leftover_export` is a no-op (no file appears).
    unsafe { unset_export_var() };
    electricity_adapters::write_leftover_export();

    // 2. Set, with no instance ever registered: still no file (the
    // variable alone changes nothing; registration happens on load).
    let dir = TempDir::new();
    let export_path = dir.join("export-empty.json");
    unsafe { set_export_var(&export_path.to_string_lossy()) };
    electricity_adapters::write_leftover_export();
    assert!(
        !export_path.exists(),
        "no instance registered, so nothing should be written"
    );

    // 3. An unwritable target never panics and never masks anything.
    let unwritable = dir.join("no/such/directory/export.json");
    unsafe { set_export_var(&unwritable.to_string_lossy()) };
    electricity_adapters::write_leftover_export();
    assert!(!unwritable.exists());

    // 4. The real scenario: two instances share one replies file.
    // `a` consumes one of `prime.x`'s two replies and all of
    // `prime.y`'s one; `b` only calls `check()` (a preflight-only
    // instance, full queue, never consumed) -- its vote must never
    // pull `prime.x`/`prime.y`'s reported count back up, and
    // `prime.z` (never touched by either instance) must appear with
    // its full, untouched count.
    let replies = write(
        &dir,
        "replies.yaml",
        "prime.x:\n  - text: \"one\"\n  - text: \"two\"\nprime.y:\n  - text: \"only\"\n\
         prime.z:\n  - text: \"untouched\"\n",
    );
    let export_path = dir.join("export-real.json");
    unsafe { set_export_var(&export_path.to_string_lossy()) };

    let a = electricity_adapters::scripted::build_scripted(&{
        let mut cfg = electricity_value::Dict::new();
        cfg.insert(
            Value::Str("replies_file".to_string()),
            Value::Str(replies.clone()),
        );
        Value::Dict(cfg)
    })
    .unwrap();
    let b = electricity_adapters::scripted::build_scripted(&{
        let mut cfg = electricity_value::Dict::new();
        cfg.insert(Value::Str("replies_file".to_string()), Value::Str(replies));
        Value::Dict(cfg)
    })
    .unwrap();

    load_via_generate_one(a.as_ref(), "prime.x").await;
    load_via_generate_one(a.as_ref(), "prime.y").await;
    let check_result = load_via_check(b.as_ref()).await;
    assert!(check_result.ok);

    electricity_adapters::write_leftover_export();
    let merged = read_json_object(&export_path);
    // `prime.x`: `a` has 1 left (consumed one of two); `b` (check-only)
    // has 2 left -- the minimum is 1.
    assert_eq!(merged.get("prime.x"), Some(&1));
    // `prime.y`: `a` has 0 left (fully consumed); `b` has 1 -- the
    // minimum is 0, so this path is omitted entirely (only nonzero
    // counts are kept).
    assert_eq!(merged.get("prime.y"), None);
    // `prime.z`: neither instance ever consumed from it, so both
    // report the full count (1) -- min is 1, kept.
    assert_eq!(merged.get("prime.z"), Some(&1));
    assert_eq!(merged.len(), 2);

    unsafe { unset_export_var() };
}
