import json
import unittest
from pathlib import Path

from scripts.ci_gate import ALWAYS_REQUIRED, gate_errors
from scripts.ci_matrix import matrix_for, select_mode


REPOSITORY_ROOT = Path(__file__).resolve().parents[1]


class CiMatrixTest(unittest.TestCase):
    def test_internal_pull_requests_keep_the_cross_platform_matrix(self) -> None:
        self.assertEqual(select_mode("pull_request", ["src/main.rs"], "full"), "full")
        matrix = matrix_for("full")["include"]
        self.assertEqual(
            [entry["name"] for entry in matrix],
            ["ubuntu-latest", "macos-latest", "windows-latest"],
        )
        self.assertEqual(
            next(entry for entry in matrix if entry["name"] == "macos-latest")[
                "warp"
            ],
            "warp-macos-latest-arm64-6x",
        )

    def test_documentation_only_pull_requests_skip_paid_tests(self) -> None:
        self.assertEqual(
            select_mode(
                "pull_request",
                ["README.md", "almanac/reference/validation/ci-and-validation.md"],
                "full",
            ),
            "docs",
        )

    def test_empty_or_mixed_pull_request_diff_fails_safe(self) -> None:
        self.assertEqual(select_mode("pull_request", [], "full"), "full")
        self.assertEqual(
            select_mode("pull_request", ["docs/ci.md", ".github/workflows/ci.yml"], "full"),
            "full",
        )

    def test_pushes_use_one_linux_smoke_runner(self) -> None:
        self.assertEqual(select_mode("push", [], "full"), "smoke")
        self.assertEqual(
            matrix_for("smoke")["include"],
            [
                {
                    "name": "ubuntu-latest",
                    "standard": "ubuntu-latest",
                    "warp": "warp-ubuntu-latest-x64-8x",
                }
            ],
        )

    def test_manual_dispatch_honors_the_requested_scope(self) -> None:
        self.assertEqual(select_mode("workflow_dispatch", [], "full"), "full")
        self.assertEqual(select_mode("workflow_dispatch", [], "smoke"), "smoke")


class CiGateTest(unittest.TestCase):
    @staticmethod
    def successful_needs(
        *, clippy: str = "success", test: str, build: str
    ) -> dict[str, dict[str, str]]:
        needs = {job: {"result": "success"} for job in ALWAYS_REQUIRED}
        needs["clippy"] = {"result": clippy}
        needs["test"] = {"result": test}
        needs["build"] = {"result": build}
        return needs

    def test_pull_request_gate_accepts_successful_matrix_and_skipped_build(self) -> None:
        self.assertEqual(
            gate_errors(
                event="pull_request",
                mode="full",
                needs=self.successful_needs(test="success", build="skipped"),
            ),
            [],
        )

    def test_documentation_gate_requires_the_test_job_to_be_skipped(self) -> None:
        self.assertEqual(
            gate_errors(
                event="pull_request",
                mode="docs",
                needs=self.successful_needs(
                    clippy="skipped", test="skipped", build="skipped"
                ),
            ),
            [],
        )

    def test_push_gate_accepts_skipped_clippy_and_requires_tests_and_builds(self) -> None:
        self.assertEqual(
            gate_errors(
                event="push",
                mode="smoke",
                needs=self.successful_needs(
                    clippy="skipped", test="success", build="success"
                ),
            ),
            [],
        )

    def test_manual_gate_requires_one_clippy_job(self) -> None:
        self.assertEqual(
            gate_errors(
                event="workflow_dispatch",
                mode="smoke",
                needs=self.successful_needs(test="success", build="success"),
            ),
            [],
        )

    def test_gate_reports_every_failed_or_missing_job(self) -> None:
        needs = self.successful_needs(
            clippy="skipped", test="failure", build="success"
        )
        needs["docs"] = {"result": "cancelled"}
        del needs["fmt"]

        self.assertEqual(
            gate_errors(event="push", mode="smoke", needs=needs),
            [
                "fmt: expected success, got missing",
                "docs: expected success, got cancelled",
                "test: expected success, got failure",
            ],
        )

    def test_main_ruleset_requires_the_workflow_gate_name(self) -> None:
        ruleset = json.loads(
            (REPOSITORY_ROOT / ".github/rulesets/main.json").read_text(
                encoding="utf-8"
            )
        )
        status_rule = next(
            rule for rule in ruleset["rules"] if rule["type"] == "required_status_checks"
        )
        contexts = {
            check["context"]
            for check in status_rule["parameters"]["required_status_checks"]
        }
        workflow = (REPOSITORY_ROOT / ".github/workflows/ci.yml").read_text(
            encoding="utf-8"
        )

        self.assertEqual(contexts, {"CI Gate"})
        self.assertIn("    name: CI Gate\n", workflow)


if __name__ == "__main__":
    unittest.main()
