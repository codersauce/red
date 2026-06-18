//! Name resolution and early semantic analysis for Husk.
//!
//! This crate currently defines:
//! - A basic symbol representation for top-level items.
//! - A resolver that collects top-level symbols from a `husk_ast::File`.
//! - A unified `StdlibIndex` for stdlib method information.

mod stdlib_index;

pub use stdlib_index::{InferenceStrategy, MethodKey, StdlibIndex, StdlibMethodInfo};

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use husk_ast::{
    Block, CfgPredicate, ClosureParam, EnumVariantFields, Expr, ExprKind, File, FormatSegment,
    Ident, Item, ItemKind, LiteralKind, MatchArm, Param, Pattern, PatternKind, Span, Stmt,
    StmtKind, TypeExpr, TypeExprKind,
};
use husk_parser::parse_str;
use husk_types::{PrimitiveType, Type};

/// Unique identifier for a symbol within a module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymbolId(pub u32);

/// Kinds of symbols that can be defined at the top level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    TypeAlias,
    ExternFn,
    ExternMod,
    ExternStatic,
    Trait,
    Impl,
}

/// A resolved symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub id: SymbolId,
    pub name: String,
    pub kind: SymbolKind,
    pub span: Span,
}

/// A collection of top-level symbols for a single Husk module/file.
#[derive(Debug, Default)]
pub struct ModuleSymbols {
    pub symbols: Vec<Symbol>,
    pub by_name: HashMap<String, SymbolId>,
    pub errors: Vec<SemanticError>,
}

impl ModuleSymbols {
    /// Resolve top-level symbols from an AST `File`.
    pub fn from_file(file: &File) -> Self {
        let mut resolver = Resolver::new();
        resolver.collect(file);
        resolver.finish()
    }

    /// Look up a symbol by name.
    pub fn get(&self, name: &str) -> Option<&Symbol> {
        let id = *self.by_name.get(name)?;
        self.symbols.get(id.0 as usize)
    }
}

/// A semantic error produced during name resolution or later phases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticError {
    pub message: String,
    pub span: Span,
}

/// Maps variable binding and usage spans to their resolved unique names.
///
/// During code generation, variable references need unique names to handle
/// shadowing correctly. JavaScript doesn't allow redeclaring `let` in the
/// same scope, so we use alpha-conversion (renaming) to generate unique names.
///
/// The map uses byte ranges (start, end) as keys since `Span` doesn't implement Hash.
/// Values are the resolved names (e.g., "x", "x$1", "x$2").
pub type NameResolution = HashMap<(usize, usize), String>;

/// Maps expression spans to their resolved types for codegen.
/// Used for type-dependent operations like .into(), .parse(), .try_into().
pub type TypeResolution = HashMap<(usize, usize), String>;

/// Maps call expression spans to (enum_name, variant_name) for imported variant calls.
/// Used by codegen to emit enum variant construction for calls like `Some(42)`.
pub type VariantCallMap = HashMap<(usize, usize), (String, String)>;

/// Maps pattern spans to (enum_name, variant_name) for imported variant patterns.
/// Used by codegen to emit proper tag checks for patterns like `None` in match arms.
pub type VariantPatternMap = HashMap<(usize, usize), (String, String)>;

/// A reference to a symbol, including its span and the file it's in (if known).
#[derive(Debug, Clone)]
pub struct SymbolReference {
    /// The byte range of this reference
    pub span: Span,
    /// Module path if this reference is in a different module (for multi-file projects)
    pub module_path: Option<Vec<String>>,
}

/// Maps symbol names to all spans where they are referenced.
/// Used by LSP for rename operations to find all usages of a symbol.
/// Keys are (symbol_name, symbol_kind) to disambiguate different kinds of symbols.
pub type ReferenceMap = HashMap<(String, ReferenceKind), Vec<SymbolReference>>;

/// Kind of symbol reference (to disambiguate same-named symbols of different kinds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReferenceKind {
    /// Function or method
    Function,
    /// Struct type
    Struct,
    /// Enum type
    Enum,
    /// Enum variant
    Variant,
    /// Type alias
    TypeAlias,
    /// Trait
    Trait,
    /// Local variable or parameter
    Variable,
    /// Struct field
    Field,
}

/// Information for LSP hover display (rust-analyzer style).
#[derive(Debug, Clone)]
pub struct HoverInfo {
    /// The formatted signature, e.g., "total: i64" or "fn foo(a: i32) -> bool"
    pub signature: String,
    /// Doc comments if available (populated by LSP from token trivia)
    pub docs: Option<String>,
    /// Span of the definition (for "Go to Definition" links)
    pub definition_span: Option<Span>,
}

/// Maps expression/identifier spans to hover information for LSP.
/// Keys are (start_byte, end_byte), values are the hover info to display.
pub type HoverMap = HashMap<(usize, usize), HoverInfo>;

/// The result of running semantic analysis (name resolution + type checking) on a Husk file.
#[derive(Debug)]
pub struct SemanticResult {
    pub symbols: ModuleSymbols,
    pub type_errors: Vec<SemanticError>,
    /// Maps variable spans to their resolved unique names for codegen.
    pub name_resolution: NameResolution,
    /// Maps expression spans to resolved type names for conversion methods.
    pub type_resolution: TypeResolution,
    /// Maps call expression spans to (enum_name, variant_name) for imported variant calls.
    pub variant_calls: VariantCallMap,
    /// Maps pattern spans to (enum_name, variant_name) for imported variant patterns.
    pub variant_patterns: VariantPatternMap,
    /// Maps spans to hover information for LSP.
    pub hover_info: HoverMap,
    /// Maps symbol names to all their references for LSP rename operations.
    pub references: ReferenceMap,
}

/// Options controlling semantic analysis.
#[derive(Debug, Clone, Default)]
pub struct SemanticOptions {
    /// If true, inject the stdlib prelude (Option/Result) into the type environment.
    pub prelude: bool,
    /// Cfg flags that are currently enabled (e.g., "test" for test mode).
    pub cfg_flags: HashSet<String>,
}

impl SemanticOptions {
    /// Create options with prelude enabled (default behavior).
    pub fn with_prelude() -> Self {
        Self {
            prelude: true,
            cfg_flags: HashSet::new(),
        }
    }

    /// Create options with the test cfg flag enabled.
    pub fn with_test() -> Self {
        let mut flags = HashSet::new();
        flags.insert("test".to_string());
        Self {
            prelude: true,
            cfg_flags: flags,
        }
    }
}

/// Evaluate a cfg predicate against a set of enabled flags.
fn evaluate_cfg(predicate: &CfgPredicate, flags: &HashSet<String>) -> bool {
    match predicate {
        CfgPredicate::Flag(name) => flags.contains(name),
        CfgPredicate::KeyValue { key, value } => {
            // For key-value predicates like cfg(feature = "foo"),
            // check if the flag "feature=foo" is enabled
            let combined = format!("{}={}", key, value);
            flags.contains(&combined)
        }
        CfgPredicate::All(predicates) => predicates.iter().all(|p| evaluate_cfg(p, flags)),
        CfgPredicate::Any(predicates) => predicates.iter().any(|p| evaluate_cfg(p, flags)),
        CfgPredicate::Not(predicate) => !evaluate_cfg(predicate, flags),
    }
}

/// Check if an item should be included based on its cfg attributes.
fn item_passes_cfg(item: &Item, flags: &HashSet<String>) -> bool {
    // If the item has a cfg predicate, evaluate it
    if let Some(predicate) = item.cfg_predicate() {
        evaluate_cfg(predicate, flags)
    } else {
        // No cfg attribute means always include
        true
    }
}

/// Filter a file's items based on cfg predicates.
pub fn filter_items_by_cfg(file: &File, flags: &HashSet<String>) -> File {
    File {
        items: file
            .items
            .iter()
            .filter(|item| item_passes_cfg(item, flags))
            .cloned()
            .collect(),
    }
}

/// Format a Type for display in error messages.
fn format_type(ty: &Type) -> String {
    match ty {
        Type::Primitive(p) => match p {
            PrimitiveType::I32 => "i32".to_string(),
            PrimitiveType::I64 => "i64".to_string(),
            PrimitiveType::F64 => "f64".to_string(),
            PrimitiveType::Bool => "bool".to_string(),
            PrimitiveType::String => "String".to_string(),
            PrimitiveType::Unit => "()".to_string(),
        },
        Type::Array(inner) => format!("[{}]", format_type(inner)),
        Type::Named { name, args } if args.is_empty() => name.clone(),
        Type::Named { name, args } => {
            let args_str = args.iter().map(format_type).collect::<Vec<_>>().join(", ");
            format!("{}<{}>", name, args_str)
        }
        Type::Function { params, ret } => {
            let params_str = params
                .iter()
                .map(format_type)
                .collect::<Vec<_>>()
                .join(", ");
            format!("fn({}) -> {}", params_str, format_type(ret))
        }
        Type::Var(id) => format!("?{}", id.0),
        Type::Tuple(elements) => {
            let elements_str = elements
                .iter()
                .map(format_type)
                .collect::<Vec<_>>()
                .join(", ");
            format!("({})", elements_str)
        }
        Type::ImplTrait { trait_ty } => {
            format!("impl {}", format_type(trait_ty))
        }
    }
}

/// Run semantic analysis (name resolution + type checking) over the given file with options.
pub fn analyze_file_with_options(file: &File, opts: SemanticOptions) -> SemanticResult {
    // Filter items based on cfg predicates
    let filtered_file = filter_items_by_cfg(file, &opts.cfg_flags);

    let symbols = ModuleSymbols::from_file(&filtered_file);

    let mut checker = TypeChecker::new();
    if opts.prelude {
        let prelude = get_prelude_file();
        checker.build_type_env(prelude);
        checker.build_type_env(js_globals_file());
    }
    checker.build_type_env(&filtered_file);
    let (
        type_errors,
        name_resolution,
        type_resolution,
        variant_calls,
        variant_patterns,
        hover_info,
        references,
    ) = checker.check_file(&filtered_file);
    SemanticResult {
        symbols,
        type_errors,
        name_resolution,
        type_resolution,
        variant_calls,
        variant_patterns,
        hover_info,
        references,
    }
}

/// Run full semantic analysis with the stdlib prelude enabled (default).
pub fn analyze_file(file: &File) -> SemanticResult {
    analyze_file_with_options(file, SemanticOptions::with_prelude())
}

/// Run semantic analysis without injecting the stdlib prelude.
pub fn analyze_file_without_prelude(file: &File) -> SemanticResult {
    analyze_file_with_options(
        file,
        SemanticOptions {
            prelude: false,
            cfg_flags: HashSet::new(),
        },
    )
}

static PRELUDE_SRC: &str = include_str!("stdlib/core.hk");
static PRELUDE_AST: OnceLock<File> = OnceLock::new();
static STDLIB_INDEX: OnceLock<StdlibIndex> = OnceLock::new();

/// Returns the stdlib prelude file for use by codegen to collect method
/// name mappings (e.g., #[js_name] attributes).
pub fn get_prelude_file() -> &'static File {
    PRELUDE_AST.get_or_init(|| {
        let parsed = parse_str(PRELUDE_SRC);
        if !parsed.errors.is_empty() {
            panic!("failed to parse stdlib prelude: {:?}", parsed.errors);
        }
        parsed.file.expect("stdlib prelude parse produced no AST")
    })
}

/// Returns the global stdlib index for method lookups.
pub fn get_stdlib_index() -> &'static StdlibIndex {
    STDLIB_INDEX.get_or_init(|| StdlibIndex::from_file(get_prelude_file()))
}

static JS_GLOBALS_SRC: &str = include_str!("std/js/globals.hk");
static JS_GLOBALS_AST: OnceLock<File> = OnceLock::new();

fn js_globals_file() -> &'static File {
    JS_GLOBALS_AST.get_or_init(|| {
        let parsed = parse_str(JS_GLOBALS_SRC);
        if !parsed.errors.is_empty() {
            panic!("failed to parse JS globals: {:?}", parsed.errors);
        }
        parsed.file.expect("JS globals parse produced no AST")
    })
}

struct Resolver {
    symbols: Vec<Symbol>,
    by_name: HashMap<String, SymbolId>,
    errors: Vec<SemanticError>,
}

// =============== Type environment and type checking ===============

/// Information about a struct type.
#[derive(Debug, Clone)]
struct StructDef {
    type_params: Vec<String>,
    fields: HashMap<String, TypeExpr>,
}

/// Information about an enum variant.
#[derive(Debug, Clone)]
struct VariantDef {
    name: String,
    fields: EnumVariantFields,
}

/// Information about an enum type.
#[derive(Debug, Clone)]
struct EnumDef {
    type_params: Vec<String>,
    variants: Vec<VariantDef>,
}

/// Information about a type parameter with optional trait bounds.
#[derive(Debug, Clone, PartialEq)]
struct TypeParamInfo {
    name: String,
    /// Trait bounds on this type parameter (e.g., `T: PartialEq + Clone`)
    bounds: Vec<String>,
}

/// Information about a function type.
#[derive(Debug, Clone)]
struct FnDef {
    type_params: Vec<TypeParamInfo>,
    params: Vec<Param>,
    ret_type: Option<TypeExpr>,
}

impl FnDef {
    /// Get just the type parameter names (for use in type resolution).
    fn type_param_names(&self) -> Vec<String> {
        self.type_params.iter().map(|tp| tp.name.clone()).collect()
    }
}

/// Convert an AST TypeParam to a TypeParamInfo.
fn type_param_to_info(tp: &husk_ast::TypeParam) -> TypeParamInfo {
    TypeParamInfo {
        name: tp.name.name.clone(),
        bounds: tp.bounds.iter().map(type_expr_to_trait_name).collect(),
    }
}

/// Extract the trait name from a TypeExpr (for bounds).
fn type_expr_to_trait_name(ty: &husk_ast::TypeExpr) -> String {
    use husk_ast::TypeExprKind;
    match &ty.kind {
        TypeExprKind::Named(name) => name.name.clone(),
        TypeExprKind::Generic { name, args } => {
            let arg_strs: Vec<String> = args.iter().map(type_expr_to_trait_name).collect();
            format!("{}<{}>", name.name, arg_strs.join(", "))
        }
        TypeExprKind::Function { .. } => "Fn".to_string(), // Simplified
        TypeExprKind::Array(elem) => format!("[{}]", type_expr_to_trait_name(elem)),
        TypeExprKind::Tuple(types) => {
            let type_strs: Vec<String> = types.iter().map(type_expr_to_trait_name).collect();
            format!("({})", type_strs.join(", "))
        }
        TypeExprKind::ImplTrait { trait_ty } => type_expr_to_trait_name(trait_ty),
    }
}

/// Information about an imported JS module.
/// The module name becomes a callable identifier that returns an opaque type.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct ModuleDef {
    name: String,
    /// Return type for calling this module as a function.
    /// Inferred from the first struct in the same extern block if not the capitalized name.
    ret_type: Option<String>,
    /// Functions defined within this module (for extern mod blocks with functions).
    functions: HashMap<String, FnDef>,
}

/// Information about a trait definition.
#[derive(Debug, Clone)]
struct TraitInfo {
    #[allow(dead_code)]
    type_params: Vec<String>,
    /// Supertraits that this trait requires (e.g., `Eq: PartialEq`)
    supertraits: Vec<String>,
    /// Method signatures: name -> (params, return type)
    methods: HashMap<String, MethodSig>,
}

/// A method signature (for traits and impls).
#[derive(Debug, Clone)]
struct MethodSig {
    #[allow(dead_code)]
    receiver: Option<husk_ast::SelfReceiver>,
    #[allow(dead_code)]
    params: Vec<Param>,
    #[allow(dead_code)]
    ret_type: Option<TypeExpr>,
}

/// Information about an impl block.
#[derive(Debug, Clone)]
struct ImplInfo {
    /// The trait being implemented (None for inherent impl).
    /// Used by `verify_trait_impls` to check method completeness.
    trait_name: Option<String>,
    /// The type this impl is for
    self_ty_name: String,
    /// Methods defined in this impl
    methods: HashMap<String, MethodInfo>,
    /// Extern properties defined in this impl
    properties: HashMap<String, PropertyInfo>,
    /// Location of the impl block for error reporting
    span: Span,
}

/// Information about an extern property in an impl block.
#[derive(Debug, Clone)]
struct PropertyInfo {
    /// The property type
    ty: TypeExpr,
    /// Whether the property has a getter
    #[allow(dead_code)]
    has_getter: bool,
    /// Whether the property has a setter
    #[allow(dead_code)]
    has_setter: bool,
}

/// Information about a method in an impl block.
#[derive(Debug, Clone)]
struct MethodInfo {
    #[allow(dead_code)]
    receiver: Option<husk_ast::SelfReceiver>,
    #[allow(dead_code)]
    params: Vec<Param>,
    ret_type: Option<TypeExpr>,
}

#[derive(Debug, Default)]
struct TypeEnv {
    structs: HashMap<String, StructDef>,
    enums: HashMap<String, EnumDef>,
    type_aliases: HashMap<String, TypeExpr>,
    functions: HashMap<String, FnDef>,
    /// Imported JS modules (from `mod name;` in extern blocks).
    /// These become callable identifiers.
    modules: HashMap<String, ModuleDef>,
    /// Trait definitions
    traits: HashMap<String, TraitInfo>,
    /// Impl blocks (can have multiple impls for the same type)
    impls: Vec<ImplInfo>,
    /// Static/global variables and constants (from `static name: Type;` and `const name: Type;` in extern blocks)
    statics: HashMap<String, TypeExpr>,
    /// Imported enum variants that can be used without the enum prefix.
    /// Maps variant name -> (enum_name, variant_def).
    /// Populated from `use Enum::*;` or `use Enum::{A, B};` statements.
    variant_imports: HashMap<String, (String, VariantDef)>,
}

impl TypeEnv {
    /// Check if a type implements a trait (directly or through supertrait requirements).
    ///
    /// This performs the following checks:
    /// 1. Looks for a direct `impl Trait for Type` block
    /// 2. For supertraits, checks if all required supertraits are also implemented
    ///
    /// Returns true if the type implements the trait (including via supertrait satisfaction).
    ///
    /// Note: For generic types like `Vec<i32>`, we also check the base type `Vec`
    /// since impls are registered on the base struct name.
    fn type_implements_trait(&self, type_name: &str, trait_name: &str) -> bool {
        // Extract base type name for generic types (e.g., "Vec<i32>" -> "Vec")
        let base_type_name = type_name.split('<').next().unwrap_or(type_name);

        // Check for direct trait implementation
        for impl_info in &self.impls {
            // Match either the exact type name or the base type for generics
            if (impl_info.self_ty_name == type_name || impl_info.self_ty_name == base_type_name)
                && let Some(ref impl_trait) = impl_info.trait_name
                && impl_trait == trait_name
            {
                return true;
            }
        }
        false
    }

    /// Get the list of missing supertrait implementations for a type implementing a trait.
    ///
    /// Returns a list of all transitively missing supertraits that are required but not implemented.
    /// For example, if Eq: PartialEq and PartialEq: SomeTrait, and SomeTrait is not implemented,
    /// this will return both PartialEq (if missing) and SomeTrait.
    fn missing_supertraits(&self, type_name: &str, trait_name: &str) -> Vec<String> {
        let mut missing = Vec::new();
        let mut visited = std::collections::HashSet::new();
        self.collect_missing_supertraits_recursive(
            type_name,
            trait_name,
            &mut missing,
            &mut visited,
        );
        missing
    }

    /// Recursive helper for missing_supertraits with cycle detection.
    fn collect_missing_supertraits_recursive(
        &self,
        type_name: &str,
        trait_name: &str,
        missing: &mut Vec<String>,
        visited: &mut std::collections::HashSet<String>,
    ) {
        // Guard against cycles
        if visited.contains(trait_name) {
            return;
        }
        visited.insert(trait_name.to_string());

        if let Some(trait_info) = self.traits.get(trait_name) {
            for supertrait in &trait_info.supertraits {
                // Check if this supertrait is missing
                if !self.type_implements_trait(type_name, supertrait)
                    && !missing.contains(supertrait)
                {
                    missing.push(supertrait.clone());
                }
                // Recursively check the supertrait's supertraits
                self.collect_missing_supertraits_recursive(type_name, supertrait, missing, visited);
            }
        }
    }
}

/// Extract the inner type from a trait name like "From<i32>" -> Some("i32")
/// Returns None if the trait name doesn't match the expected prefix/suffix pattern.
fn extract_trait_type_arg<'a>(trait_name: &'a str, prefix: &str) -> Option<&'a str> {
    if trait_name.starts_with(prefix) && trait_name.ends_with('>') {
        Some(&trait_name[prefix.len()..trait_name.len() - 1])
    } else {
        None
    }
}

/// Check if a type argument string represents a generic type parameter.
/// Currently matches single uppercase letters (e.g., "T", "U").
/// NOTE: Multi-letter type params like "Key" are not matched (known limitation).
fn is_generic_type_param(inner: &str) -> bool {
    inner.len() == 1 && inner.chars().next().is_some_and(|c| c.is_uppercase())
}

/// Extract a simple type name from a TypeExpr (for impl/trait lookups).
fn type_expr_to_name(ty: &TypeExpr) -> String {
    match &ty.kind {
        TypeExprKind::Named(ident) => ident.name.clone(),
        TypeExprKind::Generic { name, args } => {
            if args.is_empty() {
                name.name.clone()
            } else {
                let args_str = args
                    .iter()
                    .map(type_expr_to_name)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{}<{}>", name.name, args_str)
            }
        }
        TypeExprKind::Function { .. } => "<fn>".to_string(),
        TypeExprKind::Array(elem) => format!("[{}]", type_expr_to_name(elem)),
        TypeExprKind::Tuple(types) => {
            let type_strs: Vec<String> = types.iter().map(type_expr_to_name).collect();
            format!("({})", type_strs.join(", "))
        }
        TypeExprKind::ImplTrait { trait_ty } => type_expr_to_name(trait_ty),
    }
}

/// Get the canonical name for a primitive type.
/// Used to look up impl blocks for primitive types (e.g., `impl String { ... }`).
fn primitive_type_name(p: &PrimitiveType) -> &'static str {
    match p {
        PrimitiveType::String => "String",
        PrimitiveType::I32 => "i32",
        PrimitiveType::I64 => "i64",
        PrimitiveType::F64 => "f64",
        PrimitiveType::Bool => "bool",
        PrimitiveType::Unit => "()",
    }
}

/// Substitute a type parameter with a concrete type.
/// Used to resolve generic array method return types like `impl<T> [T] { fn slice() -> [T] }`.
fn substitute_type_param(ty: &Type, param: &str, replacement: &Type) -> Type {
    match ty {
        Type::Named { name, args } if name == param && args.is_empty() => replacement.clone(),
        Type::Named { name, args } => Type::Named {
            name: name.clone(),
            args: args
                .iter()
                .map(|a| substitute_type_param(a, param, replacement))
                .collect(),
        },
        Type::Array(elem) => Type::Array(Box::new(substitute_type_param(elem, param, replacement))),
        Type::Function { params, ret } => Type::Function {
            params: params
                .iter()
                .map(|p| substitute_type_param(p, param, replacement))
                .collect(),
            ret: Box::new(substitute_type_param(ret, param, replacement)),
        },
        Type::Tuple(elements) => Type::Tuple(
            elements
                .iter()
                .map(|e| substitute_type_param(e, param, replacement))
                .collect(),
        ),
        Type::ImplTrait { trait_ty } => Type::ImplTrait {
            trait_ty: Box::new(substitute_type_param(trait_ty, param, replacement)),
        },
        Type::Primitive(_) | Type::Var(_) => ty.clone(),
    }
}

struct TypeChecker {
    env: TypeEnv,
    errors: Vec<SemanticError>,
    /// Maps variable spans to their resolved unique names for codegen.
    name_resolution: NameResolution,
    /// Maps expression spans to resolved type names for conversion methods.
    type_resolution: TypeResolution,
    /// Maps call expression spans to (enum_name, variant_name) for imported variant calls.
    variant_calls: VariantCallMap,
    /// Maps pattern spans to (enum_name, variant_name) for imported variant patterns.
    variant_patterns: VariantPatternMap,
    /// Maps spans to hover information for LSP.
    hover_info: HoverMap,
    /// Maps symbol names to all their references for LSP rename operations.
    references: ReferenceMap,
}

struct IteratorMethodArgs<'a> {
    elem_ty: &'a Type,
    receiver_ty: &'a Type,
    method_name: &'a str,
    args: &'a [Expr],
    receiver: &'a Expr,
    type_args: &'a [TypeExpr],
    span: &'a husk_ast::Span,
}

impl TypeChecker {
    fn new() -> Self {
        Self {
            env: TypeEnv::default(),
            errors: Vec::new(),
            name_resolution: HashMap::new(),
            type_resolution: HashMap::new(),
            variant_calls: HashMap::new(),
            variant_patterns: HashMap::new(),
            hover_info: HashMap::new(),
            references: HashMap::new(),
        }
    }

    /// Record a reference to a symbol.
    fn add_reference(&mut self, name: &str, kind: ReferenceKind, span: Span) {
        let key = (name.to_string(), kind);
        self.references
            .entry(key)
            .or_default()
            .push(SymbolReference {
                span,
                module_path: None,
            });
    }

