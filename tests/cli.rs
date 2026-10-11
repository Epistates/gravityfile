use std::process::{Command, Output};

const GRAV: &str = env!("CARGO_BIN_EXE_grav");

fn grav() -> Command {
    let mut command = Command::new(GRAV);
    command.env_remove("NO_COLOR").env_remove("CLICOLOR_FORCE");
    command
}

fn run(args: &[&str]) -> Output {
    grav()
        .args(args)
        .output()
        .expect("CLI process should start")
}

fn json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{error}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// Arguments that emit JSON for `command`.
fn json_args(command: &str) -> &'static [&'static str] {
    match command {
        "export" => &[],
        "duplicates" => &["-f", "json", "--min-size", "0"],
        _ => &["-f", "json"],
    }
}

#[cfg(target_os = "linux")]
#[test]
fn all_report_commands_encode_native_root_and_file_paths() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture
        .path()
        .join(std::ffi::OsString::from_vec(b"root-\xff".to_vec()));
    std::fs::create_dir(&root).unwrap();
    for name in [b"file-\xfe", b"file-\xff"] {
        std::fs::write(
            root.join(std::ffi::OsString::from_vec(name.to_vec())),
            b"identical",
        )
        .unwrap();
    }
    for command in ["scan", "duplicates", "age"] {
        let mut process = grav();
        process.arg(command).arg(&root);
        if command == "duplicates" {
            process.args(["--min-size", "0"]);
        }
        let output = process.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains(r"root-\xff"), "{text:?}");
        assert!(!text.contains('\u{fffd}'), "{text:?}");
        if command != "age" {
            assert!(text.contains(r"file-\xfe"), "{text:?}");
            assert!(text.contains(r"file-\xff"), "{text:?}");
        }
    }
    for command in ["scan", "export", "duplicates", "age"] {
        let output = grav()
            .arg(command)
            .arg(&root)
            .args(json_args(command))
            .arg("--strict")
            .output()
            .unwrap();
        assert!(output.status.success(), "{command}: {output:?}");
        let report = json(&output);
        let expected = serde_json::json!({"platform":"unix","bytes":root.as_os_str().as_bytes()});
        assert_eq!(report["root"], expected);
        assert_eq!(report["scope"]["root"], expected);
        assert_eq!(report["status"]["state"], "complete");
        if command == "duplicates" {
            let paths = report["data"]["groups"][0]["paths"].as_array().unwrap();
            assert_eq!(paths.len(), 2);
            assert_ne!(paths[0], paths[1]);
            assert!(paths.iter().all(|path| path["platform"] == "unix"));
        }
        if command == "scan" {
            let entries = report["data"]["entries"].as_array().unwrap();
            assert!(
                entries
                    .iter()
                    .all(|entry| entry["path"]["platform"] == "unix")
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn text_reports_distinguish_literal_escapes_from_control_names() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("root\\n");
    std::fs::create_dir(&root).unwrap();
    for name in ["file\n", "file\\n"] {
        std::fs::write(root.join(name), b"identical").unwrap();
    }
    for command in ["scan", "duplicates", "age"] {
        let mut process = grav();
        process.arg(command).arg(&root);
        if command == "duplicates" {
            process.args(["--min-size", "0"]);
        }
        let output = process.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains(r"root\\n"), "{text:?}");
        if command != "age" {
            assert!(text.contains(r"file\n"), "{text:?}");
            assert!(text.contains(r"file\\n"), "{text:?}");
        }
    }
}

#[test]
fn scan_entry_limits_apply_to_every_report_command() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("one"), b"same").unwrap();
    std::fs::write(directory.path().join("two"), b"same").unwrap();
    let path = directory.path().to_str().unwrap();
    for command in ["scan", "duplicates", "age", "export"] {
        for limit in ["0", "1", "2"] {
            let output = grav()
                .args([command, path, "--max-entries", limit])
                .args(json_args(command))
                .output()
                .unwrap();
            if limit == "2" {
                assert!(output.status.success(), "{output:?}");
                let report = json(&output);
                assert_eq!(report["scope"]["max_entries"], 2);
                assert_eq!(report["status"]["state"], "complete");
            } else {
                assert_eq!(output.status.code(), Some(1), "{output:?}");
                assert!(output.stdout.is_empty());
                assert!(
                    String::from_utf8_lossy(&output.stderr).contains("Scan entry limit exceeded")
                );
            }
        }
        for invalid in ["-1", "18446744073709551616", "many"] {
            let output = run(&[command, "--max-entries", invalid]);
            assert_eq!(output.status.code(), Some(2));
            assert!(output.stdout.is_empty());
        }
    }
}

