"""Regression checks that incorrect results cannot become benchmark evidence."""
import copy
import json
import unittest

from benchmark import validate_output


class BenchmarkValidationTests(unittest.TestCase):
    def test_export_rejects_wrong_membership_and_metadata(self):
        node = {"name": "f000000000", "kind": {"File": {"executable": False}},
                "size": 256, "children": []}
        directory = {"name": "d000000", "kind": {"Directory": {}},
                     "size": 512, "children": [node, {**node, "name": "f000000001"}]}
        report = {"schema_version": 1, "kind": "scan", "status": {"state": "complete"},
                  "data": {"stats": {"total_files": 2, "total_dirs": 2, "total_size": 512},
                           "root": {"kind": {"Directory": {}}, "size": 512, "children": [directory]}}}
        validate_output("export", json.dumps(report).encode(), 2, 256)
        allocated = copy.deepcopy(report)
        allocated["data"]["root"]["children"][0]["children"][0]["size"] = 4096
        allocated["data"]["root"]["children"][0]["children"][1]["size"] = 8192
        allocated["data"]["root"]["children"][0]["size"] = 12288
        allocated["data"]["root"]["size"] = 12288
        allocated["data"]["stats"]["total_size"] = 12288
        validate_output("export", json.dumps(allocated).encode(), 2, 256,
                        expected_file_sizes=[4096, 8192])
        with self.assertRaises(RuntimeError):
            validate_output("export", json.dumps(allocated).encode(), 2, 256,
                            expected_file_sizes=[8192, 4096])
        for field, value in [("name", "f000000000"), ("name", "f000001000"),
                             ("size", 1), ("kind", "Other"), ("children", [node])]:
            with self.subTest(field=field, value=value):
                invalid = copy.deepcopy(report)
                invalid["data"]["root"]["children"][0]["children"][1][field] = value
                with self.assertRaises(RuntimeError):
                    validate_output("export", json.dumps(invalid).encode(), 2, 256)
        invalid = copy.deepcopy(report)
        invalid["data"]["root"]["children"][0]["children"].pop()
        with self.assertRaises(RuntimeError):
            validate_output("export", json.dumps(invalid).encode(), 2, 256)

    def test_duplicate_accounting_rejects_independent_corruption(self):
        # 257 files: content zero occurs twice; 255 other contents occur once.
        report = {"status": {"state": "complete"}, "data": {
            "files_analyzed": 257, "files_with_duplicates": 2,
            "group_count": 1, "groups_omitted": 0,
            "total_duplicate_size": 512, "total_wasted_space": 256,
            "groups": [{"size": 256, "paths": ["d/f0", "d/f256"], "wasted_bytes": 256}],
        }}
        validate_output("duplicates", json.dumps(report).encode(), 257, 256)
        for field in ["files_analyzed", "files_with_duplicates", "group_count",
                      "groups_omitted", "total_duplicate_size", "total_wasted_space"]:
            with self.subTest(field=field):
                invalid = copy.deepcopy(report)
                invalid["data"][field] += 1
                with self.assertRaises(RuntimeError):
                    validate_output("duplicates", json.dumps(invalid).encode(), 257, 256)
        for field, value in [("paths", ["d/f0", "d/f0"]), ("size", 1),
                             ("wasted_bytes", 0), ("paths", ["d/f0"])]:
            with self.subTest(field=field, value=value):
                invalid = copy.deepcopy(report)
                invalid["data"]["groups"][0][field] = value
                with self.assertRaises(RuntimeError):
                    validate_output("duplicates", json.dumps(invalid).encode(), 257, 256)

    def test_unique_fixture_has_no_duplicates(self):
        report = {"status": {"state": "complete"}, "data": {
            "files_analyzed": 256, "files_with_duplicates": 0,
            "group_count": 0, "groups_omitted": 0,
            "total_duplicate_size": 0, "total_wasted_space": 0, "groups": [],
        }}
        validate_output("duplicates", json.dumps(report).encode(), 256, 256)

    def test_scan_counts_include_root_and_partial_directory(self):
        validate_output("scan", b"1001 files, 3 directories", 1001, 256)
        for output in [b"1001 files, 2 directories", b"1000 files, 3 directories", b"missing"]:
            with self.assertRaises(RuntimeError):
                validate_output("scan", output, 1001, 256)

    def test_custom_directory_width(self):
        validate_output("scan", b"1001 files, 2 directories", 1001, 256, 1001)
        validate_output("scan", b"1001 files, 1002 directories", 1001, 256, 1)
        with self.assertRaises(RuntimeError):
            validate_output("scan", b"1001 files, 3 directories", 1001, 256, 1001)

    def test_age_bucket_accounting_and_completeness(self):
        report = {"status": {"state": "complete"}, "data": {
            "total_files": 2, "total_size": 8192,
            "buckets": [{"file_count": 2, "total_size": 8192}],
        }}
        validate_output("age", json.dumps(report).encode(), 2, 256)
        for field in ["file_count", "total_size"]:
            invalid = copy.deepcopy(report)
            invalid["data"]["buckets"][0][field] -= 1
            with self.assertRaises(RuntimeError):
                validate_output("age", json.dumps(invalid).encode(), 2, 256)
        report["status"]["state"] = "partial"
        with self.assertRaises(RuntimeError):
            validate_output("age", json.dumps(report).encode(), 2, 256)
