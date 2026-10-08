//! Which stream `osp`'s own CLI-parsing errors land on (#422 review
//! note): a usage error (exit 2) must print to stderr, same as every
//! other error osp reports; `--help`/`--version` (exit 0) still print
//! to stdout, like any other successful output. Needs no `cof`.

use std::process::{Command, Stdio};

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_osp"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("spawn osp")
}

#[test]
fn a_usage_error_prints_to_stderr_not_stdout() {
    let out = run(&["--bogus-flag", "do-thing.yml"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        out.stdout.is_empty(),
        "stdout should be empty: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !out.stderr.is_empty(),
        "the usage error should be on stderr"
    );
}

#[test]
fn no_args_prints_the_usage_error_to_stderr() {
    let out = run(&[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(!out.stderr.is_empty());
}

#[test]
fn help_prints_to_stdout_not_stderr() {
    let out = run(&["--help"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(!out.stdout.is_empty());
    assert!(
        out.stderr.is_empty(),
        "stderr should be empty: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn version_prints_to_stdout_not_stderr() {
    let out = run(&["--version"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(!out.stdout.is_empty());
    assert!(out.stderr.is_empty());
}
