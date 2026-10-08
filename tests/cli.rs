use std::process::{Command, Output};

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
    for binary in [
        env!("CARGO_BIN_EXE_grav"),
        env!("CARGO_BIN_EXE_gravityfile"),
    ] {
        for command in ["scan", "duplicates", "age"] {
            let mut process = Command::new(binary);
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
            let diagnostic = String::from_utf8(output.stderr).unwrap();
            assert!(diagnostic.contains(r"root-\xff"), "{diagnostic:?}");
        }
        for command in ["scan", "export", "duplicates", "age"] {
            let mut process = Command::new(binary);
            process
                .arg(command)
                .arg(&root)
                .args(["--envelope", "--strict"]);
            if command == "duplicates" {
                process.args(["--min-size", "0"]);
            }
            let output = process.output().unwrap();
            assert!(
                output.status.success(),
                "{command}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            let expected =
                serde_json::json!({"platform":"unix","bytes":root.as_os_str().as_bytes()});
            assert_eq!(report["root"], expected);
            assert_eq!(report["scope"]["root"], expected);
            assert_eq!(report["status"]["state"], "complete");
            if command == "duplicates" {
                let paths = report["data"]["groups"][0]["paths"].as_array().unwrap();
                assert_eq!(paths.len(), 2);
                assert_ne!(paths[0], paths[1]);
                assert!(paths.iter().all(|path| path["platform"] == "unix"));
            }
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
    for binary in [
        env!("CARGO_BIN_EXE_grav"),
        env!("CARGO_BIN_EXE_gravityfile"),
    ] {
        for command in ["scan", "duplicates", "age"] {
            let mut process = Command::new(binary);
            process.arg(command).arg(&root);
            if command == "duplicates" {
                process.args(["--min-size", "0"]);
            }
            let output = process.output().unwrap();
            assert!(output.status.success(), "{output:?}");
            let text = String::from_utf8(output.stdout).unwrap();
            let diagnostic = String::from_utf8(output.stderr).unwrap();
            assert!(text.contains(r"root\\n"), "{text:?}");
            assert!(diagnostic.contains(r"root\\n"), "{diagnostic:?}");
            if command != "age" {
                assert!(text.contains(r"file\n"), "{text:?}");
                assert!(text.contains(r"file\\n"), "{text:?}");
            }
        }
    }
}

#[test]
fn scan_entry_limits_apply_to_every_report_command_and_binary() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("one"), b"same").unwrap();
    std::fs::write(directory.path().join("two"), b"same").unwrap();
    for binary in [
        env!("CARGO_BIN_EXE_grav"),
        env!("CARGO_BIN_EXE_gravityfile"),
    ] {
        for command in ["scan", "duplicates", "age", "export"] {
            for limit in ["0", "1", "2"] {
                let output = run(
                    binary,
                    &[
                        command,
                        directory.path().to_str().unwrap(),
                        "--max-entries",
                        limit,
                        "--envelope",
                    ],
                );
                if limit == "2" {
                    assert!(output.status.success(), "{:?}", output);
                    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                    assert_eq!(report["scope"]["max_entries"], 2);
                    assert_eq!(report["status"]["state"], "complete");
                } else {
                    assert!(!output.status.success());
                    assert!(output.stdout.is_empty());
                    assert!(
                        String::from_utf8_lossy(&output.stderr)
                            .contains("Scan entry limit exceeded")
                    );
                }
            }
            for invalid in ["-1", "18446744073709551616", "many"] {
                let output = run(binary, &[command, "--max-entries", invalid]);
                assert_eq!(output.status.code(), Some(2));
                assert!(output.stdout.is_empty());
            }
        }
    }
}

#[test]
fn scan_entry_limit_failure_leaves_existing_export_untouched() {
    let directory = tempfile::tempdir().unwrap();
    let output_dir = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("one"), b"data").unwrap();
    let destination = output_dir.path().join("report.json");
    for binary in [
        env!("CARGO_BIN_EXE_grav"),
        env!("CARGO_BIN_EXE_gravityfile"),
    ] {
        std::fs::write(&destination, b"previous report").unwrap();
        let output = run(
            binary,
            &[
                "export",
                directory.path().to_str().unwrap(),
                "--max-entries",
                "0",
                "--output",
                destination.to_str().unwrap(),
            ],
        );
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(std::fs::read(&destination).unwrap(), b"previous report");
        assert_eq!(std::fs::read_dir(output_dir.path()).unwrap().count(), 1);
    }
}

