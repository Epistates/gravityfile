use std::process::Command;

fn encoded_recovery_path(path: &std::path::Path) -> serde_json::Value {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        serde_json::json!({"platform":"unix","bytes":path.as_os_str().as_bytes()})
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        serde_json::json!({"platform":"windows","wide":path.as_os_str().encode_wide().collect::<Vec<_>>()})
    }
}

#[test]
fn recovery_cli_restores_coordinated_rename_batches_for_both_aliases() {
    use gravityfile_ops::ObjectIdentity;
    for binary in [
        env!("CARGO_BIN_EXE_grav"),
        env!("CARGO_BIN_EXE_gravityfile"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let journal = root.join(".gravityfile-rename-cli-rollback");
        std::fs::create_dir(&journal).unwrap();
        std::fs::write(journal.join("lock"), b"").unwrap();
        let mut items = Vec::new();
        for (index, name, destination, bytes) in [
            (0, "first", "second", b"first".as_slice()),
            (1, "second", "first", b"second".as_slice()),
        ] {
            let staged = journal.join(format!("item-{index}"));
            std::fs::write(&staged, bytes).unwrap();
            let identity = ObjectIdentity::capture(&staged).unwrap();
            let source = root.join(name);
            let destination = root.join(destination);
            let intent = serde_json::json!({"schema_version":1,"journal_version":1,"lock_required":true,
                "purpose":"bulk_rename_inspection_only","object_identity":identity,
                "source":encoded_recovery_path(&source),"staged":encoded_recovery_path(&staged),"destination":encoded_recovery_path(&destination)});
            std::fs::write(
                staged.with_extension("json"),
                serde_json::to_vec(&intent).unwrap(),
            )
            .unwrap();
            items.push(serde_json::json!({"source":encoded_recovery_path(&source),"staged":encoded_recovery_path(&staged),
                "destination":encoded_recovery_path(&destination),"identity":identity}));
        }
        let guards: Vec<_> = [&root, &journal].iter().map(|p| serde_json::json!({"path":encoded_recovery_path(p),"identity":ObjectIdentity::capture(p).unwrap()})).collect();
        let plan = serde_json::json!({"schema_version":1,"purpose":"bulk_rename_explicit_rollback", "coordinator":encoded_recovery_path(&journal),
            "participants":[encoded_recovery_path(&journal)],"guards":guards,"items":items});
        std::fs::write(
            journal.join("plan.batch"),
            serde_json::to_vec(&plan).unwrap(),
        )
        .unwrap();
        for _ in 0..2 {
            let output = Command::new(binary)
                .arg("recover")
                .arg(&journal)
                .arg("--rollback-rename")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("Restored 2 bulk rename items")
            );
            assert_eq!(std::fs::read(root.join("first")).unwrap(), b"first");
            assert_eq!(std::fs::read(root.join("second")).unwrap(), b"second");
            assert!(journal.join("plan.batch").exists());
        }
    }
}

#[test]
fn recovery_cli_retains_extraction_output_and_reports_the_journal() {
    use gravityfile_ops::ObjectIdentity;
    for binary in [
        env!("CARGO_BIN_EXE_grav"),
        env!("CARGO_BIN_EXE_gravityfile"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let journal = root.join(".gravityfile-extraction-journal-cli");
        let staging = root.join(".gravityfile-extract-cli");
        let destination = root.join("destination");
        for path in [&journal, &staging, &destination] {
            std::fs::create_dir(path).unwrap();
        }
        std::fs::write(journal.join("lock"), b"").unwrap();
        std::fs::write(staging.join("file"), b"owned payload").unwrap();
        std::fs::hard_link(staging.join("file"), destination.join("file")).unwrap();
        std::fs::write(destination.join("unrelated"), b"existing data").unwrap();
        let guards: Vec<_> = [&root, &destination].iter().map(|p| serde_json::json!({"path":encoded_recovery_path(p),"identity":ObjectIdentity::capture(p).unwrap()})).collect();
        let plan = serde_json::json!({"schema_version":1,"purpose":"archive_extraction_retain_rollback",
            "destination":encoded_recovery_path(&destination),"staging":encoded_recovery_path(&staging),"staging_identity":ObjectIdentity::capture(&staging).unwrap(),
            "guards":guards,"items":[{"target":encoded_recovery_path(&destination.join("file")),"source":encoded_recovery_path(&staging.join("file")),
                "identity":ObjectIdentity::capture(&staging.join("file")).unwrap(),"directory":false}]});
        std::fs::write(
            journal.join("plan.archive"),
            serde_json::to_vec(&plan).unwrap(),
        )
        .unwrap();
        for _ in 0..2 {
            let output = Command::new(binary)
                .arg("recover")
                .arg(&journal)
                .arg("--retain-extraction")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                std::fs::read(journal.join("retained-0")).unwrap(),
                b"owned payload"
            );
            assert_eq!(
                std::fs::read(destination.join("unrelated")).unwrap(),
                b"existing data"
            );
            assert!(!destination.join("file").exists());
        }
        let output = Command::new(binary)
            .arg("recover")
            .arg(&root)
            .arg("--json")
            .output()
            .unwrap();
        assert!(output.status.success());
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["archive_records"][0]["status"], "retained");
    }
}