#[test]
fn scan_entry_limit_failure_leaves_existing_export_untouched() {
    let directory = tempfile::tempdir().unwrap();
    let output_dir = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("one"), b"data").unwrap();
    let destination = output_dir.path().join("report.json");
    std::fs::write(&destination, b"previous report").unwrap();
    let output = run(&[
        "export",
        directory.path().to_str().unwrap(),
        "--max-entries",
        "0",
        "--output",
        destination.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(std::fs::read(&destination).unwrap(), b"previous report");
    assert_eq!(std::fs::read_dir(output_dir.path()).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn text_reports_escape_hostile_names_while_json_preserves_them() {
    let directory = tempfile::tempdir().unwrap();
    let hostile = "line\n\u{1b}]52;c;payload\u{7}\u{202e}.txt";
    std::fs::write(directory.path().join(hostile), "same contents").unwrap();
    std::fs::write(directory.path().join("ordinary.txt"), "same contents").unwrap();
    let path = directory.path().to_str().unwrap();
    for command in ["scan", "duplicates"] {
        let mut process = grav();
        process.args([command, path, "--color", "always"]);
        if command == "duplicates" {
            process.args(["--min-size", "0"]);
        }
        let output = process.output().unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(
            text.contains("line\\n\\u{1b}]52;c;payload\\u{7}\\u{202e}.txt"),
            "{text:?}"
        );
        // Colour is the only escape sequence that reaches the terminal.
        assert!(!text.contains(['\u{7}', '\u{202e}']));
        assert!(!text.contains("\u{1b}]"));
    }
    let output = run(&["duplicates", path, "--min-size", "0", "-f", "json"]);
    assert!(output.status.success());
    let paths = json(&output)["data"]["groups"][0]["paths"]
        .as_array()
        .unwrap()
        .clone();
    assert!(paths.iter().any(|path| {
        std::path::Path::new(path.as_str().unwrap())
            .file_name()
            .unwrap()
            == hostile
    }));
}

#[test]
fn version_names_the_single_binary() {
    let output = run(&["--version"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        format!("grav {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn scan_lists_entries_with_units_and_bounds() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("small.txt"), "content").unwrap();
    std::fs::write(directory.path().join("large.bin"), vec![0u8; 3000]).unwrap();
    let path = directory.path().to_str().unwrap();

    let output = run(&["scan", path, "-A", "-n", "1"]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("2 files, 0 directories"), "{stdout}");
    assert!(
        stdout.contains("3.00 kB") && stdout.contains("large.bin"),
        "{stdout}"
    );
    assert!(stdout.contains("… 1 more, 7 B"), "{stdout}");
    assert!(!stdout.contains("small.txt"), "{stdout}");

    let output = run(&["scan", path, "-A", "--binary"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("2.93 KiB"), "{stdout}");
}

#[test]
fn invalid_arguments_fail_with_usage_status_and_a_reason() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().to_str().unwrap();
    for (args, reason) in [
        (vec!["duplicates", path, "--min-size=-1"], "is not a size"),
        (vec!["age", path, "--stale", "6m"], "ambiguous"),
        (vec!["scan", path, "-j", "0"], "--threads"),
    ] {
        let output = run(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(reason), "{args:?}: {stderr}");
        assert!(!stderr.contains("panicked at"), "CLI panicked: {stderr}");
    }
}

#[test]
fn versioned_reports_identify_scope_kind_and_completeness() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("a"), b"same content").unwrap();
    std::fs::write(directory.path().join("b"), b"same content").unwrap();
    let path = directory.path().to_str().unwrap();
    for command in ["scan", "duplicates", "age", "export"] {
        let mut args = vec![command, path, "--strict"];
        if command != "export" {
            args.extend(["-f", "json"]);
        }
        let output = run(&args);
        assert!(output.status.success(), "{output:?}");
        let report = json(&output);
        assert_eq!(report["schema_version"], 2);
        assert_eq!(
            report["kind"],
            if command == "export" { "scan" } else { command }
        );
        assert_eq!(report["status"]["state"], "complete");
        assert!(report["scope"].is_object());
        assert!(report["data"].is_object());
        match command {
            "duplicates" => {
                assert_eq!(report["parameters"]["min_size"], 1_000_000);
                assert_eq!(report["parameters"]["max_groups"], 20);
            }
            "age" => assert!(report["parameters"]["reference_time"].is_object()),
            "scan" => {
                assert_eq!(report["parameters"]["depth"], 1);
                assert_eq!(report["parameters"]["limit"], 20);
            }
            _ => assert!(report["parameters"].is_null()),
        }
    }
}

#[cfg(unix)]
#[test]
fn strict_mode_emits_partial_json_and_exits_with_status_three() {
    let directory = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink("missing", directory.path().join("broken")).unwrap();
    let path = directory.path().to_str().unwrap();
    for command in ["scan", "duplicates", "age", "export"] {
        let output = grav()
            .args([command, path, "--strict"])
            .args(json_args(command))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(3), "{command}: {output:?}");
        let report = json(&output);
        assert_eq!(report["status"]["state"], "partial");
        assert_eq!(report["status"]["scan_warning_count"], 1);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("1 entry could not be read"), "{stderr}");

        // Without --strict the same report succeeds, still with the warning.
        let output = grav()
            .args([command, path])
            .args(json_args(command))
            .output()
            .unwrap();
        assert!(output.status.success(), "{command}: {output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("warning:"));
    }
}

#[cfg(unix)]
#[test]
fn every_report_preserves_warning_totals_after_detail_omission() {
    let directory = tempfile::tempdir().unwrap();
    for index in 0..1100 {
        std::os::unix::fs::symlink("missing", directory.path().join(format!("broken-{index}")))
            .unwrap();
    }
    let path = directory.path().to_str().unwrap();
    for command in ["scan", "duplicates", "age", "export"] {
        let output = grav()
            .args([command, path, "--strict"])
            .args(json_args(command))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(3), "{command}");
        let report = json(&output);
        assert_eq!(report["status"]["state"], "partial", "{command}");
        assert_eq!(report["status"]["scan_warning_count"], 1100, "{command}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("1,100 entries could not be read"),
            "{command}: {stderr}"
        );
    }
}

#[test]
fn closed_stdout_does_not_panic() {
    use std::process::Stdio;
    let directory = tempfile::tempdir().unwrap();
    for command in ["scan", "export"] {
        let mut child = grav()
            .args([command, directory.path().to_str().unwrap()])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        drop(child.stdout.take());
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("panicked"));
    }
}

#[test]
fn strict_mode_does_not_report_success_when_stdout_is_closed() {
    use std::process::Stdio;
    let directory = tempfile::tempdir().unwrap();
    // Disable writes before spawning. Merely dropping a peer can leave another
    // descriptor alive during concurrent process creation. Keep the reader alive
    // deliberately so this fixture does not depend on peer descriptor lifetime.
    #[cfg(unix)]
    let (stdout, _reader) = {
        use std::os::{fd::OwnedFd, unix::net::UnixStream};
        let (writer, reader) = UnixStream::pair().unwrap();
        writer.shutdown(std::net::Shutdown::Write).unwrap();
        (Stdio::from(OwnedFd::from(writer)), reader)
    };
    #[cfg(not(unix))]
    let stdout = Stdio::piped();
    let mut child = grav()
        .args(["export", directory.path().to_str().unwrap(), "--strict"])
        .stdout(stdout)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("panicked"));
}