#[cfg(unix)]
#[test]
fn text_reports_escape_hostile_names_while_json_preserves_them() {
    let directory = tempfile::tempdir().unwrap();
    let hostile = "line\n\u{1b}]52;c;payload\u{7}\u{202e}.txt";
    std::fs::write(directory.path().join(hostile), "same contents").unwrap();
    std::fs::write(directory.path().join("ordinary.txt"), "same contents").unwrap();
    for command in ["scan", "duplicates"] {
        let mut args = vec![command, directory.path().to_str().unwrap()];
        if command == "duplicates" {
            args.extend(["--min-size", "0"]);
        }
        let output = run(env!("CARGO_BIN_EXE_grav"), &args);
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(
            text.contains("line\\n\\u{1b}]52;c;payload\\u{7}\\u{202e}.txt"),
            "{text:?}"
        );
        assert!(!text.contains(['\u{1b}', '\u{7}', '\u{202e}']));
    }
    let output = run(
        env!("CARGO_BIN_EXE_grav"),
        &[
            "duplicates",
            directory.path().to_str().unwrap(),
            "--min-size",
            "0",
            "--envelope",
        ],
    );
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let paths = json["data"]["groups"][0]["paths"].as_array().unwrap();
    assert!(paths.iter().any(|path| {
        std::path::Path::new(path.as_str().unwrap())
            .file_name()
            .unwrap()
            == hostile
    }));
}

fn run(binary: &str, args: &[&str]) -> Output {
    Command::new(binary)
        .args(args)
        .output()
        .expect("CLI process should start")
}

#[test]
fn both_binary_names_report_the_same_version() {
    let grav = run(env!("CARGO_BIN_EXE_grav"), &["--version"]);
    let gravityfile = run(env!("CARGO_BIN_EXE_gravityfile"), &["--version"]);

    assert!(grav.status.success());
    assert!(gravityfile.status.success());
    assert_eq!(grav.stdout, gravityfile.stdout);
    assert!(
        String::from_utf8_lossy(&grav.stdout).contains(env!("CARGO_PKG_VERSION")),
        "version output should contain the package version"
    );
}

#[test]
fn scan_subcommand_runs_end_to_end() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("file.txt"), "content").unwrap();

    let output = run(
        env!("CARGO_BIN_EXE_grav"),
        &[
            "scan",
            directory.path().to_str().unwrap(),
            "--max-depth",
            "1",
            "--top",
            "1",
        ],
    );

    assert!(
        output.status.success(),
        "scan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1 files"), "unexpected output: {stdout}");
    assert!(stdout.contains("file.txt"), "unexpected output: {stdout}");
}

#[test]
fn invalid_numeric_arguments_fail_without_panicking() {
    let directory = tempfile::tempdir().unwrap();
    let output = run(
        env!("CARGO_BIN_EXE_gravityfile"),
        &[
            "duplicates",
            directory.path().to_str().unwrap(),
            "--min-size=-1",
        ],
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Invalid size") || stderr.contains("negative"),
        "unexpected error: {stderr}"
    );
    assert!(!stderr.contains("panicked at"), "CLI panicked: {stderr}");
}

#[test]
fn versioned_reports_identify_scope_kind_and_completeness() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("a"), b"same content").unwrap();
    std::fs::write(directory.path().join("b"), b"same content").unwrap();
    for command in ["scan", "duplicates", "age", "export"] {
        let output = run(
            env!("CARGO_BIN_EXE_grav"),
            &[
                command,
                directory.path().to_str().unwrap(),
                "--envelope",
                "--strict",
            ],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["schema_version"], 2);
        assert_eq!(
            report["kind"],
            if command == "export" { "scan" } else { command }
        );
        assert_eq!(report["status"]["state"], "complete");
        assert!(report["scope"].is_object());
        assert!(report["data"].is_object());
        if command == "duplicates" {
            assert_eq!(report["parameters"]["min_size"], 1_048_576);
            assert_eq!(report["parameters"]["max_groups"], 20);
        } else if command == "age" {
            assert!(report["parameters"]["reference_time"].is_object());
        } else {
            assert!(report["parameters"].is_null());
        }
    }
}

