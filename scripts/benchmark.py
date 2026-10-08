#!/usr/bin/env python3
"""Reproducible CLI scale measurements; no third-party Python dependencies.

Creates and removes its own fixture under --fixture-parent. This measures warm
filesystem caches and whole CLI commands, including scanning and formatting.
It neither clears system caches nor measures interactive terminal latency.
"""

import argparse
from array import array
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import statistics
import subprocess
import sys
import tempfile
import time


def positive(value):
    parsed = int(value)
    if parsed <= 0:
        raise argparse.ArgumentTypeError("must be positive")
    return parsed


def measure(command, timeout):
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        started = time.perf_counter()
        child = subprocess.Popen(command, stdout=stdout, stderr=stderr)
        usage = None
        try:
            if hasattr(os, "wait4"):
                while True:
                    pid, status, usage = os.wait4(child.pid, os.WNOHANG)
                    if pid:
                        child.returncode = os.waitstatus_to_exitcode(status)
                        break
                    if time.perf_counter() - started > timeout:
                        raise subprocess.TimeoutExpired(command, timeout)
                    time.sleep(0.01)
            else:
                child.wait(timeout=timeout)
        except BaseException:
            child.kill()
            child.wait()
            raise
        elapsed = time.perf_counter() - started
        stderr.seek(0)
        if child.returncode:
            raise RuntimeError(f"{command[1]} exited {child.returncode}: "
                               f"{stderr.read(8192).decode(errors='replace')}")
        stdout.seek(0)
        result = stdout.read()
        return {
            "seconds": elapsed,
            "peak_rss_bytes": (int(usage.ru_maxrss) * (1 if sys.platform == "darwin" else 1024)
                               if usage else None),
            "user_seconds": usage.ru_utime if usage else None,
            "system_seconds": usage.ru_stime if usage else None,
        }, result