#[test]
fn reports_without_status_have_unknown_completeness() {
    let directory = tempfile::tempdir().unwrap();
    let output = run(&[
        "duplicates",
        directory.path().to_str().unwrap(),
        "-f",
        "json",
    ]);
    assert!(output.status.success());
    let mut data = json(&output)["data"].take();
    data.as_object_mut().unwrap().remove("status");
    let legacy: gravityfile_analyze::DuplicateReport = serde_json::from_value(data).unwrap();
    assert!(!legacy.status.is_complete());
    assert_eq!(
        legacy.status.state,
        gravityfile_analyze::AnalysisState::Unknown
    );
}

#[test]
fn executable_failures_escape_third_party_error_messages() {
    let output = run(&["__plugin-worker", "bad\n\u{1b}]52;c;payload\u{7}\u{202e}"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.starts_with("error: "), "{stderr:?}");
    assert!(
        stderr.contains(r"bad\n\u{1b}]52;c;payload\u{7}\u{202e}"),
        "{stderr:?}"
    );
    assert_eq!(stderr.matches('\n').count(), 1);
    assert!(!stderr.contains(['\u{1b}', '\u{7}', '\u{202e}']));
}

#[test]
fn parser_errors_escape_control_arguments_and_keep_exit_status() {
    for args in [
        vec!["--bad\n\u{202e}"],
        vec!["scan", "--max-entries", "bad\n\u{202e}"],
    ] {
        let output = run(&args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let text = String::from_utf8(output.stderr).unwrap();
        assert_eq!(text.matches('\n').count(), 1, "{text:?}");
        assert!(!text.contains(['\u{1b}', '\u{202e}']));
        assert!(text.contains(r"\n"));
    }
    for flag in ["--help", "--version"] {
        let output = run(&[flag]);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert!(!output.stdout.is_empty());
    }
    let output = run(&["--ordinary-invalid"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("\nUsage:")
    );
}

#[test]
fn colour_follows_the_flag_and_no_color() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("sub")).unwrap();
    let path = directory.path().to_str().unwrap();
    let has_escape = |output: &Output| output.stdout.contains(&0x1b);
    assert!(!has_escape(&run(&["scan", path])), "not a terminal");
    assert!(has_escape(&run(&["scan", path, "--color", "always"])));
    assert!(!has_escape(&run(&["scan", path, "--color", "never"])));
    let output = grav()
        .args(["scan", path])
        .env("CLICOLOR_FORCE", "1")
        .output()
        .unwrap();
    assert!(has_escape(&output));
    let output = grav()
        .args(["scan", path])
        .env("CLICOLOR_FORCE", "1")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(!has_escape(&output));
}

#[test]
fn init_and_completions_print_scripts() {
    for shell in ["bash", "zsh", "fish"] {
        let output = run(&["init", shell]);
        assert!(output.status.success());
        let script = String::from_utf8(output.stdout).unwrap();
        assert!(script.contains("command grav --cwd-file="), "{shell}");
    }
    for shell in ["bash", "zsh", "fish", "elvish", "powershell"] {
        let output = run(&["completions", shell]);
        assert!(output.status.success(), "{shell}");
        assert!(!output.stdout.is_empty(), "{shell}");
    }
    let output = run(&["man"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains(".TH grav"));
}

#[test]
fn logging_is_opt_in() {
    let fixture = tempfile::tempdir().unwrap();
    std::fs::write(fixture.path().join("file"), b"data").unwrap();
    let temp = tempfile::tempdir().unwrap();
    let run = |extra: &[&std::ffi::OsStr]| {
        grav()
            .arg("scan")
            .arg(fixture.path())
            .args(extra)
            .env("TMPDIR", temp.path())
            .env_remove("RUST_LOG")
            .env_remove("GRAVITYFILE_LOG_FILE")
            .output()
            .unwrap()
    };
    let output = run(&[]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read_dir(temp.path()).unwrap().count(),
        0,
        "no stray log files"
    );

    let log = temp.path().join("grav.log");
    let output = run(&["--log-file".as_ref(), log.as_os_str()]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        std::fs::read_to_string(&log)
            .unwrap()
            .contains(concat!("gravityfile v", env!("CARGO_PKG_VERSION")))
    );
}

#[test]
fn snapshots_in_every_format_reproduce_the_scan() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("sub")).unwrap();
    std::fs::write(directory.path().join("sub/data"), vec![7u8; 5000]).unwrap();
    std::fs::write(directory.path().join("small"), b"x").unwrap();
    let saved = tempfile::tempdir().unwrap();
    let path = directory.path().to_str().unwrap();
    let total = |args: &[&str]| {
        let output = run(args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        let report = json(&output);
        (
            report["data"]["total"]["size"].as_u64().unwrap(),
            report["data"]["total"]["files"].as_u64().unwrap(),
        )
    };
    let live = total(&["scan", path, "-f", "json"]);
    let live_apparent = total(&["scan", path, "-A", "-f", "json"]);
    for (format, name) in [
        ("json", "snapshot.json"),
        ("json", "snapshot.json.zst"),
        ("ncdu", "dump.json"),
        ("ncdu", "dump.json.zst"),
    ] {
        let file = saved.path().join(name);
        let file = file.to_str().unwrap();
        let output = run(&["export", path, "-f", format, "-o", file]);
        assert!(output.status.success(), "{name}: {output:?}");
        assert_eq!(total(&["scan", file, "-f", "json"]), live, "{name}");
        assert_eq!(
            total(&["scan", file, "-A", "-f", "json"]),
            live_apparent,
            "{name}"
        );
        let text = String::from_utf8(run(&["scan", file]).stdout).unwrap();
        assert!(text.contains("from a snapshot taken"), "{name}: {text}");
        assert!(run(&["age", file, "-f", "json"]).status.success(), "{name}");
    }
}

#[test]
fn snapshot_errors_are_explained() {
    let saved = tempfile::tempdir().unwrap();
    let garbage = saved.path().join("notes.txt");
    std::fs::write(&garbage, "hello").unwrap();
    let output = run(&["scan", garbage.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("expected a grav export or an ncdu dump")
    );

    let legacy = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/reports/legacy-scan.json"
    );
    let output = run(&["scan", legacy, "-e", "*.tmp", "--git"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("--exclude, --git cannot be used with a snapshot")
    );
    // The explorer opens a snapshot read-only, and explains the same errors
    // before taking over the terminal.
    let output = run(&[garbage.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot load"));
    let output = run(&[legacy, "--scan"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--scan cannot be used with a snapshot")
    );
    // Snapshots written by grav 0.3 still load.
    let output = run(&["scan", legacy, "-f", "json"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(json(&output)["data"]["entries"][0]["path"], "a\n.txt");
}

#[test]
fn diff_reports_what_changed_since_a_snapshot() {
    let place = tempfile::tempdir().unwrap();
    let root = place.path().join("proj");
    std::fs::create_dir_all(root.join("cache")).unwrap();
    std::fs::create_dir_all(root.join("old")).unwrap();
    std::fs::write(root.join("cache/blob"), vec![0u8; 10_000]).unwrap();
    std::fs::write(root.join("old/data"), vec![0u8; 3_000]).unwrap();
    std::fs::write(root.join("same"), b"same").unwrap();
    let saved = place.path().join("before.json");
    let output = run(&[
        "export",
        root.to_str().unwrap(),
        "-o",
        saved.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{output:?}");

    std::fs::write(root.join("cache/blob"), vec![0u8; 50_000]).unwrap();
    std::fs::remove_dir_all(root.join("old")).unwrap();
    std::fs::create_dir(root.join("new")).unwrap();
    std::fs::write(root.join("new/x"), vec![0u8; 7_000]).unwrap();

    // With one side, the snapshot is compared with the same place today.
    let output = run(&["diff", saved.to_str().unwrap(), "-A", "-f", "json"]);
    assert!(output.status.success(), "{output:?}");
    let report = json(&output);
    assert_eq!(report["kind"], "diff");
    assert_eq!(report["data"]["old"]["snapshot"], true);
    assert_eq!(report["data"]["new"]["snapshot"], false);
    let changes: Vec<(String, String)> = report["data"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry["path"].as_str().unwrap().to_string(),
                entry["change"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        changes[..4],
        [
            ("cache".to_string(), "changed".to_string()),
            ("cache/blob".to_string(), "changed".to_string()),
            ("new".to_string(), "added".to_string()),
            ("old".to_string(), "removed".to_string()),
        ]
    );
    assert!(!changes.iter().any(|(path, _)| path == "same"));

    let text = run(&["diff", saved.to_str().unwrap(), "--color", "never"]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("from a snapshot taken"), "{text}");
    assert!(text.contains("removed") && text.contains("new"), "{text}");

    // Two directories compare directly; one directory alone is an error.
    let output = run(&["diff", root.to_str().unwrap(), root.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("No changes in size"));
    let output = run(&["diff", root.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("give a second directory"));
}

#[test]
fn recorded_scans_show_a_directorys_growth() {
    let home = tempfile::tempdir().unwrap();
    let place = tempfile::tempdir().unwrap();
    let root = place.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("media")).unwrap();
    let grav = |args: &[&str]| {
        grav()
            .env("GRAVITYFILE_HOME", home.path())
            .args(args)
            .output()
            .expect("CLI process should start")
    };
    let path = root.to_str().unwrap();
    let output = grav(&["history", path, "--color", "never"]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("No scans recorded yet"));

    assert!(grav(&["scan", path, "--record"]).status.success());
    std::fs::write(root.join("media/video"), vec![0u8; 50_000]).unwrap();
    // A plain scan does not record.
    assert!(grav(&["scan", path]).status.success());
    assert!(grav(&["scan", path, "--record", "-A"]).status.success());
    assert!(grav(&["scan", path, "--record"]).status.success());

    let output = grav(&["history", path, "-f", "json"]);
    assert!(output.status.success(), "{output:?}");
    let report = json(&output);
    assert_eq!(report["kind"], "history");
    let scans = report["data"]["scans"].as_array().unwrap();
    assert_eq!(scans.len(), 3);
    // Changes compare scans measured the same way, so the apparent scan has none.
    assert!(scans[1].get("change").is_none());
    assert!(scans[2]["change"].as_i64().unwrap() >= 50_000);
    assert_eq!(scans[2]["largest_change"]["name"], "media");

    let all = grav(&["history", "--all", "--color", "never"]);
    assert!(String::from_utf8_lossy(&all.stdout).contains("3 scans"));
    let snapshot = root.join("saved.json");
    assert!(
        grav(&["export", path, "-o", snapshot.to_str().unwrap()])
            .status
            .success()
    );
    let output = grav(&["scan", snapshot.to_str().unwrap(), "--record"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--record needs a directory"));
}
