//! Tool-call arguments mapped onto file reads, edits, and other activity.
//!
//! The tool names and path fields follow the session logs written by Claude
//! Code, Codex, and Gemini CLI. See the module docs for attribution.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{abs_path, Event, Kind};

pub(crate) fn tool_events(
    name: &str,
    args: &Value,
    cwd: &Path,
    zero_based_offset: bool,
) -> Vec<Event> {
    let normalized = name.to_ascii_lowercase();
    if normalized.starts_with("mcp__") {
        let label = normalized
            .rsplit("__")
            .next()
            .unwrap_or(normalized.as_str());
        let mut event = Event::bare(Kind::Tool);
        event.tool = name.to_string();
        event.text = label.to_string();
        return vec![event];
    }

    let mut event = Event::bare(Kind::Tool);
    event.tool = name.to_string();
    let path = |keys: &[&str]| abs_path(&arg_str(args, keys), cwd);

    match normalized.as_str() {
        "read" | "read_file" | "view" | "view_file" | "cat" => {
            event.kind = Kind::Read;
            event.file = path(&["file_path", "filePath", "absolute_path", "path"]);
            if let Some(start) = arg_num(args, &["start_line"]) {
                let start = start.saturating_sub(1).max(0) as usize;
                event.line = Some(start);
                if let Some(end) = arg_num(args, &["end_line"]) {
                    event.end_line = Some((end.saturating_sub(1).max(0) as usize).max(start));
                }
            } else {
                let offset = arg_num(args, &["offset"]);
                let start = match offset {
                    None => 0,
                    Some(offset) if zero_based_offset => offset.max(0) as usize,
                    Some(offset) => offset.saturating_sub(1).max(0) as usize,
                };
                event.line = Some(start);
                if let Some(limit) = arg_num(args, &["limit"]).filter(|limit| *limit > 0) {
                    event.end_line = Some(start + limit as usize - 1);
                }
            }
        }
        "read_many_files" => {
            event.kind = Kind::Read;
            event.text = arg_str(args, &["include", "paths"]);
        }
        "write" | "write_file" | "create_file" | "create" => {
            event.kind = Kind::Edit;
            event.file = path(&["file_path", "filePath", "absolute_path", "path"]);
            let body = arg_str(args, &["content", "contents", "file_text"]);
            let lines = split_lines(&body);
            event.add = lines.len() as i32;
            event.line = Some(0);
            event.end_line = Some(lines.len().saturating_sub(1));
            event.needle = lines.last().copied().unwrap_or("").to_string();
            event.text = "write".to_string();
        }
        "edit" | "replace" | "str_replace" | "str_replace_editor" | "search_replace"
        | "edit_file" => {
            event.kind = Kind::Edit;
            event.file = path(&["file_path", "filePath", "absolute_path", "path"]);
            let old = arg_str(args, &["old_string", "oldString", "old_str"]);
            let new = arg_str(args, &["new_string", "newString", "new_str"]);
            let (add, del) = changed_lines(&old, &new);
            event.add = add;
            event.del = del;
            event.needle = edit_needle(&old, &new);
        }
        "multiedit" | "multi_edit" => {
            event.kind = Kind::Edit;
            event.file = path(&["file_path", "filePath"]);
            if let Some(edits) = args.get("edits").and_then(Value::as_array) {
                for edit in edits {
                    let old = arg_str(edit, &["old_string", "oldString"]);
                    let new = arg_str(edit, &["new_string", "newString"]);
                    let (add, del) = changed_lines(&old, &new);
                    event.add += add;
                    event.del += del;
                    if event.needle.is_empty() {
                        event.needle = edit_needle(&old, &new);
                    }
                }
            }
        }
        "notebookedit" | "notebook_edit" => {
            event.kind = Kind::Edit;
            event.file = path(&["notebook_path", "file_path"]);
            event.needle = arg_str(args, &["new_source"]);
            event.add = split_lines(&event.needle).len() as i32;
        }
        "apply_patch" | "patch" | "applypatch" => {
            let text = arg_str(args, &["patchText", "patch", "input", "patch_text"]);
            let files = parse_patch(&text);
            if files.is_empty() {
                event.kind = Kind::Edit;
                event.text = "patch".to_string();
                return vec![event];
            }
            return files
                .into_iter()
                .map(|file| {
                    let mut edit = event.clone();
                    edit.kind = Kind::Edit;
                    edit.file = abs_path(&file.path, cwd);
                    edit.add = file.add;
                    edit.del = file.del;
                    edit.deleted = file.deleted;
                    edit.needle = file.needle;
                    edit
                })
                .collect();
        }
        "bash" | "shell" | "shell_command" | "exec_command" | "run_shell_command"
        | "local_shell" | "run_terminal_cmd" | "terminal" => {
            let command = arg_str(args, &["command", "cmd", "script"]);
            let workdir = arg_str(
                args,
                &[
                    "workdir",
                    "dir_path",
                    "directory",
                    "cwd",
                    "working_directory",
                ],
            );
            let cwd = if workdir.is_empty() {
                cwd.to_path_buf()
            } else {
                abs_path(&workdir, cwd)
            };
            return vec![shell_event(event, &command, &cwd)];
        }
        "grep" | "search_file_content" | "grep_search" | "grep_files" | "rg" | "search" => {
            event.kind = Kind::Grep;
            event.query = arg_str(args, &["pattern", "query", "regex"]);
            event.text = arg_str(args, &["path", "dir_path", "include", "glob"]);
        }
        "glob" | "find_files" | "file_search" => {
            event.kind = Kind::Grep;
            event.query = arg_str(args, &["pattern", "glob", "query"]);
            event.text = "files".to_string();
        }
        "ls" | "list" | "list_dir" | "list_directory" => {
            event.kind = Kind::List;
            event.text = short_name(&path(&["path", "dir_path", "directory"]));
        }
        "webfetch" | "web_fetch" | "fetch" => {
            event.kind = Kind::Web;
            event.text = arg_str(args, &["url", "prompt"]);
        }
        "websearch" | "web_search" | "google_web_search" => {
            event.kind = Kind::Web;
            event.text = arg_str(args, &["query"]);
        }
        "task" | "agent" | "subagent" | "spawn_agent" | "delegate" => {
            event.kind = Kind::Task;
            event.text = arg_str(
                args,
                &["description", "agent", "subagent_type", "message", "prompt"],
            );
        }
        "todowrite" | "write_todos" | "update_plan" | "todo_write" => {
            event.kind = Kind::Plan;
            event.text = plan_text(args);
        }
        "askuserquestion"
        | "question"
        | "ask_user"
        | "request_user_input"
        | "ask_followup_question" => {
            event.kind = Kind::Ask;
            event.text = question_text(args);
        }
        "exitplanmode" | "exit_plan_mode" => {
            event.kind = Kind::Ask;
            event.text = "plan ready".to_string();
        }
        _ => {
            event.text = name.to_string();
            let extra = arg_str(args, &["description", "query", "url", "id", "name"]);
            if !extra.is_empty() {
                event.text.push(' ');
                event.text.push_str(&extra);
            }
        }
    }
    vec![event]
}