#[test]
fn recovery_cli_discovers_and_restores_without_clobbering() {
    let root = tempfile::tempdir().unwrap();
    let transaction = root.path().join(".gravityfile-recovery-fixture");
    std::fs::create_dir(&transaction).unwrap();
    std::fs::write(transaction.join("lock"), b"").unwrap();
    #[cfg(unix)]
    let native_name = serde_json::json!({"Unix": b"destination".to_vec()});
    #[cfg(windows)]
    let native_name =
        serde_json::json!({"Windows": "destination".encode_utf16().collect::<Vec<_>>()});
    let manifest = serde_json::json!({"version": 1, "destination_name": native_name, "source": null, "published": false});
    std::fs::write(
        transaction.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    std::fs::write(transaction.join("original"), b"saved original").unwrap();
    std::fs::write(transaction.join("payload"), b"replacement").unwrap();

    let listing = Command::new(env!("CARGO_BIN_EXE_grav"))
        .arg("recover")
        .arg(root.path())
        .output()
        .unwrap();
    assert!(
        listing.status.success(),
        "{}",
        String::from_utf8_lossy(&listing.stderr)
    );
    assert!(String::from_utf8_lossy(&listing.stdout).contains("original: true"));

    let restored = Command::new(env!("CARGO_BIN_EXE_grav"))
        .arg("recover")
        .arg(&transaction)
        .arg("--restore-original")
        .output()
        .unwrap();
    assert!(
        restored.status.success(),
        "{}",
        String::from_utf8_lossy(&restored.stderr)
    );
    assert_eq!(
        std::fs::read(root.path().join("destination")).unwrap(),
        b"saved original"
    );

    let collision = Command::new(env!("CARGO_BIN_EXE_grav"))
        .arg("recover")
        .arg(&transaction)
        .arg("--restore-payload-to")
        .arg(root.path().join("destination"))
        .output()
        .unwrap();
    assert!(!collision.status.success());
    assert_eq!(
        std::fs::read(transaction.join("payload")).unwrap(),
        b"replacement"
    );
    assert_eq!(
        std::fs::read(root.path().join("destination")).unwrap(),
        b"saved original"
    );

    let payload = Command::new(env!("CARGO_BIN_EXE_grav"))
        .arg("recover")
        .arg(&transaction)
        .arg("--restore-payload-to")
        .arg(root.path().join("recovered"))
        .output()
        .unwrap();
    assert!(
        payload.status.success(),
        "{}",
        String::from_utf8_lossy(&payload.stderr)
    );
    assert_eq!(
        std::fs::read(root.path().join("recovered")).unwrap(),
        b"replacement"
    );
    assert!(transaction.join("manifest.json").exists());
}

#[test]
fn recovery_cli_lists_valid_records_and_reports_every_failed_neighbor() {
    let root = tempfile::tempdir().unwrap();
    let malformed = root.path().join(".gravityfile-recovery-a-malformed");
    std::fs::create_dir(&malformed).unwrap();
    std::fs::write(malformed.join("lock"), b"").unwrap();
    std::fs::write(malformed.join("manifest.json"), b"broken").unwrap();
    let active = root.path().join(".gravityfile-recovery-b-active");
    std::fs::create_dir(&active).unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(active.join("lock"))
        .unwrap();
    lock.try_lock().unwrap();
    for with_valid in [false, true] {
        if with_valid {
            let valid = root.path().join(".gravityfile-recovery-z-valid");
            std::fs::create_dir(&valid).unwrap();
            std::fs::write(valid.join("lock"), b"").unwrap();
            #[cfg(unix)]
            let native = serde_json::json!({"Unix": b"destination".to_vec()});
            #[cfg(windows)]
            let native =
                serde_json::json!({"Windows": "destination".encode_utf16().collect::<Vec<_>>()});
            let manifest = serde_json::json!({"version": 1, "purpose": "copy_undo", "destination_name": native, "source": null, "published": false});
            std::fs::write(
                valid.join("manifest.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            std::fs::write(valid.join("original"), b"retained edits").unwrap();
        }
        let output = Command::new(env!("CARGO_BIN_EXE_grav"))
            .arg("recover")
            .arg(root.path())
            .output()
            .unwrap();
        assert!(!output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stdout.contains("No recovery records"));
        assert!(stderr.contains("a-malformed"), "{stderr}");
        assert!(stderr.contains("b-active"), "{stderr}");
        assert!(
            stderr.contains("listing incomplete: 2 inspection errors"),
            "{stderr}"
        );
        if with_valid {
            assert!(stdout.contains("z-valid"), "{stdout}");
            assert!(stdout.contains("CopyUndo"), "{stdout}");
            assert!(stdout.contains("original: true"), "{stdout}");
            assert_eq!(
                std::fs::read(root.path().join(".gravityfile-recovery-z-valid/original")).unwrap(),
                b"retained edits"
            );
        }
        assert_eq!(
            std::fs::read(malformed.join("manifest.json")).unwrap(),
            b"broken"
        );
        assert!(!root.path().join("destination").exists());
        let json_output = Command::new(env!("CARGO_BIN_EXE_grav"))
            .arg("recover")
            .arg(root.path())
            .arg("--json")
            .output()
            .unwrap();
        assert!(!json_output.status.success());
        let report: serde_json::Value = serde_json::from_slice(&json_output.stdout).unwrap();
        assert_eq!(report["status"]["state"], "partial");
        assert_eq!(report["status"]["issue_count"], 2);
        assert_eq!(report["issues"].as_array().unwrap().len(), 2);
        assert_eq!(report["issues"][0]["error_kind"], "invalid_data");
        assert_eq!(report["issues"][1]["error_kind"], "would_block");
        assert_eq!(
            report["records"].as_array().unwrap().len(),
            usize::from(with_valid)
        );
        if with_valid {
            assert_eq!(report["records"][0]["purpose"], "copy_undo");
            assert_eq!(report["records"][0]["original_available"], true);
        }
    }
}

#[test]
fn recovery_json_reports_empty_complete_and_partial_without_human_stdout() {
    let root = tempfile::tempdir().unwrap();
    for partial in [false, true] {
        if partial {
            std::fs::create_dir(root.path().join(".gravityfile-recovery-broken")).unwrap();
        }
        let output = Command::new(env!("CARGO_BIN_EXE_grav"))
            .arg("recover")
            .arg(root.path())
            .arg("--json")
            .output()
            .unwrap();
        assert_eq!(output.status.success(), !partial);
        let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["kind"], "recovery");
        assert_eq!(
            doc["status"]["state"],
            if partial { "partial" } else { "complete" }
        );
        assert_eq!(doc["status"]["issue_count"], usize::from(partial));
        assert_eq!(doc["records"], serde_json::json!([]));
        assert_eq!(
            doc["issues"].as_array().unwrap().len(),
            usize::from(partial)
        );
        if partial {
            assert!(
                doc["issues"][0]["path"]["display"]
                    .as_str()
                    .unwrap()
                    .contains("broken")
            );
        }
    }
    for flag in ["--restore-original", "--restore-payload-to"] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_grav"));
        command
            .arg("recover")
            .arg(root.path())
            .arg("--json")
            .arg(flag);
        if flag == "--restore-payload-to" {
            command.arg(root.path().join("restored"));
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used"));
    }
}

#[test]
fn recovery_cli_inspects_rename_intents_without_replaying_them() {
    fn native(path: &std::path::Path) -> serde_json::Value {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            serde_json::json!({"platform":"unix", "bytes":path.as_os_str().as_bytes(), "display":"untrusted display"})
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            serde_json::json!({"platform":"windows", "wide":path.as_os_str().encode_wide().collect::<Vec<_>>(), "display":"untrusted display"})
        }
    }
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    let directory = root.join(".gravityfile-rename-cli");
    std::fs::create_dir(&directory).unwrap();
    let source = root.join("original");
    let destination = root.join("renamed");
    let staged = directory.join("item-0");
    std::fs::write(&staged, b"retained payload").unwrap();
    let intent = serde_json::json!({"schema_version":1,"purpose":"bulk_rename_inspection_only",
        "source":native(&source),"staged":native(&staged),"destination":native(&destination)});
    std::fs::write(
        directory.join("item-0.json"),
        serde_json::to_vec(&intent).unwrap(),
    )
    .unwrap();
    for json in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_grav"));
        command.arg("recover").arg(&root);
        if json {
            command.arg("--json");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if json {
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["status"]["state"], "complete");
            assert_eq!(report["records"], serde_json::json!([]));
            assert_eq!(report["rename_intents"].as_array().unwrap().len(), 1);
            let record = &report["rename_intents"][0];
            assert_eq!(record["locked_during_inspection"], false);
            assert!(record["last_recorded_phase"].is_null());
            assert!(record["outcome"].as_str().unwrap().contains("unverified"));
            assert_eq!(
                record["source"]["display"],
                source.to_string_lossy().as_ref()
            );
        } else {
            let text = String::from_utf8_lossy(&output.stdout);
            assert!(text.contains("bulk rename intent: outcome unverified"));
            assert!(!text.contains("No recovery records"));
        }
    }
    let restore = Command::new(env!("CARGO_BIN_EXE_grav"))
        .arg("recover")
        .arg(&directory)
        .arg("--restore-original")
        .output()
        .unwrap();
    assert!(!restore.status.success());
    assert!(!source.exists());
    assert!(!destination.exists());
    assert_eq!(std::fs::read(staged).unwrap(), b"retained payload");
}

