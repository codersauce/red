//! Source-aware diagnostics shared by Husk's parser, analyzer, and runtime.

use std::fmt;
use std::sync::Arc;

use husk_ast::Span;

/// Source text and its display path.
#[derive(Debug, Clone)]
pub struct SourceFile {
    name: Arc<str>,
    text: Arc<str>,
    line_starts: Arc<[usize]>,
}

impl SourceFile {
    #[must_use]
    pub fn new(name: impl Into<Arc<str>>, text: impl Into<Arc<str>>) -> Self {
        let name = name.into();
        let text = text.into();
        let mut line_starts = vec![0];
        for (index, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(index + 1);
            }
        }
        Self {
            name,
            text,
            line_starts: line_starts.into(),
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn location(&self, offset: usize) -> Location {
        let offset = offset.min(self.text.len());
        let line_index = self.line_starts.partition_point(|start| *start <= offset) - 1;
        let line_start = self.line_starts[line_index];
        let column = self.text[line_start..offset].chars().count() + 1;
        Location {
            line: line_index + 1,
            column,
        }
    }

    fn line(&self, line_number: usize) -> &str {
        let index = line_number.saturating_sub(1);
        let start = self.line_starts[index];
        let end = self
            .line_starts
            .get(index + 1)
            .copied()
            .unwrap_or(self.text.len());
        self.text[start..end].trim_end_matches(['\r', '\n'])
    }
}

/// One-based source location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    pub line: usize,
    pub column: usize,
}

/// A source label rendered beneath an excerpt.
#[derive(Debug, Clone)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

/// One script call frame attached to a runtime failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallFrame {
    pub function: String,
    pub plugin: String,
}

/// A single structured Husk diagnostic.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: String,
    pub source: SourceFile,
    pub primary: Label,
    pub secondary: Vec<Label>,
    pub notes: Vec<String>,
    pub help: Vec<String>,
    pub stack: Vec<CallFrame>,
}

impl Diagnostic {
    #[must_use]
    pub fn new(
        code: &'static str,
        message: impl Into<String>,
        source: SourceFile,
        span: Span,
        label: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            source,
            primary: Label {
                span,
                message: label.into(),
            },
            secondary: Vec::new(),
            notes: Vec::new(),
            help: Vec::new(),
            stack: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    #[must_use]
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help.push(help.into());
        self
    }

    #[must_use]
    pub fn with_frame(mut self, frame: CallFrame) -> Self {
        if self.stack.last() != Some(&frame) {
            self.stack.push(frame);
        }
        self
    }
}

/// One or more diagnostics that can cross an `anyhow` boundary without losing
/// their source excerpts.
#[derive(Debug, Clone)]
pub struct Report {
    diagnostics: Vec<Diagnostic>,
}

impl Report {
    #[must_use]
    pub fn new(diagnostic: Diagnostic) -> Self {
        Self {
            diagnostics: vec![diagnostic],
        }
    }

    #[must_use]
    pub fn from_diagnostics(diagnostics: Vec<Diagnostic>) -> Self {
        Self { diagnostics }
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    #[must_use]
    pub fn with_frame(mut self, frame: CallFrame) -> Self {
        for diagnostic in &mut self.diagnostics {
            if diagnostic.stack.last() != Some(&frame) {
                diagnostic.stack.push(frame.clone());
            }
        }
        self
    }
}

impl fmt::Display for Report {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, diagnostic) in self.diagnostics.iter().enumerate() {
            if index > 0 {
                writeln!(formatter)?;
            }
            render_diagnostic(diagnostic, formatter)?;
        }
        Ok(())
    }
}

impl std::error::Error for Report {}

fn render_diagnostic(diagnostic: &Diagnostic, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    let location = diagnostic
        .source
        .location(diagnostic.primary.span.range.start);
    let line = diagnostic.source.line(location.line);
    let line_width = location.line.to_string().len();
    let span_start = diagnostic.primary.span.range.start;
    let span_end = diagnostic.primary.span.range.end.max(span_start + 1);
    let end_location = diagnostic.source.location(span_end);
    let marker_width = if end_location.line == location.line {
        end_location.column.saturating_sub(location.column).max(1)
    } else {
        1
    };
    let marker_padding = " ".repeat(location.column.saturating_sub(1));
    let markers = "^".repeat(marker_width);

    writeln!(
        formatter,
        "error[{}]: {}",
        diagnostic.code, diagnostic.message
    )?;
    writeln!(
        formatter,
        "  --> {}:{}:{}",
        diagnostic.source.name(),
        location.line,
        location.column
    )?;
    writeln!(formatter, "{:>width$} |", "", width = line_width)?;
    writeln!(
        formatter,
        "{:>width$} | {}",
        location.line,
        line,
        width = line_width
    )?;
    writeln!(
        formatter,
        "{:>width$} | {}{} {}",
        "",
        marker_padding,
        markers,
        diagnostic.primary.message,
        width = line_width
    )?;

    for label in &diagnostic.secondary {
        let label_location = diagnostic.source.location(label.span.range.start);
        writeln!(
            formatter,
            "  = note: {}:{}:{}: {}",
            diagnostic.source.name(),
            label_location.line,
            label_location.column,
            label.message
        )?;
    }
    for note in &diagnostic.notes {
        writeln!(formatter, "  = note: {note}")?;
    }
    for help in &diagnostic.help {
        writeln!(formatter, "  = help: {help}")?;
    }
    for frame in &diagnostic.stack {
        writeln!(
            formatter,
            "  = note: while calling `{}` in plugin `{}`",
            frame.function, frame.plugin
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_rust_style_excerpt() {
        let source = SourceFile::new("plugins/example.hk", "fn run() {\n    value.badField;\n}\n");
        let start = source.text().find("badField").unwrap();
        let diagnostic = Diagnostic::new(
            "HUSK-R0004",
            "unknown field `badField`",
            source,
            Span::new(start, start + "badField".len()),
            "unknown field",
        )
        .with_help("a similarly named field exists: `bad_field`");

        let rendered = Report::new(diagnostic).to_string();
        assert!(rendered.contains("error[HUSK-R0004]: unknown field `badField`"));
        assert!(rendered.contains("--> plugins/example.hk:2:11"));
        assert!(rendered.contains("^^^^^^^^ unknown field"));
        assert!(rendered.contains("help: a similarly named field exists: `bad_field`"));
    }
}