pub(crate) fn shell_event(mut event: Event, command: &str, cwd: &Path) -> Event {
    event.cmd = command.trim().to_string();
    event.kind = Kind::Run;
    let script = strip_cd(&event.cmd);
    if script
        .chars()
        .any(|ch| matches!(ch, ';' | '&' | '>' | '<' | '`' | '$' | '\n'))
    {
        return event;
    }
    let pipes: Vec<&str> = script.split('|').map(str::trim).collect();
    if pipes.len() > 2 {
        return event;
    }
    let words = shell_words(pipes[0]);
    if words.is_empty() {
        return event;
    }
    let tool = Path::new(&words[0])
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(words[0].as_str());
    if pipes.len() == 2 && matches!(tool, "nl" | "cat") {
        if let Some(file) = last_non_flag(&words[1..]) {
            if let Some((from, to)) = sed_range(&shell_words(pipes[1])) {
                return read_event(event, abs_path(file, cwd), from, Some(to));
            }
        }
        return event;
    }
    match tool {
        "cat" | "bat" | "less" => {
            let files = non_flags(&words[1..]);
            if files.len() == 1 {
                return read_event(event, abs_path(files[0], cwd), 0, None);
            }
        }
        "sed" => {
            if let Some((from, to)) = sed_range(&words) {
                if let Some(file) = last_non_flag(&words[1..]) {
                    if !file.contains('p')
                        || !file.chars().next().is_some_and(|ch| ch.is_ascii_digit())
                    {
                        return read_event(event, abs_path(file, cwd), from, Some(to));
                    }
                }
            }
        }
        "head" => {
            let (lines, file) = head_args(&words[1..]);
            if let (Some(file), Some(lines)) = (file, lines.filter(|n| *n > 0)) {
                return read_event(event, abs_path(file, cwd), 0, Some(lines as usize - 1));
            }
        }
        "rg" | "grep" | "ag" | "ack" => {
            if let Some(pattern) = grep_pattern(&words[1..]) {
                event.kind = Kind::Grep;
                event.query = pattern;
            }
        }
        "git" if words.get(1).map(String::as_str) == Some("grep") => {
            if let Some(pattern) = grep_pattern(&words[2..]) {
                event.kind = Kind::Grep;
                event.query = pattern;
            }
        }
        _ => {}
    }
    event
}

