//! Source symbol extraction shared by LSP features.

use std::collections::HashMap;
use std::path::Path;

use husk_ast::{
    EnumVariantFields, ExternItemKind, File, Ident, ImplItemKind, ItemKind, Pattern, PatternKind,
    TypeExpr, TypeExprKind,
};
use husk_semantic::{ReferenceKind, SemanticResult};

/// A language-level symbol category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolKind {
    Module,
    Function,
    Method,
    Struct,
    Field,
    Enum,
    Variant,
    TypeAlias,
    Trait,
    Variable,
    Parameter,
    TypeParameter,
    Property,
    Constant,
}

/// A stable definition identity within one workspace snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SymbolId(pub String);

/// A symbol definition suitable for navigation and completion.
#[derive(Debug, Clone)]
pub struct Symbol {
    pub id: SymbolId,
    pub name: String,
    pub qualified_name: String,
    pub kind: SymbolKind,
    pub span: std::ops::Range<usize>,
    pub full_span: std::ops::Range<usize>,
    pub detail: String,
    pub documentation: Option<String>,
    pub container: Option<String>,
    pub external: bool,
}

/// One definition or reference occurrence.
#[derive(Debug, Clone)]
pub struct SymbolOccurrence {
    pub id: SymbolId,
    pub path: std::path::PathBuf,
    pub span: std::ops::Range<usize>,
    pub is_definition: bool,
}

