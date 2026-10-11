//! #471 review finding 2: a malformed private `.env` must never put a
//! value on stderr or stdout -- `user_env.rs`'s own unit tests already
//! check the in-process result (`UserEnvResult.skipped`, the real
//! environment left untouched); these run the actual binary, the only
//! way to see what a panicking `std::env::set_var` (no custom hook)
//! would have put on a real process's stderr.

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
            "electricity-cli-secret-leak-test-{tag}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        TempHome { path }
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(unix)]
fn write_user_env(home: &std::path::Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    let dir = home.join(".config").join("circuitry");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(".env");
    fs::write(&path, text).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn run(home_dir: &std::path::Path, case_dir: &std::path::Path) -> std::process::Output {
    fs::write(
        case_dir.join("orchestration.yml"),
        "effects:\n  - name: ok\n    type: tool\n    provider: json\n    params: {mode: parse, input: '{}'}\n",
    )
    .unwrap();
    fs::write(case_dir.join("config.json"), "{}").unwrap();

    let mut cmd = Command::new(bin());
    cmd.env_clear();
    cmd.env("HOME", home_dir);
    cmd.env("PATH", env::var("PATH").unwrap_or_default());
    cmd.current_dir(case_dir);
    cmd.arg("config.json").arg("orchestration.yml");
    cmd.output().expect("electricity should run")
}

#[cfg(unix)]
#[test]
fn a_single_quoted_key_containing_equals_never_prints_its_value() {
    let home = TempHome::new("key-equals");
    let case_dir = home.path.join("case");
    fs::create_dir_all(&case_dir).unwrap();
    write_user_env(&home.path, "'A=B'=s3cr3t-probe\n");

    let output = run(&home.path, &case_dir);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_ne!(
        output.status.code(),
        Some(101),
        "electricity panicked on a key containing '='; stderr: {stderr}"
    );
    assert!(
        !stdout.contains("s3cr3t-probe"),
        "secret leaked to stdout: {stdout}"
    );
    assert!(
        !stderr.contains("s3cr3t-probe"),
        "secret leaked to stderr: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn a_nul_byte_in_a_value_never_prints_its_value() {
    let home = TempHome::new("nul-value");
    let case_dir = home.path.join("case");
    fs::create_dir_all(&case_dir).unwrap();
    write_user_env(
        &home.path,
        "K_468_SECRET_LEAK=before_nul_s3cr3t\u{0}after_nul\n",
    );

    let output = run(&home.path, &case_dir);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_ne!(
        output.status.code(),
        Some(101),
        "electricity panicked on a NUL byte in a value; stderr: {stderr}"
    );
    assert!(
        !stdout.contains("s3cr3t"),
        "secret leaked to stdout: {stdout}"
    );
    assert!(
        !stderr.contains("s3cr3t"),
        "secret leaked to stderr: {stderr}"
    );
}