fn read_event(mut event: Event, file: PathBuf, from: usize, to: Option<usize>) -> Event {
    event.kind = Kind::Read;
    event.file = file;
    event.line = Some(from);
    event.end_line = to;
    event
}

pub(crate) fn apply_hunks(event: &mut Event, hunks: &Value) {
    let Some(hunks) = hunks.as_array() else {
        return;
    };
    let mut first = None;
    let mut last = None;
    let mut add = 0i32;
    let mut del = 0i32;
    for hunk in hunks {
        let mut line = hunk
            .get("newStart")
            .and_then(Value::as_i64)
            .unwrap_or(1)
            .saturating_sub(1)
            .max(0);
        let Some(lines) = hunk.get("lines").and_then(Value::as_array) else {
            continue;
        };
        for entry in lines {
            let Some(text) = entry.as_str() else {
                continue;
            };
            if let Some(inserted) = text.strip_prefix('+') {
                add += 1;
                let index = line as usize;
                first.get_or_insert(index);
                last = Some(index);
                if event.needle.is_empty() && !inserted.trim().is_empty() {
                    event.needle = inserted.to_string();
                }
                line += 1;
            } else if text.starts_with('-') {
                del += 1;
                first.get_or_insert(line as usize);
            } else {
                line += 1;
            }
        }
    }
    event.add = add;
    event.del = del;
    if let Some(start) = first {
        event.line = Some(start);
        event.end_line = Some(last.unwrap_or(start));
    }
}

fn plan_text(args: &Value) -> String {
    for key in ["todos", "plan"] {
        let Some(items) = args.get(key).and_then(Value::as_array) else {
            continue;
        };
        let mut done = 0;
        let mut current = String::new();
        for item in items {
            let status = item.get("status").and_then(Value::as_str).unwrap_or("");
            match status {
                "completed" | "done" => done += 1,
                "in_progress" | "in-progress" if current.is_empty() => {
                    current = arg_str(item, &["content", "step", "activeForm", "description"]);
                }
                _ => {}
            }
        }
        let mut text = format!("{done}/{} done", items.len());
        if !current.is_empty() {
            text.push_str(" · ");
            text.push_str(&current);
        }
        return text;
    }
    "plan".to_string()
}

fn question_text(args: &Value) -> String {
    if let Some(question) = args
        .get("questions")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
    {
        let text = arg_str(question, &["question", "header"]);
        if !text.is_empty() {
            return text;
        }
    }
    arg_str(args, &["question", "prompt", "message"])
}

fn arg_str(args: &Value, keys: &[&str]) -> String {
    for key in keys {
        match args.get(*key) {
            Some(Value::String(value)) if !value.is_empty() => return value.clone(),
            Some(Value::Array(items)) => {
                let parts: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
                if !parts.is_empty() {
                    return argv_script(&parts);
                }
            }
            _ => {}
        }
    }
    String::new()
}