pub(crate) fn extract_symbols(
    path: &Path,
    module_path: &[String],
    source: &str,
    file: &File,
    semantic: &SemanticResult,
) -> (Vec<Symbol>, Vec<SymbolOccurrence>) {
    let module = module_name(path, module_path);
    let context = SymbolContext {
        source,
        module: &module,
    };
    let mut symbols = Vec::new();
    let mut definition_ids = HashMap::<(String, SymbolKind), SymbolId>::new();

    for item in &file.items {
        match &item.kind {
            ItemKind::Mod { name } => push_symbol(
                &mut symbols,
                &mut definition_ids,
                &context,
                name,
                item.span.range.clone(),
                SymbolKind::Module,
                "module".to_string(),
            ),
            ItemKind::Fn {
                name,
                type_params,
                params,
                ret_type,
                ..
            } => {
                push_symbol(
                    &mut symbols,
                    &mut definition_ids,
                    &context,
                    name,
                    item.span.range.clone(),
                    SymbolKind::Function,
                    function_detail(
                        &name.name,
                        params.iter().map(|parameter| {
                            (parameter.name.name.as_str(), type_name(&parameter.ty))
                        }),
                        ret_type.as_ref(),
                    ),
                );
                for type_parameter in type_params {
                    push_nested_symbol(
                        &mut symbols,
                        source,
                        &module,
                        &name.name,
                        &type_parameter.name,
                        SymbolKind::TypeParameter,
                        "type parameter".to_string(),
                    );
                }
                for parameter in params {
                    push_nested_symbol(
                        &mut symbols,
                        source,
                        &module,
                        &name.name,
                        &parameter.name,
                        SymbolKind::Parameter,
                        format!("{}: {}", parameter.name.name, type_name(&parameter.ty)),
                    );
                }
            }
            ItemKind::Struct {
                name,
                type_params,
                fields,
            } => {
                push_symbol(
                    &mut symbols,
                    &mut definition_ids,
                    &context,
                    name,
                    item.span.range.clone(),
                    SymbolKind::Struct,
                    format!("struct {}", name.name),
                );
                for parameter in type_params {
                    push_nested_symbol(
                        &mut symbols,
                        source,
                        &module,
                        &name.name,
                        parameter,
                        SymbolKind::TypeParameter,
                        "type parameter".to_string(),
                    );
                }
                for field in fields {
                    push_nested_symbol(
                        &mut symbols,
                        source,
                        &module,
                        &name.name,
                        &field.name,
                        SymbolKind::Field,
                        format!("{}: {}", field.name.name, type_name(&field.ty)),
                    );
                }
            }
            ItemKind::Enum {
                name,
                type_params,
                variants,
            } => {
                push_symbol(
                    &mut symbols,
                    &mut definition_ids,
                    &context,
                    name,
                    item.span.range.clone(),
                    SymbolKind::Enum,
                    format!("enum {}", name.name),
                );
                for parameter in type_params {
                    push_nested_symbol(
                        &mut symbols,
                        source,
                        &module,
                        &name.name,
                        parameter,
                        SymbolKind::TypeParameter,
                        "type parameter".to_string(),
                    );
                }
                for variant in variants {
                    let detail = match &variant.fields {
                        EnumVariantFields::Unit => {
                            format!("{}::{}", name.name, variant.name.name)
                        }
                        EnumVariantFields::Tuple(types) => format!(
                            "{}::{}({})",
                            name.name,
                            variant.name.name,
                            types.iter().map(type_name).collect::<Vec<_>>().join(", ")
                        ),
                        EnumVariantFields::Struct(fields) => format!(
                            "{}::{} {{ {} }}",
                            name.name,
                            variant.name.name,
                            fields
                                .iter()
                                .map(|field| format!(
                                    "{}: {}",
                                    field.name.name,
                                    type_name(&field.ty)
                                ))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    };
                    push_nested_symbol(
                        &mut symbols,
                        source,
                        &module,
                        &name.name,
                        &variant.name,
                        SymbolKind::Variant,
                        detail,
                    );
                }
            }
            ItemKind::TypeAlias { name, ty } => push_symbol(
                &mut symbols,
                &mut definition_ids,
                &context,
                name,
                item.span.range.clone(),
                SymbolKind::TypeAlias,
                format!("type {} = {}", name.name, type_name(ty)),
            ),
            ItemKind::Trait(definition) => {
                push_symbol(
                    &mut symbols,
                    &mut definition_ids,
                    &context,
                    &definition.name,
                    item.span.range.clone(),
                    SymbolKind::Trait,
                    format!("trait {}", definition.name.name),
                );
                for trait_item in &definition.items {
                    let husk_ast::TraitItemKind::Method(method) = &trait_item.kind;
                    push_nested_symbol(
                        &mut symbols,
                        source,
                        &module,
                        &definition.name.name,
                        &method.name,
                        SymbolKind::Method,
                        function_detail(
                            &method.name.name,
                            method.params.iter().map(|parameter| {
                                (parameter.name.name.as_str(), type_name(&parameter.ty))
                            }),
                            method.ret_type.as_ref(),
                        ),
                    );
                }
            }
            ItemKind::Impl(block) => {
                let container = type_name(&block.self_ty);
                for impl_item in &block.items {
                    match &impl_item.kind {
                        ImplItemKind::Method(method) => push_nested_symbol(
                            &mut symbols,
                            source,
                            &module,
                            &container,
                            &method.name,
                            SymbolKind::Method,
                            function_detail(
                                &method.name.name,
                                method.params.iter().map(|parameter| {
                                    (parameter.name.name.as_str(), type_name(&parameter.ty))
                                }),
                                method.ret_type.as_ref(),
                            ),
                        ),
                        ImplItemKind::Property(property) => push_nested_symbol(
                            &mut symbols,
                            source,
                            &module,
                            &container,
                            &property.name,
                            SymbolKind::Property,
                            format!("{}: {}", property.name.name, type_name(&property.ty)),
                        ),
                    }
                }
            }
            ItemKind::ExternBlock { items, .. } => {
                extract_extern_symbols(&mut symbols, &mut definition_ids, source, &module, items);
            }
            ItemKind::Use { .. } => {}
        }
    }

    collect_local_definitions(path, &module, source, file, semantic, &mut symbols);
    let mut occurrences = symbols
        .iter()
        .map(|symbol| SymbolOccurrence {
            id: symbol.id.clone(),
            path: path.to_path_buf(),
            span: symbol.span.clone(),
            is_definition: true,
        })
        .collect::<Vec<_>>();

    for ((name, kind), references) in &semantic.references {
        let symbol_kind = semantic_kind(*kind);
        for reference in references {
            if reference.span.range.end > source.len() {
                continue;
            }
            let id = if *kind == ReferenceKind::Variable {
                semantic
                    .name_resolution
                    .get(&(reference.span.range.start, reference.span.range.end))
                    .map_or_else(
                        || SymbolId(format!("{module}::local::{name}")),
                        |resolved| SymbolId(format!("{module}::local::{resolved}")),
                    )
            } else {
                definition_ids
                    .get(&(name.clone(), symbol_kind))
                    .cloned()
                    .unwrap_or_else(|| SymbolId(format!("{module}::{symbol_kind:?}::{name}")))
            };
            if occurrences.iter().any(|occurrence| {
                occurrence.id == id
                    && occurrence.span == reference.span.range
                    && occurrence.path == path
            }) {
                continue;
            }
            occurrences.push(SymbolOccurrence {
                id,
                path: path.to_path_buf(),
                span: reference.span.range.clone(),
                is_definition: false,
            });
        }
    }

    (symbols, occurrences)
}

struct SymbolContext<'a> {
    source: &'a str,
    module: &'a str,
}