def validate_output(name, output, files, file_bytes, files_per_directory=1000,
                    expected_file_sizes=None):
    """Check fixture semantics separately from timing; top=1 limits displayed groups only."""
    def require(condition, message):
        if not condition:
            raise RuntimeError(f"{name}: {message}")

    if name == "scan":
        match = re.search(rb"\b(\d+) files, (\d+) directories", output)
        require(match is not None, "missing scan counts")
        require(int(match[1]) == files, "incorrect file count")
        require(int(match[2]) == (files + files_per_directory - 1) // files_per_directory + 1,
                "incorrect directory count")
        return
    document = json.loads(output)
    require(document["status"]["state"] == "complete", "incomplete results")
    data = document["data"]
    if name == "export":
        require(document["schema_version"] == 1 and document["kind"] == "scan",
                "incorrect export envelope")
        directories = (files + files_per_directory - 1) // files_per_directory
        require(data["stats"]["total_files"] == files, "incorrect exported file count")
        require(data["stats"]["total_dirs"] == directories + 1, "incorrect exported directory count")
        expected_total = sum(expected_file_sizes) if expected_file_sizes is not None else files * file_bytes
        require(data["root"]["size"] == expected_total, "incorrect exported total size")
        require(data["stats"]["total_size"] == expected_total, "incorrect exported statistics size")
        require("Directory" in data["root"]["kind"], "export root is not a directory")
        require(len(data["root"]["children"]) == directories, "missing exported directories")
        seen_directories = set()
        seen_files = bytearray(files)
        for directory in data["root"]["children"]:
            match = re.fullmatch(r"d(\d{6,})", directory["name"])
            require(match is not None and "Directory" in directory["kind"], "invalid exported directory")
            index = int(match[1])
            require(index < directories and index not in seen_directories, "duplicate/out-of-range directory")
            require(directory["name"] == f"d{index:06}", "incorrect directory name")
            seen_directories.add(index)
            expected_count = min(files_per_directory, files - index * files_per_directory)
            require(len(directory["children"]) == expected_count, "incorrect directory membership count")
            start = index * files_per_directory
            expected_size = (sum(expected_file_sizes[start:start + expected_count])
                             if expected_file_sizes is not None else expected_count * file_bytes)
            require(directory["size"] == expected_size, "incorrect directory size")
            for node in directory["children"]:
                match = re.fullmatch(r"f(\d{9,})", node["name"])
                require(match is not None and "File" in node["kind"], "invalid exported file")
                file_index = int(match[1])
                require(file_index < files and file_index // files_per_directory == index,
                        "file exported under incorrect directory")
                require(node["name"] == f"f{file_index:09}", "incorrect file name")
                require(not seen_files[file_index], "repeated exported file")
                seen_files[file_index] = 1
                expected_size = (expected_file_sizes[file_index]
                                 if expected_file_sizes is not None else file_bytes)
                require(node["size"] == expected_size and not node["children"], "incorrect exported file metadata")
        require(all(seen_files), "missing exported files")
        return
    if name == "age":
        require(data["total_files"] == files, "incorrect file count")
        require(sum(bucket["file_count"] for bucket in data["buckets"]) == files,
                "age buckets do not cover all files")
        require(sum(bucket["total_size"] for bucket in data["buckets"]) == data["total_size"],
                "age bucket sizes disagree with total")
        return
    require(name == "duplicates", "unsupported benchmark command")
    counts = [files // 256 + (index < files % 256) for index in range(min(files, 256))]
    duplicated = [count for count in counts if count > 1]
    expected = {
        "files_analyzed": files,
        "files_with_duplicates": sum(duplicated),
        "group_count": len(duplicated),
        "groups_omitted": max(0, len(duplicated) - 1),
        "total_duplicate_size": sum(duplicated) * file_bytes,
        "total_wasted_space": sum(count - 1 for count in duplicated) * file_bytes,
    }
    for key, value in expected.items():
        require(data[key] == value, f"incorrect {key}: {data[key]} != {value}")
    require(len(data["groups"]) == min(1, len(duplicated)), "incorrect displayed group count")
    for group in data["groups"]:
        require(group["size"] == file_bytes, "incorrect group file size")
        require(len(group["paths"]) == max(duplicated), "incorrect largest group membership")
        require(len(set(group["paths"])) == len(group["paths"]), "repeated path in group")
        require(group["wasted_bytes"] == (len(group["paths"]) - 1) * file_bytes,
                "incorrect group wasted bytes")


def run(args):
    binary = Path(args.binary).resolve(strict=True)
    version = subprocess.check_output([str(binary), "--version"], text=True).strip()
    artifact = {
        "schema_version": 1,
        "created_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "platform": platform.platform(),
        "machine": platform.machine(),
        "logical_cpus": os.cpu_count(),
        "binary": str(binary),
        "binary_version": version,
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "cache_state": "warm; fixture created immediately before measurement; caches not cleared",
        "fixture": {"files": args.files, "file_bytes": args.file_bytes,
                    "files_per_directory": args.files_per_directory, "distinct_contents": min(args.files, 256)},
        "runs_per_command": args.runs,
        "include_export": args.include_export,
        "correctness_checks": "file/directory counts, duplicate totals and group membership counts, age bucket accounting",
        "measurements": {},
    }
    with tempfile.TemporaryDirectory(prefix="gravityfile-bench-", dir=args.fixture_parent) as directory:
        root = Path(directory)
        artifact["fixture_parent"] = str(root.parent)
        if hasattr(os, "statvfs"):
            filesystem = os.statvfs(root)
            artifact["filesystem"] = {"block_size": filesystem.f_bsize,
                                      "fragment_size": filesystem.f_frsize}
        started = time.perf_counter()
        payloads = [bytes([index]) * args.file_bytes for index in range(256)]
        expected_file_sizes = array('Q') if args.include_export else None
        for index in range(args.files):
            parent = root / f"d{index // args.files_per_directory:06}"
            if index % args.files_per_directory == 0:
                parent.mkdir()
            file = parent / f"f{index:09}"
            file.write_bytes(payloads[index % 256])
            if expected_file_sizes is not None:
                metadata = file.stat()
                expected_file_sizes.append(metadata.st_blocks * 512 if os.name == 'posix'
                                           else metadata.st_size)
            if (index + 1) % 100_000 == 0:
                print(f"Created {index + 1:,}/{args.files:,} fixture files", flush=True)
        artifact["fixture_creation_seconds"] = time.perf_counter() - started
        commands = {
            "scan": ["scan", str(root), "--top", "0", "--strict"],
            "duplicates": ["duplicates", str(root), "--min-size", "0", "--top", "1", "--envelope", "--strict"],
            "age": ["age", str(root), "--envelope", "--strict"],
        }
        if args.include_export:
            commands["export"] = ["export", str(root), "--envelope", "--strict"]
        for name, arguments in commands.items():
            samples = []
            for iteration in range(args.runs):
                print(f"Measuring {name} {iteration + 1}/{args.runs}", flush=True)
                sample, output = measure([str(binary), *arguments], args.timeout)
                validate_output(name, output, args.files, args.file_bytes, args.files_per_directory,
                                expected_file_sizes)
                samples.append(sample)
                print(f"  {sample['seconds']:.3f}s, peak RSS {sample['peak_rss_bytes']} bytes", flush=True)
            artifact["measurements"][name] = {
                "arguments": [part if part != str(root) else "<fixture>" for part in arguments],
                "samples": samples,
                "median_seconds": statistics.median(sample["seconds"] for sample in samples),
            }
        print("Removing benchmark fixture", flush=True)
    return artifact


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="target/release/grav")
    parser.add_argument("--files", type=positive, default=10_000)
    parser.add_argument("--files-per-directory", type=positive, default=1000)
    parser.add_argument("--file-bytes", type=positive, default=256)
    parser.add_argument("--runs", type=positive, default=3)
    parser.add_argument("--include-export", action="store_true",
                        help="Also measure full JSON export; validation materializes its JSON in the harness")
    parser.add_argument("--timeout", type=positive, default=600)
    parser.add_argument("--fixture-parent", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    artifact = run(args)
    # Publish only a complete artifact; a failed benchmark preserves old output.
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(mode="w", dir=args.output.parent, delete=False) as staged:
        temporary = Path(staged.name)
        try:
            json.dump(artifact, staged, indent=2)
            staged.write("\n")
            staged.flush()
            os.fsync(staged.fileno())
            staged.close()
            os.replace(temporary, args.output)
        finally:
            temporary.unlink(missing_ok=True)
    print(f"Wrote {args.output}")


if __name__ == "__main__":
    main()