#[test]
fn recovery_cli_reports_rename_progress_and_invalid_events_without_mutation() {
    fn native(path: &std::path::Path) -> serde_json::Value {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            serde_json::json!({"platform":"unix", "bytes":path.as_os_str().as_bytes()})
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            serde_json::json!({"platform":"windows", "wide":path.as_os_str().encode_wide().collect::<Vec<_>>()})
        }
    }
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    let directory = root.join(".gravityfile-rename-progress-cli");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("lock"), b"").unwrap();
    let payload = directory.join("item-0");
    std::fs::write(&payload, b"retained payload").unwrap();
    let intent = serde_json::json!({"schema_version":1,"journal_version":1,"lock_required":true,
        "purpose":"bulk_rename_inspection_only", "source":native(&root.join("source")),
        "staged":native(&payload), "destination":native(&root.join("destination"))});
    let intent_bytes = serde_json::to_vec(&intent).unwrap();
    std::fs::write(directory.join("item-0.json"), &intent_bytes).unwrap();
    let event = directory.join("item-0.00.event");
    for valid in [true, false] {
        let event_bytes = if valid {
            br#"{"schema_version":1,"sequence":0,"phase":"staged"}"#.to_vec()
        } else {
            b"truncated event".to_vec()
        };
        std::fs::write(&event, &event_bytes).unwrap();
        for binary in [
            env!("CARGO_BIN_EXE_grav"),
            env!("CARGO_BIN_EXE_gravityfile"),
        ] {
            let output = Command::new(binary)
                .arg("recover")
                .arg(&root)
                .arg("--json")
                .output()
                .unwrap();
            assert_eq!(output.status.success(), valid);
            let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(
                document["status"]["state"],
                if valid { "complete" } else { "partial" }
            );
            assert_eq!(document["rename_intents"].as_array().unwrap().len(), 1);
            assert_eq!(
                document["rename_intents"][0]["last_recorded_phase"],
                if valid {
                    serde_json::json!("staged")
                } else {
                    serde_json::Value::Null
                }
            );
            assert!(
                document["rename_intents"][0]["outcome"]
                    .as_str()
                    .unwrap()
                    .contains("unverified")
            );
            if !valid {
                assert_eq!(document["issues"][0]["error_kind"], "invalid_data");
            }
            assert_eq!(std::fs::read(&payload).unwrap(), b"retained payload");
            assert_eq!(std::fs::read(&event).unwrap(), event_bytes);
            assert_eq!(
                std::fs::read(directory.join("item-0.json")).unwrap(),
                intent_bytes
            );
            assert!(!root.join("source").exists() && !root.join("destination").exists());
        }
        if valid {
            let output = Command::new(env!("CARGO_BIN_EXE_grav"))
                .arg("recover")
                .arg(&root)
                .output()
                .unwrap();
            assert!(output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stdout)
                    .contains("last recorded phase: Staged (historical")
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn recovery_json_closed_output_never_reports_success() {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::process::Stdio;
    let root = tempfile::tempdir().unwrap();
    for partial in [false, true] {
        if partial {
            let malformed = root.path().join(".gravityfile-recovery-malformed");
            std::fs::create_dir(&malformed).unwrap();
            std::fs::write(malformed.join("lock"), b"").unwrap();
            std::fs::write(malformed.join("manifest.json"), b"invalid").unwrap();
        }
        let (writer, reader) = UnixStream::pair().unwrap();
        drop(reader);
        let output = Command::new(env!("CARGO_BIN_EXE_grav"))
            .arg("recover")
            .arg(root.path())
            .arg("--json")
            .stdout(Stdio::from(OwnedFd::from(writer)))
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "failed recovery output must fail even when partial={partial}"
        );
        assert!(!output.stderr.is_empty());
        if partial {
            assert_eq!(
                std::fs::read(
                    root.path()
                        .join(".gravityfile-recovery-malformed/manifest.json")
                )
                .unwrap(),
                b"invalid"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn observation_closed_output_preserves_strict_policy() {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::process::Stdio;
    let root = tempfile::tempdir().unwrap();
    for strict in [false, true] {
        let (writer, reader) = UnixStream::pair().unwrap();
        drop(reader);
        let mut command = Command::new(env!("CARGO_BIN_EXE_grav"));
        if strict {
            command.arg("--strict");
        }
        let output = command
            .arg("scan")
            .arg(root.path())
            .stdout(Stdio::from(OwnedFd::from(writer)))
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            !strict,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[cfg(unix)]
#[test]
fn recovery_restore_closed_acknowledgement_reports_completed_mutation() {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::process::Stdio;
    for payload in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let journal = root.path().join(".gravityfile-recovery-ack");
        std::fs::create_dir(&journal).unwrap();
        std::fs::write(journal.join("lock"), b"").unwrap();
        let manifest = serde_json::json!({"version":1,"destination_name":{"Unix":b"restored".to_vec()},"source":null,"published":false});
        std::fs::write(
            journal.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let leaf = if payload { "payload" } else { "original" };
        std::fs::write(journal.join(leaf), b"recovered bytes").unwrap();
        let destination = root.path().join("restored");
        let (writer, reader) = UnixStream::pair().unwrap();
        drop(reader);
        let mut command = Command::new(env!("CARGO_BIN_EXE_grav"));
        command.arg("recover").arg(&journal);
        if payload {
            command.arg("--restore-payload-to").arg(&destination);
        } else {
            command.arg("--restore-original");
        }
        let output = command
            .stdout(Stdio::from(OwnedFd::from(writer)))
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(std::fs::read(&destination).unwrap(), b"recovered bytes");
        assert!(!journal.join(leaf).exists());
        assert!(journal.join("manifest.json").exists());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("Recovery completed"), "{error}");
        assert!(error.contains("before retrying"), "{error}");
    }
}

#[cfg(unix)]
#[test]
fn closed_stderr_preserves_recovery_status_and_json() {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::process::Stdio;
    let root = tempfile::tempdir().unwrap();
    let journal = root.path().join(".gravityfile-recovery-malformed");
    std::fs::create_dir(&journal).unwrap();
    std::fs::write(journal.join("lock"), b"").unwrap();
    std::fs::write(journal.join("manifest.json"), b"invalid").unwrap();
    for binary in [
        env!("CARGO_BIN_EXE_grav"),
        env!("CARGO_BIN_EXE_gravityfile"),
    ] {
        let (writer, reader) = UnixStream::pair().unwrap();
        drop(reader);
        let output = Command::new(binary)
            .arg("recover")
            .arg(root.path())
            .arg("--json")
            .stderr(Stdio::from(OwnedFd::from(writer)))
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(1),
            "diagnostic failure must not panic"
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["status"]["state"], "partial");
        assert_eq!(report["issues"][0]["error_kind"], "invalid_data");
        assert_eq!(
            std::fs::read(journal.join("manifest.json")).unwrap(),
            b"invalid"
        );
    }
}

#[cfg(unix)]
#[test]
fn closed_stderr_does_not_abort_successful_observation() {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::process::Stdio;
    let root = tempfile::tempdir().unwrap();
    for binary in [
        env!("CARGO_BIN_EXE_grav"),
        env!("CARGO_BIN_EXE_gravityfile"),
    ] {
        let (writer, reader) = UnixStream::pair().unwrap();
        drop(reader);
        let output = Command::new(binary)
            .arg("--envelope")
            .arg("scan")
            .arg(root.path())
            .stderr(Stdio::from(OwnedFd::from(writer)))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0));
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["kind"], "scan");
        assert_eq!(report["status"]["state"], "complete");
    }
}
