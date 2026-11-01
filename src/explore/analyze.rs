//! Tree-sitter reading of one buffer: functions, calls, names, and types.
//!
//! Ranges are UTF-8 bytes plus Unicode-scalar columns. The walk is syntactic.
//! It does not resolve types or distinguish two bindings that share a name.

use tree_sitter::{Node, Parser};

use crate::highlighter::LanguageRegistry;

/// Half-open source range. Columns are Unicode scalars, not bytes or graphemes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
}

impl Span {
    pub fn contains_pos(self, line: usize, column: usize) -> bool {
        (line, column) >= (self.start_line, self.start_col)
            && (line, column) < (self.end_line, self.end_col)
    }

    pub fn contains_byte(self, byte: usize) -> bool {
        byte >= self.start_byte && byte < self.end_byte
    }

    pub fn contains_span(self, other: Span) -> bool {
        self.start_byte <= other.start_byte && other.end_byte <= self.end_byte
    }

    pub fn len(self) -> usize {
        self.end_byte.saturating_sub(self.start_byte)
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Definition,
    Call,
    Type,
    Field,
    Reference,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Self::Definition => "definition",
            Self::Call => "call",
            Self::Type => "type",
            Self::Field => "field",
            Self::Reference => "name",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameOcc {
    pub name: String,
    pub span: Span,
    pub parent_kind: String,
    pub role: Role,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    pub name: String,
    pub kind: String,
    pub name_span: Span,
    pub body: Span,
    pub signature: String,
    pub return_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSite {
    pub name: String,
    pub path: String,
    pub call: Span,
    pub callee: Span,
    pub is_macro: bool,
    pub syscall: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub name: String,
    pub type_name: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionScope {
    pub name: String,
    pub span: Span,
    pub signature: Span,
    pub return_type: Option<String>,
    pub calls: Vec<CallSite>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxAt {
    pub kind: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Analysis {
    pub language: Option<String>,
    pub note: String,
    pub source: String,
    pub line_starts: Vec<usize>,
    pub names: Vec<NameOcc>,
    pub definitions: Vec<Definition>,
    pub functions: Vec<FunctionScope>,
    pub calls: Vec<CallSite>,
    pub bindings: Vec<Binding>,
    pub syntax: Vec<SyntaxAt>,
}

impl Analysis {
    pub fn empty(note: impl Into<String>) -> Self {
        Self {
            language: None,
            note: note.into(),
            source: String::new(),
            line_starts: vec![0],
            names: Vec::new(),
            definitions: Vec::new(),
            functions: Vec::new(),
            calls: Vec::new(),
            bindings: Vec::new(),
            syntax: Vec::new(),
        }
    }

    pub fn name_at(&self, line: usize, column: usize) -> Option<&NameOcc> {
        self.names
            .iter()
            .filter(|name| name.span.contains_pos(line, column))
            .min_by_key(|name| name.span.len())
    }

    pub fn function_at(&self, line: usize, column: usize) -> Option<&FunctionScope> {
        self.functions
            .iter()
            .filter(|function| function.span.contains_pos(line, column))
            .min_by_key(|function| function.span.len())
    }

    pub fn syntax_at(&self, line: usize, column: usize) -> Option<&SyntaxAt> {
        self.syntax
            .iter()
            .filter(|node| node.span.contains_pos(line, column))
            .min_by_key(|node| node.span.len())
    }

    pub fn binding_named(&self, name: &str, inside: Option<Span>) -> Option<&Binding> {
        self.bindings
            .iter()
            .filter(|binding| binding.name == name)
            .filter(|binding| inside.is_none_or(|span| span.contains_span(binding.span)))
            .min_by_key(|binding| binding.span.start_byte)
    }

    pub fn line_start(&self, line: usize) -> Option<usize> {
        self.line_starts.get(line).copied()
    }

    pub fn snippet(&self, line: usize) -> String {
        let start = self.line_starts.get(line).copied().unwrap_or(0);
        let end = self
            .line_starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.source.len());
        let raw = self
            .source
            .get(start..end.min(self.source.len()))
            .unwrap_or("");
        let trimmed = raw.trim_end_matches(['\n', '\r']).trim();
        trimmed.chars().take(42).collect()
    }
}

const MAX_SOURCE_BYTES: usize = 1_500_000;

pub fn analyze(registry: &LanguageRegistry, language_id: &str, source: &str) -> Analysis {
    if source.len() > MAX_SOURCE_BYTES {
        return Analysis::empty("file is too large for the explore parse");
    }
    let Some(language) = registry.grammar(language_id) else {
        return Analysis::empty(format!("no tree-sitter grammar for {language_id}"));
    };
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Analysis::empty(format!(
            "tree-sitter grammar for {language_id} failed to load"
        ));
    }
    let Some(tree) = parser.parse(source, None) else {
        return Analysis::empty("tree-sitter parse failed");
    };
    let line_starts = line_starts(source);
    let mut raw_functions = Vec::new();
    let mut calls = Vec::new();
    let mut definitions = Vec::new();
    let mut names = Vec::new();
    let mut bindings = Vec::new();
    let mut syntax = Vec::new();
    walk(
        tree.root_node(),
        source,
        &line_starts,
        &mut raw_functions,
        &mut calls,
        &mut definitions,
        &mut names,
        &mut bindings,
        &mut syntax,
    );
    for name in &mut names {
        if definitions.iter().any(|def| def.name_span == name.span) {
            name.role = Role::Definition;
        }
    }
    let mut functions = Vec::new();
    for index in 0..raw_functions.len() {
        let function = &raw_functions[index];
        let owned = calls
            .iter()
            .filter(|call| function.span.contains_span(call.call))
            .filter(|call| {
                !raw_functions.iter().any(|other| {
                    other.span.start_byte != function.span.start_byte
                        && function.span.contains_span(other.span)
                        && other.span.contains_span(call.call)
                })
            })
            .cloned()
            .collect();
        functions.push(FunctionScope {
            name: function.name.clone(),
            span: function.span,
            signature: function.signature,
            return_type: function.return_type.clone(),
            calls: owned,
        });
    }
    Analysis {
        language: Some(language_id.to_string()),
        note: String::new(),
        source: source.to_string(),
        line_starts,
        names,
        definitions,
        functions,
        calls,
        bindings,
        syntax,
    }
}

struct RawFunction {
    name: String,
    span: Span,
    signature: Span,
    return_type: Option<String>,
}

#[allow(clippy::too_many_arguments)]
fn walk(
    node: Node<'_>,
    source: &str,
    line_starts: &[usize],
    functions: &mut Vec<RawFunction>,
    calls: &mut Vec<CallSite>,
    definitions: &mut Vec<Definition>,
    names: &mut Vec<NameOcc>,
    bindings: &mut Vec<Binding>,
    syntax: &mut Vec<SyntaxAt>,
) {
    let kind = node.kind();
    let span = span_of(node, source, line_starts);
    if is_context(kind) {
        syntax.push(SyntaxAt {
            kind: kind.to_string(),
            span,
        });
    }
    if let Some(def_kind) = definition_kind(kind) {
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = plain_name(name_node, source);
            if is_symbol(&name) {
                let name_span = span_of(name_node, source, line_starts);
                let return_type = node
                    .child_by_field_name("return_type")
                    .and_then(|node| last_type_name(&node_text(node, source)));
                definitions.push(Definition {
                    name: name.clone(),
                    kind: def_kind.to_string(),
                    name_span,
                    body: span,
                    signature: signature_text(node, source),
                    return_type: return_type.clone(),
                });
                if def_kind == "fn" {
                    let signature_end = node
                        .child_by_field_name("body")
                        .map(|body| body.start_byte())
                        .unwrap_or(node.end_byte());
                    functions.push(RawFunction {
                        name,
                        span,
                        signature: span_between(
                            node.start_byte(),
                            signature_end,
                            source,
                            line_starts,
                        ),
                        return_type,
                    });
                }
            }
        }
    }
    if is_call(kind) {
        if let Some(target) = node
            .child_by_field_name("function")
            .or_else(|| node.child_by_field_name("macro"))
        {
            let path = callable_path(target, source);
            let name = path
                .rsplit([':', '.'])
                .next()
                .unwrap_or(path.as_str())
                .to_string();
            if is_symbol(&name) {
                let is_macro = kind == "macro_invocation";
                calls.push(CallSite {
                    syscall: is_syscall_wrapper(&path, &name, is_macro),
                    name,
                    path,
                    call: span,
                    callee: span_of(target, source, line_starts),
                    is_macro,
                });
            }
        }
    }
    record_binding(node, source, line_starts, bindings);
    if is_ident(kind) {
        let name = node_text(node, source);
        if is_symbol(&name) {
            names.push(NameOcc {
                name,
                span,
                parent_kind: node
                    .parent()
                    .map(|parent| parent.kind().to_string())
                    .unwrap_or_default(),
                role: role_for(kind),
            });
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(
            child,
            source,
            line_starts,
            functions,
            calls,
            definitions,
            names,
            bindings,
            syntax,
        );
    }
}

fn record_binding(
    node: Node<'_>,
    source: &str,
    line_starts: &[usize],
    bindings: &mut Vec<Binding>,
) {
    if !matches!(
        node.kind(),
        "let_declaration"
            | "parameter"
            | "required_parameter"
            | "optional_parameter"
            | "const_item"
            | "static_item"
            | "variable_declarator"
    ) {
        return;
    }
    let Some(name_node) = node
        .child_by_field_name("pattern")
        .or_else(|| node.child_by_field_name("name"))
        .or_else(|| node.child_by_field_name("declarator"))
    else {
        return;
    };
    let Some(ident) = first_ident(name_node) else {
        return;
    };
    let name = node_text(ident, source);
    if !is_symbol(&name) {
        return;
    }
    let type_name = node
        .child_by_field_name("type")
        .or_else(|| node.child_by_field_name("type_annotation"))
        .and_then(|node| last_type_name(&node_text(node, source)));
    bindings.push(Binding {
        name,
        type_name,
        span: span_of(ident, source, line_starts),
    });
}

fn role_for(kind: &str) -> Role {
    match kind {
        "type_identifier" => Role::Type,
        "field_identifier" | "property_identifier" => Role::Field,
        _ => Role::Reference,
    }
}

fn is_ident(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "field_identifier"
            | "property_identifier"
            | "type_identifier"
            | "shorthand_property_identifier_pattern"
    )
}

fn is_call(kind: &str) -> bool {
    matches!(
        kind,
        "call_expression"
            | "macro_invocation"
            | "function_call"
            | "method_call"
            | "method_invocation"
    )
}

fn is_context(kind: &str) -> bool {
    matches!(
        kind,
        "function_item"
            | "function_declaration"
            | "function_definition"
            | "function_signature_item"
            | "method_definition"
            | "method_declaration"
            | "impl_item"
            | "struct_item"
            | "enum_item"
            | "trait_item"
            | "mod_item"
            | "let_declaration"
            | "lexical_declaration"
            | "const_item"
            | "if_expression"
            | "if_statement"
            | "match_expression"
            | "switch_statement"
            | "for_expression"
            | "for_statement"
            | "while_expression"
            | "while_statement"
            | "loop_expression"
            | "return_expression"
            | "return_statement"
            | "call_expression"
            | "macro_invocation"
            | "function_call"
            | "closure_expression"
            | "arrow_function"
            | "class_declaration"
            | "interface_declaration"
            | "match_arm"
    )
}

fn definition_kind(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "function_item"
        | "function_declaration"
        | "function_definition"
        | "function_signature_item"
        | "method_definition"
        | "method_declaration" => "fn",
        "struct_item" | "class_declaration" => "struct",
        "enum_item" | "enum_declaration" => "enum",
        "trait_item" | "interface_declaration" => "trait",
        "type_item" | "type_alias_declaration" | "type_declaration" => "type",
        "const_item" => "const",
        "static_item" => "static",
        "mod_item" => "mod",
        "enum_variant" => "variant",
        _ => return None,
    })
}

fn first_ident<'a>(node: Node<'a>) -> Option<Node<'a>> {
    if is_ident(node.kind()) {
        return Some(node);
    }
    let mut index = 0;
    while let Some(child) = node.child(index) {
        if let Some(found) = first_ident(child) {
            return Some(found);
        }
        index += 1;
    }
    None
}