    fn build_type_env(&mut self, file: &File) {
        for item in file.items.iter() {
            match &item.kind {
                ItemKind::Struct {
                    name,
                    type_params,
                    fields,
                } => {
                    let def = StructDef {
                        type_params: type_params.iter().map(|id| id.name.clone()).collect(),
                        fields: fields
                            .iter()
                            .map(|f| (f.name.name.clone(), f.ty.clone()))
                            .collect(),
                    };
                    self.env.structs.insert(name.name.clone(), def.clone());

                    // Track struct definition for rename support
                    self.add_reference(&name.name, ReferenceKind::Struct, name.span.clone());

                    // Track field definitions for rename support
                    for field in fields {
                        let field_key = format!("{}.{}", name.name, field.name.name);
                        self.add_reference(
                            &field_key,
                            ReferenceKind::Field,
                            field.name.span.clone(),
                        );
                    }

                    // Register hover info for struct definition
                    let type_params_str = if type_params.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "<{}>",
                            type_params
                                .iter()
                                .map(|p| p.name.clone())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    let fields_str = fields
                        .iter()
                        .map(|f| format!("    {}: {}", f.name.name, self.format_type_expr(&f.ty)))
                        .collect::<Vec<_>>()
                        .join(",\n");
                    let signature = if fields.is_empty() {
                        format!("struct {}{} {{}}", name.name, type_params_str)
                    } else {
                        format!(
                            "struct {}{} {{\n{}\n}}",
                            name.name, type_params_str, fields_str
                        )
                    };
                    self.hover_info.insert(
                        (name.span.range.start, name.span.range.end),
                        HoverInfo {
                            signature,
                            docs: None,
                            definition_span: Some(item.span.clone()),
                        },
                    );
                }
                ItemKind::Enum {
                    name,
                    type_params,
                    variants,
                } => {
                    let def = EnumDef {
                        type_params: type_params.iter().map(|id| id.name.clone()).collect(),
                        variants: variants
                            .iter()
                            .map(|v| VariantDef {
                                name: v.name.name.clone(),
                                fields: v.fields.clone(),
                            })
                            .collect(),
                    };
                    self.env.enums.insert(name.name.clone(), def);

                    // Track enum definition for rename support
                    self.add_reference(&name.name, ReferenceKind::Enum, name.span.clone());

                    // Track variant definitions for rename support
                    for variant in variants {
                        let variant_key = format!("{}::{}", name.name, variant.name.name);
                        self.add_reference(
                            &variant_key,
                            ReferenceKind::Variant,
                            variant.name.span.clone(),
                        );
                    }

                    // Register hover info for enum definition
                    let type_params_str = if type_params.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "<{}>",
                            type_params
                                .iter()
                                .map(|p| p.name.clone())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    let variants_str = variants
                        .iter()
                        .map(|v| {
                            let variant_name = &v.name.name;
                            match &v.fields {
                                EnumVariantFields::Unit => format!("    {}", variant_name),
                                EnumVariantFields::Tuple(types) => {
                                    let types_str = types
                                        .iter()
                                        .map(|t| self.format_type_expr(t))
                                        .collect::<Vec<_>>()
                                        .join(", ");
                                    format!("    {}({})", variant_name, types_str)
                                }
                                EnumVariantFields::Struct(fields) => {
                                    let fields_str = fields
                                        .iter()
                                        .map(|f| {
                                            format!(
                                                "{}: {}",
                                                f.name.name,
                                                self.format_type_expr(&f.ty)
                                            )
                                        })
                                        .collect::<Vec<_>>()
                                        .join(", ");
                                    format!("    {} {{ {} }}", variant_name, fields_str)
                                }
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(",\n");
                    let signature = format!(
                        "enum {}{} {{\n{}\n}}",
                        name.name, type_params_str, variants_str
                    );
                    self.hover_info.insert(
                        (name.span.range.start, name.span.range.end),
                        HoverInfo {
                            signature,
                            docs: None,
                            definition_span: Some(item.span.clone()),
                        },
                    );
                }
                ItemKind::TypeAlias { name, ty } => {
                    self.env.type_aliases.insert(name.name.clone(), ty.clone());
                    // Track type alias definition for rename support
                    self.add_reference(&name.name, ReferenceKind::TypeAlias, name.span.clone());
                }
                ItemKind::Fn {
                    name,
                    type_params,
                    params,
                    ret_type,
                    ..
                } => {
                    let def = FnDef {
                        type_params: type_params.iter().map(type_param_to_info).collect(),
                        params: params.clone(),
                        ret_type: ret_type.clone(),
                    };
                    self.env.functions.insert(name.name.clone(), def);

                    // Track function definition for rename support
                    self.add_reference(&name.name, ReferenceKind::Function, name.span.clone());

                    // Register hover info for function definition
                    let type_params_str = if type_params.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "<{}>",
                            type_params
                                .iter()
                                .map(|p| {
                                    if p.bounds.is_empty() {
                                        p.name.name.clone()
                                    } else {
                                        let bounds = p
                                            .bounds
                                            .iter()
                                            .map(|b| self.format_type_expr(b))
                                            .collect::<Vec<_>>()
                                            .join(" + ");
                                        format!("{}: {}", p.name.name, bounds)
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    let params_str = params
                        .iter()
                        .map(|p| format!("{}: {}", p.name.name, self.format_type_expr(&p.ty)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let ret_str = ret_type
                        .as_ref()
                        .map(|t| self.format_type_expr(t))
                        .unwrap_or_else(|| "()".to_string());
                    let signature = format!(
                        "fn {}{}({}) -> {}",
                        name.name, type_params_str, params_str, ret_str
                    );
                    self.hover_info.insert(
                        (name.span.range.start, name.span.range.end),
                        HoverInfo {
                            signature,
                            docs: None,
                            definition_span: Some(item.span.clone()),
                        },
                    );
                }
                ItemKind::ExternBlock { items, .. } => {
                    // First pass: collect all struct names in this extern block
                    let mut struct_names: Vec<String> = Vec::new();
                    for ext in items {
                        if let husk_ast::ExternItemKind::Struct { name, .. } = &ext.kind {
                            struct_names.push(name.name.clone());
                        }
                    }

                    // Second pass: register all items
                    for ext in items {
                        match &ext.kind {
                            husk_ast::ExternItemKind::Fn {
                                name,
                                params,
                                ret_type,
                            } => {
                                let def = FnDef {
                                    type_params: Vec::new(), // Extern functions don't have generics
                                    params: params.clone(),
                                    ret_type: ret_type.clone(),
                                };
                                self.env.functions.insert(name.name.clone(), def);
                            }
                            husk_ast::ExternItemKind::Mod {
                                binding,
                                items: mod_items,
                                ..
                            } => {
                                if mod_items.is_empty() {
                                    // Simple module import becomes a callable identifier.
                                    // Try to infer return type from struct in same block:
                                    // 1. First, try capitalizing the first letter (e.g., express -> Express)
                                    // 2. If that doesn't match, use the first struct in the block
                                    let capitalized = {
                                        let mut chars = binding.name.chars();
                                        match chars.next() {
                                            Some(c) => {
                                                c.to_uppercase().collect::<String>()
                                                    + chars.as_str()
                                            }
                                            None => binding.name.clone(),
                                        }
                                    };
                                    let ret_type = if struct_names.contains(&capitalized) {
                                        Some(capitalized)
                                    } else if struct_names.contains(&"Database".to_string()) {
                                        // Common pattern for modules that export a Database constructor
                                        Some("Database".to_string())
                                    } else {
                                        // Use the first struct in the block as a fallback
                                        struct_names.first().cloned()
                                    };
                                    let def = ModuleDef {
                                        name: binding.name.clone(),
                                        ret_type,
                                        functions: HashMap::new(),
                                    };
                                    self.env.modules.insert(binding.name.clone(), def);
                                } else {
                                    // Mod block with functions - create module with its functions.
                                    // Check for #[default] on mod or on any function
                                    let mod_is_default = ext.is_default();
                                    let fn_has_default = mod_items.iter().any(|mi| mi.is_default());
                                    let has_default = mod_is_default || fn_has_default;

                                    let mut functions = HashMap::new();
                                    for mod_item in mod_items {
                                        // ModItemKind has only Fn variant (MVP scope)
                                        let husk_ast::ModItemKind::Fn {
                                            name,
                                            params,
                                            ret_type,
                                        } = &mod_item.kind;
                                        let def = FnDef {
                                            type_params: Vec::new(), // Mod functions don't have generics
                                            params: params.clone(),
                                            ret_type: ret_type.clone(),
                                        };
                                        functions.insert(name.name.clone(), def.clone());

                                        // When #[default] is present (on mod or functions),
                                        // codegen creates wrapper functions that allow direct calls.
                                        // Register these functions in the env so semantic analysis passes.
                                        if has_default {
                                            self.env.functions.insert(name.name.clone(), def);
                                        }
                                    }
                                    let module_def = ModuleDef {
                                        name: binding.name.clone(),
                                        ret_type: None,
                                        functions,
                                    };
                                    self.env.modules.insert(binding.name.clone(), module_def);
                                }
                            }
                            husk_ast::ExternItemKind::Struct { name, type_params } => {
                                // Register extern struct as a type
                                let def = StructDef {
                                    type_params: type_params
                                        .iter()
                                        .map(|p| p.name.clone())
                                        .collect(),
                                    fields: HashMap::new(), // Extern structs are opaque
                                };
                                self.env.structs.insert(name.name.clone(), def);
                            }
                            husk_ast::ExternItemKind::Static { name, ty } => {
                                // Register extern static variable
                                self.env.statics.insert(name.name.clone(), ty.clone());
                            }
                            husk_ast::ExternItemKind::Const { name, ty } => {
                                // Register extern const - treated same as static for lookups
                                self.env.statics.insert(name.name.clone(), ty.clone());
                            }
                            husk_ast::ExternItemKind::Impl { self_ty, items, .. } => {
                                // Handle impl blocks inside extern blocks - register methods on the type
                                let self_ty_name = type_expr_to_name(self_ty);
                                let mut methods = HashMap::new();
                                let mut properties = HashMap::new();
                                for item in items {
                                    match &item.kind {
                                        husk_ast::ImplItemKind::Method(method) => {
                                            methods.insert(
                                                method.name.name.clone(),
                                                MethodInfo {
                                                    receiver: method.receiver,
                                                    params: method.params.clone(),
                                                    ret_type: method.ret_type.clone(),
                                                },
                                            );
                                        }
                                        husk_ast::ImplItemKind::Property(prop) => {
                                            properties.insert(
                                                prop.name.name.clone(),
                                                PropertyInfo {
                                                    ty: prop.ty.clone(),
                                                    has_getter: prop.has_getter(),
                                                    has_setter: prop.has_setter(),
                                                },
                                            );
                                        }
                                    }
                                }
                                self.env.impls.push(ImplInfo {
                                    trait_name: None, // Extern impls don't implement traits
                                    self_ty_name,
                                    methods,
                                    properties,
                                    span: ext.span.clone(),
                                });
                            }
                        }
                    }
                }
                ItemKind::Use { path, kind } => {
                    self.process_variant_import(path, kind);
                }
                ItemKind::Trait(trait_def) => {
                    let mut methods = HashMap::new();
                    for item in &trait_def.items {
                        // TraitItemKind has only Method variant (MVP scope)
                        let husk_ast::TraitItemKind::Method(method) = &item.kind;
                        methods.insert(
                            method.name.name.clone(),
                            MethodSig {
                                receiver: method.receiver,
                                params: method.params.clone(),
                                ret_type: method.ret_type.clone(),
                            },
                        );
                    }
                    let info = TraitInfo {
                        type_params: trait_def
                            .type_params
                            .iter()
                            .map(|p| p.name.name.clone())
                            .collect(),
                        supertraits: trait_def
                            .supertraits
                            .iter()
                            .map(type_expr_to_name)
                            .collect(),
                        methods,
                    };
                    self.env.traits.insert(trait_def.name.name.clone(), info);
                    // Track trait definition for rename support
                    self.add_reference(
                        &trait_def.name.name,
                        ReferenceKind::Trait,
                        trait_def.name.span.clone(),
                    );
                }
                ItemKind::Impl(impl_block) => {
                    let self_ty_name = type_expr_to_name(&impl_block.self_ty);
                    let trait_name = impl_block.trait_ref.as_ref().map(type_expr_to_name);

                    let mut methods = HashMap::new();
                    let mut properties = HashMap::new();
                    for item in &impl_block.items {
                        match &item.kind {
                            husk_ast::ImplItemKind::Method(method) => {
                                methods.insert(
                                    method.name.name.clone(),
                                    MethodInfo {
                                        receiver: method.receiver,
                                        params: method.params.clone(),
                                        ret_type: method.ret_type.clone(),
                                    },
                                );
                            }
                            husk_ast::ImplItemKind::Property(prop) => {
                                properties.insert(
                                    prop.name.name.clone(),
                                    PropertyInfo {
                                        ty: prop.ty.clone(),
                                        has_getter: prop.has_getter(),
                                        has_setter: prop.has_setter(),
                                    },
                                );
                            }
                        }
                    }
                    self.env.impls.push(ImplInfo {
                        trait_name,
                        self_ty_name,
                        methods,
                        properties,
                        span: impl_block.span.clone(),
                    });
                }
            }
        }
    }

    /// Process a use statement that may import enum variants.
    /// For `use Enum::*;` imports all variants.
    /// For `use Enum::{A, B};` or `use Enum::A;` imports specific variants.
    fn process_variant_import(&mut self, path: &[Ident], kind: &husk_ast::UseKind) {
        // Skip regular item imports
        if matches!(kind, husk_ast::UseKind::Item) {
            return;
        }

        // Get the enum name (last path segment)
        let Some(enum_ident) = path.last() else {
            return;
        };
        let enum_name = &enum_ident.name;

        // Look up the enum definition
        let Some(enum_def) = self.env.enums.get(enum_name).cloned() else {
            // The enum might not be defined yet (processed later)
            // or it's not an enum. We'll silently skip for now.
            // Semantic errors will be caught during type checking.
            return;
        };

        match kind {
            husk_ast::UseKind::Item => {
                // Already handled above
            }
            husk_ast::UseKind::Glob => {
                // Import all variants
                for variant in &enum_def.variants {
                    self.env
                        .variant_imports
                        .insert(variant.name.clone(), (enum_name.clone(), variant.clone()));
                }
            }
            husk_ast::UseKind::Variants(variant_idents) => {
                // Import specific variants
                for variant_ident in variant_idents {
                    if let Some(variant) = enum_def
                        .variants
                        .iter()
                        .find(|v| v.name == variant_ident.name)
                    {
                        self.env
                            .variant_imports
                            .insert(variant.name.clone(), (enum_name.clone(), variant.clone()));
                    }
                    // Note: Unknown variant errors will be caught during type checking
                }
            }
        }
    }

    /// Verify that all trait implementations provide required methods and supertraits.
    fn verify_trait_impls(&mut self) {
        // Collect supertrait errors separately to avoid borrow issues
        let mut supertrait_errors: Vec<(String, String, Vec<String>, Span)> = Vec::new();

        for impl_info in self.env.impls.iter() {
            // Only check trait impls (skip inherent impls)
            let Some(trait_name) = &impl_info.trait_name else {
                continue;
            };

            let Some(trait_info) = self.env.traits.get(trait_name) else {
                // Unknown trait - could report error, but may be external
                continue;
            };

            // Check all required trait methods are implemented
            for method_name in trait_info.methods.keys() {
                if !impl_info.methods.contains_key(method_name) {
                    self.errors.push(SemanticError {
                        message: format!(
                            "impl of trait `{}` for `{}` is missing method `{}`",
                            trait_name, impl_info.self_ty_name, method_name
                        ),
                        span: impl_info.span.clone(),
                    });
                }
            }

            // Check that all supertraits are also implemented
            // For example, `impl Eq for Foo` requires `impl PartialEq for Foo`
            let missing = self
                .env
                .missing_supertraits(&impl_info.self_ty_name, trait_name);
            if !missing.is_empty() {
                supertrait_errors.push((
                    trait_name.clone(),
                    impl_info.self_ty_name.clone(),
                    missing,
                    impl_info.span.clone(),
                ));
            }
        }

        // Report supertrait errors
        for (trait_name, type_name, missing, span) in supertrait_errors {
            let missing_list = missing.join("`, `");
            self.errors.push(SemanticError {
                message: format!(
                    "the trait bound `{}: {}` is not satisfied: missing implementation of supertrait{} `{}`",
                    type_name,
                    trait_name,
                    if missing.len() > 1 { "s" } else { "" },
                    missing_list
                ),
                span,
            });
        }
    }

    fn check_file(
        &mut self,
        file: &File,
    ) -> (
        Vec<SemanticError>,
        NameResolution,
        TypeResolution,
        VariantCallMap,
        VariantPatternMap,
        HoverMap,
        ReferenceMap,
    ) {
        // Verify trait implementations
        self.verify_trait_impls();

        // Type check each function body independently.
        for item in &file.items {
            if let ItemKind::Fn {
                name,
                type_params,
                params,
                ret_type,
                body,
                ..
            } = &item.kind
            {
                self.check_fn(
                    name,
                    type_params,
                    params,
                    ret_type.as_ref(),
                    body,
                    item.span.clone(),
                );
            }
        }

        // Validate main() return type - must implement Termination trait
        // Currently only () and Result<T, E> are allowed
        for item in &file.items {
            if let ItemKind::Fn { name, ret_type, .. } = &item.kind
                && name.name == "main"
            {
                let return_type = ret_type
                    .as_ref()
                    .map(|ty| self.resolve_type_expr(ty, &[]))
                    .unwrap_or(Type::Primitive(PrimitiveType::Unit));

                let is_valid_termination = match &return_type {
                    // () implements Termination
                    Type::Primitive(PrimitiveType::Unit) => true,
                    // Result<T, E> implements Termination (where T: Termination, E: Debug)
                    // For now, we accept any Result type
                    Type::Named { name, args } if name == "Result" && args.len() == 2 => true,
                    _ => false,
                };

                if !is_valid_termination {
                    let span = ret_type
                        .as_ref()
                        .map(|ty| ty.span.clone())
                        .unwrap_or(name.span.clone());
                    self.errors.push(SemanticError {
                        message: format!(
                            "`main` has invalid return type `{}`\n\
                                 `main` can only return types that implement `Termination`\n\
                                 help: consider using `()`, or a `Result`",
                            format_type(&return_type)
                        ),
                        span,
                    });
                }
            }
        }
        (
            self.errors.clone(),
            std::mem::take(&mut self.name_resolution),
            std::mem::take(&mut self.type_resolution),
            std::mem::take(&mut self.variant_calls),
            std::mem::take(&mut self.variant_patterns),
            std::mem::take(&mut self.hover_info),
            std::mem::take(&mut self.references),
        )
    }

    fn check_fn(
        &mut self,
        name: &Ident,
        type_params: &[husk_ast::TypeParam],
        params: &[Param],
        ret_type_expr: Option<&TypeExpr>,
        body: &[Stmt],
        span: Span,
    ) {
        // Convert type_params to Vec<String> for resolve_type_expr
        let generic_params: Vec<String> =
            type_params.iter().map(|tp| tp.name.name.clone()).collect();

        let ret_ty = if let Some(ty_expr) = ret_type_expr {
            self.resolve_type_expr(ty_expr, &generic_params)
        } else {
            Type::Primitive(PrimitiveType::Unit)
        };

        let mut locals: HashMap<String, Type> = HashMap::new();
        let mut shadow_counts: HashMap<String, u32> = HashMap::new();
        let mut resolved_names: HashMap<String, String> = HashMap::new();

        // Parameters must have explicit types.
        for param in params {
            let ty = self.resolve_type_expr(&param.ty, &generic_params);
            if locals.insert(param.name.name.clone(), ty).is_some() {
                self.errors.push(SemanticError {
                    message: format!(
                        "duplicate parameter name `{}` in function `{}`",
                        param.name.name, name.name
                    ),
                    span: param.name.span.clone(),
                });
            }
            // Register parameter in name resolution (no shadowing for params)
            // Set shadow_counts to 1 because the parameter uses slot 0 (plain name).
            // Next shadowing will get name$1.
            let resolved = param.name.name.clone();
            shadow_counts.insert(param.name.name.clone(), 1);
            resolved_names.insert(param.name.name.clone(), resolved.clone());
            self.name_resolution.insert(
                (param.name.span.range.start, param.name.span.range.end),
                resolved,
            );
        }

        let mut ctx = FnContext {
            tcx: self,
            locals,
            shadow_counts,
            resolved_names,
            ret_ty,
            in_loop: false,
            enclosing_fn_name: Some(name.name.clone()),
        };

        for stmt in body {
            ctx.check_stmt(stmt);
        }

        let _ = span; // reserved for potential future checks (e.g., missing returns).
    }

    fn resolve_type_expr(&mut self, ty: &TypeExpr, generic_params: &[String]) -> Type {
        match &ty.kind {
            TypeExprKind::Named(id) => {
                self.resolve_named_type(&id.name, &[], ty.span.clone(), generic_params)
            }
            TypeExprKind::Generic { name, args } => {
                let resolved_args: Vec<Type> = args
                    .iter()
                    .map(|a| self.resolve_type_expr(a, generic_params))
                    .collect();

                self.resolve_named_type(&name.name, &resolved_args, ty.span.clone(), generic_params)
            }
            TypeExprKind::Function { params, ret } => {
                let param_types: Vec<Type> = params
                    .iter()
                    .map(|p| self.resolve_type_expr(p, generic_params))
                    .collect();
                let ret_type = self.resolve_type_expr(ret, generic_params);
                Type::Function {
                    params: param_types,
                    ret: Box::new(ret_type),
                }
            }
            TypeExprKind::Array(elem_ty) => {
                let elem = self.resolve_type_expr(elem_ty, generic_params);
                Type::Array(Box::new(elem))
            }
            TypeExprKind::Tuple(types) => {
                let element_types: Vec<Type> = types
                    .iter()
                    .map(|t| self.resolve_type_expr(t, generic_params))
                    .collect();
                Type::Tuple(element_types)
            }
            TypeExprKind::ImplTrait { trait_ty } => {
                let resolved_trait_ty = self.resolve_type_expr(trait_ty, generic_params);
                Type::ImplTrait {
                    trait_ty: Box::new(resolved_trait_ty),
                }
            }
        }
    }

    /// Format a TypeExpr as a human-readable string for hover information.
    fn format_type_expr(&self, ty: &TypeExpr) -> String {
        match &ty.kind {
            TypeExprKind::Named(ident) => ident.name.clone(),
            TypeExprKind::Generic { name, args } => {
                if args.is_empty() {
                    name.name.clone()
                } else {
                    let args_str = args
                        .iter()
                        .map(|a| self.format_type_expr(a))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{}<{}>", name.name, args_str)
                }
            }
            TypeExprKind::Function { params, ret } => {
                let params_str = params
                    .iter()
                    .map(|p| self.format_type_expr(p))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("fn({}) -> {}", params_str, self.format_type_expr(ret))
            }
            TypeExprKind::Array(elem) => format!("[{}]", self.format_type_expr(elem)),
            TypeExprKind::Tuple(types) => {
                let types_str = types
                    .iter()
                    .map(|t| self.format_type_expr(t))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({})", types_str)
            }
            TypeExprKind::ImplTrait { trait_ty } => {
                format!("impl {}", self.format_type_expr(trait_ty))
            }
        }
    }

    fn resolve_named_type(
        &mut self,
        name: &str,
        args: &[Type],
        span: Span,
        generic_params: &[String],
    ) -> Type {
        // Generic parameter in scope?
        if generic_params.contains(&name.to_string()) {
            return Type::Named {
                name: name.to_string(),
                args: Vec::new(),
            };
        }

        // Primitive types.
        let prim = match name {
            "i32" => Some(Type::Primitive(PrimitiveType::I32)),
            "i64" => Some(Type::Primitive(PrimitiveType::I64)),
            "f64" => Some(Type::Primitive(PrimitiveType::F64)),
            "bool" => Some(Type::Primitive(PrimitiveType::Bool)),
            "String" => Some(Type::Primitive(PrimitiveType::String)),
            "()" => Some(Type::Primitive(PrimitiveType::Unit)),
            _ => None,
        };
        if let Some(t) = prim {
            if !args.is_empty() {
                self.errors.push(SemanticError {
                    message: format!("primitive type `{}` does not take type arguments", name),
                    span,
                });
            }
            return t;
        }

        // NOTE: Range, Set, and Map are reserved built-in type names.
        // These special-case handlers bypass struct/enum lookup, effectively
        // reserving these identifiers as built-ins. If user code is ever allowed
        // to define its own Range/Set/Map, this logic should be gated on stdlib
        // presence or these should be clearly documented as reserved names.
        //
        // Range<T> is a built-in generic type for range expressions
        if name == "Range" {
            // Range expects exactly 1 type argument (the element type)
            // Default to i32 if no argument provided (for backwards compatibility)
            let elem_ty = if args.is_empty() {
                Type::Primitive(PrimitiveType::I32)
            } else if args.len() == 1 {
                args[0].clone()
            } else {
                self.errors.push(SemanticError {
                    message: format!(
                        "type `Range` expects 0 or 1 type argument(s), got {}",
                        args.len()
                    ),
                    span,
                });
                Type::Primitive(PrimitiveType::I32)
            };
            return Type::Named {
                name: "Range".to_string(),
                args: vec![elem_ty],
            };
        }

        // Set<T> is a built-in generic type for JavaScript Set
        if name == "Set" {
            // Set expects exactly 1 type argument (the element type)
            // Default to i32 if no argument provided (for backwards compatibility)
            let elem_ty = if args.is_empty() {
                Type::Primitive(PrimitiveType::I32)
            } else if args.len() == 1 {
                args[0].clone()
            } else {
                self.errors.push(SemanticError {
                    message: format!(
                        "type `Set` expects 0 or 1 type argument(s), got {}",
                        args.len()
                    ),
                    span,
                });
                Type::Primitive(PrimitiveType::I32)
            };
            return Type::Named {
                name: "Set".to_string(),
                args: vec![elem_ty],
            };
        }

        // Map<K, V> is a built-in generic type for JavaScript Map
        if name == "Map" {
            // Map expects exactly 2 type arguments (key and value types)
            // Default to Map<String, i32> if no arguments provided
            let (key_ty, val_ty) = if args.is_empty() {
                (
                    Type::Primitive(PrimitiveType::String),
                    Type::Primitive(PrimitiveType::I32),
                )
            } else if args.len() == 2 {
                (args[0].clone(), args[1].clone())
            } else {
                self.errors.push(SemanticError {
                    message: format!(
                        "type `Map` expects 0 or 2 type argument(s), got {}",
                        args.len()
                    ),
                    span,
                });
                (
                    Type::Primitive(PrimitiveType::String),
                    Type::Primitive(PrimitiveType::I32),
                )
            };
            return Type::Named {
                name: "Map".to_string(),
                args: vec![key_ty, val_ty],
            };
        }

        // Known struct or enum: check arity.
        if let Some(def) = self.env.structs.get(name) {
            if def.type_params.len() != args.len() {
                self.errors.push(SemanticError {
                    message: format!(
                        "struct `{}` expects {} type argument(s), got {}",
                        name,
                        def.type_params.len(),
                        args.len()
                    ),
                    span: span.clone(),
                });
            }
            // Track struct type reference for rename support
            self.add_reference(name, ReferenceKind::Struct, span);
            return Type::Named {
                name: name.to_string(),
                args: args.to_vec(),
            };
        }

        if let Some(def) = self.env.enums.get(name).cloned() {
            if def.type_params.len() != args.len() {
                self.errors.push(SemanticError {
                    message: format!(
                        "enum `{}` expects {} type argument(s), got {}",
                        name,
                        def.type_params.len(),
                        args.len()
                    ),
                    span: span.clone(),
                });
            }
            // Track enum type reference for rename support
            self.add_reference(name, ReferenceKind::Enum, span);
            return Type::Named {
                name: name.to_string(),
                args: args.to_vec(),
            };
        }

        // Type alias: expand once.
        if let Some(alias) = self.env.type_aliases.get(name).cloned() {
            // Track type alias reference for rename support
            self.add_reference(name, ReferenceKind::TypeAlias, span);
            return self.resolve_type_expr(&alias, generic_params);
        }

        // Unknown type - but might be a trait name used in impl Trait
        // For now, allow it as a Named type (used in impl Trait)
        // This allows Iterator<T> to be used in impl Iterator<T>
        if name == "Iterator" || self.env.traits.contains_key(name) {
            return Type::Named {
                name: name.to_string(),
                args: args.to_vec(),
            };
        }

        // Unknown type.
        self.errors.push(SemanticError {
            message: format!("unknown type `{}`", name),
            span,
        });
        Type::Named {
            name: name.to_string(),
            args: args.to_vec(),
        }
    }
}

struct FnContext<'a> {
    tcx: &'a mut TypeChecker,
    locals: HashMap<String, Type>,
    /// Maps variable names to their current shadow count.
    /// When a variable is shadowed, we increment its count.
    shadow_counts: HashMap<String, u32>,
    /// Maps variable names to their current resolved name (e.g., "x" -> "x$1").
    /// Used to resolve variable references to the correct shadowed version.
    resolved_names: HashMap<String, String>,
    ret_ty: Type,
    in_loop: bool,
    /// Name of the enclosing function for error messages (None for closures).
    enclosing_fn_name: Option<String>,
}

impl<'a> FnContext<'a> {
    /// Bind a variable with shadowing support.
    ///
    /// Returns the resolved name (e.g., "x", "x$1", "x$2") and registers it
    /// in the name resolution map.
    fn bind_variable(&mut self, name: &str, ty: Type, span: &Span) -> String {
        // Get or initialize the shadow count for this variable name
        let count = self.shadow_counts.entry(name.to_string()).or_insert(0);
        let resolved = if *count == 0 {
            name.to_string()
        } else {
            format!("{}${}", name, count)
        };
        *count += 1;

        // Update the current resolved name for this variable
        self.resolved_names
            .insert(name.to_string(), resolved.clone());

        // Add to locals for type checking
        self.locals.insert(name.to_string(), ty.clone());

        // Register in name resolution map
        self.tcx
            .name_resolution
            .insert((span.range.start, span.range.end), resolved.clone());

        // Track variable binding for rename support
        self.tcx
            .add_reference(name, ReferenceKind::Variable, span.clone());

        // Register hover info for the variable binding
        let type_str = self.format_type(&ty);
        self.tcx.hover_info.insert(
            (span.range.start, span.range.end),
            HoverInfo {
                signature: format!("{}: {}", name, type_str),
                docs: None,
                definition_span: Some(span.clone()),
            },
        );

        resolved
    }

    fn check_path_expr(&mut self, expr: &Expr, segments: &[Ident]) -> Type {
        // Check for single-segment paths that might be imported variants (e.g., `Some`, `None`, `Ok`, `Err`)
        if segments.len() == 1 {
            let name = &segments[0].name;
            if let Some((enum_name, variant_def)) = self.tcx.env.variant_imports.get(name).cloned()
            {
                // This is an imported variant - resolve as if it were Enum::Variant
                let enum_def = self.tcx.env.enums.get(&enum_name).cloned();

                // Track variant reference for rename support
                self.tcx.add_reference(
                    &format!("{}::{}", enum_name, variant_def.name),
                    ReferenceKind::Variant,
                    segments[0].span.clone(),
                );
                let enum_ty = Type::Named {
                    name: enum_name.clone(),
                    args: Vec::new(),
                };

                // Check variant fields to determine the type
                match &variant_def.fields {
                    EnumVariantFields::Unit => {
                        return enum_ty;
                    }
                    EnumVariantFields::Tuple(field_types) => {
                        let type_params = enum_def
                            .as_ref()
                            .map(|d| d.type_params.clone())
                            .unwrap_or_default();
                        let params: Vec<Type> = field_types
                            .iter()
                            .map(|ty| self.tcx.resolve_type_expr(ty, &type_params))
                            .collect();
                        return Type::Function {
                            params,
                            ret: Box::new(enum_ty),
                        };
                    }
                    EnumVariantFields::Struct(_fields) => {
                        return enum_ty;
                    }
                }
            }
            // Not an imported variant - this is an error for a single-segment Path
            self.tcx.errors.push(SemanticError {
                message: "path expression must have at least two segments".to_string(),
                span: expr.span.clone(),
            });
            return Type::Primitive(PrimitiveType::Unit);
        }

        // Handle `Enum::Variant` paths.
        if segments.len() >= 2 {
            let enum_name = &segments[0].name;
            let variant_name = &segments[segments.len() - 1].name;

            if let Some(def) = self.tcx.env.enums.get(enum_name).cloned() {
                let variant = def.variants.iter().find(|v| &v.name == variant_name);

                if variant.is_none() {
                    self.tcx.errors.push(SemanticError {
                        message: format!(
                            "unknown variant `{}` on enum `{}`",
                            variant_name, enum_name
                        ),
                        span: expr.span.clone(),
                    });
                }

                // Track enum and variant references for rename support
                self.tcx
                    .add_reference(enum_name, ReferenceKind::Enum, segments[0].span.clone());
                self.tcx.add_reference(
                    &format!("{}::{}", enum_name, variant_name),
                    ReferenceKind::Variant,
                    segments[segments.len() - 1].span.clone(),
                );

                let enum_ty = Type::Named {
                    name: enum_name.clone(),
                    args: Vec::new(),
                };

                // Check variant fields to determine the type:
                // - Unit variants: return the enum type directly
                // - Tuple/Struct variants: return a constructor function type
                match variant.map(|v| &v.fields) {
                    Some(EnumVariantFields::Unit) | None => {
                        // Unit variant or unknown variant - return enum type directly
                        return enum_ty;
                    }
                    Some(EnumVariantFields::Tuple(field_types)) => {
                        // Tuple variant - return function type with field types as params
                        let params: Vec<Type> = field_types
                            .iter()
                            .map(|ty| self.tcx.resolve_type_expr(ty, &def.type_params))
                            .collect();
                        return Type::Function {
                            params,
                            ret: Box::new(enum_ty),
                        };
                    }
                    Some(EnumVariantFields::Struct(_fields)) => {
                        // Struct variants are constructed differently (not as function calls)
                        // For now, return the enum type
                        return enum_ty;
                    }
                }
            }

            // Handle built-in type static methods (e.g., Set::new(), Map::new())
            if enum_name == "Set" && variant_name == "new" {
                // Set::new() returns a Set<T> where T is inferred from context
                // For now, default to Set<i32>
                return Type::Function {
                    params: vec![],
                    ret: Box::new(Type::Named {
                        name: "Set".to_string(),
                        args: vec![Type::Primitive(PrimitiveType::I32)],
                    }),
                };
            }

            if enum_name == "Map" && variant_name == "new" {
                // Map::new() returns a Map<K, V> where K, V are inferred from context
                // For now, default to Map<String, i32>
                return Type::Function {
                    params: vec![],
                    ret: Box::new(Type::Named {
                        name: "Map".to_string(),
                        args: vec![
                            Type::Primitive(PrimitiveType::String),
                            Type::Primitive(PrimitiveType::I32),
                        ],
                    }),
                };
            }

            // Fallback: unknown enum name.
            self.tcx.errors.push(SemanticError {
                message: format!("unknown enum `{}` in path expression", enum_name),
                span: expr.span.clone(),
            });
        } else {
            self.tcx.errors.push(SemanticError {
                message: "path expression must have at least two segments".to_string(),
                span: expr.span.clone(),
            });
        }
        Type::Primitive(PrimitiveType::Unit)
    }

    fn check_match_expr(&mut self, expr: &Expr, scrutinee: &Expr, arms: &[MatchArm]) -> Type {
        let scrut_ty = self.check_expr(scrutinee);

        // Try to interpret scrutinee as an enum.
        let enum_info = match &scrut_ty {
            Type::Named { name, .. } => self.tcx.env.enums.get(name).map(|def| {
                let variant_names: Vec<String> =
                    def.variants.iter().map(|v| v.name.clone()).collect();
                (name.clone(), variant_names)
            }),
            _ => None,
        };

        let mut result_ty: Option<Type> = None;
        let mut seen_variants: HashSet<String> = HashSet::new();
        let mut has_catch_all = false;

        for arm in arms {
            // Track patterns for exhaustiveness.
            match &arm.pattern.kind {
                PatternKind::Wildcard => {
                    has_catch_all = true;
                }
                PatternKind::Binding(ident) => {
                    // Check if this binding name matches an imported unit variant
                    if let Some((enum_name, variant_names)) = &enum_info {
                        if let Some((imported_enum, _)) =
                            self.tcx.env.variant_imports.get(&ident.name)
                        {
                            if imported_enum == enum_name {
                                // This binding is actually an imported unit variant pattern
                                if variant_names.contains(&ident.name) {
                                    seen_variants.insert(ident.name.clone());
                                    // Record this pattern as a variant pattern for codegen
                                    self.tcx.variant_patterns.insert(
                                        (arm.pattern.span.range.start, arm.pattern.span.range.end),
                                        (enum_name.clone(), ident.name.clone()),
                                    );
                                    // Track imported variant reference for rename support
                                    self.tcx.add_reference(
                                        &format!("{}::{}", enum_name, ident.name),
                                        ReferenceKind::Variant,
                                        ident.span.clone(),
                                    );
                                } else {
                                    self.tcx.errors.push(SemanticError {
                                        message: format!(
                                            "unknown variant `{}` for enum `{}`",
                                            ident.name, enum_name
                                        ),
                                        span: arm.pattern.span.clone(),
                                    });
                                }
                            } else {
                                self.tcx.errors.push(SemanticError {
                                    message: format!(
                                        "pattern for `{}` cannot use variant `{}` from `{}`",
                                        enum_name, ident.name, imported_enum
                                    ),
                                    span: arm.pattern.span.clone(),
                                });
                            }
                        } else {
                            // Just a regular binding pattern
                            has_catch_all = true;
                        }
                    } else {
                        // Not matching on an enum, treat as normal binding
                        has_catch_all = true;
                    }
                }
                PatternKind::EnumUnit { path } | PatternKind::EnumTuple { path, .. } => {
                    if let Some((enum_name, variant_names)) = &enum_info {
                        // Check for single-segment imported variant (e.g., `Some`, `None`)
                        if path.len() == 1 {
                            let variant = path[0].name.clone();
                            // Check if this is an imported variant for the expected enum
                            if let Some((imported_enum, _)) =
                                self.tcx.env.variant_imports.get(&variant)
                            {
                                if imported_enum == enum_name {
                                    if !variant_names.contains(&variant) {
                                        self.tcx.errors.push(SemanticError {
                                            message: format!(
                                                "unknown variant `{}` for enum `{}`",
                                                variant, enum_name
                                            ),
                                            span: arm.pattern.span.clone(),
                                        });
                                    } else {
                                        seen_variants.insert(variant.clone());
                                        // Track imported variant reference for rename support
                                        self.tcx.add_reference(
                                            &format!("{}::{}", enum_name, variant),
                                            ReferenceKind::Variant,
                                            path[0].span.clone(),
                                        );
                                    }
                                } else {
                                    self.tcx.errors.push(SemanticError {
                                        message: format!(
                                            "pattern for `{}` cannot use variant `{}` from `{}`",
                                            enum_name, variant, imported_enum
                                        ),
                                        span: arm.pattern.span.clone(),
                                    });
                                }
                            } else {
                                self.tcx.errors.push(SemanticError {
                                    message: format!(
                                        "unknown variant `{}` (did you mean `{}::{}`?)",
                                        variant, enum_name, variant
                                    ),
                                    span: arm.pattern.span.clone(),
                                });
                            }
                        } else if path.len() == 2 && path[0].name == *enum_name {
                            // Expect path like Enum::Variant
                            let variant = path[1].name.clone();
                            if !variant_names.contains(&variant) {
                                self.tcx.errors.push(SemanticError {
                                    message: format!(
                                        "unknown variant `{}` for enum `{}`",
                                        variant, enum_name
                                    ),
                                    span: arm.pattern.span.clone(),
                                });
                            } else {
                                seen_variants.insert(variant.clone());
                                // Track enum and variant references for rename support
                                self.tcx.add_reference(
                                    enum_name,
                                    ReferenceKind::Enum,
                                    path[0].span.clone(),
                                );
                                self.tcx.add_reference(
                                    &format!("{}::{}", enum_name, variant),
                                    ReferenceKind::Variant,
                                    path[1].span.clone(),
                                );
                            }
                        } else {
                            self.tcx.errors.push(SemanticError {
                                message: format!(
                                    "enum pattern must use `{0}::Variant` for enum `{0}`",
                                    enum_name
                                ),
                                span: arm.pattern.span.clone(),
                            });
                        }
                    }
                }
                // Struct patterns not yet used in exhaustiveness.
                PatternKind::EnumStruct { .. } => {}
                // Tuple patterns - treat as irrefutable for now (they always match if types match)
                PatternKind::Tuple { .. } => {
                    has_catch_all = true;
                }
            }

            // Type-check the arm expression in a fresh scope with any bindings from the pattern.
            let saved_locals = self.locals.clone();
            let saved_shadow_counts = self.shadow_counts.clone();
            let saved_resolved_names = self.resolved_names.clone();
            let mut pattern_bindings = HashSet::new();
            self.bind_pattern_locals(&arm.pattern, &scrut_ty, &mut pattern_bindings);
            let arm_ty = self.check_expr(&arm.expr);
            self.locals = saved_locals;
            self.shadow_counts = saved_shadow_counts;
            self.resolved_names = saved_resolved_names;

            match &mut result_ty {
                None => result_ty = Some(arm_ty),
                Some(expected) => {
                    if !self.types_compatible(expected, &arm_ty) {
                        self.tcx.errors.push(SemanticError {
                            message: format!(
                                "mismatched types in match arms: expected `{}`, found `{}`",
                                self.format_type(expected),
                                self.format_type(&arm_ty)
                            ),
                            span: arm.expr.span.clone(),
                        });
                    }
                }
            }
        }

        // Exhaustiveness for simple enums.
        if let Some((enum_name, variant_names)) = enum_info
            && !has_catch_all
        {
            for variant in &variant_names {
                if !seen_variants.contains(variant) {
                    self.tcx.errors.push(SemanticError {
                        message: format!(
                            "non-exhaustive match on enum `{}`: missing variant `{}`",
                            enum_name, variant
                        ),
                        span: expr.span.clone(),
                    });
                }
            }
        }

        result_ty.unwrap_or(Type::Primitive(PrimitiveType::Unit))
    }

    /// Bind pattern locals with shadowing support.
    ///
    /// The `pattern_bindings` set tracks bindings within the current pattern
    /// to detect duplicate bindings like `(x, x)` which should still be an error.
    /// Shadowing outer variables is allowed.
    fn bind_pattern_locals(
        &mut self,
        pat: &Pattern,
        scrut_ty: &Type,
        pattern_bindings: &mut HashSet<String>,
    ) {
        match &pat.kind {
            PatternKind::Wildcard => {}
            PatternKind::Binding(id) => {
                // Check if this is actually an imported variant pattern, not a variable binding.
                // We check variant_patterns which was populated by check_match_expr.
                let is_variant_pattern = self
                    .tcx
                    .variant_patterns
                    .contains_key(&(pat.span.range.start, pat.span.range.end));

                if !is_variant_pattern {
                    // Check for duplicate binding within the same pattern (e.g., `(x, x)`)
                    if !pattern_bindings.insert(id.name.clone()) {
                        self.tcx.errors.push(SemanticError {
                            message: format!(
                                "identifier `{}` is bound more than once in the same pattern",
                                id.name
                            ),
                            span: id.span.clone(),
                        });
                    }
                    // Allow shadowing outer variables
                    self.bind_variable(&id.name, scrut_ty.clone(), &id.span);
                }
                // If it's a variant pattern, don't bind it as a variable
            }
            PatternKind::EnumUnit { .. } => {}
            PatternKind::EnumTuple { fields, .. } => {
                // Extract bindings from each field in the tuple pattern.
                // For Option::Some(x), the scrutinee type is Option<T>, so x has type T.
                // For multi-field variants, we'd need the full enum definition to get field types.
                // For now, handle the common single-field case (Option, Result).
                if fields.len() == 1 {
                    // Extract inner type from Option<T> or Result<T, E>
                    let inner_ty = match scrut_ty {
                        Type::Named { args, .. } if !args.is_empty() => args[0].clone(),
                        _ => scrut_ty.clone(),
                    };
                    self.bind_pattern_locals(&fields[0], &inner_ty, pattern_bindings);
                } else {
                    // Multi-field: bind each to the scrutinee type (imprecise but functional)
                    for field in fields {
                        self.bind_pattern_locals(field, scrut_ty, pattern_bindings);
                    }
                }
            }
            PatternKind::EnumStruct { .. } => {
                // Not yet supported for bindings.
            }
            PatternKind::Tuple { fields } => {
                // For tuple patterns, bind each field to the corresponding element type
                match scrut_ty {
                    Type::Tuple(element_types) => {
                        // Validate arity matches
                        if fields.len() != element_types.len() {
                            self.tcx.errors.push(SemanticError {
                                message: format!(
                                    "tuple pattern has {} fields but type has {} elements",
                                    fields.len(),
                                    element_types.len()
                                ),
                                span: pat.span.clone(),
                            });
                        }
                        for (i, field) in fields.iter().enumerate() {
                            let field_ty = element_types.get(i).cloned().unwrap_or(Type::unit());
                            self.bind_pattern_locals(field, &field_ty, pattern_bindings);
                        }
                    }
                    _ => {
                        // Type mismatch - report error for tuple pattern on non-tuple type
                        self.tcx.errors.push(SemanticError {
                            message: format!(
                                "expected tuple type for tuple pattern, found `{}`",
                                self.format_type(scrut_ty)
                            ),
                            span: pat.span.clone(),
                        });
                        for field in fields {
                            self.bind_pattern_locals(field, &Type::unit(), pattern_bindings);
                        }
                    }
                }
            }
        }
    }
    fn check_stmt(&mut self, stmt: &Stmt) -> Option<Type> {
        match &stmt.kind {
            StmtKind::Let {
                mutable: _,
                pattern,
                ty,
                value,
                else_block,
            } => {
                let annotated_ty = ty.as_ref().map(|t| self.tcx.resolve_type_expr(t, &[]));
                // Use check_expr_with_expected to enable closure parameter inference
                // from type annotation: `let f: fn(i32) -> i32 = |x| x + 1`
                let value_ty = value
                    .as_ref()
                    .map(|expr| self.check_expr_with_expected(expr, annotated_ty.as_ref()));

                let final_ty = match (annotated_ty.clone(), value_ty) {
                    (Some(a), Some(v)) => {
                        if !self.types_compatible(&a, &v) {
                            self.tcx.errors.push(SemanticError {
                                message: format!(
                                    "mismatched types in `let`: expected `{}`, found `{}`",
                                    self.format_type(&a),
                                    self.format_type(&v)
                                ),
                                span: stmt.span.clone(),
                            });
                        }
                        a
                    }
                    (Some(a), None) => a,
                    (None, Some(v)) => v,
                    (None, None) => {
                        self.tcx.errors.push(SemanticError {
                            message: "cannot infer type for `let` without annotation or value"
                                .to_string(),
                            span: stmt.span.clone(),
                        });
                        Type::Primitive(PrimitiveType::Unit)
                    }
                };

                // Check for refutable patterns without else block
                let is_refutable = self.pattern_is_refutable(pattern);
                if is_refutable && else_block.is_none() {
                    self.tcx.errors.push(SemanticError {
                        message: "refutable pattern in let binding requires `else` block"
                            .to_string(),
                        span: pattern.span.clone(),
                    });
                }

                if let Some(else_blk) = else_block {
                    // Validate pattern is refutable (warn if irrefutable with else)
                    self.validate_refutable_pattern(pattern, &final_ty, &stmt.span);

                    // Track imported variants for codegen
                    self.record_variant_pattern(pattern, &final_ty);

                    // Check divergence
                    if !self.block_diverges(else_blk) {
                        self.tcx.errors.push(SemanticError {
                            message: "`else` block in `let ... else` must diverge".to_string(),
                            span: else_blk.span.clone(),
                        });
                    }

                    // Type-check else block (no pattern bindings visible here)
                    self.check_block(else_blk);
                }

                // Bind variables from pattern with shadowing support
                // (pattern validation happens in bind_pattern_locals)
                let mut pattern_bindings = HashSet::new();
                self.bind_pattern_locals(pattern, &final_ty, &mut pattern_bindings);
                None
            }
            StmtKind::Assign { target, op, value } => {
                // Validate target is assignable (lvalue: ident, field, or index)
                if !self.is_lvalue(target) {
                    self.tcx.errors.push(SemanticError {
                        message: "invalid assignment target".to_string(),
                        span: target.span.clone(),
                    });
                    return None;
                }

                let target_ty = self.check_expr(target);
                let value_ty = self.check_expr(value);

                match op {
                    husk_ast::AssignOp::Assign => {
                        if !self.types_compatible(&target_ty, &value_ty) {
                            self.tcx.errors.push(SemanticError {
                                message: format!(
                                    "type mismatch: cannot assign `{:?}` to `{:?}`",
                                    value_ty, target_ty
                                ),
                                span: value.span.clone(),
                            });
                        }
                    }
                    husk_ast::AssignOp::AddAssign
                    | husk_ast::AssignOp::SubAssign
                    | husk_ast::AssignOp::ModAssign => {
                        // Compound assignment requires numeric types
                        if !self.is_numeric(&target_ty) {
                            self.tcx.errors.push(SemanticError {
                                message: format!(
                                    "compound assignment requires numeric type, found `{:?}`",
                                    target_ty
                                ),
                                span: target.span.clone(),
                            });
                        }
                        if !self.types_compatible(&target_ty, &value_ty) {
                            self.tcx.errors.push(SemanticError {
                                message: format!(
                                    "type mismatch in compound assignment: `{:?}` vs `{:?}`",
                                    target_ty, value_ty
                                ),
                                span: value.span.clone(),
                            });
                        }
                    }
                }
                None
            }
            StmtKind::Expr(expr) | StmtKind::Semi(expr) => {
                let ty = self.check_expr(expr);
                if matches!(stmt.kind, StmtKind::Semi(_)) {
                    return Some(Type::Primitive(PrimitiveType::Unit));
                }
                Some(ty)
            }
            StmtKind::Return { value } => {
                let actual_ty = if let Some(expr) = value {
                    self.check_expr(expr)
                } else {
                    Type::Primitive(PrimitiveType::Unit)
                };
                if !self.types_compatible(&self.ret_ty, &actual_ty) {
                    self.tcx.errors.push(SemanticError {
                        message: format!(
                            "mismatched return type: expected `{:?}`, found `{:?}`",
                            self.ret_ty, actual_ty
                        ),
                        span: stmt.span.clone(),
                    });
                }
                None
            }
            StmtKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let cond_ty = self.check_expr(cond);
                if !matches!(cond_ty, Type::Primitive(PrimitiveType::Bool)) {
                    self.tcx.errors.push(SemanticError {
                        message: "if condition must have type `bool`".to_string(),
                        span: cond.span.clone(),
                    });
                }

                let then_ty = self.check_block(then_branch);
                if let Some(else_stmt) = else_branch {
                    let else_ty = self.check_stmt(else_stmt);
                    // If both branches have matching types, return that type
                    // This allows if/else statements to be used as expressions in blocks
                    if let Some(else_ty) = else_ty
                        && self.types_compatible(&then_ty, &else_ty)
                    {
                        return Some(then_ty);
                    }
                }
                None
            }
            StmtKind::While { cond, body } => {
                let cond_ty = self.check_expr(cond);
                if !matches!(cond_ty, Type::Primitive(PrimitiveType::Bool)) {
                    self.tcx.errors.push(SemanticError {
                        message: "while condition must have type `bool`".to_string(),
                        span: cond.span.clone(),
                    });
                }

                let prev_in_loop = self.in_loop;
                self.in_loop = true;
                self.check_block(body);
                self.in_loop = prev_in_loop;
                None
            }
            StmtKind::Loop { body } => {
                let prev_in_loop = self.in_loop;
                self.in_loop = true;
                self.check_block(body);
                self.in_loop = prev_in_loop;
                None
            }
            StmtKind::ForIn {
                binding,
                iterable,
                body,
            } => {
                let iter_ty = self.check_expr(iterable);

                // Record iterable type for codegen (needed for Range iteration)
                self.tcx.type_resolution.insert(
                    (iterable.span.range.start, iterable.span.range.end),
                    self.format_type(&iter_ty),
                );

                // Extract element type from iterable ([T], Vec<T>, Range<T>, String)
                let elem_ty = match &iter_ty {
                    Type::Array(elem) => (**elem).clone(),
                    Type::Named { name, args } if name == "Vec" && !args.is_empty() => {
                        args[0].clone()
                    }
                    Type::Named { name, args } if name == "Range" && !args.is_empty() => {
                        args[0].clone() // Range<i32> yields i32
                    }
                    Type::Named { name, args } if name == "Set" && !args.is_empty() => {
                        args[0].clone() // Set<T> yields T
                    }
                    // Also allow String iteration (iterates over chars as strings)
                    Type::Primitive(PrimitiveType::String) => {
                        Type::Primitive(PrimitiveType::String)
                    }
                    _ => {
                        self.tcx.errors.push(SemanticError {
                            message: format!(
                                "for-in loop requires iterable collection, found `{:?}`",
                                iter_ty
                            ),
                            span: iterable.span.clone(),
                        });
                        Type::unit()
                    }
                };

                // Save current scope state
                let old_locals = self.locals.clone();
                let old_shadow_counts = self.shadow_counts.clone();
                let old_resolved_names = self.resolved_names.clone();

                // Bind loop variable with shadowing support
                self.bind_variable(&binding.name, elem_ty, &binding.span);

                // Check body in loop context
                let prev_in_loop = self.in_loop;
                self.in_loop = true;
                self.check_block(body);
                self.in_loop = prev_in_loop;

                // Restore scope state
                self.locals = old_locals;
                self.shadow_counts = old_shadow_counts;
                self.resolved_names = old_resolved_names;
                None
            }
            StmtKind::Break | StmtKind::Continue => {
                if !self.in_loop {
                    self.tcx.errors.push(SemanticError {
                        message: format!(
                            "`{}` used outside of loop",
                            if matches!(stmt.kind, StmtKind::Break) {
                                "break"
                            } else {
                                "continue"
                            }
                        ),
                        span: stmt.span.clone(),
                    });
                }
                None
            }
            StmtKind::Block(block) => Some(self.check_block(block)),
            StmtKind::IfLet {
                pattern,
                scrutinee,
                then_branch,
                else_branch,
            } => {
                self.check_if_let_stmt(pattern, scrutinee, then_branch, else_branch, &stmt.span);
                None
            }
        }
    }

    fn check_if_let_stmt(
        &mut self,
        pattern: &Pattern,
        scrutinee: &Expr,
        then_branch: &Block,
        else_branch: &Option<Box<Stmt>>,
        span: &Span,
    ) {
        let scrut_ty = self.check_expr(scrutinee);

        // Validate pattern is refutable
        self.validate_refutable_pattern(pattern, &scrut_ty, span);

        // Track imported variants for codegen
        self.record_variant_pattern(pattern, &scrut_ty);

        // Save scope
        let old_locals = self.locals.clone();
        let old_shadow_counts = self.shadow_counts.clone();
        let old_resolved_names = self.resolved_names.clone();

        // Bind pattern locals and check then branch
        let mut pattern_bindings = HashSet::new();
        self.bind_pattern_locals(pattern, &scrut_ty, &mut pattern_bindings);
        self.check_block(then_branch);

        // Restore scope (bindings don't leak)
        self.locals = old_locals;
        self.shadow_counts = old_shadow_counts;
        self.resolved_names = old_resolved_names;

        // Check else branch without bindings
        if let Some(else_stmt) = else_branch {
            self.check_stmt(else_stmt);
        }
    }

    fn check_block(&mut self, block: &Block) -> Type {
        let old_locals = self.locals.clone();
        let old_shadow_counts = self.shadow_counts.clone();
        let old_resolved_names = self.resolved_names.clone();

        let mut last_ty: Option<Type> = None;
        for stmt in &block.stmts {
            last_ty = self.check_stmt(stmt);
        }

        self.locals = old_locals;
        self.shadow_counts = old_shadow_counts;
        self.resolved_names = old_resolved_names;

        last_ty.unwrap_or(Type::Primitive(PrimitiveType::Unit))
    }

    fn check_expr(&mut self, expr: &Expr) -> Type {
        match &expr.kind {
            ExprKind::Literal(lit) => match lit.kind {
                LiteralKind::Int(n) => {
                    // Use i64 for values outside i32 range
                    if n > i32::MAX as i64 || n < i32::MIN as i64 {
                        Type::Primitive(PrimitiveType::I64)
                    } else {
                        Type::Primitive(PrimitiveType::I32)
                    }
                }
                LiteralKind::Float(_) => Type::Primitive(PrimitiveType::F64),
                LiteralKind::Bool(_) => Type::Primitive(PrimitiveType::Bool),
                LiteralKind::String(_) => Type::Primitive(PrimitiveType::String),
            },
            ExprKind::Path { segments } => self.check_path_expr(expr, segments),
            ExprKind::Ident(id) => {
                if let Some(ty) = self.locals.get(&id.name).cloned() {
                    // Register the resolved name for this variable usage
                    if let Some(resolved) = self.resolved_names.get(&id.name) {
                        self.tcx
                            .name_resolution
                            .insert((id.span.range.start, id.span.range.end), resolved.clone());
                    }
                    // Register type in type_resolution for codegen (needed for iterator method dispatch)
                    let type_str = self.format_type(&ty);
                    self.tcx
                        .type_resolution
                        .insert((id.span.range.start, id.span.range.end), type_str.clone());
                    // Register hover info for variable usage
                    self.tcx.hover_info.insert(
                        (id.span.range.start, id.span.range.end),
                        HoverInfo {
                            signature: format!("{}: {}", id.name, type_str),
                            docs: None,
                            definition_span: None, // Could look up definition span if stored
                        },
                    );
                    // Track variable reference for rename support
                    self.tcx
                        .add_reference(&id.name, ReferenceKind::Variable, id.span.clone());
                    return ty;
                }
                // Try top-level function.
                if let Some(fn_def) = self.tcx.env.functions.get(&id.name).cloned() {
                    // Pass the function's type params so generic types like T are recognized
                    let type_param_names = fn_def.type_param_names();
                    let param_types: Vec<Type> = fn_def
                        .params
                        .iter()
                        .map(|p| self.tcx.resolve_type_expr(&p.ty, &type_param_names))
                        .collect();
                    let ret_ty = if let Some(ret_expr) = fn_def.ret_type.as_ref() {
                        self.tcx.resolve_type_expr(ret_expr, &type_param_names)
                    } else {
                        Type::Primitive(PrimitiveType::Unit)
                    };
                    // Register hover info for function reference
                    let fn_type = Type::Function {
                        params: param_types.clone(),
                        ret: Box::new(ret_ty.clone()),
                    };
                    let params_str = fn_def
                        .params
                        .iter()
                        .map(|p| format!("{}: {}", p.name.name, self.tcx.format_type_expr(&p.ty)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let ret_str = fn_def
                        .ret_type
                        .as_ref()
                        .map(|t| self.tcx.format_type_expr(t))
                        .unwrap_or_else(|| "()".to_string());
                    self.tcx.hover_info.insert(
                        (id.span.range.start, id.span.range.end),
                        HoverInfo {
                            signature: format!("fn {}({}) -> {}", id.name, params_str, ret_str),
                            docs: None,
                            definition_span: None,
                        },
                    );
                    // Track function reference for rename support
                    self.tcx
                        .add_reference(&id.name, ReferenceKind::Function, id.span.clone());
                    return fn_type;
                }

                // Try imported JS module (from `mod name;` in extern block).
                // Modules are treated as callable with any args, returning an opaque type.
                if let Some(module_def) = self.tcx.env.modules.get(&id.name) {
                    // Use the stored return type if available, otherwise use the module name
                    let ret_type_name = module_def
                        .ret_type
                        .clone()
                        .unwrap_or_else(|| id.name.clone());
                    return Type::Function {
                        params: Vec::new(),
                        ret: Box::new(Type::Named {
                            name: ret_type_name,
                            args: Vec::new(),
                        }),
                    };
                }

                // Try extern static variable (from `static name: Type;` in extern block).
                if let Some(ty_expr) = self.tcx.env.statics.get(&id.name).cloned() {
                    return self.tcx.resolve_type_expr(&ty_expr, &[]);
                }

                // Try imported enum variant (from `use Enum::*;` or `use Enum::{A, B};`)
                if let Some((enum_name, variant_def)) =
                    self.tcx.env.variant_imports.get(&id.name).cloned()
                {
                    let enum_def = self.tcx.env.enums.get(&enum_name).cloned();
                    let enum_ty = Type::Named {
                        name: enum_name.clone(),
                        args: Vec::new(),
                    };

                    // Track imported variant reference for rename support
                    self.tcx.add_reference(
                        &format!("{}::{}", enum_name, id.name),
                        ReferenceKind::Variant,
                        id.span.clone(),
                    );

                    // Return appropriate type based on variant fields
                    match &variant_def.fields {
                        EnumVariantFields::Unit => {
                            // Record unit variant idents for codegen (they need to become {tag: "Variant"})
                            self.tcx.variant_calls.insert(
                                (id.span.range.start, id.span.range.end),
                                (enum_name, id.name.clone()),
                            );
                            return enum_ty;
                        }
                        EnumVariantFields::Tuple(field_types) => {
                            let type_params = enum_def
                                .as_ref()
                                .map(|d| d.type_params.clone())
                                .unwrap_or_default();
                            let params: Vec<Type> = field_types
                                .iter()
                                .map(|ty| self.tcx.resolve_type_expr(ty, &type_params))
                                .collect();
                            return Type::Function {
                                params,
                                ret: Box::new(enum_ty),
                            };
                        }
                        EnumVariantFields::Struct(_fields) => {
                            // Record struct variant idents for codegen (they need to become {tag: "Variant"})
                            self.tcx.variant_calls.insert(
                                (id.span.range.start, id.span.range.end),
                                (enum_name, id.name.clone()),
                            );
                            return enum_ty;
                        }
                    }
                }

                // Built-in functions
                match id.name.as_str() {
                    "parse_int" => {
                        // parse_int(s: String, radix: i32) -> i32
                        return Type::Function {
                            params: vec![
                                Type::Primitive(PrimitiveType::String),
                                Type::Primitive(PrimitiveType::I32),
                            ],
                            ret: Box::new(Type::Primitive(PrimitiveType::I32)),
                        };
                    }
                    "parse_long" => {
                        // parse_long(s: String) -> i64
                        return Type::Function {
                            params: vec![Type::Primitive(PrimitiveType::String)],
                            ret: Box::new(Type::Primitive(PrimitiveType::I64)),
                        };
                    }
                    "assert" => {
                        // assert(condition: bool) -> ()
                        return Type::Function {
                            params: vec![Type::Primitive(PrimitiveType::Bool)],
                            ret: Box::new(Type::Primitive(PrimitiveType::Unit)),
                        };
                    }
                    _ => {}
                }

                self.tcx.errors.push(SemanticError {
                    message: format!("unknown identifier `{}`", id.name),
                    span: id.span.clone(),
                });
                Type::Primitive(PrimitiveType::Unit)
            }
            ExprKind::Call {
                callee,
                type_args: _,
                args,
            } => {
                // Check if the callee is a module import (which accepts any arguments)
                let is_module_call = match &callee.kind {
                    ExprKind::Ident(id) => self.tcx.env.modules.contains_key(&id.name),
                    _ => false,
                };

                // Check if the callee is an imported enum variant and record it for codegen
                if let ExprKind::Ident(id) = &callee.kind
                    && let Some((enum_name, _variant_def)) =
                        self.tcx.env.variant_imports.get(&id.name).cloned()
                {
                    // Record this call as a variant call for codegen
                    self.tcx.variant_calls.insert(
                        (expr.span.range.start, expr.span.range.end),
                        (enum_name, id.name.clone()),
                    );
                }

                // Get the function name for looking up generic type parameters
                let fn_name = match &callee.kind {
                    ExprKind::Ident(id) => Some(id.name.clone()),
                    _ => None,
                };

                // Get the function definition for generic type inference
                let fn_def = fn_name
                    .as_ref()
                    .and_then(|name| self.tcx.env.functions.get(name).cloned());

                let callee_ty = self.check_expr(callee);
                let (param_tys, ret_ty) = match callee_ty {
                    Type::Function { params, ret } => (params, *ret),
                    other => {
                        self.tcx.errors.push(SemanticError {
                            message: format!(
                                "cannot call non-function type `{}`",
                                self.format_type(&other)
                            ),
                            span: expr.span.clone(),
                        });
                        return Type::Primitive(PrimitiveType::Unit);
                    }
                };

                // Skip arity checking for module imports - they accept any number of args
                if !is_module_call && param_tys.len() != args.len() {
                    self.tcx.errors.push(SemanticError {
                        message: format!(
                            "function expects {} argument(s), got {}",
                            param_tys.len(),
                            args.len()
                        ),
                        span: expr.span.clone(),
                    });
                }

                // Collect type substitutions for generic functions
                let mut substitutions = std::collections::HashMap::new();
                let type_param_names: Vec<String> = fn_def
                    .as_ref()
                    .map(|d| d.type_param_names())
                    .unwrap_or_default();

                // Type-check arguments with expected types for closure inference
                // (skip for module calls since we don't know the signature)
                if !is_module_call {
                    for (i, arg) in args.iter().enumerate() {
                        let expected = param_tys.get(i);
                        // Use check_expr_with_expected to enable closure parameter inference
                        let arg_ty = self.check_expr_with_expected(arg, expected);

                        // Collect substitutions for generic type parameters
                        if let Some(param_ty) = expected {
                            self.unify_types(
                                param_ty,
                                &arg_ty,
                                &type_param_names,
                                &mut substitutions,
                            );
                        }

                        // For generic functions, we skip strict type checking since types
                        // like T should match any concrete type
                        if let Some(expected) = expected {
                            // Only check compatibility for non-generic parameter types
                            let is_generic_param = match expected {
                                Type::Named { name, args } if args.is_empty() => {
                                    type_param_names.contains(name)
                                }
                                _ => false,
                            };
                            if !is_generic_param && !self.types_compatible(expected, &arg_ty) {
                                self.tcx.errors.push(SemanticError {
                                    message: format!(
                                        "mismatched argument type at position {}: expected `{:?}`, found `{:?}`",
                                        i, expected, arg_ty
                                    ),
                                    span: arg.span.clone(),
                                });
                            }
                        }
                    }
                } else {
                    // Still type-check the arguments, but don't enforce types
                    for arg in args.iter() {
                        let _ = self.check_expr(arg);
                    }
                }

                // Verify trait bounds for generic type parameters
                if let Some(ref fn_def) = fn_def {
                    for type_param in &fn_def.type_params {
                        if !type_param.bounds.is_empty() {
                            // Get the substituted type for this type parameter
                            if let Some(concrete_type) = substitutions.get(&type_param.name) {
                                // Extract the type name for trait lookup
                                let concrete_type_name = self.type_to_name(concrete_type);

                                // Check each trait bound
                                for bound in &type_param.bounds {
                                    if !self
                                        .tcx
                                        .env
                                        .type_implements_trait(&concrete_type_name, bound)
                                    {
                                        self.tcx.errors.push(SemanticError {
                                            message: format!(
                                                "the trait bound `{}: {}` is not satisfied",
                                                concrete_type_name, bound
                                            ),
                                            span: expr.span.clone(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }

                // Special handling for assert_eq and assert_ne: enforce PartialEq on arguments
                // These are extern "js" functions that accept JsValue, but semantically we want
                // to require PartialEq on the types being compared.
                if let Some(ref name) = fn_name
                    && (name == "assert_eq" || name == "assert_ne")
                {
                    // Check first two arguments for PartialEq
                    for (i, arg) in args.iter().take(2).enumerate() {
                        let arg_ty = self.check_expr(arg);
                        let arg_type_name = self.type_to_name(&arg_ty);

                        // Skip if type is JsValue or an extern type (already opaque)
                        if arg_type_name != "JsValue"
                            && !arg_type_name.starts_with('?')
                            && !self
                                .tcx
                                .env
                                .type_implements_trait(&arg_type_name, "PartialEq")
                        {
                            self.tcx.errors.push(SemanticError {
                                message: format!(
                                    "the trait bound `{}: PartialEq` is not satisfied",
                                    arg_type_name
                                ),
                                span: args
                                    .get(i)
                                    .map(|a| a.span.clone())
                                    .unwrap_or_else(|| expr.span.clone()),
                            });
                        }
                    }
                }

                // Apply substitutions to the return type for generic functions
                if !type_param_names.is_empty() {
                    self.apply_substitutions(&ret_ty, &substitutions)
                } else {
                    ret_ty
                }
            }
            ExprKind::Field { base, member } => {
                let base_ty = self.check_expr(base);

                // Handle .length on arrays and strings
                if member.name == "length" {
                    match &base_ty {
                        Type::Array(_) | Type::Primitive(PrimitiveType::String) => {
                            let field_ty = Type::Primitive(PrimitiveType::I32);
                            // Register hover info for .length field
                            let base_name = self.format_type(&base_ty);
                            self.tcx.hover_info.insert(
                                (member.span.range.start, member.span.range.end),
                                HoverInfo {
                                    signature: format!("{}.length: i32", base_name),
                                    docs: None,
                                    definition_span: None,
                                },
                            );
                            return field_ty;
                        }
                        _ => {}
                    }
                }

                // Handle Range.start and Range.end fields
                if let Type::Named { name, args } = &base_ty
                    && name == "Range"
                    && (member.name == "start"
                        || member.name == "end"
                        || member.name == "inclusive")
                {
                    // Range<T> has start/end fields of type T and an inclusive flag
                    let field_ty = if member.name == "inclusive" {
                        Type::Primitive(PrimitiveType::Bool)
                    } else {
                        args.first()
                            .cloned()
                            .unwrap_or(Type::Primitive(PrimitiveType::I32))
                    };
                    let type_str = self.format_type(&field_ty);
                    self.tcx.hover_info.insert(
                        (member.span.range.start, member.span.range.end),
                        HoverInfo {
                            signature: format!("Range.{}: {}", member.name, type_str),
                            docs: None,
                            definition_span: None,
                        },
                    );
                    return field_ty;
                }

                if let Type::Named { name, args } = &base_ty {
                    // First, check regular struct fields
                    if let Some(def) = self.tcx.env.structs.get(name).cloned()
                        && let Some(field_ty_expr) = def.fields.get(&member.name)
                    {
                        // For now, ignore generic substitution and just resolve as-is.
                        let _ = args;
                        let field_ty = self.tcx.resolve_type_expr(field_ty_expr, &def.type_params);
                        // Register hover info for struct field access
                        let type_str = self.format_type(&field_ty);
                        self.tcx.hover_info.insert(
                            (member.span.range.start, member.span.range.end),
                            HoverInfo {
                                signature: format!("{}.{}: {}", name, member.name, type_str),
                                docs: None,
                                definition_span: None,
                            },
                        );
                        // Track field reference for rename support
                        // Use format "StructName.field_name" to identify fields uniquely
                        let field_key = format!("{}.{}", name, member.name);
                        self.tcx.add_reference(
                            &field_key,
                            ReferenceKind::Field,
                            member.span.clone(),
                        );
                        return field_ty;
                    }

                    // Then check extern properties from impl blocks
                    let prop_ty = self
                        .tcx
                        .env
                        .impls
                        .iter()
                        .find(|info| &info.self_ty_name == name)
                        .and_then(|info| info.properties.get(&member.name))
                        .map(|prop| prop.ty.clone());
                    if let Some(ty) = prop_ty {
                        return self.tcx.resolve_type_expr(&ty, &[]);
                    }

                    // Not found in either
                    if self.tcx.env.structs.contains_key(name) {
                        self.tcx.errors.push(SemanticError {
                            message: format!(
                                "no field named `{}` on struct `{}`",
                                member.name, name
                            ),
                            span: member.span.clone(),
                        });
                    } else {
                        // Extern struct - try to be permissive for JS FFI
                        // Return Unit as fallback (JS property access is dynamic)
                        return Type::Primitive(PrimitiveType::Unit);
                    }
                } else {
                    self.tcx.errors.push(SemanticError {
                        message: "field access is only supported on struct types".to_string(),
                        span: expr.span.clone(),
                    });
                }
                Type::Primitive(PrimitiveType::Unit)
            }
            ExprKind::MethodCall {
                receiver,
                method,
                type_args,
                args,
            } => {
                let method_name = &method.name;

                // Check if receiver is an extern module (e.g., Array.from, Math.ceil)
                if let ExprKind::Ident(ref id) = receiver.kind {
                    // Clone the return type to avoid borrow conflicts
                    let module_fn_ret_type: Option<Option<TypeExpr>> = self
                        .tcx
                        .env
                        .modules
                        .get(&id.name)
                        .and_then(|m| m.functions.get(method_name))
                        .map(|fn_info| fn_info.ret_type.clone());

                    if let Some(ret_type_opt) = module_fn_ret_type {
                        // Type-check arguments
                        for arg in args {
                            let _ = self.check_expr(arg);
                        }
                        // Return the function's return type
                        if let Some(ret_ty) = ret_type_opt {
                            return self.tcx.resolve_type_expr(&ret_ty, &[]);
                        }
                        return Type::Primitive(PrimitiveType::Unit);
                    }
                }

                // Type-check receiver
                let receiver_ty = self.check_expr(receiver);

                // Record receiver type for codegen (used for js_name resolution)
                // This allows codegen to know when a method is called on a known stdlib type
                let receiver_type_name = self.format_type(&receiver_ty);
                self.tcx.type_resolution.insert(
                    (receiver.span.range.start, receiver.span.range.end),
                    receiver_type_name,
                );

                // Handle closure-taking array methods that require special type inference.
                // These methods need the element type to construct expected closure types,
                // enabling parameter inference for closures like `|x| x * 2`.
                if let Type::Array(elem_ty) = &receiver_ty {
                    match method_name.as_str() {
                        "some" | "every" => {
                            // Closure type: Fn(T) -> bool
                            if let Some(closure_arg) = args.first() {
                                let expected_closure = Type::Function {
                                    params: vec![(**elem_ty).clone()],
                                    ret: Box::new(Type::Primitive(PrimitiveType::Bool)),
                                };
                                let _ = self
                                    .check_expr_with_expected(closure_arg, Some(&expected_closure));
                            }
                            return Type::Primitive(PrimitiveType::Bool);
                        }
                        "filter" => {
                            // Closure type: Fn(T) -> bool
                            if let Some(closure_arg) = args.first() {
                                let expected_closure = Type::Function {
                                    params: vec![(**elem_ty).clone()],
                                    ret: Box::new(Type::Primitive(PrimitiveType::Bool)),
                                };
                                let _ = self
                                    .check_expr_with_expected(closure_arg, Some(&expected_closure));
                            }
                            return receiver_ty.clone();
                        }
                        "map" => {
                            // Closure type: Fn(T) -> U, infer return type from closure
                            if let Some(closure_arg) = args.first() {
                                // Create expected type with known param, unknown return
                                // We pass a function type so params can be inferred
                                let expected_closure = Type::Function {
                                    params: vec![(**elem_ty).clone()],
                                    ret: Box::new(Type::Primitive(PrimitiveType::Unit)), // placeholder
                                };
                                let closure_ty = self
                                    .check_expr_with_expected(closure_arg, Some(&expected_closure));
                                if let Type::Function { ret, .. } = closure_ty {
                                    return Type::Array(ret);
                                }
                            }
                            return receiver_ty.clone(); // fallback
                        }
                        "reduce" => {
                            // Closure type: Fn(T, T) -> T for simple reduce
                            if let Some(closure_arg) = args.first() {
                                let expected_closure = Type::Function {
                                    params: vec![(**elem_ty).clone(), (**elem_ty).clone()],
                                    ret: Box::new((**elem_ty).clone()),
                                };
                                let _ = self
                                    .check_expr_with_expected(closure_arg, Some(&expected_closure));
                            }
                            return (**elem_ty).clone();
                        }
                        "forEach" => {
                            // Closure type: Fn(T) -> ()
                            if let Some(closure_arg) = args.first() {
                                let expected_closure = Type::Function {
                                    params: vec![(**elem_ty).clone()],
                                    ret: Box::new(Type::Primitive(PrimitiveType::Unit)),
                                };
                                let _ = self
                                    .check_expr_with_expected(closure_arg, Some(&expected_closure));
                            }
                            return Type::Primitive(PrimitiveType::Unit);
                        }
                        "find" => {
                            // Closure type: Fn(T) -> bool, returns Option<T>
                            if let Some(closure_arg) = args.first() {
                                let expected_closure = Type::Function {
                                    params: vec![(**elem_ty).clone()],
                                    ret: Box::new(Type::Primitive(PrimitiveType::Bool)),
                                };
                                let _ = self
                                    .check_expr_with_expected(closure_arg, Some(&expected_closure));
                            }
                            return Type::Named {
                                name: "Option".to_string(),
                                args: vec![(**elem_ty).clone()],
                            };
                        }
                        "findIndex" | "findLastIndex" => {
                            // Closure type: Fn(T) -> bool, returns i32
                            if let Some(closure_arg) = args.first() {
                                let expected_closure = Type::Function {
                                    params: vec![(**elem_ty).clone()],
                                    ret: Box::new(Type::Primitive(PrimitiveType::Bool)),
                                };
                                let _ = self
                                    .check_expr_with_expected(closure_arg, Some(&expected_closure));
                            }
                            return Type::Primitive(PrimitiveType::I32);
                        }
                        "sort" | "sortBy" => {
                            // sort takes Fn(T, T) -> i32
                            if let Some(closure_arg) = args.first() {
                                let expected_closure = Type::Function {
                                    params: vec![(**elem_ty).clone(), (**elem_ty).clone()],
                                    ret: Box::new(Type::Primitive(PrimitiveType::I32)),
                                };
                                let _ = self
                                    .check_expr_with_expected(closure_arg, Some(&expected_closure));
                            }
                            return receiver_ty.clone();
                        }
                        _ => {}
                    }
                }

                // Handle iterator methods that require closure type inference.
                // Extract element type from iterator types (impl Iterator<T> or Iterator<T>)
                let iterator_elem_ty = if let Type::ImplTrait { trait_ty } = &receiver_ty {
                    // Check if it's impl Iterator<T>
                    if let Type::Named { name, args } = trait_ty.as_ref() {
                        if name == "Iterator" && !args.is_empty() {
                            Some(args[0].clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else if let Type::Named { name, args } = &receiver_ty {
                    // Check if it's Iterator<T> directly
                    if name == "Iterator" && !args.is_empty() {
                        Some(args[0].clone())
                    } else {
                        None
                    }
                } else {
                    None
                };

                // Use StdlibIndex to infer iterator method return types
                if let Some(elem_ty) = iterator_elem_ty
                    && let Some(result_ty) = self.infer_iterator_method(IteratorMethodArgs {
                        elem_ty: &elem_ty,
                        receiver_ty: &receiver_ty,
                        method_name,
                        args,
                        receiver,
                        type_args,
                        span: &expr.span,
                    })
                {
                    return result_ty;
                }

                // Type-check remaining arguments without expected types
                for arg in args {
                    let _ = self.check_expr(arg);
                }

                // Handle tuple.to_array() specially
                if let Type::Tuple(elements) = &receiver_ty
                    && method_name == "to_array"
                {
                    // Check that all elements have the same type
                    if elements.is_empty() {
                        // Empty tuple -> Result<[()], String>
                        return Type::Named {
                            name: "Result".to_string(),
                            args: vec![
                                Type::Array(Box::new(Type::Primitive(PrimitiveType::Unit))),
                                Type::Primitive(PrimitiveType::String),
                            ],
                        };
                    }

                    let first_type = &elements[0];
                    let all_same = elements
                        .iter()
                        .skip(1)
                        .all(|t| self.types_compatible(first_type, t));

                    if all_same {
                        // Homogeneous tuple -> Result<[T], String>
                        return Type::Named {
                            name: "Result".to_string(),
                            args: vec![
                                Type::Array(Box::new(first_type.clone())),
                                Type::Primitive(PrimitiveType::String),
                            ],
                        };
                    } else {
                        // Heterogeneous tuple -> compile error
                        self.tcx.errors.push(SemanticError {
                            message: format!(
                                "cannot call `to_array()` on tuple with mixed types `{}`",
                                self.format_type(&receiver_ty)
                            ),
                            span: expr.span.clone(),
                        });
                        return Type::Primitive(PrimitiveType::Unit);
                    }
                }

                // Handle Result/Option unwrap methods specially to extract inner type
                if let Type::Named { name, args } = &receiver_ty {
                    match (name.as_str(), method_name.as_str()) {
                        ("Result", "unwrap") | ("Result", "expect") => {
                            // Result<T, E>.unwrap() returns T
                            if let Some(ok_type) = args.first() {
                                return ok_type.clone();
                            }
                        }
                        ("Option", "unwrap") | ("Option", "expect") => {
                            // Option<T>.unwrap() returns T
                            if let Some(inner_type) = args.first() {
                                return inner_type.clone();
                            }
                        }
                        _ => {}
                    }
                }

                // Try to resolve the method's return type from impl blocks
                // Include primitive types so we can find `impl String { ... }` etc.
                // For arrays, we use "[T]" to match generic impl blocks like `impl<T> [T] { ... }`.
                let receiver_type_name = match &receiver_ty {
                    Type::Named { name, .. } => Some(name.clone()),
                    Type::Primitive(p) => Some(primitive_type_name(p).to_string()),
                    Type::Array(_) => Some("[T]".to_string()),
                    _ => None,
                };

                // For generic types like Set<i32> or Map<String, i32>, generate the generic form
                // so we can match impl<T> Set<T> { ... } or impl<K, V> Map<K, V> { ... } blocks
                let generic_type_name = match &receiver_ty {
                    Type::Named { name, args } if args.len() == 1 => Some(format!("{}<T>", name)),
                    Type::Named { name, args } if args.len() == 2 => {
                        Some(format!("{}<K, V>", name))
                    }
                    _ => None,
                };

                // Look up the method in impl blocks and get its return type.
                // Use Option<Option<TypeExpr>> to distinguish "method not found" from "method found with no return type"
                let method_lookup_result: Option<Option<TypeExpr>> = if let Some(ref type_name) =
                    receiver_type_name
                {
                    let mut found = None;
                    for impl_info in &self.tcx.env.impls {
                        // Match either the exact type name or the generic form
                        let matches = impl_info.self_ty_name == *type_name
                            || generic_type_name
                                .as_ref()
                                .is_some_and(|g| impl_info.self_ty_name == *g);
                        if matches && let Some(method_info) = impl_info.methods.get(method_name) {
                            // Found the method - wrap ret_type in Some to indicate success
                            found = Some(method_info.ret_type.clone());
                            break;
                        }
                    }
                    found
                } else {
                    None
                };

                // Resolve the return type
                match method_lookup_result {
                    Some(Some(ret_type_expr)) => {
                        // Method found with explicit return type
                        // For generic methods from `impl<T> [T]`, `impl<T> Set<T>`, or
                        // `impl<K, V> Map<K, V>`, we need to substitute type params.
                        if let Type::Array(elem_ty) = &receiver_ty {
                            // Resolve using "T" as a generic param, then substitute
                            let resolved = self
                                .tcx
                                .resolve_type_expr(&ret_type_expr, &["T".to_string()]);
                            substitute_type_param(&resolved, "T", elem_ty)
                        } else if let Type::Named { args, .. } = &receiver_ty {
                            match args.len() {
                                1 => {
                                    // Single-param generic like Set<i32> - substitute T
                                    let resolved = self
                                        .tcx
                                        .resolve_type_expr(&ret_type_expr, &["T".to_string()]);
                                    substitute_type_param(&resolved, "T", &args[0])
                                }
                                2 => {
                                    // Two-param generic like Map<String, i32> - substitute K and V
                                    let resolved = self.tcx.resolve_type_expr(
                                        &ret_type_expr,
                                        &["K".to_string(), "V".to_string()],
                                    );
                                    let resolved = substitute_type_param(&resolved, "K", &args[0]);
                                    substitute_type_param(&resolved, "V", &args[1])
                                }
                                _ => self.tcx.resolve_type_expr(&ret_type_expr, &[]),
                            }
                        } else {
                            self.tcx.resolve_type_expr(&ret_type_expr, &[])
                        }
                    }
                    Some(None) => {
                        // Method found but returns unit (no explicit return type)
                        Type::Primitive(PrimitiveType::Unit)
                    }
                    None => {
                        // Method not found - report error unless it's a special pseudo-method
                        // that's handled in check_expr_with_expected (into, parse, try_into)
                        let pseudo_methods = ["into", "parse", "try_into"];
                        if !pseudo_methods.contains(&method_name.as_str()) {
                            self.tcx.errors.push(SemanticError {
                                message: format!(
                                    "no method named `{}` found for type `{}`",
                                    method_name,
                                    self.format_type(&receiver_ty)
                                ),
                                span: method.span.clone(),
                            });
                        }
                        Type::Primitive(PrimitiveType::Unit)
                    }
                }
            }
            ExprKind::Unary { op, expr: inner } => {
                let inner_ty = self.check_expr(inner);
                match op {
                    husk_ast::UnaryOp::Not => {
                        if !matches!(inner_ty, Type::Primitive(PrimitiveType::Bool)) {
                            self.tcx.errors.push(SemanticError {
                                message: "operator `!` expects operand of type `bool`".to_string(),
                                span: expr.span.clone(),
                            });
                        }
                        Type::Primitive(PrimitiveType::Bool)
                    }
                    husk_ast::UnaryOp::Neg => {
                        if !matches!(inner_ty, Type::Primitive(PrimitiveType::I32)) {
                            self.tcx.errors.push(SemanticError {
                                message: "unary `-` expects operand of type `i32`".to_string(),
                                span: expr.span.clone(),
                            });
                        }
                        Type::Primitive(PrimitiveType::I32)
                    }
                }
            }
            ExprKind::Binary { op, left, right } => {
                let left_ty = self.check_expr(left);
                let right_ty = self.check_expr(right);
                use husk_ast::BinaryOp::*;
                match op {
                    Add => {
                        // Add supports i32 + i32, i64 + i64, and String + String
                        if matches!(left_ty, Type::Primitive(PrimitiveType::String))
                            && matches!(right_ty, Type::Primitive(PrimitiveType::String))
                        {
                            Type::Primitive(PrimitiveType::String)
                        } else if matches!(left_ty, Type::Primitive(PrimitiveType::I32))
                            && matches!(right_ty, Type::Primitive(PrimitiveType::I32))
                        {
                            Type::Primitive(PrimitiveType::I32)
                        } else if matches!(left_ty, Type::Primitive(PrimitiveType::I64))
                            && matches!(right_ty, Type::Primitive(PrimitiveType::I64))
                        {
                            Type::Primitive(PrimitiveType::I64)
                        } else {
                            self.tcx.errors.push(SemanticError {
                                message:
                                    "`+` requires operands of the same type (`i32`, `i64`, or `String`)"
                                        .to_string(),
                                span: expr.span.clone(),
                            });
                            Type::Primitive(PrimitiveType::I32)
                        }
                    }
                    Sub | Mul | Div | Mod => {
                        // Arithmetic supports i32 and i64, operands must match
                        if matches!(left_ty, Type::Primitive(PrimitiveType::I64))
                            && matches!(right_ty, Type::Primitive(PrimitiveType::I64))
                        {
                            Type::Primitive(PrimitiveType::I64)
                        } else if matches!(left_ty, Type::Primitive(PrimitiveType::I32))
                            && matches!(right_ty, Type::Primitive(PrimitiveType::I32))
                        {
                            Type::Primitive(PrimitiveType::I32)
                        } else {
                            self.tcx.errors.push(SemanticError {
                                message: "arithmetic operators expect operands of the same type (`i32` or `i64`)"
                                    .to_string(),
                                span: expr.span.clone(),
                            });
                            Type::Primitive(PrimitiveType::I32)
                        }
                    }
                    Eq | NotEq | Lt | Gt | Le | Ge => {
                        if !self.types_compatible(&left_ty, &right_ty) {
                            self.tcx.errors.push(SemanticError {
                                message: format!(
                                    "cannot compare `{}` with `{}`",
                                    self.format_type(&left_ty),
                                    self.format_type(&right_ty)
                                ),
                                span: expr.span.clone(),
                            });
                        }
                        Type::Primitive(PrimitiveType::Bool)
                    }
                    And | Or => {
                        if !matches!(left_ty, Type::Primitive(PrimitiveType::Bool))
                            || !matches!(right_ty, Type::Primitive(PrimitiveType::Bool))
                        {
                            self.tcx.errors.push(SemanticError {
                                message: "logical operators expect operands of type `bool`"
                                    .to_string(),
                                span: expr.span.clone(),
                            });
                        }
                        Type::Primitive(PrimitiveType::Bool)
                    }
                }
            }
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let cond_ty = self.check_expr(cond);
                if !matches!(cond_ty, Type::Primitive(PrimitiveType::Bool)) {
                    self.tcx.errors.push(SemanticError {
                        message: "if condition must be bool".to_string(),
                        span: cond.span.clone(),
                    });
                }

                let then_ty = self.check_expr(then_branch);
                let else_ty = self.check_expr(else_branch);

                if !self.types_compatible(&then_ty, &else_ty) {
                    self.tcx.errors.push(SemanticError {
                        message: format!(
                            "if/else branches must have the same type, found `{}` and `{}`",
                            self.format_type(&then_ty),
                            self.format_type(&else_ty)
                        ),
                        span: expr.span.clone(),
                    });
                }

                then_ty
            }
            ExprKind::Match { scrutinee, arms } => self.check_match_expr(expr, scrutinee, arms),
            ExprKind::Block(block) => self.check_block(block),
            ExprKind::Struct { name, fields } => {
                // Type-check field expressions and resolve to the struct type.
                for field in fields {
                    self.check_expr(&field.value);
                }
                // Use the last segment of the path as the type name.
                let type_name = name.last().map(|id| id.name.clone()).unwrap_or_default();

                // Track struct literal reference for rename support
                if let Some(name_ident) = name.last() {
                    self.tcx.add_reference(
                        &type_name,
                        ReferenceKind::Struct,
                        name_ident.span.clone(),
                    );
                }

                // Track field references in struct literal for rename support
                for field in fields {
                    let field_key = format!("{}.{}", type_name, field.name.name);
                    self.tcx.add_reference(
                        &field_key,
                        ReferenceKind::Field,
                        field.name.span.clone(),
                    );
                }

                Type::Named {
                    name: type_name,
                    args: Vec::new(),
                }
            }
            ExprKind::FormatPrint {
                format,
                args,
                newline: _,
            } => {
                // Count placeholders (excluding escaped braces which are literals)
                let mut placeholders: Vec<&husk_ast::FormatPlaceholder> = Vec::new();
                for segment in &format.segments {
                    if let FormatSegment::Placeholder(ph) = segment {
                        placeholders.push(ph);
                    }
                }

                // Check for mixing explicit numeric positions (like {0}) with implicit positions ({}).
                // Named placeholders like {x} are allowed to mix with implicit {} since the parser
                // synthesizes arguments for them separately.
                let has_explicit_numeric_position = placeholders
                    .iter()
                    .any(|ph| ph.position.is_some() && ph.name.is_none());
                let has_implicit_position = placeholders
                    .iter()
                    .any(|ph| ph.position.is_none() && ph.name.is_none());

                if has_explicit_numeric_position && has_implicit_position {
                    self.tcx.errors.push(SemanticError {
                        message: "cannot mix positional and implicit argument indexing".to_string(),
                        span: format.span.clone(),
                    });
                }

                // Validate argument count
                // Named placeholders have synthesized arguments with explicit positions
                let has_explicit_position = placeholders.iter().any(|ph| ph.position.is_some());
                if has_explicit_position {
                    // With explicit positions, check that all indices are in bounds
                    for ph in &placeholders {
                        if let Some(pos) = ph.position
                            && pos >= args.len()
                        {
                            self.tcx.errors.push(SemanticError {
                                    message: format!(
                                        "positional argument {} is out of range (only {} argument(s) provided)",
                                        pos,
                                        args.len()
                                    ),
                                    span: ph.span.clone(),
                                });
                        }
                    }
                } else {
                    // With implicit positions, count must match exactly
                    let placeholder_count = placeholders.len();
                    if placeholder_count != args.len() {
                        self.tcx.errors.push(SemanticError {
                            message: format!(
                                "format string requires {} argument(s), but {} provided",
                                placeholder_count,
                                args.len()
                            ),
                            span: expr.span.clone(),
                        });
                    }
                }

                // Type-check all format arguments
                let arg_types: Vec<Type> = args.iter().map(|arg| self.check_expr(arg)).collect();

                // Validate type compatibility for numeric format specifiers
                for (i, ph) in placeholders.iter().enumerate() {
                    let arg_index = ph.position.unwrap_or(i);
                    if let Some(arg_ty) = arg_types.get(arg_index) {
                        // Numeric format specifiers require i32
                        if let Some(ty_char) = ph.spec.ty {
                            match ty_char {
                                'x' | 'X' | 'b' | 'o' => {
                                    if !matches!(arg_ty, Type::Primitive(PrimitiveType::I32)) {
                                        self.tcx.errors.push(SemanticError {
                                            message: format!(
                                                "format specifier `:{ty_char}` requires numeric type, found `{arg_ty:?}`"
                                            ),
                                            span: ph.span.clone(),
                                        });
                                    }
                                }
                                '?' => {
                                    // Debug format works with any type
                                }
                                _ => {}
                            }
                        }
                    }
                }

                // println returns unit
                Type::Primitive(PrimitiveType::Unit)
            }
            ExprKind::Format { format, args } => {
                // Same validation as FormatPrint
                let mut placeholders: Vec<&husk_ast::FormatPlaceholder> = Vec::new();
                for segment in &format.segments {
                    if let FormatSegment::Placeholder(ph) = segment {
                        placeholders.push(ph);
                    }
                }

                // Check for mixing explicit numeric positions (like {0}) with implicit positions ({}).
                // Named placeholders like {x} are allowed to mix with implicit {} since the parser
                // synthesizes arguments for them separately.
                let has_explicit_numeric_position = placeholders
                    .iter()
                    .any(|ph| ph.position.is_some() && ph.name.is_none());
                let has_implicit_position = placeholders
                    .iter()
                    .any(|ph| ph.position.is_none() && ph.name.is_none());

                if has_explicit_numeric_position && has_implicit_position {
                    self.tcx.errors.push(SemanticError {
                        message: "cannot mix positional and implicit argument indexing".to_string(),
                        span: format.span.clone(),
                    });
                }

                // Named placeholders have synthesized arguments with explicit positions
                let has_explicit_position = placeholders.iter().any(|ph| ph.position.is_some());
                if has_explicit_position {
                    for ph in &placeholders {
                        if let Some(pos) = ph.position
                            && pos >= args.len()
                        {
                            self.tcx.errors.push(SemanticError {
                                    message: format!(
                                        "positional argument {} is out of range (only {} argument(s) provided)",
                                        pos,
                                        args.len()
                                    ),
                                    span: ph.span.clone(),
                                });
                        }
                    }
                } else {
                    let placeholder_count = placeholders.len();
                    if placeholder_count != args.len() {
                        self.tcx.errors.push(SemanticError {
                            message: format!(
                                "format string requires {} argument(s), but {} provided",
                                placeholder_count,
                                args.len()
                            ),
                            span: expr.span.clone(),
                        });
                    }
                }

                let arg_types: Vec<Type> = args.iter().map(|arg| self.check_expr(arg)).collect();

                for (i, ph) in placeholders.iter().enumerate() {
                    let arg_index = ph.position.unwrap_or(i);
                    if let Some(arg_ty) = arg_types.get(arg_index)
                        && let Some(ty_char) = ph.spec.ty
                    {
                        match ty_char {
                            'x' | 'X' | 'b' | 'o' => {
                                if !matches!(arg_ty, Type::Primitive(PrimitiveType::I32)) {
                                    self.tcx.errors.push(SemanticError {
                                            message: format!(
                                                "format specifier `:{ty_char}` requires numeric type, found `{arg_ty:?}`"
                                            ),
                                            span: ph.span.clone(),
                                        });
                                }
                            }
                            '?' => {}
                            _ => {}
                        }
                    }
                }

                // format returns String
                Type::Primitive(PrimitiveType::String)
            }
            ExprKind::Closure {
                params,
                ret_type,
                body,
            } => self.check_closure_expr(expr, params, ret_type.as_ref(), body, None),
            ExprKind::Array { elements } => {
                if elements.is_empty() {
                    // Empty array - for now, allow it with unit element type
                    // A more complete implementation would require type annotation
                    Type::Array(Box::new(Type::unit()))
                } else {
                    // Infer element type from first element
                    let first_ty = self.check_expr(&elements[0]);

                    // Check all elements have compatible types
                    for (i, elem) in elements.iter().enumerate().skip(1) {
                        let elem_ty = self.check_expr(elem);
                        // For now, just check they match (could be smarter about unions/coercion)
                        if elem_ty != first_ty {
                            self.tcx.errors.push(SemanticError {
                                message: format!(
                                    "array element {} has type `{:?}`, expected `{:?}`",
                                    i, elem_ty, first_ty
                                ),
                                span: elem.span.clone(),
                            });
                        }
                    }

                    Type::Array(Box::new(first_ty))
                }
            }
            ExprKind::Index { base, index } => {
                let base_ty = self.check_expr(base);

                // Check if this is a slice operation (index is a Range) or simple indexing
                let is_slice = matches!(index.kind, ExprKind::Range { .. });

                if is_slice {
                    // Slice operation: arr[start..end], arr[..], etc.
                    // Check the range expression (will validate start/end are i32)
                    let _range_ty = self.check_expr(index);

                    // Slicing an array returns an array of the same element type
                    match base_ty {
                        Type::Array(_) => base_ty,
                        Type::Named { ref name, ref args } if name == "Vec" && !args.is_empty() => {
                            // Vec slice returns Vec
                            base_ty
                        }
                        Type::Named { .. } => Type::Named {
                            name: "JsValue".to_string(),
                            args: vec![],
                        },
                        _ => {
                            self.tcx.errors.push(SemanticError {
                                message: format!(
                                    "cannot slice type `{}`",
                                    self.format_type(&base_ty)
                                ),
                                span: base.span.clone(),
                            });
                            Type::unit()
                        }
                    }
                } else {
                    // Simple index operation: arr[i]
                    let index_ty = self.check_expr(index);

                    // Verify index is integer
                    let is_valid_index = matches!(index_ty, Type::Primitive(PrimitiveType::I32))
                        || matches!(&index_ty, Type::Named { name, .. } if name == "number");
                    if !is_valid_index {
                        self.tcx.errors.push(SemanticError {
                            message: format!(
                                "array index must be integer, found `{}`",
                                self.format_type(&index_ty)
                            ),
                            span: index.span.clone(),
                        });
                    }

                    // Extract element type from [T]
                    match base_ty {
                        Type::Array(elem_ty) => (*elem_ty).clone(),
                        // Also accept Vec<T> for backwards compat
                        Type::Named { ref name, ref args } if name == "Vec" && !args.is_empty() => {
                            args[0].clone()
                        }
                        // Allow indexing any type that we don't know (e.g. extern types, JsValue)
                        Type::Named { .. } => Type::Named {
                            name: "JsValue".to_string(),
                            args: vec![],
                        },
                        _ => {
                            self.tcx.errors.push(SemanticError {
                                message: format!(
                                    "cannot index into type `{}`",
                                    self.format_type(&base_ty)
                                ),
                                span: base.span.clone(),
                            });
                            Type::unit()
                        }
                    }
                }
            }
            ExprKind::Range {
                start,
                end,
                inclusive: _,
            } => {
                // Infer element type from bounds, defaulting to i32
                let mut elem_ty = Type::Primitive(PrimitiveType::I32);

                // Verify start is integer if present
                if let Some(start_expr) = start {
                    let start_ty = self.check_expr(start_expr);
                    if matches!(
                        start_ty,
                        Type::Primitive(PrimitiveType::I32 | PrimitiveType::I64)
                    ) {
                        // Use i64 if either bound is i64
                        if matches!(start_ty, Type::Primitive(PrimitiveType::I64)) {
                            elem_ty = Type::Primitive(PrimitiveType::I64);
                        }
                    } else {
                        self.tcx.errors.push(SemanticError {
                            message: format!(
                                "range start must be an integer (i32 or i64), found {}",
                                self.format_type(&start_ty)
                            ),
                            span: start_expr.span.clone(),
                        });
                    }
                }

                // Verify end is integer if present
                if let Some(end_expr) = end {
                    let end_ty = self.check_expr(end_expr);
                    if matches!(
                        end_ty,
                        Type::Primitive(PrimitiveType::I32 | PrimitiveType::I64)
                    ) {
                        // Use i64 if either bound is i64
                        if matches!(end_ty, Type::Primitive(PrimitiveType::I64)) {
                            elem_ty = Type::Primitive(PrimitiveType::I64);
                        }
                    } else {
                        self.tcx.errors.push(SemanticError {
                            message: format!(
                                "range end must be an integer (i32 or i64), found {}",
                                self.format_type(&end_ty)
                            ),
                            span: end_expr.span.clone(),
                        });
                    }
                }

                // Range type - acts like an iterable of the element type
                Type::Named {
                    name: "Range".to_string(),
                    args: vec![elem_ty],
                }
            }
            ExprKind::Assign {
                target,
                op: _,
                value,
            } => {
                // Type check both sides
                let _target_ty = self.check_expr(target);

                // Assignment expression returns the assigned value
                self.check_expr(value)
            }
            ExprKind::JsLiteral { .. } => {
                // Raw JavaScript literals are treated as dynamically typed (JsValue)
                // They can evaluate to any JavaScript value at runtime.
                Type::Named {
                    name: "JsValue".to_string(),
                    args: Vec::new(),
                }
            }
            ExprKind::Cast {
                expr: inner,
                target_ty,
            } => {
                let inner_ty = self.check_expr(inner);
                let target = self.tcx.resolve_type_expr(target_ty, &[]);

                // Check if the cast is allowed between primitive types
                let allowed = match (&inner_ty, &target) {
                    // Numeric conversions: i32 <-> i64 <-> f64
                    (Type::Primitive(PrimitiveType::I32), Type::Primitive(PrimitiveType::I64)) => {
                        true
                    }
                    (Type::Primitive(PrimitiveType::I64), Type::Primitive(PrimitiveType::I32)) => {
                        true
                    }
                    (Type::Primitive(PrimitiveType::I32), Type::Primitive(PrimitiveType::F64)) => {
                        true
                    }
                    (Type::Primitive(PrimitiveType::F64), Type::Primitive(PrimitiveType::I32)) => {
                        true
                    }
                    (Type::Primitive(PrimitiveType::I64), Type::Primitive(PrimitiveType::F64)) => {
                        true
                    }
                    (Type::Primitive(PrimitiveType::F64), Type::Primitive(PrimitiveType::I64)) => {
                        true
                    }

                    // Bool to numeric
                    (Type::Primitive(PrimitiveType::Bool), Type::Primitive(PrimitiveType::I32)) => {
                        true
                    }
                    (Type::Primitive(PrimitiveType::Bool), Type::Primitive(PrimitiveType::I64)) => {
                        true
                    }
                    (Type::Primitive(PrimitiveType::Bool), Type::Primitive(PrimitiveType::F64)) => {
                        true
                    }

                    // Primitives to String
                    (
                        Type::Primitive(PrimitiveType::I32),
                        Type::Primitive(PrimitiveType::String),
                    ) => true,
                    (
                        Type::Primitive(PrimitiveType::I64),
                        Type::Primitive(PrimitiveType::String),
                    ) => true,
                    (
                        Type::Primitive(PrimitiveType::F64),
                        Type::Primitive(PrimitiveType::String),
                    ) => true,
                    (
                        Type::Primitive(PrimitiveType::Bool),
                        Type::Primitive(PrimitiveType::String),
                    ) => true,

                    // Same type (no-op, but allowed) - exclude bool since codegen
                    // doesn't support casting to bool
                    (a, b) if a == b && !matches!(b, Type::Primitive(PrimitiveType::Bool)) => true,

                    _ => false,
                };

                if !allowed {
                    // Generate helpful error message with hint
                    let hint = match (&inner_ty, &target) {
                        (
                            Type::Primitive(
                                PrimitiveType::I32 | PrimitiveType::I64 | PrimitiveType::F64,
                            ),
                            Type::Primitive(PrimitiveType::Bool),
                        ) => Some("use explicit comparison like `x != 0` instead"),
                        (
                            Type::Primitive(PrimitiveType::String),
                            Type::Primitive(PrimitiveType::I32),
                        ) => Some("use `parseInt(s)` instead"),
                        (
                            Type::Primitive(PrimitiveType::String),
                            Type::Primitive(PrimitiveType::I64),
                        ) => Some("use `parseLong(s)` instead"),
                        (
                            Type::Primitive(PrimitiveType::String),
                            Type::Primitive(PrimitiveType::F64),
                        ) => Some("use `parseFloat(s)` instead"),
                        _ => None,
                    };

                    let message = if let Some(hint) = hint {
                        format!(
                            "cannot cast `{}` to `{}`; {}",
                            self.format_type(&inner_ty),
                            self.format_type(&target),
                            hint
                        )
                    } else {
                        format!(
                            "cannot cast `{}` to `{}`",
                            self.format_type(&inner_ty),
                            self.format_type(&target)
                        )
                    };

                    self.tcx.errors.push(SemanticError {
                        message,
                        span: expr.span.clone(),
                    });
                }

                target
            }
            ExprKind::Tuple { elements } => {
                // Type check each element and collect their types
                let element_types: Vec<Type> =
                    elements.iter().map(|elem| self.check_expr(elem)).collect();
                Type::Tuple(element_types)
            }
            ExprKind::TupleField { base, index } => {
                let base_ty = self.check_expr(base);
                match base_ty {
                    Type::Tuple(ref element_types) => {
                        if *index < element_types.len() {
                            element_types[*index].clone()
                        } else {
                            self.tcx.errors.push(SemanticError {
                                message: format!(
                                    "tuple index {} out of bounds for tuple with {} elements",
                                    index,
                                    element_types.len()
                                ),
                                span: expr.span.clone(),
                            });
                            Type::unit()
                        }
                    }
                    _ => {
                        self.tcx.errors.push(SemanticError {
                            message: format!(
                                "type `{}` cannot be indexed with a tuple field access; expected a tuple",
                                self.format_type(&base_ty)
                            ),
                            span: base.span.clone(),
                        });
                        Type::unit()
                    }
                }
            }
            ExprKind::Try { expr: inner } => {
                // ? operator: for Result<T, E> returns T (early return on Err)
                //             for Option<T> returns T (early return on None)

                // Reject ? operator inside closures - closures cannot use ? as it would
                // either incorrectly return from an outer function or manifest as an uncaught
                // exception. If closure-local early returns are needed, they should be
                // implemented via explicit control flow.
                if self.enclosing_fn_name.is_none() {
                    self.tcx.errors.push(SemanticError {
                        message: "the `?` operator cannot be used inside closures. Use explicit error handling or control flow instead.".to_string(),
                        span: expr.span.clone(),
                    });
                    return Type::unit();
                }

                // First, validate that the enclosing function returns Result or Option
                let ret_is_result = matches!(&self.ret_ty, Type::Named { name, args } if name == "Result" && args.len() == 2);
                let ret_is_option = matches!(&self.ret_ty, Type::Named { name, args } if name == "Option" && args.len() == 1);
                let is_valid_return = ret_is_result || ret_is_option;

                if !is_valid_return {
                    let fn_context = match &self.enclosing_fn_name {
                        Some(name) => format!("function `{}`", name),
                        None => "closure".to_string(),
                    };
                    self.tcx.errors.push(SemanticError {
                        message: format!(
                            "the `?` operator can only be used in a function that returns `Result` or `Option`\n\
                             {} returns `{}`",
                            fn_context,
                            self.format_type(&self.ret_ty)
                        ),
                        span: expr.span.clone(),
                    });
                }

                // Then, validate the expression type and ensure it matches the return type
                let inner_ty = self.check_expr(inner);
                match &inner_ty {
                    Type::Named { name, args } if name == "Result" && args.len() == 2 => {
                        // Result<T, E> -> T, but only if function also returns Result
                        if !ret_is_result {
                            self.tcx.errors.push(SemanticError {
                                message: format!(
                                    "cannot use `?` on a `Result` value in a function that returns `{}`. \
                                     Use `?` on `Result` values only in functions that return `Result`",
                                    self.format_type(&self.ret_ty)
                                ),
                                span: expr.span.clone(),
                            });
                            return Type::unit();
                        }
                        args[0].clone()
                    }
                    Type::Named { name, args } if name == "Option" && args.len() == 1 => {
                        // Option<T> -> T, but only if function also returns Option
                        if !ret_is_option {
                            self.tcx.errors.push(SemanticError {
                                message: format!(
                                    "cannot use `?` on an `Option` value in a function that returns `{}`. \
                                     Use `?` on `Option` values only in functions that return `Option`",
                                    self.format_type(&self.ret_ty)
                                ),
                                span: expr.span.clone(),
                            });
                            return Type::unit();
                        }
                        args[0].clone()
                    }
                    _ => {
                        self.tcx.errors.push(SemanticError {
                            message: format!(
                                "the `?` operator can only be applied to values of type `Result` or `Option`, found `{}`",
                                self.format_type(&inner_ty)
                            ),
                            span: expr.span.clone(),
                        });
                        Type::unit()
                    }
                }
            }
        }
    }

    /// Check expression with optional expected type for bidirectional inference.
    /// This enables closure parameter type inference from call-site context,
    /// as well as type inference for conversion methods like .into() and .parse().
    fn check_expr_with_expected(&mut self, expr: &Expr, expected: Option<&Type>) -> Type {
        match &expr.kind {
            ExprKind::Closure {
                params,
                ret_type,
                body,
            } => self.check_closure_expr(expr, params, ret_type.as_ref(), body, expected),

            // Handle .into() and .parse() method calls with type inference
            ExprKind::MethodCall {
                receiver,
                method,
                type_args,
                args,
            } => {
                let method_name = method.name.as_str();

                // Handle .into() - infer target type from context or turbofish
                if method_name == "into" && args.is_empty() {
                    return self.check_into_method(receiver, type_args, expected, &expr.span);
                }

                // Handle .parse() - String.parse::<T>() -> Result<T, String>
                if method_name == "parse" && args.is_empty() {
                    return self.check_parse_method(receiver, type_args, expected, &expr.span);
                }

                // Handle .try_into() - infer target type from context
                if method_name == "try_into" && args.is_empty() {
                    return self.check_try_into_method(receiver, type_args, expected, &expr.span);
                }

                // Handle .collect() on iterators - infer collection type from context or turbofish
                if method_name == "collect" && args.is_empty() {
                    let receiver_ty = self.check_expr(receiver);
                    // Extract element type from iterator
                    let elem_ty = if let Type::ImplTrait { trait_ty } = &receiver_ty {
                        if let Type::Named { name, args } = trait_ty.as_ref() {
                            if name == "Iterator" && !args.is_empty() {
                                Some(args[0].clone())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    if let Some(elem_ty) = elem_ty {
                        return self.check_collect_method(
                            receiver, type_args, expected, &expr.span, &elem_ty,
                        );
                    }
                }

                // Fall back to regular check_expr for other method calls
                self.check_expr(expr)
            }

            // Handle empty array literals with expected type inference
            // This allows `let ranges: [Range<i32>] = [];` to properly type the array
            ExprKind::Array { elements } if elements.is_empty() => {
                if let Some(Type::Array(elem_ty)) = expected {
                    // Use the expected element type instead of unit
                    Type::Array(elem_ty.clone())
                } else {
                    // No expected type - fall back to unit element type
                    Type::Array(Box::new(Type::unit()))
                }
            }

            _ => self.check_expr(expr),
        }
    }

    /// Handle .into() method call: value.into() where the target type is inferred
    fn check_into_method(
        &mut self,
        receiver: &Expr,
        type_args: &[TypeExpr],
        expected: Option<&Type>,
        span: &husk_ast::Span,
    ) -> Type {
        let receiver_ty = self.check_expr(receiver);

        // Resolve target type from turbofish or expected type
        let target_ty = if !type_args.is_empty() {
            // Turbofish: .into::<TargetType>()
            Some(self.tcx.resolve_type_expr(&type_args[0], &[]))
        } else {
            expected.cloned()
        };

        match target_ty {
            Some(target) => {
                // Verify that From<ReceiverType> is implemented for TargetType
                if !self.type_implements_from(&target, &receiver_ty) {
                    self.tcx.errors.push(SemanticError {
                        message: format!(
                            "the trait `From<{}>` is not implemented for `{}`",
                            self.format_type(&receiver_ty),
                            self.format_type(&target)
                        ),
                        span: span.clone(),
                    });
                }
                // Record the resolved type for codegen
                self.tcx.type_resolution.insert(
                    (span.range.start, span.range.end),
                    self.format_type(&target),
                );
                target
            }
            None => {
                self.tcx.errors.push(SemanticError {
                    message: "type annotations needed: cannot infer type for `.into()`\n\
                              help: consider using turbofish syntax: `.into::<TargetType>()`"
                        .to_string(),
                    span: span.clone(),
                });
                Type::Primitive(PrimitiveType::Unit)
            }
        }
    }

    /// Handle .parse() method call: "str".parse::<i32>() -> Result<i32, String>
    fn check_parse_method(
        &mut self,
        receiver: &Expr,
        type_args: &[TypeExpr],
        expected: Option<&Type>,
        span: &husk_ast::Span,
    ) -> Type {
        let receiver_ty = self.check_expr(receiver);

        // parse() is only valid on String
        if !matches!(receiver_ty, Type::Primitive(PrimitiveType::String)) {
            self.tcx.errors.push(SemanticError {
                message: format!(
                    "`.parse()` is only available on `String`, found `{}`",
                    self.format_type(&receiver_ty)
                ),
                span: span.clone(),
            });
            return Type::Primitive(PrimitiveType::Unit);
        }

        // Resolve target type from turbofish or expected type
        let target_ty = if !type_args.is_empty() {
            // Turbofish: .parse::<i32>()
            Some(self.tcx.resolve_type_expr(&type_args[0], &[]))
        } else if let Some(Type::Named { name, args }) = expected {
            // Expected Result<T, E> - extract T
            if name == "Result" && !args.is_empty() {
                Some(args[0].clone())
            } else {
                None
            }
        } else {
            None
        };

        match target_ty {
            Some(target) => {
                // Verify TryFrom<String> is implemented for target type
                if !self.type_implements_try_from(&target, &Type::Primitive(PrimitiveType::String))
                {
                    self.tcx.errors.push(SemanticError {
                        message: format!(
                            "the trait `TryFrom<String>` is not implemented for `{}`",
                            self.format_type(&target)
                        ),
                        span: span.clone(),
                    });
                }
                // Record the resolved type for codegen
                self.tcx.type_resolution.insert(
                    (span.range.start, span.range.end),
                    self.format_type(&target),
                );
                // parse() returns Result<TargetType, String>
                Type::Named {
                    name: "Result".to_string(),
                    args: vec![target, Type::Primitive(PrimitiveType::String)],
                }
            }
            None => {
                self.tcx.errors.push(SemanticError {
                    message: "type annotations needed: cannot infer type for `.parse()`\n\
                              help: consider using turbofish syntax: `.parse::<i32>()`"
                        .to_string(),
                    span: span.clone(),
                });
                Type::Primitive(PrimitiveType::Unit)
            }
        }
    }

    /// Handle .try_into() method call: value.try_into() -> Result<TargetType, Error>
    fn check_try_into_method(
        &mut self,
        receiver: &Expr,
        type_args: &[TypeExpr],
        expected: Option<&Type>,
        span: &husk_ast::Span,
    ) -> Type {
        let receiver_ty = self.check_expr(receiver);

        // Resolve target type from turbofish or expected type
        let target_ty = if !type_args.is_empty() {
            Some(self.tcx.resolve_type_expr(&type_args[0], &[]))
        } else if let Some(Type::Named { name, args }) = expected {
            if name == "Result" && !args.is_empty() {
                Some(args[0].clone())
            } else {
                None
            }
        } else {
            None
        };

        match target_ty {
            Some(target) => {
                // Verify TryFrom<ReceiverType> is implemented for TargetType
                if !self.type_implements_try_from(&target, &receiver_ty) {
                    self.tcx.errors.push(SemanticError {
                        message: format!(
                            "the trait `TryFrom<{}>` is not implemented for `{}`",
                            self.format_type(&receiver_ty),
                            self.format_type(&target)
                        ),
                        span: span.clone(),
                    });
                }
                // Record the resolved type for codegen
                self.tcx.type_resolution.insert(
                    (span.range.start, span.range.end),
                    self.format_type(&target),
                );
                Type::Named {
                    name: "Result".to_string(),
                    args: vec![target, Type::Primitive(PrimitiveType::String)],
                }
            }
            None => {
                self.tcx.errors.push(SemanticError {
                    message: "type annotations needed: cannot infer type for `.try_into()`\n\
                              help: consider using turbofish syntax: `.try_into::<TargetType>()`"
                        .to_string(),
                    span: span.clone(),
                });
                Type::Primitive(PrimitiveType::Unit)
            }
        }
    }

    /// Handle .collect() method call: iter.collect::<[T]>() -> [T]
    /// Infers collection type from context or turbofish syntax
    fn check_collect_method(
        &mut self,
        _receiver: &Expr,
        type_args: &[TypeExpr],
        expected: Option<&Type>,
        span: &husk_ast::Span,
        elem_ty: &Type,
    ) -> Type {
        // Resolve collection type from turbofish or expected type
        let collection_ty = if !type_args.is_empty() {
            // Turbofish: .collect::<[T]>()
            Some(self.tcx.resolve_type_expr(&type_args[0], &[]))
        } else if let Some(expected) = expected {
            // Infer from let binding or function argument
            // For arrays, we can infer directly
            if let Type::Array(_) = expected {
                Some(expected.clone())
            } else {
                // Try to infer array type from expected
                Some(expected.clone())
            }
        } else {
            None
        };

        match collection_ty {
            Some(collection) => {
                // Record the resolved type for codegen
                self.tcx.type_resolution.insert(
                    (span.range.start, span.range.end),
                    self.format_type(&collection),
                );
                collection
            }
            None => {
                // Default to array if no type annotation
                // This allows iter.collect() to work without explicit type
                let array_ty = Type::Array(Box::new(elem_ty.clone()));
                self.tcx.type_resolution.insert(
                    (span.range.start, span.range.end),
                    self.format_type(&array_ty),
                );
                array_ty
            }
        }
    }

    /// Infer return type for iterator methods using StdlibIndex strategy.
    ///
    /// This uses the InferenceStrategy from the stdlib index to determine
    /// how to type-check closure parameters and infer return types.
    fn infer_iterator_method(&mut self, method: IteratorMethodArgs<'_>) -> Option<Type> {
        let IteratorMethodArgs {
            elem_ty,
            receiver_ty,
            method_name,
            args,
            receiver,
            type_args,
            span,
        } = method;
        let index = get_stdlib_index();
        let strategy = index.get_inference_strategy("Iterator", method_name);

        // Check if this is a known iterator method
        if !index.has_method("Iterator", method_name) {
            return None;
        }

        match strategy {
            InferenceStrategy::MapLike => {
                // Closure type: Fn(T) -> U, infer return type from closure
                if let Some(closure_arg) = args.first() {
                    let expected_closure = Type::Function {
                        params: vec![elem_ty.clone()],
                        ret: Box::new(Type::Primitive(PrimitiveType::Unit)), // placeholder
                    };
                    let closure_ty =
                        self.check_expr_with_expected(closure_arg, Some(&expected_closure));
                    if let Type::Function { ret, .. } = closure_ty {
                        return Some(Type::ImplTrait {
                            trait_ty: Box::new(Type::Named {
                                name: "Iterator".to_string(),
                                args: vec![*ret],
                            }),
                        });
                    }
                }
                Some(receiver_ty.clone())
            }
            InferenceStrategy::FilterLike => {
                // Closure type: Fn(&T) -> bool, returns same iterator type
                if let Some(closure_arg) = args.first() {
                    let expected_closure = Type::Function {
                        params: vec![elem_ty.clone()],
                        ret: Box::new(Type::Primitive(PrimitiveType::Bool)),
                    };
                    let _ = self.check_expr_with_expected(closure_arg, Some(&expected_closure));
                }
                Some(receiver_ty.clone())
            }
            InferenceStrategy::FindLike => {
                // Closure type: Fn(&T) -> bool, returns Option<T>
                if let Some(closure_arg) = args.first() {
                    let expected_closure = Type::Function {
                        params: vec![elem_ty.clone()],
                        ret: Box::new(Type::Primitive(PrimitiveType::Bool)),
                    };
                    let _ = self.check_expr_with_expected(closure_arg, Some(&expected_closure));
                }
                Some(Type::Named {
                    name: "Option".to_string(),
                    args: vec![elem_ty.clone()],
                })
            }
            InferenceStrategy::ConsumerLike => {
                // Closure type: Fn(T) -> (), returns ()
                if let Some(closure_arg) = args.first() {
                    let expected_closure = Type::Function {
                        params: vec![elem_ty.clone()],
                        ret: Box::new(Type::Primitive(PrimitiveType::Unit)),
                    };
                    let _ = self.check_expr_with_expected(closure_arg, Some(&expected_closure));
                }
                Some(Type::Primitive(PrimitiveType::Unit))
            }
            InferenceStrategy::FoldLike => {
                // Closure type: Fn(B, T) -> B
                if args.len() >= 2 {
                    let init_ty = self.check_expr(&args[0]);
                    if let Some(closure_arg) = args.get(1) {
                        let expected_closure = Type::Function {
                            params: vec![init_ty.clone(), elem_ty.clone()],
                            ret: Box::new(init_ty.clone()),
                        };
                        let _ = self.check_expr_with_expected(closure_arg, Some(&expected_closure));
                    }
                    return Some(init_ty);
                }
                None
            }
            InferenceStrategy::PredicateLike => {
                // Closure type: Fn(&T) -> bool, returns bool
                if let Some(closure_arg) = args.first() {
                    let expected_closure = Type::Function {
                        params: vec![elem_ty.clone()],
                        ret: Box::new(Type::Primitive(PrimitiveType::Bool)),
                    };
                    let _ = self.check_expr_with_expected(closure_arg, Some(&expected_closure));
                }
                Some(Type::Primitive(PrimitiveType::Bool))
            }
            InferenceStrategy::CountLike => Some(Type::Primitive(PrimitiveType::I32)),
            InferenceStrategy::CollectLike => {
                // Special handling via check_collect_method
                Some(self.check_collect_method(receiver, type_args, None, span, elem_ty))
            }
            InferenceStrategy::FilterMapLike => {
                // Closure type: Fn(T) -> Option<T>, returns impl Iterator<T>
                if let Some(closure_arg) = args.first() {
                    let expected_closure = Type::Function {
                        params: vec![elem_ty.clone()],
                        ret: Box::new(Type::Named {
                            name: "Option".to_string(),
                            args: vec![elem_ty.clone()],
                        }),
                    };
                    let _ = self.check_expr_with_expected(closure_arg, Some(&expected_closure));
                }
                Some(receiver_ty.clone())
            }
            InferenceStrategy::PassThrough => {
                // Methods like take, skip, enumerate, zip, chain
                // Need special handling for some
                match method_name {
                    "enumerate" => Some(Type::ImplTrait {
                        trait_ty: Box::new(Type::Named {
                            name: "Iterator".to_string(),
                            args: vec![Type::Tuple(vec![
                                Type::Primitive(PrimitiveType::I32),
                                elem_ty.clone(),
                            ])],
                        }),
                    }),
                    "take" | "skip" => {
                        if args.len() == 1 {
                            let count_ty = self.check_expr(&args[0]);
                            let is_valid_count = matches!(
                                count_ty,
                                Type::Primitive(PrimitiveType::I32)
                            ) || matches!(&count_ty, Type::Named { name, .. } if name == "number");
                            if !is_valid_count {
                                self.tcx.errors.push(SemanticError {
                                    message: format!(
                                        "take/skip count must be an integer, found `{}`",
                                        self.format_type(&count_ty)
                                    ),
                                    span: args[0].span.clone(),
                                });
                            }
                        }
                        Some(receiver_ty.clone())
                    }
                    "zip" => {
                        // Takes another iterator, returns impl Iterator<(T, T)>
                        if let Some(other_iter) = args.first() {
                            let _ = self.check_expr(other_iter);
                        }
                        Some(Type::ImplTrait {
                            trait_ty: Box::new(Type::Named {
                                name: "Iterator".to_string(),
                                args: vec![Type::Tuple(vec![elem_ty.clone(), elem_ty.clone()])],
                            }),
                        })
                    }
                    "chain" => {
                        // Takes another iterator of same type, returns impl Iterator<T>
                        if let Some(other_iter) = args.first() {
                            let _ = self.check_expr(other_iter);
                        }
                        Some(receiver_ty.clone())
                    }
                    _ => {
                        // Generic pass-through - check args but return same type
                        for arg in args {
                            let _ = self.check_expr(arg);
                        }
                        Some(receiver_ty.clone())
                    }
                }
            }
            InferenceStrategy::Standard => {
                // Not a special iterator method, let standard resolution handle it
                None
            }
        }
    }

    /// Check if TargetType implements From<SourceType>
    fn type_implements_from(&self, target: &Type, source: &Type) -> bool {
        let target_name = self.format_type(target);
        let source_name = self.format_type(source);

        for impl_info in &self.tcx.env.impls {
            if impl_info.self_ty_name == target_name
                && let Some(trait_name) = &impl_info.trait_name
            {
                // Check if this is impl From<source> for target
                let expected_trait = format!("From<{}>", source_name);
                if trait_name == &expected_trait {
                    return true;
                }
                // Also check for generic From<T> implementations
                if let Some(inner) = extract_trait_type_arg(trait_name, "From<")
                    && (is_generic_type_param(inner) || inner == source_name)
                {
                    return true;
                }
            }
        }
        false
    }

    /// Check if TargetType implements TryFrom<SourceType>
    fn type_implements_try_from(&self, target: &Type, source: &Type) -> bool {
        let target_name = self.format_type(target);
        let source_name = self.format_type(source);

        for impl_info in &self.tcx.env.impls {
            if impl_info.self_ty_name == target_name
                && let Some(trait_name) = &impl_info.trait_name
            {
                let expected_trait = format!("TryFrom<{}>", source_name);
                if trait_name == &expected_trait {
                    return true;
                }
                // Check for generic TryFrom<T> implementations
                if let Some(inner) = extract_trait_type_arg(trait_name, "TryFrom<")
                    && (is_generic_type_param(inner) || inner == source_name)
                {
                    return true;
                }
            }
        }
        false
    }

    /// Check a closure expression and return its function type.
    /// If `expected` is provided and is a function type, use it to infer parameter types.
    fn check_closure_expr(
        &mut self,
        _expr: &Expr,
        params: &[ClosureParam],
        ret_type: Option<&TypeExpr>,
        body: &Expr,
        expected: Option<&Type>,
    ) -> Type {
        // Extract expected parameter types if available
        let expected_params = match expected {
            Some(Type::Function { params, .. }) => Some(params.as_slice()),
            _ => None,
        };

        // Extract expected return type if available
        let expected_ret = match expected {
            Some(Type::Function { ret, .. }) => Some(ret.as_ref()),
            _ => None,
        };

        // Resolve parameter types
        let mut param_types = Vec::new();
        let mut closure_locals = self.locals.clone();
        let mut closure_shadow_counts = self.shadow_counts.clone();
        let mut closure_resolved_names = self.resolved_names.clone();

        for (i, param) in params.iter().enumerate() {
            let ty = if let Some(type_expr) = &param.ty {
                // Explicit annotation provided - use it
                let annotated = self.tcx.resolve_type_expr(type_expr, &[]);

                // Validate against expected type if available
                if let Some(expected_params) = expected_params
                    && let Some(expected_ty) = expected_params.get(i)
                    && !self.types_compatible(expected_ty, &annotated)
                {
                    self.tcx.errors.push(SemanticError {
                        message: format!(
                            "closure parameter type `{:?}` does not match expected `{:?}`",
                            annotated, expected_ty
                        ),
                        span: param.name.span.clone(),
                    });
                }
                annotated
            } else if let Some(expected_params) = expected_params {
                // No annotation - infer from expected type
                if let Some(expected_ty) = expected_params.get(i) {
                    expected_ty.clone()
                } else {
                    // More params than expected - error
                    self.tcx.errors.push(SemanticError {
                        message: format!(
                            "closure has more parameters than expected (expected {}, got {})",
                            expected_params.len(),
                            params.len()
                        ),
                        span: param.name.span.clone(),
                    });
                    Type::Primitive(PrimitiveType::Unit)
                }
            } else {
                // No context available - require annotation
                self.tcx.errors.push(SemanticError {
                    message: format!(
                        "cannot infer type for closure parameter `{}`. \
                         Add type annotation: `|{}: Type|`",
                        param.name.name, param.name.name
                    ),
                    span: param.name.span.clone(),
                });
                Type::Primitive(PrimitiveType::Unit)
            };

            param_types.push(ty.clone());
            closure_locals.insert(param.name.name.clone(), ty);

            // Register closure parameter in name resolution (may shadow outer variables)
            let count = closure_shadow_counts
                .entry(param.name.name.clone())
                .or_insert(0);
            let resolved = if *count == 0 {
                param.name.name.clone()
            } else {
                format!("{}${}", param.name.name, count)
            };
            *count += 1;
            closure_resolved_names.insert(param.name.name.clone(), resolved.clone());
            self.tcx.name_resolution.insert(
                (param.name.span.range.start, param.name.span.range.end),
                resolved,
            );
        }

        // Check arity mismatch (fewer params than expected)
        if let Some(expected_params) = expected_params
            && params.len() < expected_params.len()
        {
            self.tcx.errors.push(SemanticError {
                message: format!(
                    "closure has fewer parameters than expected (expected {}, got {})",
                    expected_params.len(),
                    params.len()
                ),
                span: _expr.span.clone(),
            });
        }

        // Resolve return type if specified
        let expected_ret_ty = if let Some(ret_expr) = ret_type {
            self.tcx.resolve_type_expr(ret_expr, &[])
        } else if let Some(expected_ret) = expected_ret {
            // Use expected return type for body checking
            expected_ret.clone()
        } else {
            // Will be inferred from body
            Type::Primitive(PrimitiveType::Unit)
        };

        // Create a nested context for the closure body
        let old_locals = std::mem::replace(&mut self.locals, closure_locals);
        let old_shadow_counts = std::mem::replace(&mut self.shadow_counts, closure_shadow_counts);
        let old_resolved_names =
            std::mem::replace(&mut self.resolved_names, closure_resolved_names);
        let old_ret_ty = std::mem::replace(&mut self.ret_ty, expected_ret_ty.clone());
        // Closures have their own return type context - set fn name to None for error messages
        let old_fn_name = self.enclosing_fn_name.take();

        // Check the body and infer return type
        let body_ty = self.check_expr(body);

        // Restore the outer context
        self.locals = old_locals;
        self.shadow_counts = old_shadow_counts;
        self.resolved_names = old_resolved_names;
        self.ret_ty = old_ret_ty;
        self.enclosing_fn_name = old_fn_name;

        // Use explicit return type if specified, otherwise infer from body
        let actual_ret_ty = if ret_type.is_some() {
            expected_ret_ty
        } else {
            body_ty
        };

        Type::Function {
            params: param_types,
            ret: Box::new(actual_ret_ty),
        }
    }

    fn format_type(&self, ty: &Type) -> String {
        // Delegate to the free function to avoid duplication
        format_type(ty)
    }

    /// Convert a Type to a string name for trait lookup.
    fn type_to_name(&self, ty: &Type) -> String {
        match ty {
            Type::Named { name, args } => {
                if args.is_empty() {
                    name.clone()
                } else {
                    let arg_strs: Vec<String> = args.iter().map(|a| self.type_to_name(a)).collect();
                    format!("{}<{}>", name, arg_strs.join(", "))
                }
            }
            Type::Primitive(prim) => match prim {
                PrimitiveType::I32 => "i32".to_string(),
                PrimitiveType::I64 => "i64".to_string(),
                PrimitiveType::F64 => "f64".to_string(),
                PrimitiveType::Bool => "bool".to_string(),
                PrimitiveType::String => "String".to_string(),
                PrimitiveType::Unit => "()".to_string(),
            },
            Type::Array(elem) => format!("[{}]", self.type_to_name(elem)),
            Type::Function { params, ret } => {
                let param_strs: Vec<String> = params.iter().map(|p| self.type_to_name(p)).collect();
                format!(
                    "fn({}) -> {}",
                    param_strs.join(", "),
                    self.type_to_name(ret)
                )
            }
            Type::Var(id) => format!("?{}", id.0),
            Type::Tuple(elements) => {
                let elem_strs: Vec<String> =
                    elements.iter().map(|e| self.type_to_name(e)).collect();
                format!("({})", elem_strs.join(", "))
            }
            Type::ImplTrait { trait_ty } => self.type_to_name(trait_ty),
        }
    }

    fn types_compatible(&self, expected: &Type, actual: &Type) -> bool {
        self.types_compatible_inner(expected, actual)
    }

    fn types_compatible_inner(&self, expected: &Type, actual: &Type) -> bool {
        // JsValue is compatible with any type (it's JavaScript's dynamic "any" type)
        // This allows passing primitives to functions expecting JsValue (e.g., assert_eq)
        if let Type::Named { name, args } = expected
            && name == "JsValue"
            && args.is_empty()
        {
            return true;
        }

        // Empty array [()] is compatible with any array type
        // This allows `[] == [1, 2, 3]` without explicit type annotation
        if let (Type::Array(expected_elem), Type::Array(actual_elem)) = (expected, actual) {
            if matches!(actual_elem.as_ref(), Type::Primitive(PrimitiveType::Unit)) {
                return true;
            }
            if matches!(expected_elem.as_ref(), Type::Primitive(PrimitiveType::Unit)) {
                return true;
            }
        }

        // Generic type parameters (like T, U) are compatible with any type
        // This enables type inference to work with explicit closure annotations
        if let Type::Named { name, args } = expected
            && args.is_empty()
        {
            // Check if this is a generic type parameter (single uppercase letter
            // or a name that's not a known type)
            let is_generic = name.len() == 1 && name.chars().next().unwrap().is_uppercase()
                || (!self.tcx.env.structs.contains_key(name)
                    && !self.tcx.env.enums.contains_key(name)
                    && !matches!(name.as_str(), "i32" | "bool" | "String" | "Unit"));
            if is_generic {
                return true;
            }
        }

        // Handle function types: compare structurally with generic-aware compatibility
        if let (
            Type::Function {
                params: expected_params,
                ret: expected_ret,
            },
            Type::Function {
                params: actual_params,
                ret: actual_ret,
            },
        ) = (expected, actual)
        {
            if expected_params.len() != actual_params.len() {
                return false;
            }
            // Check each parameter is compatible
            for (exp, act) in expected_params.iter().zip(actual_params.iter()) {
                if !self.types_compatible_inner(exp, act) {
                    return false;
                }
            }
            // Check return type is compatible
            return self.types_compatible_inner(expected_ret, actual_ret);
        }

        // Handle Named types: if both are the same enum/struct name, treat as compatible
        // even if type args differ (MVP approach for generic enums like Result/Option)
        if let (
            Type::Named {
                name: expected_name,
                ..
            },
            Type::Named {
                name: actual_name,
                args: actual_args,
            },
        ) = (expected, actual)
            && expected_name == actual_name
        {
            // If actual has no type args (from enum constructor), allow it
            if actual_args.is_empty() {
                return true;
            }
        }

        // Handle tuple types: compare element-wise
        if let (Type::Tuple(expected_elems), Type::Tuple(actual_elems)) = (expected, actual) {
            if expected_elems.len() != actual_elems.len() {
                return false;
            }
            for (exp, act) in expected_elems.iter().zip(actual_elems.iter()) {
                if !self.types_compatible_inner(exp, act) {
                    return false;
                }
            }
            return true;
        }

        expected == actual
    }

    /// Check if an expression is a valid assignment target (lvalue).
    fn is_lvalue(&self, expr: &Expr) -> bool {
        matches!(
            &expr.kind,
            ExprKind::Ident(_) | ExprKind::Field { .. } | ExprKind::Index { .. }
        )
    }

    /// Check if a type is numeric (supports arithmetic operations).
    fn is_numeric(&self, ty: &Type) -> bool {
        matches!(
            ty,
            Type::Primitive(PrimitiveType::I32)
                | Type::Primitive(PrimitiveType::I64)
                | Type::Primitive(PrimitiveType::F64)
        )
    }

    /// Unify a parameter type with a concrete argument type, collecting substitutions
    /// for generic type parameters. This enables type inference for generic functions.
    fn unify_types(
        &self,
        param_ty: &Type,
        arg_ty: &Type,
        type_params: &[String],
        substitutions: &mut std::collections::HashMap<String, Type>,
    ) {
        match param_ty {
            Type::Named { name, args } if args.is_empty() && type_params.contains(name) => {
                // This is a generic type parameter - record or check substitution
                if let Some(existing) = substitutions.get(name) {
                    // Already have a substitution - should be compatible
                    // For now, we just accept the first one
                    let _ = existing;
                } else {
                    substitutions.insert(name.clone(), arg_ty.clone());
                }
            }
            Type::Named { name, args } => {
                // Concrete named type - recursively unify type arguments
                if let Type::Named {
                    name: arg_name,
                    args: arg_args,
                } = arg_ty
                    && name == arg_name
                    && args.len() == arg_args.len()
                {
                    for (param_arg, arg_arg) in args.iter().zip(arg_args.iter()) {
                        self.unify_types(param_arg, arg_arg, type_params, substitutions);
                    }
                }
            }
            Type::Function { params, ret } => {
                // Function type - recursively unify params and return
                if let Type::Function {
                    params: arg_params,
                    ret: arg_ret,
                } = arg_ty
                    && params.len() == arg_params.len()
                {
                    for (p, a) in params.iter().zip(arg_params.iter()) {
                        self.unify_types(p, a, type_params, substitutions);
                    }
                    self.unify_types(ret, arg_ret, type_params, substitutions);
                }
            }
            _ => {
                // Primitive types don't contribute to substitutions
            }
        }
    }

    /// Apply collected substitutions to a type, replacing generic parameters
    /// with their inferred concrete types.
    fn apply_substitutions(
        &self,
        ty: &Type,
        substitutions: &std::collections::HashMap<String, Type>,
    ) -> Type {
        match ty {
            Type::Named { name, args } if args.is_empty() => {
                // Could be a generic parameter
                if let Some(concrete) = substitutions.get(name) {
                    concrete.clone()
                } else {
                    ty.clone()
                }
            }
            Type::Named { name, args } => {
                // Apply substitutions to type arguments
                Type::Named {
                    name: name.clone(),
                    args: args
                        .iter()
                        .map(|a| self.apply_substitutions(a, substitutions))
                        .collect(),
                }
            }
            Type::Function { params, ret } => Type::Function {
                params: params
                    .iter()
                    .map(|p| self.apply_substitutions(p, substitutions))
                    .collect(),
                ret: Box::new(self.apply_substitutions(ret, substitutions)),
            },
            _ => ty.clone(),
        }
    }

    // =========================================================================
    // Helper functions for if-let and let-else support
    // =========================================================================

    /// Check if a pattern is refutable (can fail to match).
    /// Used to detect refutable patterns in let without else.
    fn pattern_is_refutable(&self, pattern: &Pattern) -> bool {
        match &pattern.kind {
            PatternKind::EnumUnit { .. }
            | PatternKind::EnumTuple { .. }
            | PatternKind::EnumStruct { .. } => true,
            PatternKind::Binding(ident) => {
                // Imported variant names (e.g., `None`, `Err`) are refutable
                self.tcx.env.variant_imports.contains_key(&ident.name)
            }
            PatternKind::Wildcard => false,
            PatternKind::Tuple { fields } => {
                // Tuple is refutable if any nested pattern is refutable
                // e.g., (Some(x), y) contains refutable Some(x)
                fields.iter().any(|f| self.pattern_is_refutable(f))
            }
        }
    }

    /// Check that pattern is refutable (can fail to match).
    /// Emits warning for irrefutable patterns used in if-let or let-else.
    fn validate_refutable_pattern(&mut self, pattern: &Pattern, scrut_ty: &Type, span: &Span) {
        match &pattern.kind {
            PatternKind::EnumUnit { path }
            | PatternKind::EnumTuple { path, .. }
            | PatternKind::EnumStruct { path, .. } => {
                // Verify the pattern's enum matches the scrutinee's type
                if let Type::Named { name, .. } = scrut_ty {
                    // For single-segment paths (e.g., `Some`), check if it's an imported variant
                    // from the scrutinee's enum type. This allows `if let Some(x) = opt` where
                    // `Some` is imported from Option.
                    if path.len() == 1 {
                        let variant_name = &path[0].name;
                        // Check if this is a valid imported variant for the scrutinee type
                        if let Some((imported_enum, _)) =
                            self.tcx.env.variant_imports.get(variant_name)
                        {
                            if imported_enum != name {
                                self.tcx.errors.push(SemanticError {
                                    message: format!(
                                        "pattern `{}` does not match type `{}` (variant is from `{}`)",
                                        variant_name, name, imported_enum
                                    ),
                                    span: span.clone(),
                                });
                            }
                        } else {
                            // Single-segment path not in variant_imports - error like match does
                            self.tcx.errors.push(SemanticError {
                                message: format!(
                                    "unknown variant `{}` (did you mean `{}::{}`?)",
                                    variant_name, name, variant_name
                                ),
                                span: span.clone(),
                            });
                        }
                    } else {
                        // Multi-segment path like Option::Some - check enum name matches
                        let path_enum = path.first().map(|id| &id.name);
                        if path_enum != Some(name) {
                            self.tcx.errors.push(SemanticError {
                                message: format!(
                                    "pattern `{}` does not match type `{}`",
                                    path.iter()
                                        .map(|id| id.name.as_str())
                                        .collect::<Vec<_>>()
                                        .join("::"),
                                    name
                                ),
                                span: span.clone(),
                            });
                        } else if path.len() == 2 {
                            // Enum name matches, now verify it's actually an enum and variant exists
                            let variant_name = &path[1].name;
                            if let Some(enum_def) = self.tcx.env.enums.get(name) {
                                let variant_exists =
                                    enum_def.variants.iter().any(|v| &v.name == variant_name);
                                if !variant_exists {
                                    self.tcx.errors.push(SemanticError {
                                        message: format!(
                                            "unknown variant `{}` for enum `{}`",
                                            variant_name, name
                                        ),
                                        span: span.clone(),
                                    });
                                }
                            } else {
                                // Type exists but is not an enum (e.g., a struct)
                                self.tcx.errors.push(SemanticError {
                                    message: format!(
                                        "cannot use enum pattern on non-enum type `{}`",
                                        name
                                    ),
                                    span: span.clone(),
                                });
                            }
                        }
                    }
                }
                // Enum patterns are refutable - OK

                // Check for nested patterns in struct variants (not supported)
                if let PatternKind::EnumStruct { fields, .. } = &pattern.kind {
                    self.check_struct_pattern_depth(fields, span);
                }
            }
            PatternKind::Binding(ident) => {
                if let Some((imported_enum, _)) = self.tcx.env.variant_imports.get(&ident.name) {
                    // Validate that scrutinee type matches the imported variant's enum
                    match scrut_ty {
                        Type::Named { name, .. } => {
                            if name != imported_enum {
                                self.tcx.errors.push(SemanticError {
                                    message: format!(
                                        "pattern `{}` does not match type `{}` (variant is from `{}`)",
                                        ident.name, name, imported_enum
                                    ),
                                    span: span.clone(),
                                });
                            }
                        }
                        Type::Var(_) => {
                            // Allow generic scrutinee; will be resolved later
                        }
                        _ => {
                            self.tcx.errors.push(SemanticError {
                                message: format!(
                                    "pattern `{}` can only be used with enum `{}`",
                                    ident.name, imported_enum
                                ),
                                span: span.clone(),
                            });
                        }
                    }
                } else {
                    // Plain binding: irrefutable in if-let / let-else
                    self.tcx.errors.push(SemanticError {
                        message: format!("irrefutable pattern `{}`: will always match", ident.name),
                        span: span.clone(),
                    });
                }
            }
            PatternKind::Wildcard => {
                self.tcx.errors.push(SemanticError {
                    message: "irrefutable pattern `_`: will always match".to_string(),
                    span: span.clone(),
                });
            }
            PatternKind::Tuple { fields } => {
                // Only error if the tuple pattern is entirely irrefutable
                // (i.e., no nested refutable patterns like Some(x))
                if !self.pattern_is_refutable(pattern) {
                    self.tcx.errors.push(SemanticError {
                        message: "irrefutable tuple pattern: will always match".to_string(),
                        span: span.clone(),
                    });
                } else {
                    // Recursively validate nested patterns
                    // Extract element types from scrutinee if it's a tuple type
                    let elem_types: Vec<Type> = if let Type::Tuple(types) = scrut_ty {
                        types.clone()
                    } else {
                        // If not a tuple type, use Unknown for each field
                        fields.iter().map(|_| Type::unit()).collect()
                    };
                    for (field, elem_ty) in fields.iter().zip(elem_types.iter()) {
                        if self.pattern_is_refutable(field) {
                            self.validate_refutable_pattern(field, elem_ty, &field.span);
                        }
                    }
                }
            }
        }
    }

    /// Check that struct pattern fields don't contain nested refutable patterns.
    /// We only support top-level field bindings in this initial implementation.
    fn check_struct_pattern_depth(&mut self, fields: &[(Ident, Pattern)], span: &Span) {
        for (_, sub_pattern) in fields {
            match &sub_pattern.kind {
                // Only simple bindings and wildcards are allowed
                PatternKind::Binding(_) | PatternKind::Wildcard => {}
                _ => {
                    self.tcx.errors.push(SemanticError {
                        message: "nested patterns in struct fields are not yet supported"
                            .to_string(),
                        span: span.clone(),
                    });
                    return; // One error is enough
                }
            }
        }
    }

    /// Record imported variant patterns in variant_patterns map for codegen.
    fn record_variant_pattern(&mut self, pattern: &Pattern, scrut_ty: &Type) {
        match &pattern.kind {
            PatternKind::Binding(ident) => {
                // Unit variant imported as bare name (e.g., `None`)
                if let Some((imported_enum, _)) = self.tcx.env.variant_imports.get(&ident.name) {
                    let should_record = match scrut_ty {
                        Type::Named {
                            name: enum_name, ..
                        } => imported_enum == enum_name,
                        Type::Var(_) => true,
                        _ => false,
                    };
                    if should_record {
                        self.tcx.variant_patterns.insert(
                            (pattern.span.range.start, pattern.span.range.end),
                            (imported_enum.clone(), ident.name.clone()),
                        );
                        // Track variant reference for rename support
                        self.tcx.add_reference(
                            &format!("{}::{}", imported_enum, ident.name),
                            ReferenceKind::Variant,
                            ident.span.clone(),
                        );
                    }
                }
            }
            PatternKind::EnumUnit { path }
            | PatternKind::EnumTuple { path, .. }
            | PatternKind::EnumStruct { path, .. } => {
                // Single-segment imported variant (e.g., `Some(x)`)
                if path.len() == 1 {
                    let variant_name = &path[0].name;
                    if let Some((imported_enum, _)) = self.tcx.env.variant_imports.get(variant_name)
                    {
                        let should_record = match scrut_ty {
                            Type::Named {
                                name: enum_name, ..
                            } => imported_enum == enum_name,
                            Type::Var(_) => true,
                            _ => false,
                        };
                        if should_record {
                            self.tcx.variant_patterns.insert(
                                (pattern.span.range.start, pattern.span.range.end),
                                (imported_enum.clone(), variant_name.clone()),
                            );
                            // Track variant reference for rename support
                            self.tcx.add_reference(
                                &format!("{}::{}", imported_enum, variant_name),
                                ReferenceKind::Variant,
                                path[0].span.clone(),
                            );
                        }
                    }
                } else if path.len() >= 2 {
                    // Qualified variant (e.g., `MyEnum::Variant`)
                    let enum_name = &path[0].name;
                    let variant_name = &path[path.len() - 1].name;
                    // Track both enum and variant references
                    self.tcx
                        .add_reference(enum_name, ReferenceKind::Enum, path[0].span.clone());
                    self.tcx.add_reference(
                        &format!("{}::{}", enum_name, variant_name),
                        ReferenceKind::Variant,
                        path[path.len() - 1].span.clone(),
                    );
                }
            }
            PatternKind::Tuple { fields } => {
                // Recursively record nested patterns
                if let Type::Tuple(elem_types) = scrut_ty {
                    for (field, elem_ty) in fields.iter().zip(elem_types.iter()) {
                        self.record_variant_pattern(field, elem_ty);
                    }
                }
            }
            PatternKind::Wildcard => {}
        }
    }

    /// Check if a block diverges (definitely cannot complete normally).
    fn block_diverges(&self, block: &Block) -> bool {
        // Check if any statement in the block unconditionally diverges
        for stmt in &block.stmts {
            if self.stmt_diverges(stmt) {
                return true; // Found unconditional divergence - rest is unreachable
            }
        }
        false // No unconditional divergence, block may complete normally
    }

    /// Check if a statement diverges.
    fn stmt_diverges(&self, stmt: &Stmt) -> bool {
        match &stmt.kind {
            // Unconditionally divergent
            StmtKind::Return { .. } => true,
            StmtKind::Break => self.in_loop,
            StmtKind::Continue => self.in_loop,

            // Nested block
            StmtKind::Block(block) => self.block_diverges(block),

            // Loop: diverges unless there's a reachable break
            StmtKind::Loop { body, .. } => !self.loop_may_complete(body),

            // If: ALL branches must diverge for the if to diverge
            StmtKind::If {
                then_branch,
                else_branch,
                ..
            } => {
                self.block_diverges(then_branch)
                    && else_branch.as_ref().is_some_and(|e| self.stmt_diverges(e))
            }

            // IfLet: same as if - both branches must diverge
            StmtKind::IfLet {
                then_branch,
                else_branch,
                ..
            } => {
                self.block_diverges(then_branch)
                    && else_branch.as_ref().is_some_and(|e| self.stmt_diverges(e))
            }

            // Expression statements: check for divergent expressions (panic!, etc.)
            StmtKind::Expr(expr) | StmtKind::Semi(expr) => self.expr_diverges(expr),

            _ => false,
        }
    }

    /// Check if a loop may complete (has a reachable break on SOME path).
    fn loop_may_complete(&self, block: &Block) -> bool {
        self.block_has_reachable_break(block)
    }

    /// Check if a block has a reachable break statement.
    fn block_has_reachable_break(&self, block: &Block) -> bool {
        for stmt in &block.stmts {
            match &stmt.kind {
                StmtKind::Break => return true, // Found a reachable break

                StmtKind::Block(b) => {
                    if self.block_has_reachable_break(b) {
                        return true;
                    }
                    // If block diverges, subsequent statements are unreachable
                    if self.block_diverges(b) {
                        return false;
                    }
                }

                StmtKind::If {
                    then_branch,
                    else_branch,
                    ..
                } => {
                    // Check both branches for breaks
                    let then_has_break = self.block_has_reachable_break(then_branch);
                    let else_has_break = else_branch
                        .as_ref()
                        .is_some_and(|e| self.stmt_has_reachable_break(e));

                    if then_has_break || else_has_break {
                        return true;
                    }

                    // If BOTH branches diverge, subsequent statements are unreachable
                    let then_diverges = self.block_diverges(then_branch);
                    let else_diverges = else_branch.as_ref().is_some_and(|e| self.stmt_diverges(e));
                    if then_diverges && else_diverges {
                        return false;
                    }
                }

                StmtKind::IfLet {
                    then_branch,
                    else_branch,
                    ..
                } => {
                    let then_has_break = self.block_has_reachable_break(then_branch);
                    let else_has_break = else_branch
                        .as_ref()
                        .is_some_and(|e| self.stmt_has_reachable_break(e));

                    if then_has_break || else_has_break {
                        return true;
                    }

                    let then_diverges = self.block_diverges(then_branch);
                    let else_diverges = else_branch.as_ref().is_some_and(|e| self.stmt_diverges(e));
                    if then_diverges && else_diverges {
                        return false;
                    }
                }

                // Don't recurse into nested loops - their breaks don't affect outer loop
                StmtKind::Loop { .. } => {}

                // Check for ANY divergent statement, not just return/continue
                _ => {
                    if self.stmt_diverges(stmt) {
                        return false; // Subsequent statements are unreachable
                    }
                }
            }
        }
        false
    }

    fn stmt_has_reachable_break(&self, stmt: &Stmt) -> bool {
        match &stmt.kind {
            StmtKind::Break => true,
            StmtKind::Block(b) => self.block_has_reachable_break(b),
            _ => false,
        }
    }

    /// Check if an expression diverges (e.g., panic!, todo!, unreachable!)
    // TODO: Currently only recognizes built-in diverging functions by name.
    // Future improvements could include:
    // - Tracking user-defined functions with `-> !` return type
    // - Recognizing std::process::exit and similar stdlib functions
    fn expr_diverges(&self, expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Call { callee, .. } => {
                if let ExprKind::Path { segments } = &callee.kind {
                    let name = segments.last().map(|id| id.name.as_str()).unwrap_or("");
                    matches!(name, "panic" | "todo" | "unreachable" | "unimplemented")
                } else if let ExprKind::Ident(id) = &callee.kind {
                    matches!(
                        id.name.as_str(),
                        "panic" | "todo" | "unreachable" | "unimplemented"
                    )
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}

impl Resolver {
    fn new() -> Self {
        Self {
            symbols: Vec::new(),
            by_name: HashMap::new(),
            errors: Vec::new(),
        }
    }

    fn finish(self) -> ModuleSymbols {
        ModuleSymbols {
            symbols: self.symbols,
            by_name: self.by_name,
            errors: self.errors,
        }
    }

    fn collect(&mut self, file: &File) {
        for item in &file.items {
            self.collect_item(item);
        }
    }

    fn collect_item(&mut self, item: &Item) {
        match &item.kind {
            ItemKind::Fn { name, .. } => self.add_symbol(name, SymbolKind::Function),
            ItemKind::Struct { name, .. } => self.add_symbol(name, SymbolKind::Struct),
            ItemKind::Enum { name, .. } => self.add_symbol(name, SymbolKind::Enum),
            ItemKind::TypeAlias { name, .. } => self.add_symbol(name, SymbolKind::TypeAlias),
            ItemKind::ExternBlock { items, .. } => {
                for ext in items {
                    match &ext.kind {
                        husk_ast::ExternItemKind::Fn { name, .. } => {
                            self.add_symbol(name, SymbolKind::ExternFn);
                        }
                        husk_ast::ExternItemKind::Mod { binding, items, .. } => {
                            if items.is_empty() {
                                self.add_symbol(binding, SymbolKind::ExternMod);
                            } else {
                                // Register functions from mod block
                                for mod_item in items {
                                    // ModItemKind has only Fn variant (MVP scope)
                                    let husk_ast::ModItemKind::Fn { name, .. } = &mod_item.kind;
                                    self.add_symbol(name, SymbolKind::ExternFn);
                                }
                            }
                        }
                        husk_ast::ExternItemKind::Struct { name, .. } => {
                            self.add_symbol(name, SymbolKind::Struct);
                        }
                        husk_ast::ExternItemKind::Static { name, .. } => {
                            self.add_symbol(name, SymbolKind::ExternStatic);
                        }
                        husk_ast::ExternItemKind::Const { name, .. } => {
                            self.add_symbol(name, SymbolKind::ExternStatic);
                        }
                        husk_ast::ExternItemKind::Impl { self_ty, .. } => {
                            // Impl blocks inside extern don't define a named symbol,
                            // but we track them with a synthetic name
                            let self_ty_name = type_expr_to_name(self_ty);
                            let impl_name = format!("<extern impl {}>", self_ty_name);
                            let synth_ident = Ident {
                                name: impl_name,
                                span: ext.span.clone(),
                            };
                            self.add_symbol(&synth_ident, SymbolKind::Impl);
                        }
                    }
                }
            }
            ItemKind::Use { .. } => {}
            ItemKind::Trait(trait_def) => {
                self.add_symbol(&trait_def.name, SymbolKind::Trait);
            }
            ItemKind::Impl(impl_block) => {
                // Impl blocks don't define a named symbol, but we track them
                // We can use a synthetic name for debugging/tracking
                let self_ty_name = type_expr_to_name(&impl_block.self_ty);
                let impl_name = if let Some(trait_ref) = &impl_block.trait_ref {
                    let trait_name = type_expr_to_name(trait_ref);
                    format!("<impl {} for {}>", trait_name, self_ty_name)
                } else {
                    format!("<impl {}>", self_ty_name)
                };
                // Create a synthetic ident for the impl
                let synth_ident = Ident {
                    name: impl_name,
                    span: impl_block.span.clone(),
                };
                self.add_symbol(&synth_ident, SymbolKind::Impl);
            }
        }
    }

    fn add_symbol(&mut self, ident: &Ident, kind: SymbolKind) {
        let name = ident.name.clone();
        if let Some(existing_id) = self.by_name.get(&name).copied() {
            // Duplicate symbol; record an error but keep the first definition.
            if let Some(existing) = self.symbols.get(existing_id.0 as usize) {
                self.errors.push(SemanticError {
                    message: format!("duplicate definition of `{}`", name),
                    span: ident.span.clone(),
                });
                // Optionally attach a note in the future pointing to `existing.span`.
                let _ = existing;
            }
            return;
        }

        let id = SymbolId(self.symbols.len() as u32);
        let symbol = Symbol {
            id,
            name: name.clone(),
            kind,
            span: ident.span.clone(),
        };
        self.symbols.push(symbol);
        self.by_name.insert(name, id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use husk_ast::{
        EnumVariant, EnumVariantFields, Expr, ExprKind, File, Ident, Item, ItemKind, Literal,
        LiteralKind, MatchArm, Pattern, PatternKind, Span, Stmt, StmtKind, TypeExpr, TypeExprKind,
    };
    use husk_parser::parse_str;

    fn ident(name: &str, start: usize) -> Ident {
        Ident {
            name: name.to_string(),
            span: Span {
                range: start..start + name.len(),
                file: None,
            },
        }
    }

    fn ident_pattern(name: &str, start: usize) -> Pattern {
        let id = ident(name, start);
        Pattern {
            kind: PatternKind::Binding(id.clone()),
            span: id.span,
        }
    }

    #[test]
    fn collects_unique_top_level_symbols() {
        let f = File {
            items: vec![
                Item {
                    attributes: Vec::new(),
                    visibility: husk_ast::Visibility::Private,
                    kind: ItemKind::Fn {
                        name: ident("foo", 0),
                        type_params: Vec::new(),
                        params: Vec::new(),
                        ret_type: None,
                        body: Vec::new(),
                    },
                    span: Span {
                        range: 0..3,
                        file: None,
                    },
                },
                Item {
                    attributes: Vec::new(),
                    visibility: husk_ast::Visibility::Private,
                    kind: ItemKind::Struct {
                        name: ident("Bar", 10),
                        type_params: Vec::new(),
                        fields: Vec::new(),
                    },
                    span: Span {
                        range: 10..13,
                        file: None,
                    },
                },
            ],
        };

        let module = ModuleSymbols::from_file(&f);
        assert!(module.errors.is_empty());
        assert!(module.get("foo").is_some());
        assert!(module.get("Bar").is_some());
        assert_eq!(module.symbols.len(), 2);
    }

    #[test]
    fn reports_duplicate_definitions() {
        let f = File {
            items: vec![
                Item {
                    attributes: Vec::new(),
                    visibility: husk_ast::Visibility::Private,
                    kind: ItemKind::Fn {
                        name: ident("foo", 0),
                        type_params: Vec::new(),
                        params: Vec::new(),
                        ret_type: None,
                        body: Vec::new(),
                    },
                    span: Span {
                        range: 0..3,
                        file: None,
                    },
                },
                Item {
                    attributes: Vec::new(),
                    visibility: husk_ast::Visibility::Private,
                    kind: ItemKind::Struct {
                        name: ident("foo", 10),
                        type_params: Vec::new(),
                        fields: Vec::new(),
                    },
                    span: Span {
                        range: 10..13,
                        file: None,
                    },
                },
            ],
        };

        let module = ModuleSymbols::from_file(&f);
        assert_eq!(module.symbols.len(), 1);
        assert_eq!(module.errors.len(), 1);
        assert!(module.get("foo").is_some());
    }

    fn type_ident(name: &str, start: usize) -> TypeExpr {
        let id = ident(name, start);
        TypeExpr {
            kind: TypeExprKind::Named(id.clone()),
            span: id.span,
        }
    }

    #[test]
    fn analyze_well_typed_function_with_primitives() {
        // fn test_fn() -> i32 {
        //     let x: i32 = 1;
        //     x
        // }
        let x_ident = ident("x", 20);
        let one_lit = Expr {
            kind: ExprKind::Literal(Literal {
                kind: LiteralKind::Int(1),
                span: Span {
                    range: 30..31,
                    file: None,
                },
            }),
            span: Span {
                range: 30..31,
                file: None,
            },
        };
        let let_stmt = Stmt {
            kind: StmtKind::Let {
                mutable: false,
                pattern: Pattern {
                    kind: PatternKind::Binding(x_ident.clone()),
                    span: x_ident.span.clone(),
                },
                ty: Some(type_ident("i32", 25)),
                value: Some(one_lit),
                else_block: None,
            },
            span: Span {
                range: 20..32,
                file: None,
            },
        };
        let ret_expr = Expr {
            kind: ExprKind::Ident(x_ident.clone()),
            span: x_ident.span.clone(),
        };
        let ret_stmt = Stmt {
            kind: StmtKind::Return {
                value: Some(ret_expr),
            },
            span: Span {
                range: 40..45,
                file: None,
            },
        };

        let file = File {
            items: vec![Item {
                attributes: Vec::new(),
                visibility: husk_ast::Visibility::Private,
                kind: ItemKind::Fn {
                    name: ident("test_fn", 0),
                    type_params: Vec::new(),
                    params: Vec::new(),
                    ret_type: Some(type_ident("i32", 10)),
                    body: vec![let_stmt, ret_stmt],
                },
                span: Span {
                    range: 0..50,
                    file: None,
                },
            }],
        };

        let result = analyze_file(&file);
        assert!(
            result.symbols.errors.is_empty(),
            "name errors: {:?}",
            result.symbols.errors
        );
        assert!(
            result.type_errors.is_empty(),
            "type errors: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn analyze_reports_mismatched_let_type() {
        // fn main() {
        //   let x: i32 = "oops";
        // }
        let x_ident = ident("x", 15);
        let string_lit = Expr {
            kind: ExprKind::Literal(Literal {
                kind: LiteralKind::String("oops".to_string()),
                span: Span {
                    range: 25..31,
                    file: None,
                },
            }),
            span: Span {
                range: 25..31,
                file: None,
            },
        };
        let let_stmt = Stmt {
            kind: StmtKind::Let {
                mutable: false,
                pattern: Pattern {
                    kind: PatternKind::Binding(x_ident.clone()),
                    span: x_ident.span.clone(),
                },
                ty: Some(type_ident("i32", 20)),
                value: Some(string_lit),
                else_block: None,
            },
            span: Span {
                range: 15..32,
                file: None,
            },
        };
        let file = File {
            items: vec![Item {
                attributes: Vec::new(),
                visibility: husk_ast::Visibility::Private,
                kind: ItemKind::Fn {
                    name: ident("main", 0),
                    type_params: Vec::new(),
                    params: Vec::new(),
                    ret_type: None,
                    body: vec![let_stmt],
                },
                span: Span {
                    range: 0..40,
                    file: None,
                },
            }],
        };

        let result = analyze_file(&file);
        assert!(
            result
                .type_errors
                .iter()
                .any(|e| e.message.contains("mismatched types in `let`")),
            "expected mismatched let type error, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn path_expr_for_enum_variant_has_enum_type() {
        // enum Color { Red, Blue }
        // fn make_red() -> Color { Color::Red }
        let color_ident = ident("Color", 0);
        let enum_item = Item {
            attributes: Vec::new(),
            visibility: husk_ast::Visibility::Private,
            kind: ItemKind::Enum {
                name: color_ident.clone(),
                type_params: Vec::new(),
                variants: vec![
                    EnumVariant {
                        name: ident("Red", 10),
                        fields: EnumVariantFields::Unit,
                    },
                    EnumVariant {
                        name: ident("Blue", 20),
                        fields: EnumVariantFields::Unit,
                    },
                ],
            },
            span: Span {
                range: 0..30,
                file: None,
            },
        };

        let path_expr = Expr {
            kind: ExprKind::Path {
                segments: vec![color_ident.clone(), ident("Red", 40)],
            },
            span: Span {
                range: 30..45,
                file: None,
            },
        };
        let ret_stmt = Stmt {
            kind: StmtKind::Return {
                value: Some(path_expr),
            },
            span: Span {
                range: 30..50,
                file: None,
            },
        };
        let fn_item = Item {
            attributes: Vec::new(),
            visibility: husk_ast::Visibility::Private,
            kind: ItemKind::Fn {
                name: ident("make_red", 30),
                type_params: Vec::new(),
                params: Vec::new(),
                ret_type: Some(type_ident("Color", 35)),
                body: vec![ret_stmt],
            },
            span: Span {
                range: 30..60,
                file: None,
            },
        };

        let file = File {
            items: vec![enum_item, fn_item],
        };

        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors, got {:?}",
            result.type_errors
        );
    }

    #[test]
    fn match_on_enum_exhaustive_is_ok() {
        // enum Color { Red, Blue }
        // fn f(c: Color) -> i32 {
        //     return match c {
        //         Color::Red => 1,
        //         Color::Blue => 2,
        //     };
        // }
        let color_ident = ident("Color", 0);
        let enum_item = Item {
            attributes: Vec::new(),
            visibility: husk_ast::Visibility::Private,
            kind: ItemKind::Enum {
                name: color_ident.clone(),
                type_params: Vec::new(),
                variants: vec![
                    EnumVariant {
                        name: ident("Red", 10),
                        fields: EnumVariantFields::Unit,
                    },
                    EnumVariant {
                        name: ident("Blue", 20),
                        fields: EnumVariantFields::Unit,
                    },
                ],
            },
            span: Span {
                range: 0..30,
                file: None,
            },
        };

        let c_ident = ident("c", 40);
        let param = Param {
            attributes: Vec::new(),
            name: c_ident.clone(),
            ty: type_ident("Color", 42),
        };
        let scrutinee = Expr {
            kind: ExprKind::Ident(c_ident.clone()),
            span: c_ident.span.clone(),
        };
        let pat_red = Pattern {
            kind: PatternKind::EnumUnit {
                path: vec![color_ident.clone(), ident("Red", 60)],
            },
            span: Span {
                range: 50..63,
                file: None,
            },
        };
        let pat_blue = Pattern {
            kind: PatternKind::EnumUnit {
                path: vec![color_ident.clone(), ident("Blue", 70)],
            },
            span: Span {
                range: 64..78,
                file: None,
            },
        };
        let arm_red = MatchArm {
            pattern: pat_red,
            expr: Expr {
                kind: ExprKind::Literal(Literal {
                    kind: LiteralKind::Int(1),
                    span: Span {
                        range: 80..81,
                        file: None,
                    },
                }),
                span: Span {
                    range: 80..81,
                    file: None,
                },
            },
        };
        let arm_blue = MatchArm {
            pattern: pat_blue,
            expr: Expr {
                kind: ExprKind::Literal(Literal {
                    kind: LiteralKind::Int(2),
                    span: Span {
                        range: 90..91,
                        file: None,
                    },
                }),
                span: Span {
                    range: 90..91,
                    file: None,
                },
            },
        };
        let match_expr = Expr {
            kind: ExprKind::Match {
                scrutinee: Box::new(scrutinee),
                arms: vec![arm_red, arm_blue],
            },
            span: Span {
                range: 50..100,
                file: None,
            },
        };
        let ret_stmt = Stmt {
            kind: StmtKind::Return {
                value: Some(match_expr),
            },
            span: Span {
                range: 50..105,
                file: None,
            },
        };
        let fn_item = Item {
            attributes: Vec::new(),
            visibility: husk_ast::Visibility::Private,
            kind: ItemKind::Fn {
                name: ident("f", 40),
                type_params: Vec::new(),
                params: vec![param],
                ret_type: Some(type_ident("i32", 45)),
                body: vec![ret_stmt],
            },
            span: Span {
                range: 40..110,
                file: None,
            },
        };

        let file = File {
            items: vec![enum_item, fn_item],
        };

        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors, got {:?}",
            result.type_errors
        );
    }

    #[test]
    fn match_on_enum_non_exhaustive_reports_error() {
        // enum Color { Red, Blue }
        // fn f(c: Color) -> i32 {
        //     return match c {
        //         Color::Red => 1,
        //     };
        // }
        let color_ident = ident("Color", 0);
        let enum_item = Item {
            attributes: Vec::new(),
            visibility: husk_ast::Visibility::Private,
            kind: ItemKind::Enum {
                name: color_ident.clone(),
                type_params: Vec::new(),
                variants: vec![
                    EnumVariant {
                        name: ident("Red", 10),
                        fields: EnumVariantFields::Unit,
                    },
                    EnumVariant {
                        name: ident("Blue", 20),
                        fields: EnumVariantFields::Unit,
                    },
                ],
            },
            span: Span {
                range: 0..30,
                file: None,
            },
        };

        let c_ident = ident("c", 40);
        let param = Param {
            attributes: Vec::new(),
            name: c_ident.clone(),
            ty: type_ident("Color", 42),
        };
        let scrutinee = Expr {
            kind: ExprKind::Ident(c_ident.clone()),
            span: c_ident.span.clone(),
        };
        let pat_red = Pattern {
            kind: PatternKind::EnumUnit {
                path: vec![color_ident.clone(), ident("Red", 60)],
            },
            span: Span {
                range: 50..63,
                file: None,
            },
        };
        let arm_red = MatchArm {
            pattern: pat_red,
            expr: Expr {
                kind: ExprKind::Literal(Literal {
                    kind: LiteralKind::Int(1),
                    span: Span {
                        range: 80..81,
                        file: None,
                    },
                }),
                span: Span {
                    range: 80..81,
                    file: None,
                },
            },
        };
        let match_expr = Expr {
            kind: ExprKind::Match {
                scrutinee: Box::new(scrutinee),
                arms: vec![arm_red],
            },
            span: Span {
                range: 50..100,
                file: None,
            },
        };
        let ret_stmt = Stmt {
            kind: StmtKind::Return {
                value: Some(match_expr),
            },
            span: Span {
                range: 50..105,
                file: None,
            },
        };
        let fn_item = Item {
            attributes: Vec::new(),
            visibility: husk_ast::Visibility::Private,
            kind: ItemKind::Fn {
                name: ident("f", 40),
                type_params: Vec::new(),
                params: vec![param],
                ret_type: Some(type_ident("i32", 45)),
                body: vec![ret_stmt],
            },
            span: Span {
                range: 40..110,
                file: None,
            },
        };

        let file = File {
            items: vec![enum_item, fn_item],
        };

        let result = analyze_file(&file);
        assert!(
            result
                .type_errors
                .iter()
                .any(|e| e.message.contains("non-exhaustive match on enum `Color`")),
            "expected non-exhaustive match error, got {:?}",
            result.type_errors
        );
    }

    #[test]
    fn module_imports_accept_any_number_of_arguments() {
        // extern "js" { mod express; }
        // fn main() {
        //     let app = express();           // 0 args - should be fine
        //     let app2 = express(42);        // 1 arg - should be fine
        //     let app3 = express(1, 2, 3);   // 3 args - should be fine
        // }
        let express_ident = ident("express", 0);
        let extern_item = husk_ast::ExternItem {
            attributes: Vec::new(),
            kind: husk_ast::ExternItemKind::Mod {
                package: "express".to_string(),
                binding: express_ident.clone(),
                items: Vec::new(),
                is_global: false,
            },
            span: Span {
                range: 0..15,
                file: None,
            },
        };
        let extern_block = Item {
            attributes: Vec::new(),
            visibility: husk_ast::Visibility::Private,
            kind: ItemKind::ExternBlock {
                abi: "js".to_string(),
                items: vec![extern_item],
            },
            span: Span {
                range: 0..20,
                file: None,
            },
        };

        // let app = express();
        let call_0_args = Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Ident(express_ident.clone()),
                    span: express_ident.span.clone(),
                }),
                type_args: vec![],
                args: vec![],
            },
            span: Span {
                range: 30..40,
                file: None,
            },
        };
        let let_app = Stmt {
            kind: StmtKind::Let {
                mutable: false,
                pattern: ident_pattern("app", 25),
                ty: None,
                value: Some(call_0_args),
                else_block: None,
            },
            span: Span {
                range: 25..45,
                file: None,
            },
        };

        // let app2 = express(42);
        let call_1_arg = Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Ident(express_ident.clone()),
                    span: express_ident.span.clone(),
                }),
                type_args: vec![],
                args: vec![Expr {
                    kind: ExprKind::Literal(Literal {
                        kind: LiteralKind::Int(42),
                        span: Span {
                            range: 60..62,
                            file: None,
                        },
                    }),
                    span: Span {
                        range: 60..62,
                        file: None,
                    },
                }],
            },
            span: Span {
                range: 50..65,
                file: None,
            },
        };
        let let_app2 = Stmt {
            kind: StmtKind::Let {
                mutable: false,
                pattern: ident_pattern("app2", 45),
                ty: None,
                value: Some(call_1_arg),
                else_block: None,
            },
            span: Span {
                range: 45..70,
                file: None,
            },
        };

        // let app3 = express(1, 2, 3);
        let call_3_args = Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Ident(express_ident.clone()),
                    span: express_ident.span.clone(),
                }),
                type_args: vec![],
                args: vec![
                    Expr {
                        kind: ExprKind::Literal(Literal {
                            kind: LiteralKind::Int(1),
                            span: Span {
                                range: 80..81,
                                file: None,
                            },
                        }),
                        span: Span {
                            range: 80..81,
                            file: None,
                        },
                    },
                    Expr {
                        kind: ExprKind::Literal(Literal {
                            kind: LiteralKind::Int(2),
                            span: Span {
                                range: 83..84,
                                file: None,
                            },
                        }),
                        span: Span {
                            range: 83..84,
                            file: None,
                        },
                    },
                    Expr {
                        kind: ExprKind::Literal(Literal {
                            kind: LiteralKind::Int(3),
                            span: Span {
                                range: 86..87,
                                file: None,
                            },
                        }),
                        span: Span {
                            range: 86..87,
                            file: None,
                        },
                    },
                ],
            },
            span: Span {
                range: 75..90,
                file: None,
            },
        };
        let let_app3 = Stmt {
            kind: StmtKind::Let {
                mutable: false,
                pattern: ident_pattern("app3", 70),
                ty: None,
                value: Some(call_3_args),
                else_block: None,
            },
            span: Span {
                range: 70..95,
                file: None,
            },
        };

        let fn_item = Item {
            attributes: Vec::new(),
            visibility: husk_ast::Visibility::Private,
            kind: ItemKind::Fn {
                name: ident("main", 100),
                type_params: Vec::new(),
                params: Vec::new(),
                ret_type: None,
                body: vec![let_app, let_app2, let_app3],
            },
            span: Span {
                range: 100..150,
                file: None,
            },
        };

        let file = File {
            items: vec![extern_block, fn_item],
        };

        let result = analyze_file(&file);
        // Should have no type errors - module imports accept any number of arguments
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for module calls with any args, got {:?}",
            result.type_errors
        );
    }

    #[test]
    fn prelude_option_available_by_default() {
        let src = r#"
fn main() {
    let _v: Option<i32>;
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.symbols.errors.is_empty() && result.type_errors.is_empty(),
            "semantic errors: symbols={:?}, types={:?}",
            result.symbols.errors,
            result.type_errors
        );
    }

    #[test]
    fn prelude_variant_imports_allow_unqualified_some_none() {
        // Some, None, Ok, Err should be available without enum prefix due to prelude variant imports
        let src = r#"
fn main() {
    let x = Some(42);
    let y = None;
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.symbols.errors.is_empty() && result.type_errors.is_empty(),
            "semantic errors: symbols={:?}, types={:?}",
            result.symbols.errors,
            result.type_errors
        );
    }

    #[test]
    fn prelude_variant_imports_allow_unqualified_ok_err() {
        let src = r#"
fn main() {
    let x: Result<i32, String> = Ok(42);
    let y: Result<i32, String> = Err("error");
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.symbols.errors.is_empty() && result.type_errors.is_empty(),
            "semantic errors: symbols={:?}, types={:?}",
            result.symbols.errors,
            result.type_errors
        );
    }

    #[test]
    fn variant_import_match_pattern() {
        // Using imported variants in match patterns
        let src = r#"
fn main() {
    let x: Option<i32> = Some(42);
    match x {
        Some(v) => v,
        None => 0,
    };
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.symbols.errors.is_empty() && result.type_errors.is_empty(),
            "semantic errors: symbols={:?}, types={:?}",
            result.symbols.errors,
            result.type_errors
        );
    }

    #[test]
    fn variant_import_records_variant_calls() {
        // Verify that variant calls are recorded for codegen
        let src = r#"
fn main() {
    let x = Some(42);
}
"#;
        let parsed = parse_str(src);
        assert!(parsed.errors.is_empty());
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(result.symbols.errors.is_empty() && result.type_errors.is_empty());

        // Should have recorded the Some(42) call as a variant call
        assert!(
            !result.variant_calls.is_empty(),
            "variant_calls should not be empty for Some(42)"
        );

        // Check that the variant call was recorded correctly
        let mut found_some = false;
        for (_span, (enum_name, variant_name)) in &result.variant_calls {
            if enum_name == "Option" && variant_name == "Some" {
                found_some = true;
            }
        }
        assert!(
            found_some,
            "Should have recorded Some as a variant call for Option enum"
        );
    }

    #[test]
    fn prelude_jsvalue_available_by_default() {
        // JsValue and jsvalue_get should be available without explicit declaration
        let src = r#"
fn main() {
    let _v: JsValue;
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.symbols.errors.is_empty() && result.type_errors.is_empty(),
            "semantic errors: symbols={:?}, types={:?}",
            result.symbols.errors,
            result.type_errors
        );
    }

    #[test]
    fn prelude_js_globals_not_available_with_no_prelude() {
        // With --no-prelude, JsValue should not be available
        let src = r#"
fn main() {
    let _v: JsValue;
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file_without_prelude(&file);
        // Should have a type error for unknown type JsValue
        assert!(
            result
                .type_errors
                .iter()
                .any(|e| e.message.contains("unknown type `JsValue`")),
            "expected unknown type error for JsValue, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn loop_allows_break_inside() {
        let src = r#"
fn main() {
    loop {
        break;
    }
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "unexpected type errors: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn loop_allows_continue_inside() {
        let src = r#"
fn main() {
    loop {
        continue;
    }
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "unexpected type errors: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn break_outside_loop_reports_error() {
        let src = r#"
fn main() {
    break;
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result
                .type_errors
                .iter()
                .any(|e| e.message.contains("`break` used outside of loop")),
            "expected break outside loop error, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn continue_outside_loop_reports_error() {
        let src = r#"
fn main() {
    continue;
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result
                .type_errors
                .iter()
                .any(|e| e.message.contains("`continue` used outside of loop")),
            "expected continue outside loop error, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn nested_loop_allows_break() {
        let src = r#"
fn main() {
    loop {
        loop {
            break;
        }
        break;
    }
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "unexpected type errors: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn string_substring_returns_string() {
        // substring should be typed as returning String
        let src = r#"
fn test(s: String) -> String {
    s.substring(0)
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for String.substring(), got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn shadow_param_in_loop_generates_unique_names() {
        // When shadowing a function parameter inside a loop with `let s = s.something()`,
        // the new variable should get a unique name (s$1) while the RHS reference
        // should still use the original parameter name (s).
        let src = r#"
fn process(s: String) -> String {
    for n in 0..3 {
        let s = s.substring(n);
    }
    s
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors, got: {:?}",
            result.type_errors
        );

        // Check that name resolution produces different names for LHS and RHS
        // The new `s` binding (LHS) should be renamed to avoid JS "cannot access before init" error
        let mut found_shadowed_binding = false;
        for ((_start, _end), resolved_name) in &result.name_resolution {
            if resolved_name.starts_with("s$") {
                found_shadowed_binding = true;
                break;
            }
        }
        assert!(
            found_shadowed_binding,
            "expected shadowed variable 's' to be renamed to 's$N', but name_resolution only has: {:?}",
            result.name_resolution
        );
    }

    // ========== Trait Bound Tests ==========

    #[test]
    fn supertrait_parsing() {
        // Test that trait Eq: PartialEq parses correctly
        let src = r#"
trait PartialEq {}
trait Eq: PartialEq {}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "unexpected type errors: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn trait_impl_requires_supertrait() {
        // Implementing Eq without PartialEq should error
        let src = r#"
trait PartialEq {}
trait Eq: PartialEq {}

struct Foo {}

impl Eq for Foo {}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.iter().any(|e| e
                .message
                .contains("missing implementation of supertrait")
                && e.message.contains("PartialEq")),
            "expected supertrait error, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn trait_impl_with_supertrait_satisfied() {
        // Implementing both PartialEq and Eq should succeed
        let src = r#"
trait PartialEq {}
trait Eq: PartialEq {}

struct Foo {}

impl PartialEq for Foo {}
impl Eq for Foo {}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "unexpected type errors: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn function_with_trait_bound_parses() {
        // Test that function with trait bound parses correctly
        let src = r#"
trait PartialEq {}

fn compare<T: PartialEq>(a: T, b: T) -> bool {
    true
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "unexpected type errors: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn function_call_with_trait_bound_satisfied() {
        // Calling a generic function with a type that implements the bound should succeed
        let src = r#"
trait PartialEq {}

impl PartialEq for i32 {}

fn compare<T: PartialEq>(a: T, b: T) -> bool {
    true
}

fn main() {
    let x = compare(1, 2);
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "unexpected type errors: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn function_call_with_trait_bound_not_satisfied() {
        // Calling a generic function with a type that doesn't implement the bound should error
        let src = r#"
trait PartialEq {}

struct NoEq {}

fn compare<T: PartialEq>(a: T, b: T) -> bool {
    true
}

fn main() {
    let x = NoEq {};
    let y = NoEq {};
    let z = compare(x, y);
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result
                .type_errors
                .iter()
                .any(|e| e.message.contains("NoEq: PartialEq")
                    && e.message.contains("not satisfied")),
            "expected trait bound error, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn transitive_supertrait_chain_requires_all() {
        // A: B: C means implementing A requires implementing both B and C
        let src = r#"
trait Base {}
trait Middle: Base {}
trait Top: Middle {}

struct Foo {}

// Only implementing Top without Middle or Base should fail
impl Top for Foo {}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        // Should error about missing Middle (direct supertrait)
        assert!(
            result
                .type_errors
                .iter()
                .any(|e| e.message.contains("Middle")),
            "expected error about missing Middle, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn transitive_supertrait_chain_satisfied() {
        // Implementing all traits in the chain should succeed
        let src = r#"
trait Base {}
trait Middle: Base {}
trait Top: Middle {}

struct Foo {}

impl Base for Foo {}
impl Middle for Foo {}
impl Top for Foo {}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "unexpected type errors: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn transitive_supertrait_missing_indirect() {
        // Middle: Base, implementing Middle without Base should report Base as missing
        let src = r#"
trait Base {}
trait Middle: Base {}

struct Foo {}

impl Middle for Foo {}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result
                .type_errors
                .iter()
                .any(|e| e.message.contains("Base")),
            "expected error about missing Base, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn generic_type_matches_base_type_impl() {
        // A generic struct with trait impl should satisfy bounds when instantiated
        // e.g., Vec<i32> should match impl PartialEq for Vec
        let src = r#"
trait PartialEq {}

struct Container<T> {
    value: T,
}

impl PartialEq for Container {}

fn compare<T: PartialEq>(a: T, b: T) -> bool {
    true
}

fn main() {
    let c1 = Container { value: 42 };
    let c2 = Container { value: 43 };
    let result = compare(c1, c2);
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no errors for generic type with base impl, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn range_with_f64_reports_formatted_type_error() {
        // Range expressions require i32, using f64 should report error with "f64" not "Primitive(F64)"
        let src = r#"
fn main() {
    let x: f64 = 1.0;
    for i in x..10 {
    }
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result
                .type_errors
                .iter()
                .any(|e| e.message.contains("found f64")),
            "expected error message to contain 'found f64', got: {:?}",
            result.type_errors
        );
        assert!(
            !result
                .type_errors
                .iter()
                .any(|e| e.message.contains("Primitive")),
            "error message should not contain 'Primitive', got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn range_with_string_reports_formatted_type_error() {
        // Range expressions require i32, using String should report error with "String" not debug format
        let src = r#"
fn main() {
    let x = "hello";
    for i in x..10 {
    }
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result
                .type_errors
                .iter()
                .any(|e| e.message.contains("found String")),
            "expected error message to contain 'found String', got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn range_with_named_type_reports_formatted_type_error() {
        // Range expressions require i32, using a named type should report the type name
        let src = r#"
struct Foo {}

fn main() {
    let x = Foo {};
    for i in x..10 {
    }
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result
                .type_errors
                .iter()
                .any(|e| e.message.contains("found Foo")),
            "expected error message to contain 'found Foo', got: {:?}",
            result.type_errors
        );
        assert!(
            !result
                .type_errors
                .iter()
                .any(|e| e.message.contains("Named {")),
            "error message should not contain debug format 'Named {{', got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn string_indexof_returns_i32() {
        let src = r#"
fn test(s: String) -> i32 {
    s.index_of("x")
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for String.index_of(), got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn string_lastindexof_returns_i32() {
        let src = r#"
fn test(s: String) -> i32 {
    s.last_index_of("x")
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for String.last_index_of(), got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn string_startswith_returns_bool() {
        let src = r#"
fn test(s: String) -> bool {
    s.starts_with("hello")
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for String.starts_with(), got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn string_endswith_returns_bool() {
        let src = r#"
fn test(s: String) -> bool {
    s.ends_with("world")
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for String.ends_with(), got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn string_includes_returns_bool() {
        let src = r#"
fn test(s: String) -> bool {
    s.includes("sub")
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for String.includes(), got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn array_sort_returns_same_array_type() {
        let src = r#"
fn test(arr: [i32]) -> [i32] {
    arr.sort()
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for [i32].sort(), got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn array_reverse_returns_same_array_type() {
        let src = r#"
fn test(arr: [String]) -> [String] {
    arr.reverse()
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for [String].reverse(), got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn array_join_returns_string() {
        let src = r#"
fn test(arr: [String]) -> String {
    arr.join(",")
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for [String].join(), got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn string_split_sort_join_chain_returns_string() {
        // This tests the common pattern used in the advent code
        let src = r#"
fn test(s: String) -> String {
    s.split("").sort().join("")
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for split().sort().join() chain, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn i64_type_annotation_works() {
        let src = r#"fn foo() { let x: i64 = 0 as i64; }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for i64 type annotation, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn i64_arithmetic_works() {
        let src = r#"fn foo() { let a: i64 = 1 as i64; let b: i64 = 2 as i64; let c = a + b; let d = a * b; }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for i64 arithmetic, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn i64_i32_arithmetic_mismatch_error() {
        let src = r#"fn foo() { let a: i64 = 1 as i64; let b: i32 = 2; let c = a + b; }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            !result.type_errors.is_empty(),
            "expected type error for i64 + i32 mismatch"
        );
    }

    #[test]
    fn i64_cast_from_i32_works() {
        let src = r#"fn foo() { let x: i32 = 42; let y: i64 = x as i64; }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for i32 to i64 cast, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn i64_cast_to_i32_works() {
        let src = r#"fn foo() { let x: i64 = 42 as i64; let y: i32 = x as i32; }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for i64 to i32 cast, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn parse_long_returns_i64() {
        let src = r#"fn foo() { let x: i64 = parseLong("123"); }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for parseLong returning i64, got: {:?}",
            result.type_errors
        );
    }

    // =====================================================================
    // Type Inference Tests (From/Into traits, turbofish syntax)
    // =====================================================================

    #[test]
    fn into_with_type_annotation_infers_target() {
        // let s: String = 42.into();
        let src = r#"fn foo() { let s: String = 42.into(); }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for .into() with type annotation, got: {:?}",
            result.type_errors
        );
        // Check that type_resolution recorded the target type
        assert!(
            !result.type_resolution.is_empty(),
            "expected type_resolution to have entries for .into() call"
        );
    }

    #[test]
    fn into_with_turbofish_explicit_type() {
        // let s = 42.into::<String>();
        let src = r#"fn foo() { let s = 42.into::<String>(); }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for .into::<String>() turbofish, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn into_without_type_context_errors() {
        // let s = 42.into(); // Cannot infer target type
        let src = r#"fn foo() { let s = 42.into(); }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            !result.type_errors.is_empty(),
            "expected type error for .into() without type context"
        );
        let err_msg = format!("{:?}", result.type_errors);
        assert!(
            err_msg.contains("cannot infer type"),
            "expected 'cannot infer type' error, got: {}",
            err_msg
        );
    }

    #[test]
    fn parse_with_turbofish_for_i32() {
        // let n = "123".parse::<i32>();
        let src = r#"fn foo() { let n = "123".parse::<i32>(); }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for .parse::<i32>(), got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn parse_with_turbofish_for_f64() {
        // let n = "3.14".parse::<f64>();
        let src = r#"fn foo() { let n = "3.14".parse::<f64>(); }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for .parse::<f64>(), got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn parse_without_turbofish_errors() {
        // let n = "123".parse(); // Missing type argument
        let src = r#"fn foo() { let n = "123".parse(); }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            !result.type_errors.is_empty(),
            "expected type error for .parse() without type argument"
        );
    }

    #[test]
    fn into_i32_to_i64_widening() {
        // let x: i64 = 42.into();
        let src = r#"fn foo() { let x: i64 = 42.into(); }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for i32 to i64 .into(), got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn into_i32_to_f64_widening() {
        // let x: f64 = 42.into();
        let src = r#"fn foo() { let x: f64 = 42.into(); }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for i32 to f64 .into(), got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn into_bool_to_string() {
        // let s: String = true.into();
        let src = r#"fn foo() { let s: String = true.into(); }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for bool to String .into(), got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn into_invalid_conversion_errors() {
        // let x: i32 = "hello".into(); // No From<String> for i32
        let src = r#"fn foo() { let x: i32 = "hello".into(); }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            !result.type_errors.is_empty(),
            "expected type error for invalid .into() conversion"
        );
    }

    #[test]
    fn parse_for_non_string_errors() {
        // let n = 42.parse::<i32>(); // parse only works on String
        let src = r#"fn foo() { let n = 42.parse::<i32>(); }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            !result.type_errors.is_empty(),
            "expected type error for .parse() on non-String"
        );
    }

    #[test]
    fn type_resolution_records_correct_target() {
        // Verify that the type resolution map contains the correct resolved type
        let src = r#"fn foo() { let s: String = 42.into(); }"#;
        let parsed = parse_str(src);
        assert!(parsed.errors.is_empty());
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(result.type_errors.is_empty());

        // Find the entry in type_resolution - should be "String"
        let has_string_target = result.type_resolution.values().any(|v| v == "String");
        assert!(
            has_string_target,
            "expected type_resolution to contain 'String', got: {:?}",
            result.type_resolution
        );
    }

    #[test]
    fn parse_returns_result_type() {
        // Verify that parse returns Result<T, String>
        let src = r#"
            fn foo() {
                let n = "123".parse::<i32>();
                match n {
                    Result::Ok(v) => v,
                    Result::Err(e) => 0,
                }
            }
        "#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for parse result matching, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn into_in_function_argument() {
        // fn takes_string(s: String) {}
        // takes_string(42.into());
        let src = r#"
            fn takes_string(s: String) {}
            fn foo() { takes_string(42.into()); }
        "#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for .into() in function argument, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn into_in_return_position() {
        // fn to_string() -> String { 42.into() }
        let src = r#"fn to_string() -> String { 42.into() }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for .into() in return position, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn into_with_i64_turbofish() {
        // let x = 42.into::<i64>();
        let src = r#"fn foo() { let x = 42.into::<i64>(); }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for .into::<i64>() turbofish, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn parse_i64_turbofish() {
        // let x = "123".parse::<i64>();
        let src = r#"fn foo() { let x = "123".parse::<i64>(); }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for .parse::<i64>(), got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn try_into_with_turbofish() {
        // let r = (42 as i64).try_into::<i32>();
        let src = r#"fn foo() { let r = (42 as i64).try_into::<i32>(); }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors for .try_into::<i32>(), got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn try_into_without_turbofish_errors() {
        // let r = (42 as i64).try_into(); // Missing type argument
        let src = r#"fn foo() { let r = (42 as i64).try_into(); }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            !result.type_errors.is_empty(),
            "expected type error for .try_into() without type argument"
        );
    }

    #[test]
    fn try_into_invalid_conversion_errors() {
        // let r = true.try_into::<i32>(); // No TryFrom<bool> for i32
        let src = r#"fn foo() { let r = true.try_into::<i32>(); }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            !result.type_errors.is_empty(),
            "expected type error for invalid .try_into() conversion"
        );
    }

    // =========================================================================
    // Tuple semantic tests
    // =========================================================================

    #[test]
    fn tuple_pattern_with_non_tuple_type_errors() {
        // let (x, y): i32 = 5; should error
        let src = r#"fn foo() { let (x, y): i32 = 5; }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result
                .type_errors
                .iter()
                .any(|e| e.message.contains("expected tuple type for tuple pattern")),
            "expected 'expected tuple type for tuple pattern' error, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn tuple_pattern_arity_mismatch_errors() {
        // let (x, y, z): (i32, String) = (1, "hello"); should error
        let src = r#"fn foo() { let (x, y, z): (i32, String) = (1, "hello"); }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result
                .type_errors
                .iter()
                .any(|e| e.message.contains("tuple pattern has")
                    && e.message.contains("fields but type has")),
            "expected arity mismatch error, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn tuple_type_and_pattern_match_succeeds() {
        // Valid tuple destructuring
        let src = r#"fn foo() { let pair: (i32, String) = (42, "hello"); let (x, y) = pair; }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn tuple_field_access_type_checks() {
        // Access tuple fields
        let src = r#"fn foo() { let pair: (i32, String) = (42, "hello"); let x: i32 = pair.0; let y: String = pair.1; }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no type errors, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn tuple_field_access_wrong_type_errors() {
        // Type mismatch on tuple field access
        let src =
            r#"fn foo() { let pair: (i32, String) = (42, "hello"); let x: String = pair.0; }"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            !result.type_errors.is_empty(),
            "expected type mismatch error for accessing i32 field as String"
        );
    }

    // =========================================================================
    // if-let and let-else tests
    // =========================================================================

    #[test]
    fn if_let_with_qualified_variant() {
        // if let with fully qualified Option::Some
        let src = r#"
fn test() -> i32 {
    let opt: Option<i32> = Option::Some(42);
    if let Option::Some(x) = opt {
        x
    } else {
        0
    }
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.symbols.errors.is_empty() && result.type_errors.is_empty(),
            "semantic errors: symbols={:?}, types={:?}",
            result.symbols.errors,
            result.type_errors
        );
    }

    #[test]
    fn if_let_with_imported_variant() {
        // if let with imported Some variant (prelude import)
        let src = r#"
fn test() -> i32 {
    let opt: Option<i32> = Some(42);
    if let Some(x) = opt {
        x
    } else {
        0
    }
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.symbols.errors.is_empty() && result.type_errors.is_empty(),
            "semantic errors: symbols={:?}, types={:?}",
            result.symbols.errors,
            result.type_errors
        );
    }

    #[test]
    fn let_else_with_qualified_variant() {
        // let-else with fully qualified Option::Some
        let src = r#"
fn test() -> i32 {
    let opt: Option<i32> = Option::Some(42);
    let Option::Some(x) = opt else {
        return 0;
    };
    x
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.symbols.errors.is_empty() && result.type_errors.is_empty(),
            "semantic errors: symbols={:?}, types={:?}",
            result.symbols.errors,
            result.type_errors
        );
    }

    #[test]
    fn let_else_with_imported_variant() {
        // let-else with imported Some variant (prelude import)
        let src = r#"
fn test() -> i32 {
    let opt: Option<i32> = Some(42);
    let Some(x) = opt else {
        return 0;
    };
    x
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.symbols.errors.is_empty() && result.type_errors.is_empty(),
            "semantic errors: symbols={:?}, types={:?}",
            result.symbols.errors,
            result.type_errors
        );
    }

    #[test]
    fn if_let_mismatched_enum_errors() {
        // if let with Option pattern on Result type should error
        let src = r#"
fn test() -> i32 {
    let res: Result<i32, String> = Ok(42);
    if let Option::Some(x) = res {
        x
    } else {
        0
    }
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            !result.type_errors.is_empty() || !result.symbols.errors.is_empty(),
            "expected error for mismatched enum types"
        );
    }

    #[test]
    fn if_let_tuple_with_refutable_element() {
        // if let with tuple containing refutable Some(x)
        let src = r#"
fn test() -> i32 {
    let pair: (Option<i32>, i32) = (Option::Some(42), 10);
    if let (Option::Some(x), y) = pair {
        x + y
    } else {
        0
    }
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.symbols.errors.is_empty() && result.type_errors.is_empty(),
            "semantic errors: symbols={:?}, types={:?}",
            result.symbols.errors,
            result.type_errors
        );
    }

    #[test]
    fn if_let_irrefutable_pattern_errors() {
        // if let with irrefutable pattern should error
        let src = r#"
fn test() -> i32 {
    let x: i32 = 42;
    if let y = x {
        y
    } else {
        0
    }
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            !result.type_errors.is_empty() || !result.symbols.errors.is_empty(),
            "expected error for irrefutable pattern in if-let"
        );
    }

    #[test]
    fn let_else_without_diverging_block_errors() {
        // let-else without diverging else block should error
        let src = r#"
fn test() -> i32 {
    let opt: Option<i32> = Option::Some(42);
    let Option::Some(x) = opt else {
        let y = 0;
    };
    x
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            !result.type_errors.is_empty() || !result.symbols.errors.is_empty(),
            "expected error for non-diverging else block in let-else"
        );
    }

    #[test]
    fn if_let_imported_variant_wrong_scrutinee_type_errors() {
        // Using an imported variant (None) on wrong scrutinee type (bool) should error
        let src = r#"
fn test() -> i32 {
    let b: bool = true;
    if let None = b {
        0
    } else {
        1
    }
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            !result.type_errors.is_empty() || !result.symbols.errors.is_empty(),
            "expected error for imported variant pattern on wrong type"
        );
    }

    #[test]
    fn if_let_unknown_variant_errors() {
        // Using a non-existent variant (typo: Smoe instead of Some) should error
        let src = r#"
fn test() -> i32 {
    let opt: Option<i32> = Option::Some(42);
    if let Option::Smoe(x) = opt {
        x
    } else {
        0
    }
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            !result.type_errors.is_empty() || !result.symbols.errors.is_empty(),
            "expected error for unknown variant in if-let pattern"
        );
    }

    #[test]
    fn if_let_enum_pattern_on_struct_errors() {
        // Using enum pattern syntax on a struct type should error
        let src = r#"
struct Foo {
    x: i32,
}

fn test() -> i32 {
    let f: Foo = Foo { x: 42 };
    if let Foo::Bar = f {
        0
    } else {
        1
    }
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            !result.type_errors.is_empty() || !result.symbols.errors.is_empty(),
            "expected error for enum pattern on struct type"
        );
    }

    // ==========================================================================
    // ReferenceMap tests for LSP rename support
    // ==========================================================================

    #[test]
    fn reference_map_tracks_function_definition_and_calls() {
        let src = r#"
fn greet(name: String) -> String {
    name
}

fn main() {
    let x = greet("world");
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);

        // Check that function definition and call are tracked
        let key = ("greet".to_string(), ReferenceKind::Function);
        let refs = result.references.get(&key);
        assert!(refs.is_some(), "expected references for function 'greet'");
        let refs = refs.unwrap();
        // Should have at least 2 references: definition + call
        assert!(
            refs.len() >= 2,
            "expected at least 2 references for 'greet', got {}",
            refs.len()
        );
    }

    #[test]
    fn reference_map_tracks_struct_definition_and_usage() {
        let src = r#"
struct Point {
    x: i32,
    y: i32,
}

fn create_point() -> Point {
    Point { x: 10, y: 20 }
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);

        // Check struct definition and type usage
        let key = ("Point".to_string(), ReferenceKind::Struct);
        let refs = result.references.get(&key);
        assert!(refs.is_some(), "expected references for struct 'Point'");
        let refs = refs.unwrap();
        // Should have at least 2: definition + return type annotation
        assert!(
            refs.len() >= 2,
            "expected at least 2 references for 'Point', got {}",
            refs.len()
        );
    }

    #[test]
    fn reference_map_tracks_struct_field_definition_and_access() {
        let src = r#"
struct Point {
    x: i32,
    y: i32,
}

fn get_x(p: Point) -> i32 {
    p.x
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);

        // Check field definition and access
        let key = ("Point.x".to_string(), ReferenceKind::Field);
        let refs = result.references.get(&key);
        assert!(refs.is_some(), "expected references for field 'Point.x'");
        let refs = refs.unwrap();
        // Should have 2: definition + access
        assert!(
            refs.len() >= 2,
            "expected at least 2 references for 'Point.x', got {}",
            refs.len()
        );
    }

    #[test]
    fn reference_map_tracks_enum_and_variant_definitions() {
        let src = r#"
enum Status {
    Active,
    Inactive,
}

fn check(s: Status) -> bool {
    match s {
        Status::Active => true,
        Status::Inactive => false,
    }
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);

        // Check enum definition and usage
        let enum_key = ("Status".to_string(), ReferenceKind::Enum);
        let enum_refs = result.references.get(&enum_key);
        assert!(enum_refs.is_some(), "expected references for enum 'Status'");
        let enum_refs = enum_refs.unwrap();
        // Should have at least 2: definition + param type (match patterns track variant, not enum)
        assert!(
            enum_refs.len() >= 2,
            "expected at least 2 references for 'Status', got {}",
            enum_refs.len()
        );

        // Check variant definitions and usages
        let active_key = ("Status::Active".to_string(), ReferenceKind::Variant);
        let active_refs = result.references.get(&active_key);
        assert!(
            active_refs.is_some(),
            "expected references for variant 'Status::Active'"
        );
        let active_refs = active_refs.unwrap();
        // Should have at least 2: definition + pattern match
        assert!(
            active_refs.len() >= 2,
            "expected at least 2 references for 'Status::Active', got {}",
            active_refs.len()
        );
    }

    #[test]
    fn reference_map_tracks_variable_binding_and_usage() {
        let src = r#"
fn test() -> i32 {
    let count = 5;
    let result = count + 10;
    result
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);

        // Check variable binding and usage for 'count'
        let key = ("count".to_string(), ReferenceKind::Variable);
        let refs = result.references.get(&key);
        assert!(refs.is_some(), "expected references for variable 'count'");
        let refs = refs.unwrap();
        // Should have 2: binding + usage
        assert!(
            refs.len() >= 2,
            "expected at least 2 references for 'count', got {}",
            refs.len()
        );
    }

    #[test]
    fn reference_map_tracks_type_alias() {
        let src = r#"
type UserId = i32;

fn get_user(id: UserId) -> UserId {
    id
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);

        // Check type alias definition and usages
        let key = ("UserId".to_string(), ReferenceKind::TypeAlias);
        let refs = result.references.get(&key);
        assert!(
            refs.is_some(),
            "expected references for type alias 'UserId'"
        );
        let refs = refs.unwrap();
        // Should have at least 3: definition + param type + return type
        assert!(
            refs.len() >= 3,
            "expected at least 3 references for 'UserId', got {}",
            refs.len()
        );
    }

    #[test]
    fn reference_map_tracks_trait_definition() {
        let src = r#"
trait Printable {
    fn print(self) -> String;
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);

        // Check trait definition is tracked
        let key = ("Printable".to_string(), ReferenceKind::Trait);
        let refs = result.references.get(&key);
        assert!(refs.is_some(), "expected references for trait 'Printable'");
        let refs = refs.unwrap();
        // Should have at least 1: definition
        assert!(
            !refs.is_empty(),
            "expected at least 1 reference for 'Printable'"
        );
    }

    #[test]
    fn reference_map_tracks_imported_variant_in_expression() {
        // This test uses the prelude's Option type with imported Some/None
        let src = r#"
fn wrap(x: i32) -> Option<i32> {
    Some(x)
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);

        // Check that Some variant usage is tracked
        // 1 from prelude definition + at least 1 from this file's usage
        let key = ("Option::Some".to_string(), ReferenceKind::Variant);
        let refs = result.references.get(&key);
        assert!(
            refs.is_some(),
            "expected references for variant 'Option::Some'"
        );
        let refs = refs.unwrap();
        assert!(
            refs.len() >= 2,
            "expected at least 2 references for 'Option::Some' (def + usage), got {}",
            refs.len()
        );
    }

    #[test]
    fn reference_map_tracks_variant_in_pattern_match() {
        let src = r#"
fn unwrap_or(opt: Option<i32>, default: i32) -> i32 {
    match opt {
        Some(v) => v,
        None => default,
    }
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);

        // Check that Some and None variant usages in patterns are tracked
        // Each should have at least 2: 1 from prelude definition + 1 from pattern usage
        let some_key = ("Option::Some".to_string(), ReferenceKind::Variant);
        let some_refs = result.references.get(&some_key);
        assert!(
            some_refs.is_some(),
            "expected references for variant 'Option::Some' in pattern"
        );
        let some_refs = some_refs.unwrap();
        assert!(
            some_refs.len() >= 2,
            "expected at least 2 references for 'Option::Some' (def + pattern), got {}",
            some_refs.len()
        );

        let none_key = ("Option::None".to_string(), ReferenceKind::Variant);
        let none_refs = result.references.get(&none_key);
        assert!(
            none_refs.is_some(),
            "expected references for variant 'Option::None' in pattern"
        );
        let none_refs = none_refs.unwrap();
        assert!(
            none_refs.len() >= 2,
            "expected at least 2 references for 'Option::None' (def + pattern), got {}",
            none_refs.len()
        );
    }

    #[test]
    fn reference_map_tracks_variant_in_if_let() {
        let src = r#"
fn check_some(opt: Option<i32>) -> i32 {
    if let Some(x) = opt {
        x
    } else {
        0
    }
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);

        // Check that Some variant usage in if-let pattern is tracked
        // 1 from prelude definition + at least 1 from this file's usage
        let key = ("Option::Some".to_string(), ReferenceKind::Variant);
        let refs = result.references.get(&key);
        assert!(
            refs.is_some(),
            "expected references for variant 'Option::Some' in if-let"
        );
        let refs = refs.unwrap();
        assert!(
            refs.len() >= 2,
            "expected at least 2 references for 'Option::Some' (def + if-let), got {}",
            refs.len()
        );
    }

    #[test]
    fn reference_map_tracks_qualified_variant_in_if_let() {
        let src = r#"
enum MyEnum {
    Foo(i32),
    Bar,
}

fn check(e: MyEnum) -> i32 {
    if let MyEnum::Foo(x) = e {
        x
    } else {
        0
    }
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);

        // Check that qualified variant in if-let is tracked
        let key = ("MyEnum::Foo".to_string(), ReferenceKind::Variant);
        let refs = result.references.get(&key);
        assert!(
            refs.is_some(),
            "expected references for variant 'MyEnum::Foo' in if-let"
        );
        let refs = refs.unwrap();
        // Should have at least 2: definition + if-let pattern
        assert!(
            refs.len() >= 2,
            "expected at least 2 references for 'MyEnum::Foo', got {}",
            refs.len()
        );
    }

    #[test]
    fn reference_map_tracks_struct_literal() {
        let src = r#"
struct Point {
    x: i32,
    y: i32,
}

fn make_point() -> Point {
    Point { x: 10, y: 20 }
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);

        // Check struct name reference in literal
        let struct_key = ("Point".to_string(), ReferenceKind::Struct);
        let struct_refs = result.references.get(&struct_key);
        assert!(
            struct_refs.is_some(),
            "expected references for struct 'Point'"
        );
        let struct_refs = struct_refs.unwrap();
        // Should have at least 2: definition + literal usage
        assert!(
            struct_refs.len() >= 2,
            "expected at least 2 references for 'Point' (def + literal), got {}",
            struct_refs.len()
        );

        // Check field references in literal
        let x_key = ("Point.x".to_string(), ReferenceKind::Field);
        let x_refs = result.references.get(&x_key);
        assert!(x_refs.is_some(), "expected references for field 'Point.x'");
        let x_refs = x_refs.unwrap();
        // Should have at least 2: definition + literal usage
        assert!(
            x_refs.len() >= 2,
            "expected at least 2 references for 'Point.x' (def + literal), got {}",
            x_refs.len()
        );

        let y_key = ("Point.y".to_string(), ReferenceKind::Field);
        let y_refs = result.references.get(&y_key);
        assert!(y_refs.is_some(), "expected references for field 'Point.y'");
        let y_refs = y_refs.unwrap();
        assert!(
            y_refs.len() >= 2,
            "expected at least 2 references for 'Point.y' (def + literal), got {}",
            y_refs.len()
        );
    }

    // ==================== Try Operator Validation Tests ====================

    #[test]
    fn try_operator_in_unit_function_errors() {
        let src = r#"
fn main() {
    let x: Result<i32, String> = Ok(42);
    let y = x?;
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            !result.type_errors.is_empty(),
            "expected error for ? in function returning ()"
        );
        assert!(
            result.type_errors[0]
                .message
                .contains("can only be used in a function that returns"),
            "error message should mention return type requirement, got: {}",
            result.type_errors[0].message
        );
    }

    #[test]
    fn try_operator_in_result_function_succeeds() {
        let src = r#"
fn main() -> Result<(), String> {
    let x: Result<i32, String> = Ok(42);
    let _y = x?;
    Ok(())
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no errors for ? in Result function, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn try_operator_in_option_function_succeeds() {
        let src = r#"
fn get_value() -> Option<i32> {
    let x: Option<i32> = Some(42);
    let y = x?;
    Some(y)
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "expected no errors for ? in Option function, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn try_operator_multiple_in_same_function_all_validated() {
        let src = r#"
fn main() {
    let x: Result<i32, String> = Ok(1);
    let y: Result<i32, String> = Ok(2);
    let a = x?;
    let b = y?;
}
"#;
        let parsed = parse_str(src);
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        // Should have errors for both ? operators
        assert!(
            result.type_errors.len() >= 2,
            "expected at least 2 errors for multiple ? operators in () function, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn try_operator_error_message_includes_function_name() {
        let src = r#"
fn my_function() {
    let x: Result<i32, String> = Ok(42);
    let y = x?;
}
"#;
        let parsed = parse_str(src);
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(!result.type_errors.is_empty());
        assert!(
            result.type_errors[0].message.contains("my_function"),
            "error should mention function name, got: {}",
            result.type_errors[0].message
        );
    }

    // ==================== Termination Trait Tests ====================

    #[test]
    fn main_returning_unit_is_valid() {
        let src = r#"
fn main() {
    let x = 42;
}
"#;
        let parsed = parse_str(src);
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty() && result.symbols.errors.is_empty(),
            "main() returning () should be valid, got: {:?} {:?}",
            result.type_errors,
            result.symbols.errors
        );
    }

    #[test]
    fn main_returning_result_is_valid() {
        let src = r#"
fn main() -> Result<(), String> {
    Ok(())
}
"#;
        let parsed = parse_str(src);
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "main() returning Result should be valid, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn main_returning_i32_is_invalid() {
        let src = r#"
fn main() -> i32 {
    42
}
"#;
        let parsed = parse_str(src);
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            !result.type_errors.is_empty(),
            "main() returning i32 should be invalid"
        );
        assert!(
            result.type_errors[0].message.contains("Termination"),
            "error should mention Termination trait, got: {}",
            result.type_errors[0].message
        );
    }

    #[test]
    fn main_returning_string_is_invalid() {
        let src = r#"
fn main() -> String {
    "hello"
}
"#;
        let parsed = parse_str(src);
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            !result.type_errors.is_empty(),
            "main() returning String should be invalid"
        );
    }

    #[test]
    fn non_main_function_can_return_any_type() {
        let src = r#"
fn helper() -> i32 {
    42
}

fn main() {
    let x = helper();
}
"#;
        let parsed = parse_str(src);
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty() && result.symbols.errors.is_empty(),
            "non-main functions can return any type, got: {:?} {:?}",
            result.type_errors,
            result.symbols.errors
        );
    }

    #[test]
    fn main_with_try_operator_and_result_return_type() {
        let src = r#"
fn risky() -> Result<i32, String> {
    Ok(42)
}

fn main() -> Result<(), String> {
    let x = risky()?;
    Ok(())
}
"#;
        let parsed = parse_str(src);
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            result.type_errors.is_empty(),
            "main with ? and Result return should work, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn try_operator_result_in_option_function_errors() {
        let src = r#"
fn main() -> Option<i32> {
    let x: Result<i32, String> = Ok(42);
    let y = x?;
    Some(y)
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            !result.type_errors.is_empty(),
            "expected error for ? on Result in Option-returning function"
        );
        assert!(
            result.type_errors.iter().any(|e| e
                .message
                .contains("cannot use `?` on a `Result` value in a function that returns")
                && e.message.contains("Option<i32>")),
            "error message should mention Result/Option mismatch, got: {:?}",
            result.type_errors
        );
    }

    #[test]
    fn try_operator_option_in_result_function_errors() {
        let src = r#"
fn main() -> Result<i32, String> {
    let x: Option<i32> = Some(42);
    let y = x?;
    Ok(y)
}
"#;
        let parsed = parse_str(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = parsed.file.expect("parser produced no AST");
        let result = analyze_file(&file);
        assert!(
            !result.type_errors.is_empty(),
            "expected error for ? on Option in Result-returning function"
        );
        assert!(
            result.type_errors.iter().any(|e| e
                .message
                .contains("cannot use `?` on an `Option` value in a function that returns")
                && e.message.contains("Result<i32, String>")),
            "error message should mention Option/Result mismatch, got: {:?}",
            result.type_errors
        );
    }
}
