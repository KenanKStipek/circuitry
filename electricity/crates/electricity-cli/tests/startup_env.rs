//! Issue #468's startup-crash comment: `std::env::vars()` panics on
//! any environment variable whose name or value isn't valid UTF-8, so
//! a real `electricity` run in such an environment used to exit 101
//! before doing anything -- fixed by switching every `std::env::vars()`/
//! `var()` on the startup path (`electricity::current_env_vars`,
//! `electricity_cli::user_env::load_user_env`) to `vars_os()`/
//! `var_os()`. `cof` never had this problem: Python decodes
//! `os.environ` with `surrogateescape`, never raising.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_electricity"))
}

struct TempHome {
    path: PathBuf,
}

impl TempHome {
    fn new(tag: &str) -> Self {
        let path = env::temp_dir().join(format!(
            "electricity-cli-startup-env-test-{tag}-{}",
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
#[test]
fn electricity_does_not_panic_on_a_non_utf8_environment_variable() {
    let home = TempHome::new("non-utf8-value");
    let case_dir = home.path.join("case");
    fs::create_dir_all(&case_dir).unwrap();
    let home_dir = home.path.join("home");
    fs::create_dir_all(&home_dir).unwrap();

    fs::write(
        case_dir.join("orchestration.yml"),
        "effects:\n  - name: ok\n    type: tool\n    provider: json\n    params: {mode: parse, input: '{}'}\n",
    )
    .unwrap();
    fs::write(case_dir.join("config.json"), "{}").unwrap();

    // The issue's own repro: `X=$'\xff'` exported -- a single
    // non-UTF-8 byte is never valid UTF-8 on its own, in a variable
    // `resolve_config` never even looks at.
    let non_utf8_value = OsStr::from_bytes(&[0xff]);

    let mut cmd = Command::new(bin());
    cmd.env_clear();
    cmd.env("HOME", &home_dir);
    cmd.env("PATH", env::var("PATH").unwrap_or_default());
    cmd.env("X_468_NON_UTF8", non_utf8_value);
    cmd.current_dir(&case_dir);
    cmd.arg("config.json").arg("orchestration.yml");

    let output = cmd.output().expect("electricity should run");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_ne!(
        output.status.code(),
        Some(101),
        "electricity panicked (exit 101) on a non-UTF-8 environment variable; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("panicked at"),
        "electricity panicked on a non-UTF-8 environment variable; stderr: {stderr}"
    );
    assert!(
        output.status.success(),
        "electricity should still run the document successfully; stderr: {stderr}, stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[cfg(unix)]
#[test]
fn a_non_utf8_environment_variable_does_not_stop_the_dump_ir_path_either() {
    let home = TempHome::new("non-utf8-dump-ir");
    let case_dir = home.path.join("case");
    fs::create_dir_all(&case_dir).unwrap();
    let home_dir = home.path.join("home");
    fs::create_dir_all(&home_dir).unwrap();

    fs::write(
        case_dir.join("orchestration.yml"),
        "effects:\n  - name: ok\n    type: tool\n    provider: json\n    params: {mode: parse, input: '{}'}\n",
    )
    .unwrap();
    fs::write(case_dir.join("config.json"), "{}").unwrap();

    let non_utf8_value = OsStr::from_bytes(&[0xfe, 0xff]);

    let mut cmd = Command::new(bin());
    cmd.env_clear();
    cmd.env("HOME", &home_dir);
    cmd.env("PATH", env::var("PATH").unwrap_or_default());
    cmd.env("Y_468_NON_UTF8", non_utf8_value);
    cmd.current_dir(&case_dir);
    cmd.arg("config.json")
        .arg("orchestration.yml")
        .arg("--dump-ir");

    let output = cmd.output().expect("electricity should run");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_ne!(
        output.status.code(),
        Some(101),
        "panicked; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("panicked at"),
        "panicked; stderr: {stderr}"
    );
    assert!(output.status.success(), "stderr: {stderr}");
}

#[cfg(unix)]
#[test]
fn a_non_utf8_environment_variable_does_not_stop_the_private_dotenv_from_loading() {
    // Every test above leaves no `.env` in its
    // temp `HOME`, so `user_env::load_user_env_at`'s own `vars_os()`
    // snapshot (taken for `${VAR}` interpolation) is never actually
    // exercised with a non-UTF-8 entry in it -- this one puts a real
    // `.env` there too.
    let home = TempHome::new("non-utf8-with-dotenv");
    let case_dir = home.path.join("case");
    fs::create_dir_all(&case_dir).unwrap();
    let home_dir = home.path.join("home");
    let config_dir = home_dir.join(".config").join("circuitry");
    fs::create_dir_all(&config_dir).unwrap();

    fs::write(
        case_dir.join("orchestration.yml"),
        "effects:\n  - name: ok\n    type: tool\n    provider: json\n    params: {mode: parse, input: '{}'}\n",
    )
    .unwrap();
    fs::write(case_dir.join("config.json"), "{}").unwrap();

    let env_path = config_dir.join(".env");
    fs::write(&env_path, "STARTUP_468_FROM_FILE=loaded\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&env_path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    let non_utf8_value = OsStr::from_bytes(&[0xff]);

    let mut cmd = Command::new(bin());
    cmd.env_clear();
    cmd.env("HOME", &home_dir);
    cmd.env("PATH", env::var("PATH").unwrap_or_default());
    cmd.env("Z_468_NON_UTF8", non_utf8_value);
    cmd.current_dir(&case_dir);
    cmd.arg("config.json").arg("orchestration.yml");

    let output = cmd.output().expect("electricity should run");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_ne!(
        output.status.code(),
        Some(101),
        "electricity panicked with a non-UTF-8 variable and a real .env present; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("panicked at"),
        "panicked; stderr: {stderr}"
    );
    assert!(output.status.success(), "stderr: {stderr}");
}
