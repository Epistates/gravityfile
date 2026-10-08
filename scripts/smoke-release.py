#!/usr/bin/env python3
"""Exercise both built CLI entry points against an isolated, known fixture."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import tomllib


def require(condition, message):
    if not condition:
        raise ValueError(message)



def native_path(path):
    """Encode exact native units, including surrogate-escaped Unix filenames."""
    if os.name == "nt":
        raw = os.fspath(path).encode("utf-16-le", errors="surrogatepass")
        return {"platform": "windows", "wide": [int.from_bytes(raw[i:i + 2], "little")
                                                 for i in range(0, len(raw), 2)]}
    return {"platform": "unix", "bytes": list(os.fsencode(path))}


def windows_extended_path(path):
    """Match Rust's canonical Windows paths for drive and UNC fixture roots."""
    if path.startswith('\\\\?\\'):
        return path
    if path.startswith('\\\\'):
        return '\\\\?\\UNC\\' + path[2:]
    return '\\\\?\\' + path


def recovery_command(binary, *arguments, success=True):
    result = subprocess.run([str(binary), "recover", *map(str, arguments)],
                            capture_output=True, timeout=30)
    require((result.returncode == 0) == success,
            f"{binary.name} recover: unexpected exit {result.returncode}: "
            f"{result.stderr.decode(errors='replace')}")
    return result


def check_recovery(binary):
    with tempfile.TemporaryDirectory(prefix="gravityfile-recovery-smoke-") as temporary:
        root = Path(temporary).resolve()
        if os.name == "nt":
            root = Path(windows_extended_path(str(root)))
        undo = root / ".gravityfile-recovery-copy"
        undo.mkdir()
        (undo / "lock").write_bytes(b"")
        name = native_path("copy-é")
        manifest_name = {"Windows": name["wide"]} if os.name == "nt" else {"Unix": name["bytes"]}
        manifest = {"version": 1, "purpose": "copy_undo", "destination_name": manifest_name,
                    "source": None, "published": False}
        (undo / "manifest.json").write_text(json.dumps(manifest))
        (undo / "original").write_bytes(b"retained user edits")
        rename = root / ".gravityfile-rename-intent"
        rename.mkdir()
        (rename / "lock").write_bytes(b"")
        payload = rename / "item-0"
        payload.write_bytes(b"rename payload")
        (rename / "item-0.json").write_text(json.dumps({
            "schema_version": 1, "lock_required": True, "purpose": "bulk_rename_inspection_only",
            "source": native_path(root / "source"), "staged": native_path(payload),
            "destination": native_path(root / "destination")}))
        for partial in (False, True):
            if partial:
                (root / ".gravityfile-recovery-malformed").mkdir()
            report = json.loads(recovery_command(binary, root, "--json", success=not partial).stdout)
            require(report["schema_version"] == 1 and report["kind"] == "recovery",
                    "unexpected recovery schema")
            require(report["status"] == {"state": "partial" if partial else "complete",
                                         "issue_count": int(partial)}, "incorrect recovery status")
            require(len(report["records"]) == 1 and len(report["rename_intents"]) == 1,
                    "recovery lost valid records")
            require(len(report["issues"]) == int(partial), "incorrect recovery issue count")
            require(report["records"][0]["purpose"] == "copy_undo", "incorrect recovery purpose")
            intent = report["rename_intents"][0]
            require(intent["locked_during_inspection"] is True, "rename inspection was not locked")
            for key, value in native_path(payload).items():
                require(intent["staged"][key] == value, "native staging path changed")
        (undo / "payload").write_bytes(b"unexpected payload")
        for purpose, published in (("copy_undo", False), ("overwrite", True)):
            manifest.update(purpose=purpose, published=published)
            (undo / "manifest.json").write_text(json.dumps(manifest))
            before = (undo / "manifest.json").read_bytes()
            recovery_command(binary, undo, "--restore-payload-to", root / "unexpected-target",
                             success=False)
            require(not (root / "unexpected-target").exists(), "invalid payload was restored")
            require((undo / "payload").read_bytes() == b"unexpected payload", "payload changed")
            require((undo / "original").read_bytes() == b"retained user edits", "original changed")
            require((undo / "manifest.json").read_bytes() == before, "manifest changed")
        manifest.update(purpose="copy_undo", published=False)
        (undo / "manifest.json").write_text(json.dumps(manifest))
        recovery_command(binary, undo, "--restore-original")
        require((root / "copy-é").read_bytes() == b"retained user edits", "copy undo restore failed")
        require(payload.read_bytes() == b"rename payload", "rename inspection mutated payload")
        require(not (root / "source").exists() and not (root / "destination").exists(),
                "rename inspection replayed an intent")
    return ["complete and partial native-path JSON", "locked rename intent inspection",
            "copy-undo and committed payload rejection", "copy-undo original restoration"]

