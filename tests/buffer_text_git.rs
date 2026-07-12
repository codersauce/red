use std::{
    io::Write as _,
    path::Path,
    process::{Command, Output, Stdio},
};

use red::buffer::Buffer;

fn no_index_diff(left: &Path, right: Option<&Path>, stdin: Option<&[u8]>) -> Output {
    let mut command = Command::new("git");
    command
        .arg("-c")
        .arg("core.autocrlf=false")
        .arg("diff")
        .arg("--no-index")
        .arg("--no-ext-diff")
        .arg("--")
        .arg(left)
        .arg(right.map_or_else(|| std::ffi::OsStr::new("-"), Path::as_os_str));
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command.stdout(Stdio::piped()).spawn().unwrap();
    if let Some(stdin) = stdin {
        child.stdin.take().unwrap().write_all(stdin).unwrap();
    }
    child.wait_with_output().unwrap()
}

fn hunks(output: &Output) -> &[u8] {
    let start = output
        .stdout
        .windows(3)
        .position(|window| window == b"@@ ")
        .expect("git diff did not emit a hunk");
    &output.stdout[start..]
}

#[test]
fn exact_buffer_text_produces_the_same_git_hunks_over_stdin() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original.txt");
    let visible = root.path().join("visible.txt");

    for (before, after) in [
        ("old\nkeep\n", "new\nkeep\nunterminated"),
        ("old\r\nkeep\r\n", "new\r\nkeep\r\n終"),
    ] {
        std::fs::write(&original, before).unwrap();
        std::fs::write(&visible, after).unwrap();
        let buffer = Buffer::new(
            Some(original.to_string_lossy().into_owned()),
            after.to_string(),
        );
        let text = buffer.line_range_contents(0, usize::MAX);
        assert_eq!(text.as_bytes(), after.as_bytes());

        let from_file = no_index_diff(&original, Some(&visible), None);
        let from_stdin = no_index_diff(&original, None, Some(text.as_bytes()));

        assert_eq!(from_file.status.code(), Some(1));
        assert_eq!(from_stdin.status.code(), Some(1));
        assert_eq!(hunks(&from_stdin), hunks(&from_file));
    }
}
