//! The CLI's help and report text, pinned. After an intended change, review
//! the differences with `cargo insta review`.
//!
//! Reports read the ncdu dumps in `tests/fixtures/cli`, so sizes, ages and
//! paths are the same on every filesystem and every day. Unix only: the
//! dumps hold Unix paths.
#![cfg(unix)]

use std::path::Path;
use std::process::Command;

const GRAV: &str = env!("CARGO_BIN_EXE_grav");

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/cli")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

/// What a terminal would show for `grav ARGS`: exit code, stdout, stderr,
/// with the fixture directory as `[FIXTURES]/`.
fn run(args: &[&str]) -> String {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(GRAV)
        .args(args)
        // Isolate from the user's settings, history and environment.
        .env("GRAVITYFILE_HOME", home.path().join("home"))
        .env("NO_COLOR", "1")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("COLUMNS")
        .env_remove("GRAVITYFILE_LOG_FILE")
        .output()
        .expect("grav starts");
    let shown = format!(
        "exit: {}\n--- stdout\n{}--- stderr\n{}",
        output
            .status
            .code()
            .map_or("signal".into(), |code| code.to_string()),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    shown.replace(&fixture(""), "[FIXTURES]/")
}

/// Snapshot `grav ARGS` under `name`. Times are local, so they become
/// `[TIME]`.
fn check(name: &str, args: &[&str]) {
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path("snapshots");
    settings.set_prepend_module_to_snapshot(false);
    let command = format!("grav {}", args.join(" "));
    settings.set_description(command.replace(&fixture(""), ""));
    settings.add_filter(r"\d{4}-\d{2}-\d{2} \d{2}:\d{2}", "[TIME]");
    settings.bind(|| insta::assert_snapshot!(name, run(args)));
}

#[test]
fn help() {
    check("help", &["--help"]);
    for command in [
        "scan",
        "duplicates",
        "age",
        "diff",
        "history",
        "export",
        "init",
        "completions",
    ] {
        check(&format!("help-{command}"), &[command, "--help"]);
    }
}

#[test]
fn scan() {
    let before = fixture("before.json");
    check("scan", &["scan", &before]);
    check("scan-apparent", &["scan", "-A", &before]);
    check("scan-binary", &["scan", "--binary", &before]);
    check("scan-depth", &["scan", "--depth", "2", &before]);
    check("scan-top", &["scan", "--top", "3", &before]);
    check("scan-by-type", &["scan", "--by-type", &before]);
}

#[test]
fn age() {
    let before = fixture("before.json");
    check("age", &["age", &before]);
    check("age-stale", &["age", "--stale", "30d", &before]);
}

#[test]
fn diff() {
    let (before, after) = (fixture("before.json"), fixture("after.json"));
    check("diff", &["diff", &before, &after]);
    check("diff-reversed", &["diff", &after, &before]);
}

#[test]
fn errors() {
    check("error-missing-path", &["scan", "/nonexistent/gravityfile"]);
    check("error-ambiguous-age", &["age", "--stale", "6m"]);
    check(
        "error-scan-snapshot-explorer",
        &["--scan", &fixture("before.json")],
    );
}