fn arg_num(args: &Value, keys: &[&str]) -> Option<i64> {
    for key in keys {
        match args.get(*key) {
            Some(Value::Number(number)) => {
                if let Some(value) = number.as_i64() {
                    return Some(value);
                }
                if let Some(value) = number.as_f64() {
                    return Some(value as i64);
                }
            }
            Some(Value::String(value)) => {
                if let Ok(value) = value.parse() {
                    return Some(value);
                }
            }
            _ => {}
        }
    }
    None
}

fn argv_script(argv: &[&str]) -> String {
    if argv.len() >= 3 {
        let program = Path::new(argv[0])
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(argv[0]);
        if matches!(program, "bash" | "sh" | "zsh" | "fish")
            && matches!(argv[1], "-c" | "-lc" | "-ic")
        {
            return argv[2].to_string();
        }
    }
    argv.join(" ")
}

pub(crate) fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    text.trim_end_matches('\n').split('\n').collect()
}

fn changed_lines(old: &str, new: &str) -> (i32, i32) {
    let old = split_lines(old);
    let new = split_lines(new);
    let mut prefix = 0;
    while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old.len() - prefix
        && suffix < new.len() - prefix
        && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix]
    {
        suffix += 1;
    }
    (
        (new.len() - prefix - suffix) as i32,
        (old.len() - prefix - suffix) as i32,
    )
}

fn edit_needle(old: &str, new: &str) -> String {
    let old = split_lines(old);
    let new = split_lines(new);
    let mut prefix = 0;
    while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
        prefix += 1;
    }
    new.get(prefix).copied().unwrap_or("").trim().to_string()
}

struct PatchFile {
    path: String,
    add: i32,
    del: i32,
    deleted: bool,
    needle: String,
}

fn parse_patch(text: &str) -> Vec<PatchFile> {
    let mut files = Vec::new();
    let mut current: Option<usize> = None;
    for line in text.split('\n') {
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            files.push(PatchFile {
                path: path.trim().to_string(),
                add: 0,
                del: 0,
                deleted: false,
                needle: String::new(),
            });
            current = Some(files.len() - 1);
        } else if let Some(path) = line.strip_prefix("*** Add File: ") {
            files.push(PatchFile {
                path: path.trim().to_string(),
                add: 0,
                del: 0,
                deleted: false,
                needle: String::new(),
            });
            current = Some(files.len() - 1);
        } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
            files.push(PatchFile {
                path: path.trim().to_string(),
                add: 0,
                del: 0,
                deleted: true,
                needle: String::new(),
            });
            current = None;
        } else if let Some(path) = line.strip_prefix("*** Move to: ") {
            if let Some(index) = current {
                files[index].path = path.trim().to_string();
            }
        } else if line.starts_with("***") {
            continue;
        } else if let Some(index) = current {
            if let Some(inserted) = line.strip_prefix('+') {
                let file = &mut files[index];
                file.add += 1;
                if file.needle.is_empty() && !inserted.trim().is_empty() {
                    file.needle = inserted.to_string();
                }
            } else if line.starts_with('-') {
                files[index].del += 1;
            }
        }
    }
    files
}

fn strip_cd(command: &str) -> String {
    let trimmed = command.trim();
    if let Some(rest) = trimmed.strip_prefix("cd ") {
        if let Some((_, after)) = rest.split_once("&&") {
            return after.trim().to_string();
        }
    }
    trimmed.to_string()
}

fn shell_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                } else {
                    current.push(ch);
                }
            }
            Some('"') => {
                if ch == '"' {
                    quote = None;
                } else if ch == '\\' {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                } else {
                    current.push(ch);
                }
            }
            Some(_) => quote = None,
            None if ch == '\'' || ch == '"' => quote = Some(ch),
            None if ch == '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            None if ch.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            None => current.push(ch),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn non_flags(words: &[String]) -> Vec<&str> {
    words
        .iter()
        .filter(|word| !word.starts_with('-'))
        .map(String::as_str)
        .collect()
}

fn last_non_flag(words: &[String]) -> Option<&str> {
    non_flags(words).last().copied()
}

