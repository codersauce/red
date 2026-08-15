import unittest

from scripts.test_performance import Measurement, command_for, report, test_arguments


class TestPerformanceTest(unittest.TestCase):
    def test_root_arguments_match_the_normal_ci_suite(self) -> None:
        self.assertEqual(test_arguments("root"), ["--locked", "--all-targets", "--all-features"])

    def test_workspace_arguments_include_every_package(self) -> None:
        self.assertEqual(
            test_arguments("workspace"),
            ["--workspace", "--locked", "--tests", "--all-features"],
        )

    def test_package_arguments_select_one_workspace_member(self) -> None:
        self.assertEqual(
            test_arguments("root", "husk-lexer"),
            ["--locked", "--tests", "--all-features", "-p", "husk-lexer"],
        )

    def test_builds_nextest_command_with_the_requested_profile(self) -> None:
        self.assertEqual(
            command_for("nextest", "root", nextest_profile="ci"),
            [
                "cargo",
                "nextest",
                "run",
                "--locked",
                "--all-targets",
                "--all-features",
                "--profile",
                "ci",
            ],
        )

    def test_report_uses_only_successful_runs_for_medians(self) -> None:
        measurements = [
            Measurement("cargo", ["cargo", "test"], 2.0, 0),
            Measurement("cargo", ["cargo", "test"], 4.0, 0),
            Measurement("nextest", ["cargo", "nextest", "run"], 8.0, 1),
        ]

        result = report(scope="root", prebuild=None, measurements=measurements)

        self.assertEqual(result["median_seconds"], {"cargo": 3.0})
        self.assertEqual(len(result["measurements"]), 3)

    def test_report_preserves_the_selected_package(self) -> None:
        result = report(scope="root", package="husk-lexer", prebuild=None, measurements=[])

        self.assertEqual(result["package"], "husk-lexer")


if __name__ == "__main__":
    unittest.main()