fn push_symbol(
    symbols: &mut Vec<Symbol>,
    definition_ids: &mut HashMap<(String, SymbolKind), SymbolId>,
    context: &SymbolContext<'_>,
    ident: &Ident,
    full_span: std::ops::Range<usize>,
    kind: SymbolKind,
    detail: String,
) {
    let qualified_name = ident.name.clone();
    let id = SymbolId(format!("{}::{kind:?}::{qualified_name}", context.module));
    definition_ids.insert((reference_name(kind, &qualified_name), kind), id.clone());
    symbols.push(Symbol {
        id,
        name: ident.name.clone(),
        qualified_name,
        kind,
        span: ident.span.range.clone(),
        full_span,
        detail,
        documentation: documentation_before(context.source, ident.span.range.start),
        container: None,
        external: false,
    });
}

fn push_nested_symbol(
    symbols: &mut Vec<Symbol>,
    source: &str,
    module: &str,
    container: &str,
    ident: &Ident,
    kind: SymbolKind,
    detail: String,
) {
    let qualified_name = format!("{container}::{}", ident.name);
    symbols.push(Symbol {
        id: SymbolId(format!("{module}::{kind:?}::{qualified_name}")),
        name: ident.name.clone(),
        qualified_name,
        kind,
        span: ident.span.range.clone(),
        full_span: ident.span.range.clone(),
        detail,
        documentation: documentation_before(source, ident.span.range.start),
        container: Some(container.to_string()),
        external: false,
    });
}

fn extract_extern_symbols(
    symbols: &mut Vec<Symbol>,
    definition_ids: &mut HashMap<(String, SymbolKind), SymbolId>,
    source: &str,
    module: &str,
    items: &[husk_ast::ExternItem],
) {
    let context = SymbolContext { source, module };
    for item in items {
        match &item.kind {
            ExternItemKind::Fn {
                name,
                params,
                ret_type,
            } => push_symbol(
                symbols,
                definition_ids,
                &context,
                name,
                item.span.range.clone(),
                SymbolKind::Function,
                function_detail(
                    &name.name,
                    params
                        .iter()
                        .map(|parameter| (parameter.name.name.as_str(), type_name(&parameter.ty))),
                    ret_type.as_ref(),
                ),
            ),
            ExternItemKind::Mod { binding, items, .. } => {
                push_symbol(
                    symbols,
                    definition_ids,
                    &context,
                    binding,
                    item.span.range.clone(),
                    SymbolKind::Module,
                    format!("extern module {}", binding.name),
                );
                for nested in items {
                    let husk_ast::ModItemKind::Fn {
                        name,
                        params,
                        ret_type,
                    } = &nested.kind;
                    push_nested_symbol(
                        symbols,
                        source,
                        module,
                        &binding.name,
                        name,
                        SymbolKind::Function,
                        function_detail(
                            &name.name,
                            params.iter().map(|parameter| {
                                (parameter.name.name.as_str(), type_name(&parameter.ty))
                            }),
                            ret_type.as_ref(),
                        ),
                    );
                }
            }
            ExternItemKind::Struct { name, .. } => push_symbol(
                symbols,
                definition_ids,
                &context,
                name,
                item.span.range.clone(),
                SymbolKind::Struct,
                format!("extern struct {}", name.name),
            ),
            ExternItemKind::Static { name, ty } | ExternItemKind::Const { name, ty } => {
                push_symbol(
                    symbols,
                    definition_ids,
                    &context,
                    name,
                    item.span.range.clone(),
                    SymbolKind::Constant,
                    format!("{}: {}", name.name, type_name(ty)),
                );
            }
            ExternItemKind::Impl { self_ty, items, .. } => {
                let container = type_name(self_ty);
                for nested in items {
                    match &nested.kind {
                        ImplItemKind::Method(method) => push_nested_symbol(
                            symbols,
                            source,
                            module,
                            &container,
                            &method.name,
                            SymbolKind::Method,
                            function_detail(
                                &method.name.name,
                                method.params.iter().map(|parameter| {
                                    (parameter.name.name.as_str(), type_name(&parameter.ty))
                                }),
                                method.ret_type.as_ref(),
                            ),
                        ),
                        ImplItemKind::Property(property) => push_nested_symbol(
                            symbols,
                            source,
                            module,
                            &container,
                            &property.name,
                            SymbolKind::Property,
                            format!("{}: {}", property.name.name, type_name(&property.ty)),
                        ),
                    }
                }
            }
        }
    }
}

