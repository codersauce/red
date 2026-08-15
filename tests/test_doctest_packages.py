import tempfile
import unittest
from pathlib import Path

from scripts.doctest_packages import cargo_command, contains_rust_doctest, doctest_packages


class DoctestPackagesTest(unittest.TestCase):
    def test_detects_unannotated_rust_examples(self) -> None:
        self.assertTrue(contains_rust_doctest("//! ```\n//! assert!(true);\n//! ```"))

    def test_detects_annotated_rust_examples(self) -> None:
        self.assertTrue(contains_rust_doctest("/// ```rust,no_run\n/// work();\n/// ```"))

    def test_ignores_ignored_and_non_rust_examples(self) -> None:
        source = "\n".join(
            [
                "/// ```rust,ignore",
                "/// ignored();",
                "/// ```",
                "/// ```bash",
                "/// cargo test",
                "/// ```",
            ]
        )
        self.assertFalse(contains_rust_doctest(source))

    def test_discovers_only_documented_workspace_libraries(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            documented = root / "documented" / "src"
            documented.mkdir(parents=True)
            (documented / "lib.rs").write_text("//! ```\n//! assert!(true);\n//! ```\n")
            empty = root / "empty" / "src"
            empty.mkdir(parents=True)
            (empty / "lib.rs").write_text("pub fn work() {}\n")
            metadata = {
                "workspace_members": ["documented-id", "empty-id"],
                "packages": [
                    {
                        "id": "documented-id",
                        "name": "documented",
                        "targets": [{"kind": ["lib"], "src_path": str(documented / "lib.rs")}],
                    },
                    {
                        "id": "empty-id",
                        "name": "empty",
                        "targets": [{"kind": ["lib"], "src_path": str(empty / "lib.rs")}],
                    },
                ],
            }

            self.assertEqual(doctest_packages(metadata), ["documented"])

    def test_builds_scoped_no_default_features_command(self) -> None:
        self.assertEqual(
            cargo_command(["husk"], no_default_features=True),
            ["cargo", "test", "--locked", "--doc", "--no-default-features", "-p", "husk"],
        )


if __name__ == "__main__":
    unittest.main()