#[cfg(unix)]
#[test]
fn strict_mode_emits_partial_json_and_fails_on_scan_warnings() {
    let directory = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink("missing", directory.path().join("broken")).unwrap();
    for command in ["scan", "duplicates", "age", "export"] {
        let output = run(
            env!("CARGO_BIN_EXE_grav"),
            &[
                command,
                directory.path().to_str().unwrap(),
                "--strict",
                "--envelope",
            ],
        );
        assert!(!output.status.success());
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["status"]["state"], "partial");
        assert_eq!(report["status"]["scan_warning_count"], 1);
        assert!(String::from_utf8_lossy(&output.stderr).contains("incomplete report"));
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
    for binary in [
        env!("CARGO_BIN_EXE_grav"),
        env!("CARGO_BIN_EXE_gravityfile"),
    ] {
        for command in ["scan", "duplicates", "age", "export"] {
            let output = run(
                binary,
                &[
                    command,
                    directory.path().to_str().unwrap(),
                    "--strict",
                    "--envelope",
                ],
            );
            assert!(!output.status.success(), "{binary} {command}");
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["status"]["state"], "partial", "{binary} {command}");
            assert_eq!(
                report["status"]["scan_warning_count"], 1100,
                "{binary} {command}"
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("1100 scan warning(s)"),
                "{binary} {command}: {stderr}"
            );
            assert!(
                stderr.contains("incomplete report"),
                "{binary} {command}: {stderr}"
            );
        }
    }
}

#[test]
fn closed_stdout_does_not_panic() {
    use std::process::Stdio;
    let directory = tempfile::tempdir().unwrap();
    for command in ["scan", "export"] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_grav"))
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
    let mut child = Command::new(env!("CARGO_BIN_EXE_grav"))
        .args(["export", directory.path().to_str().unwrap(), "--strict"])
        .stdout(stdout)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("panicked"));
}

#[test]
fn legacy_analysis_json_has_unknown_completeness() {
    let directory = tempfile::tempdir().unwrap();
    let output = run(
        env!("CARGO_BIN_EXE_grav"),
        &[
            "duplicates",
            directory.path().to_str().unwrap(),
            "--format",
            "json",
        ],
    );
    assert!(output.status.success());
    let mut value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    value.as_object_mut().unwrap().remove("status");
    let legacy: gravityfile_analyze::DuplicateReport = serde_json::from_value(value).unwrap();
    assert!(!legacy.status.is_complete());
    assert_eq!(
        legacy.status.state,
        gravityfile_analyze::AnalysisState::Unknown
    );
}

#[test]
fn executable_failures_escape_third_party_error_messages() {
    for binary in [
        env!("CARGO_BIN_EXE_grav"),
        env!("CARGO_BIN_EXE_gravityfile"),
    ] {
        let output = run(
            binary,
            &["__plugin-worker", "bad\n\u{1b}]52;c;payload\u{7}\u{202e}"],
        );
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains(r"bad\n\u{1b}]52;c;payload\u{7}\u{202e}"),
            "{stderr:?}"
        );
        assert_eq!(stderr.matches('\n').count(), 1);
        assert!(!stderr.contains(['\u{1b}', '\u{7}', '\u{202e}']));
    }
}

#[test]
fn parser_errors_escape_control_arguments_and_keep_exit_status() {
    for binary in [
        env!("CARGO_BIN_EXE_grav"),
        env!("CARGO_BIN_EXE_gravityfile"),
    ] {
        for args in [
            vec!["--bad\n\u{202e}"],
            vec!["scan", "--max-entries", "bad\n\u{202e}"],
        ] {
            let output = run(binary, &args);
            assert_eq!(output.status.code(), Some(2));
            assert!(output.stdout.is_empty());
            let text = String::from_utf8(output.stderr).unwrap();
            assert_eq!(text.matches('\n').count(), 1, "{text:?}");
            assert!(!text.contains(['\u{1b}', '\u{202e}']));
            assert!(text.contains(r"\n"));
        }
        for flag in ["--help", "--version"] {
            let output = run(binary, &[flag]);
            assert!(output.status.success());
            assert!(output.stderr.is_empty());
            assert!(!output.stdout.is_empty());
        }
        let output = run(binary, &["--ordinary-invalid"]);
        assert_eq!(output.status.code(), Some(2));
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("\nUsage:")
        );
    }
}

#[test]
fn logging_is_opt_in() {
    let fixture = tempfile::tempdir().unwrap();
    std::fs::write(fixture.path().join("file"), b"data").unwrap();
    let temp = tempfile::tempdir().unwrap();
    let run = |extra: &[&std::ffi::OsStr]| {
        Command::new(env!("CARGO_BIN_EXE_grav"))
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
            .contains("Starting gravityfile")
    );
}
