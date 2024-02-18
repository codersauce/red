use crate::{buffer::Buffer, config::Config, theme::Theme, Editor};

pub struct EditorBuilder {
    buffer: Option<Buffer>,
    cursor_pos: Option<(usize, usize)>,
}

impl EditorBuilder {
    pub fn new() -> Self {
        Self {
            buffer: None,
            cursor_pos: None,
        }
    }

    pub fn state(mut self, state: &str) -> Self {
        let state = state
            .lines()
            .skip_while(|line| line.trim().is_empty())
            .collect::<Vec<&str>>()
            .join("\n");
        let baseline = state
            .lines()
            .next()
            .map(|s| s.chars().position(|c| !c.is_whitespace()).unwrap_or(0))
            .unwrap_or(0);
        let cursor_pos = state.lines().find(|s| s.contains('|')).map(|s| {
            (
                s.chars().position(|c| c == '|').unwrap_or(baseline) - baseline,
                state.lines().position(|l| l.contains('|')).unwrap_or(0),
            )
        });
        let contents = state
            .lines()
            .map(|s| s.to_string()[baseline..].replace("|", "").to_string())
            .collect::<Vec<_>>();
        self.buffer = Some(Buffer::new(None, contents.join("\n")));
        self.cursor_pos = cursor_pos;
        self
    }

    pub fn build(self) -> anyhow::Result<Editor> {
        let mut editor = Editor::new(
            None,
            Config::default(),
            Theme::default(),
            self.buffer.unwrap(),
        )?;
        if let Some(cursor_pos) = self.cursor_pos {
            editor.set_cursor_pos(cursor_pos.0, cursor_pos.1);
        }
        Ok(editor)
    }
}