fn collect_local_definitions(
    path: &Path,
    module: &str,
    source: &str,
    file: &File,
    semantic: &SemanticResult,
    symbols: &mut Vec<Symbol>,
) {
    for item in &file.items {
        let ItemKind::Fn { params, body, .. } = &item.kind else {
            continue;
        };
        for parameter in params {
            push_local_symbol(
                path,
                module,
                source,
                &parameter.name,
                SymbolKind::Parameter,
                semantic,
                symbols,
            );
        }
        visit_statements_for_bindings(path, module, source, body, semantic, symbols);
    }
}

fn visit_statements_for_bindings(
    path: &Path,
    module: &str,
    source: &str,
    statements: &[husk_ast::Stmt],
    semantic: &SemanticResult,
    symbols: &mut Vec<Symbol>,
) {
    for statement in statements {
        match &statement.kind {
            husk_ast::StmtKind::Let {
                pattern,
                else_block,
                ..
            } => {
                visit_pattern_bindings(path, module, source, pattern, semantic, symbols);
                if let Some(block) = else_block {
                    visit_statements_for_bindings(
                        path,
                        module,
                        source,
                        &block.stmts,
                        semantic,
                        symbols,
                    );
                }
            }
            husk_ast::StmtKind::If {
                then_branch,
                else_branch,
                ..
            } => {
                visit_statements_for_bindings(
                    path,
                    module,
                    source,
                    &then_branch.stmts,
                    semantic,
                    symbols,
                );
                if let Some(branch) = else_branch {
                    visit_statements_for_bindings(
                        path,
                        module,
                        source,
                        std::slice::from_ref(branch),
                        semantic,
                        symbols,
                    );
                }
            }
            husk_ast::StmtKind::While { body, .. }
            | husk_ast::StmtKind::Loop { body }
            | husk_ast::StmtKind::Block(body) => {
                visit_statements_for_bindings(path, module, source, &body.stmts, semantic, symbols)
            }
            husk_ast::StmtKind::ForIn { binding, body, .. } => {
                push_local_symbol(
                    path,
                    module,
                    source,
                    binding,
                    SymbolKind::Variable,
                    semantic,
                    symbols,
                );
                visit_statements_for_bindings(path, module, source, &body.stmts, semantic, symbols);
            }
            husk_ast::StmtKind::IfLet {
                pattern,
                then_branch,
                else_branch,
                ..
            } => {
                visit_pattern_bindings(path, module, source, pattern, semantic, symbols);
                visit_statements_for_bindings(
                    path,
                    module,
                    source,
                    &then_branch.stmts,
                    semantic,
                    symbols,
                );
                if let Some(branch) = else_branch {
                    visit_statements_for_bindings(
                        path,
                        module,
                        source,
                        std::slice::from_ref(branch),
                        semantic,
                        symbols,
                    );
                }
            }
            husk_ast::StmtKind::Assign { .. }
            | husk_ast::StmtKind::Expr(_)
            | husk_ast::StmtKind::Semi(_)
            | husk_ast::StmtKind::Return { .. }
            | husk_ast::StmtKind::Break
            | husk_ast::StmtKind::Continue => {}
        }
    }
}

fn visit_pattern_bindings(
    path: &Path,
    module: &str,
    source: &str,
    pattern: &Pattern,
    semantic: &SemanticResult,
    symbols: &mut Vec<Symbol>,
) {
    match &pattern.kind {
        PatternKind::Binding(ident) => push_local_symbol(
            path,
            module,
            source,
            ident,
            SymbolKind::Variable,
            semantic,
            symbols,
        ),
        PatternKind::EnumTuple { fields, .. } | PatternKind::Tuple { fields } => {
            for field in fields {
                visit_pattern_bindings(path, module, source, field, semantic, symbols);
            }
        }
        PatternKind::EnumStruct { fields, .. } => {
            for (_, field) in fields {
                visit_pattern_bindings(path, module, source, field, semantic, symbols);
            }
        }
        PatternKind::Wildcard | PatternKind::EnumUnit { .. } => {}
    }
}