fn sed_range(words: &[String]) -> Option<(usize, usize)> {
    if words.len() < 3 || words[1] != "-n" {
        return None;
    }
    let program = Path::new(&words[0])
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(words[0].as_str());
    if program != "sed" {
        return None;
    }
    let expr = words[2].trim_matches('\'');
    let (start, end) = expr
        .strip_suffix('p')?
        .split_once(',')
        .unwrap_or((expr.strip_suffix('p')?, expr.strip_suffix('p')?));
    let start: i64 = start.parse().ok()?;
    let end: i64 = end.parse().ok()?;
    Some((
        start.saturating_sub(1).max(0) as usize,
        end.saturating_sub(1).max(0) as usize,
    ))
}

fn head_args(words: &[String]) -> (Option<i64>, Option<&str>) {
    let mut lines = Some(10i64);
    let mut file = None;
    let mut index = 0;
    while index < words.len() {
        let word = words[index].as_str();
        if word == "-n" {
            index += 1;
            lines = words.get(index).and_then(|value| value.parse().ok());
        } else if let Some(value) = word.strip_prefix("-n") {
            lines = value.parse().ok();
        } else if let Some(value) = word.strip_prefix('-') {
            if let Ok(count) = value.parse::<i64>() {
                lines = Some(count);
            }
        } else {
            file = Some(word);
        }
        index += 1;
    }
    (lines, file)
}

fn grep_pattern(words: &[String]) -> Option<String> {
    let mut index = 0;
    while index < words.len() {
        let word = words[index].as_str();
        if word == "-e" {
            return words.get(index + 1).cloned();
        }
        if matches!(
            word,
            "-g" | "--glob"
                | "-t"
                | "--type"
                | "-A"
                | "-B"
                | "-C"
                | "-m"
                | "--max-count"
                | "--include"
                | "--exclude"
                | "--exclude-dir"
                | "-f"
                | "--max-depth"
        ) {
            index += 2;
            continue;
        }
        if word.starts_with('-') {
            index += 1;
            continue;
        }
        return Some(word.to_string());
    }
    None
}

fn short_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn edit_needle_skips_the_unchanged_prefix() {
        let events = tool_events(
            "Edit",
            &json!({
                "file_path": "src/snake.js",
                "old_string": "keep\nold line",
                "new_string": "keep\nbrand new line"
            }),
            Path::new("/work/proj"),
            false,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, Kind::Edit);
        assert_eq!(events[0].needle, "brand new line");
        assert_eq!(events[0].add, 1);
        assert_eq!(events[0].del, 1);
        assert_eq!(events[0].file, PathBuf::from("/work/proj/src/snake.js"));
    }

    #[test]
    fn claude_read_offset_is_one_based_and_gemini_offset_is_zero_based() {
        let args = json!({"file_path": "/work/proj/snake.js", "offset": 3, "limit": 2});
        let claude = tool_events("Read", &args, Path::new("/work/proj"), false);
        assert_eq!(claude[0].line, Some(2));
        assert_eq!(claude[0].end_line, Some(3));
        let gemini = tool_events("read_file", &args, Path::new("/work/proj"), true);
        assert_eq!(gemini[0].line, Some(3));
        assert_eq!(gemini[0].end_line, Some(4));
    }

    #[test]
    fn shell_cat_and_head_become_reads_and_patches_list_files() {
        let cat = shell_event(
            Event::bare(Kind::Tool),
            "cat snake.js",
            Path::new("/work/proj"),
        );
        assert_eq!(cat.kind, Kind::Read);
        assert_eq!(cat.file, PathBuf::from("/work/proj/snake.js"));

        let head = shell_event(
            Event::bare(Kind::Tool),
            "head -n 4 src/lib.rs",
            Path::new("/work/proj"),
        );
        assert_eq!(head.kind, Kind::Read);
        assert_eq!(head.line, Some(0));
        assert_eq!(head.end_line, Some(3));

        let events = tool_events(
            "apply_patch",
            &json!({"input": "*** Begin Patch\n*** Update File: snake.js\n@@\n-old\n+brand new line\n*** End Patch\n"}),
            Path::new("/work/proj"),
            false,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, Kind::Edit);
        assert_eq!(events[0].file, PathBuf::from("/work/proj/snake.js"));
        assert_eq!(events[0].needle, "brand new line");
        assert_eq!(events[0].add, 1);
        assert_eq!(events[0].del, 1);
    }
}