fn plain_name(node: Node<'_>, source: &str) -> String {
    if is_ident(node.kind()) {
        return node_text(node, source);
    }
    callable_path(node, source)
        .rsplit([':', '.'])
        .next()
        .unwrap_or("")
        .to_string()
}

fn callable_path(node: Node<'_>, source: &str) -> String {
    match node.kind() {
        "identifier" | "field_identifier" | "property_identifier" | "type_identifier" => {
            node_text(node, source)
        }
        "scoped_identifier" | "scoped_type_identifier" => {
            let path = node
                .child_by_field_name("path")
                .map(|child| callable_path(child, source))
                .unwrap_or_default();
            let name = node
                .child_by_field_name("name")
                .map(|child| callable_path(child, source))
                .unwrap_or_default();
            join_path(&path, &name, "::")
        }
        "field_expression" | "member_expression" => {
            let object = node
                .child_by_field_name("value")
                .or_else(|| node.child_by_field_name("object"))
                .map(|child| callable_path(child, source))
                .unwrap_or_default();
            let field = node
                .child_by_field_name("field")
                .or_else(|| node.child_by_field_name("property"))
                .map(|child| callable_path(child, source))
                .unwrap_or_default();
            join_path(&object, &field, ".")
        }
        "generic_function" | "generic_type" => node
            .child(0)
            .map(|child| callable_path(child, source))
            .unwrap_or_default(),
        _ => first_ident(node)
            .map(|ident| node_text(ident, source))
            .unwrap_or_default(),
    }
}