fn push_local_symbol(
    _path: &Path,
    module: &str,
    source: &str,
    ident: &Ident,
    kind: SymbolKind,
    semantic: &SemanticResult,
    symbols: &mut Vec<Symbol>,
) {
    let resolved = semantic
        .name_resolution
        .get(&(ident.span.range.start, ident.span.range.end))
        .cloned()
        .unwrap_or_else(|| format!("{}@{}", ident.name, ident.span.range.start));
    let id = SymbolId(format!("{module}::local::{resolved}"));
    if symbols
        .iter()
        .any(|symbol| symbol.id == id && symbol.span == ident.span.range)
    {
        return;
    }
    symbols.push(Symbol {
        id,
        name: ident.name.clone(),
        qualified_name: resolved,
        kind,
        span: ident.span.range.clone(),
        full_span: ident.span.range.clone(),
        detail: ident.name.clone(),
        documentation: documentation_before(source, ident.span.range.start),
        container: None,
        external: false,
    });
}

fn module_name(path: &Path, module_path: &[String]) -> String {
    if module_path.is_empty() {
        path.to_string_lossy().into_owned()
    } else {
        format!("crate::{}", module_path.join("::"))
    }
}

fn semantic_kind(kind: ReferenceKind) -> SymbolKind {
    match kind {
        ReferenceKind::Function => SymbolKind::Function,
        ReferenceKind::Struct => SymbolKind::Struct,
        ReferenceKind::Enum => SymbolKind::Enum,
        ReferenceKind::Variant => SymbolKind::Variant,
        ReferenceKind::TypeAlias => SymbolKind::TypeAlias,
        ReferenceKind::Trait => SymbolKind::Trait,
        ReferenceKind::Variable => SymbolKind::Variable,
        ReferenceKind::Field => SymbolKind::Field,
    }
}

fn reference_name(kind: SymbolKind, qualified_name: &str) -> String {
    match kind {
        SymbolKind::Field => qualified_name.replace("::", "."),
        SymbolKind::Variant => qualified_name.to_string(),
        _ => qualified_name
            .rsplit("::")
            .next()
            .unwrap_or(qualified_name)
            .to_string(),
    }
}

fn function_detail<'a>(
    name: &str,
    parameters: impl Iterator<Item = (&'a str, String)>,
    ret_type: Option<&TypeExpr>,
) -> String {
    let parameters = parameters
        .map(|(name, ty)| format!("{name}: {ty}"))
        .collect::<Vec<_>>()
        .join(", ");
    let result = ret_type.map_or_else(|| "()".to_string(), type_name);
    format!("fn {name}({parameters}) -> {result}")
}

pub(crate) fn type_name(ty: &TypeExpr) -> String {
    match &ty.kind {
        TypeExprKind::Named(name) => name.name.clone(),
        TypeExprKind::Generic { name, args } => format!(
            "{}<{}>",
            name.name,
            args.iter().map(type_name).collect::<Vec<_>>().join(", ")
        ),
        TypeExprKind::Function { params, ret } => format!(
            "fn({}) -> {}",
            params.iter().map(type_name).collect::<Vec<_>>().join(", "),
            type_name(ret)
        ),
        TypeExprKind::Array(element) => format!("[{}]", type_name(element)),
        TypeExprKind::Tuple(elements) => format!(
            "({})",
            elements
                .iter()
                .map(type_name)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TypeExprKind::ImplTrait { trait_ty } => format!("impl {}", type_name(trait_ty)),
    }
}

fn documentation_before(source: &str, offset: usize) -> Option<String> {
    let prefix = &source[..offset.min(source.len())];
    let mut lines = prefix.lines().rev();
    let mut documentation = Vec::new();
    for line in &mut lines {
        let trimmed = line.trim();
        if let Some(text) = trimmed.strip_prefix("///") {
            documentation.push(text.trim().to_string());
        } else if trimmed.is_empty() && documentation.is_empty() {
            continue;
        } else {
            break;
        }
    }
    documentation.reverse();
    (!documentation.is_empty()).then(|| documentation.join("\n"))
}
