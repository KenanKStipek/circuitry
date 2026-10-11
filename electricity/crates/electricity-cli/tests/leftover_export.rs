//! Issue #478's own leftover-replies export (`electricity/docs/spec/
//! scripted-replies.md` §7): `electricity::write_leftover_export`'s own
//! call site in `main` runs on every exit path a document that never
//! loads a `scripted` adapter reaches here -- 0, 1 and 2. (The three
//! signal exits, 129/130/143, share this exact same call site through
//! `run_action`'s own `exit_code_for`; one of them is additionally
//! covered end to end in `tests/signals.rs`, `test-tools`-gated.) No
//! document can make the adapter load until lane G flips `prompt`, so
//! every case here only proves the hook runs without panicking or
//! wrongly creating a file -- never that the export carries real
//! content. That end-to-end case is lane G's own, said in the PR per
//! issue #478's own test-plan note.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_electricity"))
}

struct TempHome {
    path: PathBuf,
}

impl TempHome {
    fn new(tag: &str) -> Self {
        let path = env::temp_dir().join(format!(
            "electricity-leftover-export-test-{tag}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp HOME");
        Self { path }
    }

    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.path.join(name);
        fs::write(&path, contents).unwrap();
        path
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn command(tag: &str, export_path: &PathBuf) -> (Command, TempHome) {
    let home = TempHome::new(tag);
    let mut cmd = Command::new(bin());
    cmd.env_clear();
    cmd.env("HOME", &home.path);
    cmd.env("PATH", env::var("PATH").unwrap_or_default());
    cmd.env("CIRCUITRY_TEST_SCRIPTED_LEFTOVER_REPLIES_FILE", export_path);
    (cmd, home)
}

#[test]
fn an_ordinary_successful_run_exits_zero_and_writes_no_export() {
    let export = env::temp_dir().join(format!(
        "electricity-leftover-export-ok-{}.json",
        std::process::id()
    ));
    let (mut cmd, home) = command("ok", &export);
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", "effects: []\n");
    let output = cmd
        .args([config.to_str().unwrap(), doc.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(
        !export.exists(),
        "no scripted adapter ever loaded, so the export must stay absent"
    );
    let _ = fs::remove_file(&export);
}

#[test]
fn an_ordinary_run_failure_exits_one_and_writes_no_export() {
    let export = env::temp_dir().join(format!(
        "electricity-leftover-export-fail-{}.json",
        std::process::id()
    ));
    let (mut cmd, home) = command("fail", &export);
    let config = home.write("config.json", "{}");
    let doc = home.write(
        "doc.yml",
        "effects:\n  - name: broken\n    type: tool\n    provider: json\n    params: {mode: parse, input: 123}\n",
    );
    let output = cmd
        .args([config.to_str().unwrap(), doc.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        !export.exists(),
        "no scripted adapter ever loaded, so the export must stay absent"
    );
    let _ = fs::remove_file(&export);
}

#[test]
fn a_usage_error_exits_two_and_writes_no_export() {
    let export = env::temp_dir().join(format!(
        "electricity-leftover-export-usage-{}.json",
        std::process::id()
    ));
    let (mut cmd, _home) = command("usage", &export);
    let output = cmd.arg("--bogus").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        !export.exists(),
        "a usage error runs the export hook too, but nothing ever registers"
    );
    let _ = fs::remove_file(&export);
}