def run(binary_directory, expected_version):
    records = []
    with tempfile.TemporaryDirectory(prefix="gravityfile-release-smoke-") as temporary:
        fixture = Path(temporary).resolve()
        (fixture / "nested").mkdir()
        duplicates = [fixture / "same-a", fixture / "nested" / "same-b"]
        for path in duplicates:
            path.write_bytes(b"x" * 256)
        (fixture / "unique").write_bytes(b"y" * 256)
        for name in ("grav", "gravityfile"):
            binary = binary_directory / name
            if not binary.exists():
                binary = binary.with_suffix(".exe")
            binary = binary.resolve(strict=True)
            version = subprocess.run([str(binary), "--version"], check=True,
                                     capture_output=True, text=True, timeout=30).stdout.strip()
            require(version == f"grav {expected_version}", f"{name}: unexpected version {version!r}")
            for command in ("scan", "export", "duplicates", "age"):
                arguments = [str(binary), command, str(fixture), "--strict", "--envelope",
                             "--max-entries", "4"]
                if command == "duplicates":
                    arguments.extend(["--min-size", "0"])
                result = subprocess.run(arguments, check=True, capture_output=True, timeout=30)
                report = json.loads(result.stdout)
                context = f"{name} {command}"
                require(report["schema_version"] == 1, f"{context}: unexpected schema")
                require(report["kind"] == ("scan" if command == "export" else command),
                        f"{context}: unexpected report kind")
                require(report["status"]["state"] == "complete", f"{context}: incomplete report")
                require(report["scope"]["max_entries"] == 4,
                        f"{context}: entry budget missing from scope")
                data = report["data"]
                if command in ("scan", "export"):
                    require(data["stats"]["total_files"] == 3, f"{context}: wrong file count")
                    require(data["stats"]["total_dirs"] == 2, f"{context}: wrong directory count")
                    require(not data["warnings"], f"{context}: unexpected scan warnings")
                elif command == "duplicates":
                    require(data["files_analyzed"] == 3, f"{context}: wrong analyzed count")
                    require(data["group_count"] == 1 and len(data["groups"]) == 1,
                            f"{context}: wrong duplicate groups")
                    require(data["total_wasted_space"] == 256, f"{context}: wrong duplicate size")
                    require({Path(path) for path in data["groups"][0]["paths"]} == set(duplicates),
                            f"{context}: wrong duplicate paths")
                else:
                    require(data["total_files"] == 3, f"{context}: wrong analyzed count")
                # Three files plus one directory: the root is not charged.
                rejected = subprocess.run(
                    [str(binary), command, str(fixture), "--envelope", "--max-entries", "3"],
                    capture_output=True, timeout=30)
                require(rejected.returncode != 0 and not rejected.stdout,
                        f"{context}: exceeded entry budget emitted a report or succeeded")
                require(b"Scan entry limit exceeded" in rejected.stderr,
                        f"{context}: missing entry-budget diagnostic")
            with tempfile.TemporaryDirectory(prefix="gravityfile-budget-export-") as output_dir:
                destination = Path(output_dir) / "report.json"
                destination.write_bytes(b"previous report")
                rejected = subprocess.run(
                    [str(binary), "export", str(fixture), "--max-entries", "3",
                     "--output", str(destination)], capture_output=True, timeout=30)
                require(rejected.returncode != 0 and not rejected.stdout,
                        f"{name}: over-budget export succeeded or emitted a report")
                require(b"Scan entry limit exceeded" in rejected.stderr,
                        f"{name}: export failed for an unexpected reason")
                require(destination.read_bytes() == b"previous report"
                        and list(Path(output_dir).iterdir()) == [destination],
                        f"{name}: over-budget export modified output directory")
            recovery_checks = check_recovery(binary)
            with binary.open("rb") as stream:
                digest = hashlib.file_digest(stream, "sha256").hexdigest()
            records.append({"binary": str(binary), "version": version, "sha256": digest,
                            "commands": ["scan", "export", "duplicates", "age", "recover"],
                            "entry_budget_checks": ["exact_boundary", "exceeded_all_reports",
                                                    "existing_export_preserved"],
                            "recovery_checks": recovery_checks})
    return {"schema_version": 1, "binaries": records}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary-dir", type=Path, default=Path("target/release"))
    args = parser.parse_args()
    manifest = Path(__file__).resolve().parent.parent / "Cargo.toml"
    with manifest.open("rb") as stream:
        version = tomllib.load(stream)["workspace"]["package"]["version"]
    try:
        print(json.dumps(run(args.binary_dir, version), indent=2))
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        parser.exit(1, f"Release smoke check failed: {error}\n")