fn join_path(left: &str, right: &str, separator: &str) -> String {
    if left.is_empty() {
        right.to_string()
    } else if right.is_empty() {
        left.to_string()
    } else {
        format!("{left}{separator}{right}")
    }
}

fn node_text(node: Node<'_>, source: &str) -> String {
    source
        .get(node.byte_range())
        .unwrap_or("")
        .trim()
        .to_string()
}

fn signature_text(node: Node<'_>, source: &str) -> String {
    let text = source.get(node.byte_range()).unwrap_or("");
    let line = text.lines().next().unwrap_or("").trim();
    let mut shortened = String::new();
    for character in line.chars().take(88) {
        shortened.push(character);
    }
    if line.chars().count() > 88 {
        shortened.push('…');
    }
    shortened
}

pub fn last_type_name(text: &str) -> Option<String> {
    let mut last = None;
    let mut current = String::new();
    let flush = |current: &mut String, last: &mut Option<String>| {
        if !current.is_empty() && !matches!(current.as_str(), "mut" | "dyn" | "impl" | "const") {
            *last = Some(std::mem::take(current));
        } else {
            current.clear();
        }
    };
    for character in text.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            current.push(character);
        } else {
            flush(&mut current, &mut last);
        }
    }
    flush(&mut current, &mut last);
    last
}

pub fn human_kind(kind: &str) -> String {
    let known = match kind {
        "function_item" | "function_declaration" | "function_definition" | "method_definition" => {
            "function"
        }
        "function_signature_item" => "signature",
        "impl_item" => "impl",
        "struct_item" | "class_declaration" => "struct",
        "enum_item" => "enum",
        "trait_item" | "interface_declaration" => "trait",
        "let_declaration" | "lexical_declaration" | "variable_declarator" => "binding",
        "const_item" => "const",
        "if_expression" | "if_statement" => "if",
        "match_expression" | "switch_statement" => "match",
        "for_expression" | "for_statement" => "for",
        "while_expression" | "while_statement" => "while",
        "loop_expression" => "loop",
        "return_expression" | "return_statement" => "return",
        "call_expression" | "function_call" | "method_call" => "function call",
        "macro_invocation" => "macro",
        "closure_expression" | "arrow_function" => "closure",
        "match_arm" => "match arm",
        "mod_item" => "module",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_string();
    }
    kind.replace('_', " ")
}

fn is_symbol(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 80
        && !name.contains(char::is_whitespace)
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '$'
        })
}

pub fn is_syscall_wrapper(path: &str, name: &str, is_macro: bool) -> bool {
    if is_macro {
        return matches!(
            name,
            "println" | "eprintln" | "print" | "eprint" | "dbg" | "writeln" | "write"
        );
    }
    const PREFIXES: &[&str] = &[
        "std::fs::",
        "std::process::",
        "std::net::",
        "std::os::",
        "std::env::",
        "std::thread::",
        "std::io::",
        "tokio::fs::",
        "tokio::net::",
        "tokio::process::",
        "tokio::io::",
        "nix::",
        "libc::",
        "rustix::",
        "mio::",
        "socket2::",
    ];
    if PREFIXES.iter().any(|prefix| path.contains(prefix)) {
        return true;
    }
    const PATHS: &[&str] = &[
        "File::open",
        "File::create",
        "File::options",
        "OpenOptions::",
        "Command::new",
        "Command::output",
        "Command::status",
        "Command::spawn",
        "TcpStream::connect",
        "TcpListener::bind",
        "UdpSocket::bind",
    ];
    if PATHS.iter().any(|needle| path.contains(needle)) {
        return true;
    }
    matches!(
        name,
        "read_to_string"
            | "read_to_end"
            | "write_all"
            | "create_dir"
            | "create_dir_all"
            | "remove_file"
            | "remove_dir"
            | "remove_dir_all"
            | "read_dir"
            | "canonicalize"
            | "symlink"
            | "hard_link"
            | "soft_link"
            | "set_permissions"
            | "copy"
            | "rename"
            | "metadata"
            | "spawn"
            | "syscall"
            | "mmap"
            | "munmap"
            | "ioctl"
            | "fcntl"
    )
}

fn line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, byte) in source.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

fn span_of(node: Node<'_>, source: &str, line_starts: &[usize]) -> Span {
    span_between(node.start_byte(), node.end_byte(), source, line_starts)
}

fn span_between(start: usize, end: usize, source: &str, line_starts: &[usize]) -> Span {
    let (start_line, start_col) = line_col(source, line_starts, start);
    let (end_line, end_col) = line_col(source, line_starts, end);
    Span {
        start_byte: start,
        end_byte: end,
        start_line,
        start_col,
        end_line,
        end_col,
    }
}

fn line_col(source: &str, starts: &[usize], byte: usize) -> (usize, usize) {
    if starts.is_empty() {
        return (0, 0);
    }
    let byte = byte.min(source.len());
    let mut line = starts.partition_point(|start| *start <= byte);
    if line == 0 {
        line = 1;
    }
    line -= 1;
    let start = starts[line];
    let column = source
        .get(start..byte)
        .map(|text| text.chars().count())
        .unwrap_or(0);
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rust(source: &str) -> Analysis {
        analyze(&LanguageRegistry::bundled(), "rust", source)
    }

    #[test]
    fn rust_function_knows_calls_bindings_and_the_enclosing_scope() {
        let source = r#"
struct LaunchSpec;
fn helper() -> bool { true }
fn review(owned: bool) -> bool {
    if owned {
        return true;
    }
    let template: LaunchSpec = LaunchSpec;
    let worker = template;
    std::fs::read_to_string("log").ok();
    helper();
    worker.starts_with("a");
    template
}
"#;
        let analysis = rust(source);
        let review = analysis
            .functions
            .iter()
            .find(|function| function.name == "review")
            .expect("review");
        let helper_call = review
            .calls
            .iter()
            .find(|call| call.name == "helper")
            .expect("helper call");
        assert!(!helper_call.syscall);
        let read = review
            .calls
            .iter()
            .find(|call| call.name == "read_to_string")
            .expect("read");
        assert!(read.syscall);
        assert!(read.path.contains("std::fs"));
        assert!(review.calls.iter().any(|call| call.name == "starts_with"));
        let template = analysis
            .binding_named("template", Some(review.span))
            .expect("binding");
        assert_eq!(template.type_name.as_deref(), Some("LaunchSpec"));
        let inside = analysis
            .function_at(template.span.start_line, template.span.start_col)
            .expect("enclosing");
        assert_eq!(inside.name, "review");
        let name = analysis
            .name_at(template.span.start_line, template.span.start_col)
            .expect("name");
        assert_eq!(name.name, "template");
        assert!(analysis
            .definitions
            .iter()
            .any(|def| def.name == "LaunchSpec"));
        assert!(analysis
            .definitions
            .iter()
            .any(|def| { def.name == "review" && def.return_type.as_deref() == Some("bool") }));
    }

    #[test]
    fn syscall_wrappers_include_process_and_io_macros() {
        assert!(is_syscall_wrapper(
            "std::process::Command::new",
            "new",
            false
        ));
        assert!(is_syscall_wrapper("File::open", "open", false));
        assert!(is_syscall_wrapper("println", "println", true));
        assert!(!is_syscall_wrapper("starts_with", "starts_with", false));
        assert!(!is_syscall_wrapper("vec", "vec", true));
        assert_eq!(
            last_type_name("&mut Option<LaunchSpec>").as_deref(),
            Some("LaunchSpec")
        );
    }
}
