//! Backend-neutral compiler orchestration, interpreter, modules, and instance
//! ownership for the Husk scripting language.
//!
//! Applications normally depend on the public `husk` facade. This crate owns
//! implementation details while remaining independent from Red and the
//! standalone CLI.

mod embedding;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
    sync::Arc,
};

use husk_ast::{
    EnumVariantFields, File, FormatSegment, FormatSpec, Ident, ImplItemKind, ItemKind, Param,
    SelfReceiver, Span, TypeExpr, TypeExprKind, Visibility,
};
use husk_diagnostics::{CallFrame, Diagnostic, Report, SourceFile, SourceMap};
use husk_hir::{
    AssignOp, BinaryOp, Block, CallTarget, Expr, ExprKind, Function as HirFunction,
    IntrinsicFunction, LiteralKind, MethodTarget, PatternKind, Stmt, StmtKind, UnaryOp,
    lower_function,
};
use husk_semantic::{SemanticOptions, SemanticResult};

pub use embedding::{
    CallContext, CompiledModule, ConversionError, Engine, EngineBuilder, FromHusk, HuskType,
    Instance, IntoHusk, Limits, NativeError, NativeModule, NativeModuleBuilder, ReplOutcome,
    ReplSession, ScriptResult,
};
pub use husk_hir::{FunctionId, IntrinsicMethodId, LocalId, ModuleFunctionId, NodeId};
pub use husk_package::{
    ExtensionSource, LOCK_FILE, LockedExtension, LockedPackage, MANIFEST_FILE, PackageError,
    PackageLimits, PackageLock, PackageManifest, PackageSection, ResolvedExtension,
    ResolvedPackage, SourceModule, discover_manifest,
};
pub use husk_semantic::SemanticProfile;
pub use husk_types::{
    DescriptorError, DescriptorHash, FieldDescriptor, FunctionDescriptor, InterfaceDescriptor,
    ModuleDescriptor, ModuleName, ParameterDescriptor, TypeDefinitionDescriptor,
    TypeDefinitionKind, TypeDescriptor, VariantCaseDescriptor, Version,
};
pub use husk_value::OwnedValue;
#[cfg(feature = "wasm-extensions")]
pub use husk_wasm::{
    WasmCompileOptions, WasmComponent, WasmDescriptorError, WasmInstance, WasmLimits,
    normalize_wit_name,
};

/// A dynamically typed value crossing the Husk/host boundary.
#[derive(Debug, Clone)]
pub enum Value {
    /// Husk's no-value result.
    Unit,
    /// Explicit JSON-like null.
    Null,
    /// Boolean value.
    Bool(bool),
    /// Signed integer value.
    Int(i64),
    /// Floating-point value.
    Float(f64),
    /// Owned UTF-8 string.
    String(String),
    /// Shared array whose cloning does not recursively clone its elements.
    Array(Arc<Vec<Value>>),
    Tuple(Arc<Vec<Value>>),
    Range {
        start: i64,
        end: i64,
        inclusive: bool,
    },
    /// Shared string-keyed object whose cloning does not recursively clone its fields.
    Object(Arc<BTreeMap<String, Value>>),
    Struct {
        type_name: String,
        fields: Arc<BTreeMap<String, Value>>,
    },
    Variant {
        type_name: String,
        case: String,
        fields: Arc<Vec<Value>>,
    },
    /// Opaque JSON from legacy host paths. New runtime values use `Array` and
    /// `Object` so cloning plugin state does not recursively clone JSON.
    Json(serde_json::Value),
    /// Reference to a loaded plugin function.
    Callback(Callback),
    /// A closure owned by this runtime instance.
    Closure(FunctionHandle),
    /// Deferred missing-field value that preserves diagnostic context.
    Missing(MissingValue),
}

/// A missing JSON field that remains null-like until code tries to use it as a
/// value. Keeping its origin makes chained field failures point at the first
/// wrong field instead of a later access on `null`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingValue {
    field: String,
    span: Span,
    available_fields: Vec<String>,
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Unit, Self::Unit)
            | (Self::Null, Self::Null)
            | (Self::Unit, Self::Null)
            | (Self::Null, Self::Unit) => true,
            (Self::Missing(_), Self::Null)
            | (Self::Null, Self::Missing(_))
            | (Self::Missing(_), Self::Unit)
            | (Self::Unit, Self::Missing(_))
            | (Self::Missing(_), Self::Missing(_)) => true,
            (Self::Bool(left), Self::Bool(right)) => left == right,
            (Self::Int(left), Self::Int(right)) => left == right,
            (Self::Float(left), Self::Float(right)) => left == right,
            (Self::String(left), Self::String(right)) => left == right,
            (Self::Array(left), Self::Array(right)) => left == right,
            (Self::Tuple(left), Self::Tuple(right)) => left == right,
            (
                Self::Range {
                    start: left_start,
                    end: left_end,
                    inclusive: left_inclusive,
                },
                Self::Range {
                    start: right_start,
                    end: right_end,
                    inclusive: right_inclusive,
                },
            ) => {
                left_start == right_start
                    && left_end == right_end
                    && left_inclusive == right_inclusive
            }
            (Self::Object(left), Self::Object(right)) => left == right,
            (
                Self::Struct {
                    type_name: left_name,
                    fields: left_fields,
                },
                Self::Struct {
                    type_name: right_name,
                    fields: right_fields,
                },
            ) => left_name == right_name && left_fields == right_fields,
            (
                Self::Variant {
                    type_name: left_type,
                    case: left_case,
                    fields: left_fields,
                },
                Self::Variant {
                    type_name: right_type,
                    case: right_case,
                    fields: right_fields,
                },
            ) => left_type == right_type && left_case == right_case && left_fields == right_fields,
            (Self::Json(left), Self::Json(right)) => left == right,
            (Self::Callback(left), Self::Callback(right)) => left == right,
            (Self::Closure(left), Self::Closure(right)) => left == right,
            (
                Self::Array(_)
                | Self::Tuple(_)
                | Self::Range { .. }
                | Self::Object(_)
                | Self::Struct { .. }
                | Self::Variant { .. },
                Self::Json(_),
            )
            | (
                Self::Json(_),
                Self::Array(_)
                | Self::Tuple(_)
                | Self::Range { .. }
                | Self::Object(_)
                | Self::Struct { .. }
                | Self::Variant { .. },
            ) => value_to_json(self) == value_to_json(other),
            _ => false,
        }
    }
}

impl Value {
    /// Returns the borrowed string when this value is [`Value::String`].
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    /// Returns the boolean when this value is [`Value::Bool`].
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    fn kind_name(&self) -> &'static str {
        match self {
            Self::Unit => "unit",
            Self::Null | Self::Missing(_) => "null",
            Self::Bool(_) => "bool",
            Self::Int(_) => "int",
            Self::Float(_) => "float",
            Self::String(_) => "string",
            Self::Array(_) => "array",
            Self::Tuple(_) => "tuple",
            Self::Range { .. } => "range",
            Self::Object(_) => "object",
            Self::Struct { .. } => "struct",
            Self::Variant { .. } => "enum variant",
            Self::Json(serde_json::Value::Array(_)) => "array",
            Self::Json(serde_json::Value::Object(_)) => "object",
            Self::Json(_) => "JSON value",
            Self::Callback(_) | Self::Closure(_) => "function",
        }
    }

    /// Converts JSON into Husk's shared runtime representation.
    #[must_use]
    pub fn from_json(value: serde_json::Value) -> Self {
        json_to_value(&value)
    }

    /// Converts a runtime value into its JSON boundary representation.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        value_to_json(self)
    }
}

/// A Husk function reference stored for commands and event listeners.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Callback {
    plugin: String,
    function: String,
    function_id: Option<FunctionId>,
    program_generation: Option<u64>,
}

/// Opaque identity of a closure allocated by a Husk runtime instance.
///
/// Handles include both instance and slot generations, so a handle copied out
/// of an instance cannot accidentally call a closure after that instance or
/// heap slot has been replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FunctionHandle {
    instance_generation: u64,
    slot: u32,
    slot_generation: u32,
}

impl FunctionHandle {
    /// Runtime-instance generation that owns this handle.
    #[must_use]
    pub const fn instance_generation(self) -> u64 {
        self.instance_generation
    }
}

impl Callback {
    /// Creates a function reference owned by `plugin`.
    #[must_use]
    pub fn new(plugin: impl Into<String>, function: impl Into<String>) -> Self {
        Self {
            plugin: plugin.into(),
            function: function.into(),
            function_id: None,
            program_generation: None,
        }
    }

    fn resolved(
        plugin: impl Into<String>,
        function: impl Into<String>,
        function_id: FunctionId,
        program_generation: u64,
    ) -> Self {
        Self {
            plugin: plugin.into(),
            function: function.into(),
            function_id: Some(function_id),
            program_generation: Some(program_generation),
        }
    }

    /// Returns the plugin that owns this callback.
    #[must_use]
    pub fn plugin(&self) -> &str {
        &self.plugin
    }

    #[must_use]
    pub fn function(&self) -> &str {
        &self.function
    }
}

/// Rust host operations callable from Husk.
pub trait Host {
    /// Writes a plugin-scoped diagnostic message.
    fn log(&mut self, message: &str);

    /// Dispatch a registered backend-neutral module function.
    ///
    /// Returning `None` means this host does not own the qualified path and
    /// lets the compatibility evaluator continue its local/built-in lookup.
    fn call_module(
        &mut self,
        _plugin: &str,
        _path: &str,
        _args: &[Value],
    ) -> Option<anyhow::Result<Value>> {
        None
    }

    /// Mark the start of replacement activation/state-import effects during a staged reload.
    fn begin_reload_replacement(&mut self, _program: &str) {}

    /// Mark the start of the previous plugin's teardown effects during a staged reload.
    /// Hosts that defer side effects can commit teardown before replacement setup.
    fn begin_reload_teardown(&mut self, _program: &str) {}
}

/// Resource limits enforced while producing a compiled artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompileLimits {
    /// Maximum accepted UTF-8 source length.
    pub max_source_bytes: usize,
    /// Maximum accepted number of top-level AST items.
    pub max_top_level_items: usize,
}

impl Default for CompileLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: usize::MAX,
            max_top_level_items: usize::MAX,
        }
    }
}

/// Inputs that affect parsing and semantic analysis.
#[derive(Debug, Clone)]
pub struct CompileOptions {
    /// Native language semantics or the historical JavaScript compatibility
    /// profile.
    pub semantic_profile: SemanticProfile,
    /// Enabled `cfg` predicates.
    pub cfg_flags: HashSet<String>,
    /// Trusted declaration ASTs made visible during type checking.
    pub declarations: Vec<File>,
    /// Typed external modules visible to both checking and later dispatch.
    pub modules: Vec<ModuleDescriptor>,
    /// Whether semantic errors reject the artifact.
    pub typecheck: bool,
    /// Whether the selected profile's prelude is visible.
    pub prelude: bool,
    /// Compile-time resource limits.
    pub limits: CompileLimits,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            semantic_profile: SemanticProfile::Native,
            cfg_flags: HashSet::new(),
            declarations: Vec::new(),
            modules: Vec::new(),
            typecheck: true,
            prelude: true,
            limits: CompileLimits::default(),
        }
    }
}

impl CompileOptions {
    /// Options that preserve the original production VM's parse-only load
    /// behavior while callers migrate to explicit semantic compilation.
    #[must_use]
    pub fn legacy_runtime_compatibility() -> Self {
        Self {
            semantic_profile: SemanticProfile::LegacyJavaScript,
            typecheck: false,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_typecheck(mut self, typecheck: bool) -> Self {
        self.typecheck = typecheck;
        self
    }

    #[must_use]
    pub fn with_profile(mut self, profile: SemanticProfile) -> Self {
        self.semantic_profile = profile;
        self
    }

    #[must_use]
    pub fn with_declaration(mut self, declaration: File) -> Self {
        self.declarations.push(declaration);
        self
    }

    #[must_use]
    pub fn with_module(mut self, module: ModuleDescriptor) -> Self {
        self.modules.push(module);
        self
    }

    #[must_use]
    pub fn with_cfg_flag(mut self, flag: impl Into<String>) -> Self {
        self.cfg_flags.insert(flag.into());
        self
    }
}

/// A parsed and optionally type-checked Husk program.
///
/// The source, AST, semantic maps, and executable function table are produced
/// by one compiler pass and travel together into the runtime.
#[derive(Debug, Clone)]
pub struct CompiledProgram {
    name: Arc<str>,
    functions: Arc<FunctionTable>,
    module_functions: Arc<HashMap<ModuleFunctionId, ModuleFunctionTarget>>,
    intrinsic_methods: Arc<HashMap<IntrinsicMethodId, IntrinsicMethodTarget>>,
    source: SourceFile,
    source_map: SourceMap,
    syntax: Arc<File>,
    semantic: Option<Arc<SemanticResult>>,
    module_semantics: Arc<BTreeMap<String, Arc<SemanticResult>>>,
    semantic_profile: SemanticProfile,
    modules: Arc<[ModuleDescriptor]>,
    source_modules: Arc<[ModuleDescriptor]>,
}

#[derive(Debug, Clone, Default)]
struct FunctionTable {
    entries: Vec<Function>,
    by_name: HashMap<String, FunctionId>,
    by_id: HashMap<FunctionId, usize>,
}

impl FunctionTable {
    fn get_by_name(&self, name: &str) -> Option<&Function> {
        self.id(name).and_then(|id| self.get(id))
    }

    fn get(&self, id: FunctionId) -> Option<&Function> {
        self.entries.get(*self.by_id.get(&id)?)
    }

    fn id(&self, name: &str) -> Option<FunctionId> {
        self.by_name.get(name).copied()
    }

    fn contains(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }
}

#[derive(Debug, Clone)]
struct ModuleFunctionTarget {
    path: String,
}

#[derive(Debug, Clone)]
struct IntrinsicMethodTarget {
    receiver_type: String,
    method: String,
}

/// Valid standalone `main` argument shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainArguments {
    None,
    Strings,
}

/// Valid standalone `main` return behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainResult {
    Unit,
    ExitCode,
    Result,
}

/// Checked standalone entry-point contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MainSignature {
    pub arguments: MainArguments,
    pub result: MainResult,
}

/// Stable, backend-neutral execution metadata for one compiled function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirFunctionSummary {
    pub id: FunctionId,
    pub qualified_name: String,
    pub node_count: u32,
    pub local_count: u32,
}

/// Expected outcome of a compiled `#[test]` function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestExpectation {
    Pass,
    Panic { expected: Option<String> },
}

/// One test discovered in a compiled script or package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestDescriptor {
    pub qualified_name: String,
    pub ignored: bool,
    pub expectation: TestExpectation,
}

impl CompiledProgram {
    /// Compile a Husk source file using explicit options.
    ///
    /// # Errors
    ///
    /// Returns one ordered, source-aware parser or semantic report.
    pub fn compile(
        name: impl Into<String>,
        source: &str,
        options: &CompileOptions,
    ) -> anyhow::Result<Self> {
        let name = name.into();
        Self::compile_at(name.clone(), format!("plugins/{name}.hk"), source, options)
    }

    /// Compile a Husk source file with a specific diagnostic display path.
    ///
    /// # Errors
    ///
    /// Returns one ordered, source-aware parser or semantic report.
    pub fn compile_at(
        name: impl Into<String>,
        path: impl Into<String>,
        source: &str,
        options: &CompileOptions,
    ) -> anyhow::Result<Self> {
        let name = name.into();
        let path = path.into();
        let source_file = SourceFile::new(path, source);
        if source.len() > options.limits.max_source_bytes {
            return Err(anyhow::Error::new(Report::new(
                Diagnostic::new(
                    "HUSK-C0001",
                    format!(
                        "Husk source is {} bytes; the compile limit is {} bytes",
                        source.len(),
                        options.limits.max_source_bytes
                    ),
                    source_file,
                    Span {
                        range: 0..source.len().min(1),
                        file: None,
                    },
                    "source exceeds the configured compile limit",
                )
                .with_note(format!("while compiling `{name}`")),
            )));
        }

        let parsed = husk_parser::parse_str(source);
        if !parsed.errors.is_empty() {
            let mut errors = parsed.errors;
            errors.sort_by_key(|error| (error.span.range.start, error.span.range.end));
            let diagnostics = errors
                .into_iter()
                .map(|error| {
                    Diagnostic::new(
                        "HUSK-P0001",
                        error.message,
                        source_file.clone(),
                        error.span,
                        "here",
                    )
                    .with_note(format!("while parsing plugin `{name}`"))
                })
                .collect::<Vec<_>>();
            return Err(anyhow::Error::new(Report::from_diagnostics(diagnostics)));
        }

        let file = parsed
            .file
            .ok_or_else(|| anyhow::anyhow!("parser did not return a file for `{name}`"))?;
        if file.items.len() > options.limits.max_top_level_items {
            let span = file.items.get(options.limits.max_top_level_items).map_or(
                Span {
                    range: 0..source.len().min(1),
                    file: None,
                },
                |item| item.span.clone(),
            );
            return Err(anyhow::Error::new(Report::new(
                Diagnostic::new(
                    "HUSK-C0002",
                    format!(
                        "Husk source has {} top-level items; the compile limit is {}",
                        file.items.len(),
                        options.limits.max_top_level_items
                    ),
                    source_file,
                    span,
                    "top-level item limit exceeded here",
                )
                .with_note(format!("while compiling `{name}`")),
            )));
        }

        let mut declarations = options.declarations.clone();
        for module in &options.modules {
            module.validate()?;
            declarations.push(module_declaration_ast(module)?);
        }

        let semantic = if options.typecheck {
            let result = husk_semantic::analyze_file_with_declarations_and_options(
                &file,
                &declarations,
                SemanticOptions {
                    prelude: options.prelude,
                    cfg_flags: options.cfg_flags.clone(),
                    profile: options.semantic_profile,
                    module_path: Vec::new(),
                },
            );
            let mut errors = result
                .symbols
                .errors
                .iter()
                .chain(&result.type_errors)
                .cloned()
                .collect::<Vec<_>>();
            errors.sort_by(|left, right| {
                (
                    left.span.range.start,
                    left.span.range.end,
                    left.message.as_str(),
                )
                    .cmp(&(
                        right.span.range.start,
                        right.span.range.end,
                        right.message.as_str(),
                    ))
            });
            if !errors.is_empty() {
                let diagnostics = errors
                    .into_iter()
                    .map(|error| {
                        Diagnostic::new(
                            "HUSK-T0001",
                            error.message,
                            source_file.clone(),
                            error.span,
                            "incompatible Husk expression",
                        )
                        .with_note(format!("while typechecking plugin `{name}`"))
                    })
                    .collect::<Vec<_>>();
                return Err(anyhow::Error::new(Report::from_diagnostics(diagnostics)));
            }
            Some(Arc::new(result))
        } else {
            None
        };

        let mut functions = HashMap::new();
        let empty_name_resolution = HashMap::new();
        let name_resolution = semantic
            .as_deref()
            .map(|result| &result.name_resolution)
            .unwrap_or(&empty_name_resolution);
        let empty_type_resolution = HashMap::new();
        let type_resolution = semantic
            .as_deref()
            .map(|result| &result.type_resolution)
            .unwrap_or(&empty_type_resolution);
        let empty_variant_calls = HashMap::new();
        let variant_calls = semantic
            .as_deref()
            .map(|result| &result.variant_calls)
            .unwrap_or(&empty_variant_calls);
        let empty_variant_patterns = HashMap::new();
        let variant_patterns = semantic
            .as_deref()
            .map(|result| &result.variant_patterns)
            .unwrap_or(&empty_variant_patterns);
        let imports = collect_function_imports(&file, &[]);
        for item in &file.items {
            if let ItemKind::Fn {
                name, params, body, ..
            } = &item.kind
            {
                functions.insert(
                    name.name.clone(),
                    Function {
                        qualified_name: name.name.clone(),
                        hir: lower_function(
                            params,
                            body,
                            name_resolution,
                            type_resolution,
                            variant_calls,
                            variant_patterns,
                        ),
                        module_path: Vec::new(),
                        imports: imports.clone(),
                        test: test_descriptor(item, &name.name),
                        receiver: None,
                    },
                );
            }
        }
        insert_impl_functions(&mut functions, &file, &[], &imports, semantic.as_deref())?;
        let FinalizedCallTables {
            functions,
            module_functions,
            intrinsic_methods,
        } = finalize_function_table(functions, &options.modules, options.semantic_profile)?;
        let source_map = SourceMap::single(source_file.clone());

        Ok(Self {
            name: name.into(),
            functions: Arc::new(functions),
            module_functions: module_functions.into(),
            intrinsic_methods: intrinsic_methods.into(),
            source: source_file,
            source_map,
            syntax: Arc::new(file),
            semantic,
            module_semantics: Arc::new(BTreeMap::new()),
            semantic_profile: options.semantic_profile,
            modules: options.modules.clone().into(),
            source_modules: Arc::from([]),
        })
    }

    /// Compile every source module in a deterministically resolved package.
    ///
    /// External native/Wasm modules come from `options.modules`; source-module
    /// descriptors are derived from public declarations and are used only for
    /// cross-file checking and local dispatch.
    pub fn compile_package(
        package: &ResolvedPackage,
        options: &CompileOptions,
    ) -> anyhow::Result<Self> {
        let source_modules = source_module_descriptors(package)?;
        let external_names = options
            .modules
            .iter()
            .map(|module| module.name.as_str())
            .collect::<HashSet<_>>();
        for module in &source_modules {
            if external_names.contains(module.name.as_str()) {
                anyhow::bail!(
                    "source module `{}` conflicts with a registered external module",
                    module.name
                );
            }
        }

        let mut declarations = options.declarations.clone();
        for module in options.modules.iter().chain(&source_modules) {
            module.validate()?;
            declarations.push(module_declaration_ast(module)?);
        }

        let mut functions = HashMap::new();
        let mut sources = Vec::new();
        let mut semantics = BTreeMap::new();
        let mut root_source = None;
        let mut root_syntax = None;
        let mut root_semantic = None;
        let mut diagnostics = Vec::new();

        for module in &package.modules {
            if module.source.len() > options.limits.max_source_bytes {
                anyhow::bail!(
                    "Husk module `{}` is {} bytes; the compile limit is {} bytes",
                    module.display_path.display(),
                    module.source.len(),
                    options.limits.max_source_bytes
                );
            }
            if module.syntax.items.len() > options.limits.max_top_level_items {
                anyhow::bail!(
                    "Husk module `{}` has {} top-level items; the compile limit is {}",
                    module.display_path.display(),
                    module.syntax.items.len(),
                    options.limits.max_top_level_items
                );
            }
            let source = SourceFile::new(
                module.display_path.to_string_lossy().into_owned(),
                Arc::clone(&module.source),
            );
            sources.push(source.clone());

            let semantic = if options.typecheck {
                let result = husk_semantic::analyze_file_with_declarations_and_options(
                    &module.syntax,
                    &declarations,
                    SemanticOptions {
                        prelude: options.prelude,
                        cfg_flags: options.cfg_flags.clone(),
                        profile: options.semantic_profile,
                        module_path: module.module_path.clone(),
                    },
                );
                let mut errors = result
                    .symbols
                    .errors
                    .iter()
                    .chain(&result.type_errors)
                    .cloned()
                    .collect::<Vec<_>>();
                errors.sort_by(|left, right| {
                    (
                        left.span.range.start,
                        left.span.range.end,
                        left.message.as_str(),
                    )
                        .cmp(&(
                            right.span.range.start,
                            right.span.range.end,
                            right.message.as_str(),
                        ))
                });
                diagnostics.extend(errors.into_iter().map(|error| {
                    Diagnostic::new(
                        "HUSK-T0001",
                        error.message,
                        source.clone(),
                        error.span,
                        "incompatible Husk expression",
                    )
                    .with_note(format!(
                        "while typechecking module `{}`",
                        display_source_module(&module.module_path)
                    ))
                }));
                Some(Arc::new(result))
            } else {
                None
            };

            let empty_name_resolution = HashMap::new();
            let name_resolution = semantic
                .as_deref()
                .map(|result| &result.name_resolution)
                .unwrap_or(&empty_name_resolution);
            let empty_type_resolution = HashMap::new();
            let type_resolution = semantic
                .as_deref()
                .map(|result| &result.type_resolution)
                .unwrap_or(&empty_type_resolution);
            let empty_variant_calls = HashMap::new();
            let variant_calls = semantic
                .as_deref()
                .map(|result| &result.variant_calls)
                .unwrap_or(&empty_variant_calls);
            let empty_variant_patterns = HashMap::new();
            let variant_patterns = semantic
                .as_deref()
                .map(|result| &result.variant_patterns)
                .unwrap_or(&empty_variant_patterns);
            let imports = collect_function_imports(&module.syntax, &module.module_path);
            for item in &module.syntax.items {
                let ItemKind::Fn {
                    name, params, body, ..
                } = &item.kind
                else {
                    continue;
                };
                let key = if module.module_path.is_empty() {
                    name.name.clone()
                } else {
                    format!("{}::{}", module.module_path.join("::"), name.name)
                };
                let function = Function {
                    qualified_name: key.clone(),
                    hir: lower_function(
                        params,
                        body,
                        name_resolution,
                        type_resolution,
                        variant_calls,
                        variant_patterns,
                    ),
                    module_path: module.module_path.clone(),
                    imports: imports.clone(),
                    test: test_descriptor(item, &key),
                    receiver: None,
                };
                if functions.insert(key.clone(), function).is_some() {
                    anyhow::bail!("duplicate package function `{key}`");
                }
            }
            insert_impl_functions(
                &mut functions,
                &module.syntax,
                &module.module_path,
                &imports,
                semantic.as_deref(),
            )?;

            let module_name = display_source_module(&module.module_path);
            if let Some(semantic) = semantic {
                if module.module_path.is_empty() {
                    root_semantic = Some(Arc::clone(&semantic));
                }
                semantics.insert(module_name, semantic);
            }
            if module.module_path.is_empty() {
                root_source = Some(source);
                root_syntax = Some(Arc::new(module.syntax.clone()));
            }
        }

        if !diagnostics.is_empty() {
            return Err(anyhow::Error::new(Report::from_diagnostics(diagnostics)));
        }
        let FinalizedCallTables {
            functions,
            module_functions,
            intrinsic_methods,
        } = finalize_function_table(functions, &options.modules, options.semantic_profile)?;
        let source = root_source.ok_or_else(|| anyhow::anyhow!("package has no entry module"))?;
        let syntax = root_syntax.ok_or_else(|| anyhow::anyhow!("package has no entry syntax"))?;
        let source_map = SourceMap::new(sources);
        Ok(Self {
            name: package.manifest.package.name.clone().into(),
            functions: Arc::new(functions),
            module_functions: module_functions.into(),
            intrinsic_methods: intrinsic_methods.into(),
            source,
            source_map,
            syntax,
            semantic: root_semantic,
            module_semantics: Arc::new(semantics),
            semantic_profile: options.semantic_profile,
            modules: options.modules.clone().into(),
            source_modules: source_modules.into(),
        })
    }

    /// Parse a Husk source file into a program.
    ///
    /// # Errors
    ///
    /// Returns all parser diagnostics as a single error until the diagnostic
    /// API grows a richer type.
    pub fn parse(name: impl Into<String>, source: &str) -> anyhow::Result<Self> {
        let name = name.into();
        Self::parse_at(name.clone(), format!("plugins/{name}.hk"), source)
    }

    /// Parse a Husk source file using a specific display path for diagnostics.
    ///
    /// # Errors
    ///
    /// Returns all parser diagnostics in one source-aware report.
    pub fn parse_at(
        name: impl Into<String>,
        path: impl Into<String>,
        source: &str,
    ) -> anyhow::Result<Self> {
        Self::compile_at(
            name,
            path,
            source,
            &CompileOptions::legacy_runtime_compatibility(),
        )
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn source(&self) -> &SourceFile {
        &self.source
    }

    #[must_use]
    pub fn source_map(&self) -> &SourceMap {
        &self.source_map
    }

    #[must_use]
    pub fn syntax(&self) -> &File {
        &self.syntax
    }

    #[must_use]
    pub fn semantic_result(&self) -> Option<&SemanticResult> {
        self.semantic.as_deref()
    }

    /// Semantic results for every source module in a compiled package.
    #[must_use]
    pub fn module_semantic_results(&self) -> &BTreeMap<String, Arc<SemanticResult>> {
        &self.module_semantics
    }

    #[must_use]
    pub fn semantic_profile(&self) -> SemanticProfile {
        self.semantic_profile
    }

    #[must_use]
    pub fn modules(&self) -> &[ModuleDescriptor] {
        &self.modules
    }

    /// Descriptors derived from public source-module exports.
    #[must_use]
    pub fn source_modules(&self) -> &[ModuleDescriptor] {
        &self.source_modules
    }

    /// Summarize the HIR that execution will consume.
    #[must_use]
    pub fn hir_functions(&self) -> Vec<HirFunctionSummary> {
        let mut functions = self
            .functions
            .entries
            .iter()
            .map(|function| HirFunctionSummary {
                id: self
                    .functions
                    .id(&function.qualified_name)
                    .expect("compiled function table indexes every entry"),
                qualified_name: function.qualified_name.clone(),
                node_count: function.hir.node_count,
                local_count: function.hir.local_count,
            })
            .collect::<Vec<_>>();
        functions.sort_unstable_by(|left, right| left.qualified_name.cmp(&right.qualified_name));
        functions
    }

    /// Discover source functions marked with `#[test]`.
    #[must_use]
    pub fn tests(&self) -> Vec<TestDescriptor> {
        let mut tests = self
            .functions
            .entries
            .iter()
            .filter_map(|function| function.test.clone())
            .collect::<Vec<_>>();
        tests.sort_unstable_by(|left, right| left.qualified_name.cmp(&right.qualified_name));
        tests
    }

    /// Inspect the standalone entry point after semantic validation.
    #[must_use]
    pub fn main_signature(&self) -> Option<MainSignature> {
        self.syntax.items.iter().find_map(|item| {
            let ItemKind::Fn {
                name,
                params,
                ret_type,
                ..
            } = &item.kind
            else {
                return None;
            };
            if name.name != "main" {
                return None;
            }

            let arguments = if params.is_empty() {
                MainArguments::None
            } else {
                MainArguments::Strings
            };
            let result = match ret_type.as_ref().map(|ty| &ty.kind) {
                None => MainResult::Unit,
                Some(TypeExprKind::Named(name)) if name.name == "i32" => MainResult::ExitCode,
                Some(TypeExprKind::Generic { name, .. }) if name.name == "Result" => {
                    MainResult::Result
                }
                _ => MainResult::Unit,
            };
            Some(MainSignature { arguments, result })
        })
    }
}

fn display_source_module(path: &[String]) -> String {
    if path.is_empty() {
        "crate".to_string()
    } else {
        format!("crate::{}", path.join("::"))
    }
}

#[derive(Default)]
struct SourceModuleDescriptorBuilder {
    functions: Vec<FunctionDescriptor>,
    types: Vec<TypeDefinitionDescriptor>,
    interfaces: BTreeMap<String, SourceInterfaceDescriptorBuilder>,
}

#[derive(Default)]
struct SourceInterfaceDescriptorBuilder {
    functions: Vec<FunctionDescriptor>,
    types: Vec<TypeDefinitionDescriptor>,
}

fn source_module_descriptors(package: &ResolvedPackage) -> anyhow::Result<Vec<ModuleDescriptor>> {
    let mut named_types = HashMap::new();
    for module in package
        .modules
        .iter()
        .filter(|module| !module.module_path.is_empty())
    {
        for item in &module.syntax.items {
            let (name, is_public) = match &item.kind {
                ItemKind::Struct { name, .. }
                | ItemKind::Enum { name, .. }
                | ItemKind::TypeAlias { name, .. } => {
                    (&name.name, item.visibility == Visibility::Public)
                }
                _ => continue,
            };
            if is_public {
                named_types.insert(
                    (module.module_path.clone(), name.clone()),
                    package_type_name(package, &module.module_path, name),
                );
            }
        }
    }

    let mut builders = BTreeMap::<String, SourceModuleDescriptorBuilder>::new();
    for module in package
        .modules
        .iter()
        .filter(|module| !module.module_path.is_empty())
    {
        let root = module.module_path[0].clone();
        if root == "std" {
            anyhow::bail!("`std` is reserved and cannot be a package source module");
        }
        let interface = (module.module_path.len() > 1).then(|| module.module_path[1..].join("_"));
        let builder = builders.entry(root).or_default();

        for item in &module.syntax.items {
            if item.visibility != Visibility::Public {
                continue;
            }
            match &item.kind {
                ItemKind::Fn {
                    name,
                    type_params,
                    params,
                    ret_type,
                    ..
                } => {
                    if !type_params.is_empty() {
                        anyhow::bail!(
                            "public package function `{}` cannot be generic yet",
                            name.name
                        );
                    }
                    let parameters = params
                        .iter()
                        .map(|parameter| {
                            ParameterDescriptor::new(
                                parameter.name.name.clone(),
                                source_type_descriptor(
                                    &parameter.ty,
                                    &module.module_path,
                                    &named_types,
                                )?,
                            )
                            .map_err(anyhow::Error::from)
                        })
                        .collect::<anyhow::Result<Vec<_>>>()?;
                    let result = ret_type
                        .as_ref()
                        .map(|ty| source_type_descriptor(ty, &module.module_path, &named_types))
                        .transpose()?
                        .unwrap_or(TypeDescriptor::Unit);
                    let function = FunctionDescriptor::new(name.name.clone(), parameters, result)?;
                    match &interface {
                        Some(interface) => builder
                            .interfaces
                            .entry(interface.clone())
                            .or_default()
                            .functions
                            .push(function),
                        None => builder.functions.push(function),
                    }
                }
                ItemKind::Struct { name, fields, .. } => {
                    let definition = TypeDefinitionDescriptor::new(
                        named_types
                            .get(&(module.module_path.clone(), name.name.clone()))
                            .expect("public type name was indexed")
                            .clone(),
                        TypeDefinitionKind::Record(
                            fields
                                .iter()
                                .map(|field| {
                                    FieldDescriptor::new(
                                        field.name.name.clone(),
                                        source_type_descriptor(
                                            &field.ty,
                                            &module.module_path,
                                            &named_types,
                                        )?,
                                    )
                                    .map_err(anyhow::Error::from)
                                })
                                .collect::<anyhow::Result<Vec<_>>>()?,
                        ),
                    )?;
                    push_source_type(builder, interface.as_deref(), definition);
                }
                ItemKind::Enum { name, variants, .. } => {
                    let cases = variants
                        .iter()
                        .map(|variant| {
                            let payload = match &variant.fields {
                                EnumVariantFields::Unit => None,
                                EnumVariantFields::Tuple(types) if types.len() == 1 => Some(
                                    source_type_descriptor(
                                        &types[0],
                                        &module.module_path,
                                        &named_types,
                                    )?,
                                ),
                                EnumVariantFields::Tuple(types) => Some(TypeDescriptor::Tuple(
                                    types
                                        .iter()
                                        .map(|ty| {
                                            source_type_descriptor(
                                                ty,
                                                &module.module_path,
                                                &named_types,
                                            )
                                        })
                                        .collect::<anyhow::Result<Vec<_>>>()?,
                                )),
                                EnumVariantFields::Struct(_) => {
                                    anyhow::bail!(
                                        "public struct-like enum variant `{}::{}` is not yet supported across modules",
                                        name.name,
                                        variant.name.name
                                    )
                                }
                            };
                            VariantCaseDescriptor::new(variant.name.name.clone(), payload)
                                .map_err(anyhow::Error::from)
                        })
                        .collect::<anyhow::Result<Vec<_>>>()?;
                    let definition = TypeDefinitionDescriptor::new(
                        named_types
                            .get(&(module.module_path.clone(), name.name.clone()))
                            .expect("public type name was indexed")
                            .clone(),
                        TypeDefinitionKind::Variant(cases),
                    )?;
                    push_source_type(builder, interface.as_deref(), definition);
                }
                ItemKind::TypeAlias { name, ty } => {
                    let definition = TypeDefinitionDescriptor::new(
                        named_types
                            .get(&(module.module_path.clone(), name.name.clone()))
                            .expect("public type name was indexed")
                            .clone(),
                        TypeDefinitionKind::Alias(source_type_descriptor(
                            ty,
                            &module.module_path,
                            &named_types,
                        )?),
                    )?;
                    push_source_type(builder, interface.as_deref(), definition);
                }
                ItemKind::Mod { .. }
                | ItemKind::ExternBlock { .. }
                | ItemKind::Use { .. }
                | ItemKind::Trait(_)
                | ItemKind::Impl(_) => {}
            }
        }
    }

    builders
        .into_iter()
        .map(|(name, builder)| {
            let interfaces = builder
                .interfaces
                .into_iter()
                .map(|(name, interface)| {
                    InterfaceDescriptor::new(name, interface.functions)?
                        .with_types(interface.types)
                        .map_err(anyhow::Error::from)
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            ModuleDescriptor::new(
                name,
                package.manifest.package.version.clone(),
                builder.functions,
                interfaces,
            )?
            .with_types(builder.types)
            .map_err(Into::into)
        })
        .collect()
}

fn push_source_type(
    builder: &mut SourceModuleDescriptorBuilder,
    interface: Option<&str>,
    definition: TypeDefinitionDescriptor,
) {
    match interface {
        Some(interface) => builder
            .interfaces
            .entry(interface.to_string())
            .or_default()
            .types
            .push(definition),
        None => builder.types.push(definition),
    }
}

fn package_type_name(package: &ResolvedPackage, module_path: &[String], name: &str) -> String {
    let package = package
        .manifest
        .package
        .name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("{package}_{}_{}", module_path.join("_"), name)
}

fn source_type_descriptor(
    ty: &TypeExpr,
    module_path: &[String],
    named_types: &HashMap<(Vec<String>, String), String>,
) -> anyhow::Result<TypeDescriptor> {
    match &ty.kind {
        TypeExprKind::Named(name) => match name.name.as_str() {
            "i32" => Ok(TypeDescriptor::I32),
            "i64" => Ok(TypeDescriptor::I64),
            "f64" => Ok(TypeDescriptor::F64),
            "bool" => Ok(TypeDescriptor::Bool),
            "String" => Ok(TypeDescriptor::String),
            "Json" | "JsValue" => Ok(TypeDescriptor::Json),
            local => Ok(TypeDescriptor::Named(
                named_types
                    .get(&(module_path.to_vec(), local.to_string()))
                    .cloned()
                    .unwrap_or_else(|| local.to_string()),
            )),
        },
        TypeExprKind::Array(element) => Ok(TypeDescriptor::list(source_type_descriptor(
            element,
            module_path,
            named_types,
        )?)),
        TypeExprKind::Tuple(elements) if elements.is_empty() => Ok(TypeDescriptor::Unit),
        TypeExprKind::Tuple(elements) => Ok(TypeDescriptor::Tuple(
            elements
                .iter()
                .map(|element| source_type_descriptor(element, module_path, named_types))
                .collect::<anyhow::Result<Vec<_>>>()?,
        )),
        TypeExprKind::Generic { name, args } if name.name == "Option" && args.len() == 1 => Ok(
            TypeDescriptor::option(source_type_descriptor(&args[0], module_path, named_types)?),
        ),
        TypeExprKind::Generic { name, args } if name.name == "Result" && args.len() == 2 => {
            Ok(TypeDescriptor::result(
                source_type_descriptor(&args[0], module_path, named_types)?,
                source_type_descriptor(&args[1], module_path, named_types)?,
            ))
        }
        TypeExprKind::Generic { name, .. } => {
            anyhow::bail!("unsupported public generic type `{}`", name.name)
        }
        TypeExprKind::Function { .. } => {
            anyhow::bail!("function types cannot cross a package module boundary")
        }
        TypeExprKind::ImplTrait { .. } => {
            anyhow::bail!("`impl Trait` cannot cross a package module boundary")
        }
    }
}

fn module_declaration_ast(module: &ModuleDescriptor) -> anyhow::Result<File> {
    let mut source = String::new();
    for definition in module.types.iter().chain(
        module
            .interfaces
            .iter()
            .flat_map(|interface| &interface.types),
    ) {
        push_type_definition(&mut source, definition);
    }
    source.push_str("extern \"husk\" {\n");
    if module_uses_json(module) {
        source.push_str("    struct Json;\n");
    }
    source.push_str("    mod global ");
    source.push_str(module.name.as_str());
    source.push_str(" {\n");

    let mut names = HashSet::new();
    for function in &module.functions {
        if !names.insert(function.name.as_str()) {
            anyhow::bail!(
                "duplicate dispatch name `{}` in module `{}`",
                function.name,
                module.name
            );
        }
        push_function_declaration(&mut source, function);
    }
    for interface in &module.interfaces {
        for function in &interface.functions {
            if !names.insert(function.name.as_str()) {
                anyhow::bail!(
                    "qualified functions in module `{}` normalize to duplicate dispatch name `{}`",
                    module.name,
                    function.name
                );
            }
            push_function_declaration(&mut source, function);
        }
    }
    source.push_str("    }\n}\n");

    let parsed = husk_parser::parse_str(&source);
    if !parsed.errors.is_empty() {
        let messages = parsed
            .errors
            .iter()
            .map(|error| error.message.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        anyhow::bail!(
            "module descriptor `{}` could not become a Husk declaration: {messages}",
            module.name
        );
    }
    parsed
        .file
        .ok_or_else(|| anyhow::anyhow!("module descriptor parser returned no declaration AST"))
}

fn push_type_definition(source: &mut String, definition: &TypeDefinitionDescriptor) {
    match &definition.kind {
        TypeDefinitionKind::Alias(ty) => {
            source.push_str("type ");
            source.push_str(&definition.name);
            source.push_str(" = ");
            push_type_declaration(source, ty);
            source.push_str(";\n");
        }
        TypeDefinitionKind::Record(fields) => {
            source.push_str("struct ");
            source.push_str(&definition.name);
            source.push_str(" {\n");
            for field in fields {
                source.push_str("    ");
                source.push_str(&field.name);
                source.push_str(": ");
                push_type_declaration(source, &field.ty);
                source.push_str(",\n");
            }
            source.push_str("}\n");
        }
        TypeDefinitionKind::Enum(cases) => {
            source.push_str("enum ");
            source.push_str(&definition.name);
            source.push_str(" {\n");
            for case in cases {
                source.push_str("    ");
                source.push_str(case);
                source.push_str(",\n");
            }
            source.push_str("}\n");
        }
        TypeDefinitionKind::Variant(cases) => {
            source.push_str("enum ");
            source.push_str(&definition.name);
            source.push_str(" {\n");
            for case in cases {
                source.push_str("    ");
                source.push_str(&case.name);
                if let Some(payload) = &case.payload {
                    source.push('(');
                    push_type_declaration(source, payload);
                    source.push(')');
                }
                source.push_str(",\n");
            }
            source.push_str("}\n");
        }
    }
}

fn push_function_declaration(source: &mut String, function: &FunctionDescriptor) {
    source.push_str("        fn ");
    source.push_str(&function.name);
    source.push('(');
    for (index, parameter) in function.parameters.iter().enumerate() {
        if index > 0 {
            source.push_str(", ");
        }
        source.push_str(&parameter.name);
        source.push_str(": ");
        push_type_declaration(source, &parameter.ty);
    }
    source.push_str(") -> ");
    push_type_declaration(source, &function.result);
    source.push_str(";\n");
}

fn push_type_declaration(source: &mut String, ty: &TypeDescriptor) {
    match ty {
        TypeDescriptor::Unit => source.push_str("()"),
        TypeDescriptor::Bool => source.push_str("bool"),
        TypeDescriptor::I32 => source.push_str("i32"),
        TypeDescriptor::I64 => source.push_str("i64"),
        TypeDescriptor::F64 => source.push_str("f64"),
        TypeDescriptor::String => source.push_str("String"),
        TypeDescriptor::Json => source.push_str("Json"),
        TypeDescriptor::List(element) => {
            source.push('[');
            push_type_declaration(source, element);
            source.push(']');
        }
        TypeDescriptor::Tuple(elements) => {
            source.push('(');
            for (index, element) in elements.iter().enumerate() {
                if index > 0 {
                    source.push_str(", ");
                }
                push_type_declaration(source, element);
            }
            source.push(')');
        }
        TypeDescriptor::Option(element) => {
            source.push_str("Option<");
            push_type_declaration(source, element);
            source.push('>');
        }
        TypeDescriptor::Result { ok, error } => {
            source.push_str("Result<");
            push_type_declaration(source, ok);
            source.push_str(", ");
            push_type_declaration(source, error);
            source.push('>');
        }
        TypeDescriptor::Named(name) => source.push_str(name),
    }
}

fn module_uses_json(module: &ModuleDescriptor) -> bool {
    let function_uses_json = module
        .functions
        .iter()
        .chain(
            module
                .interfaces
                .iter()
                .flat_map(|interface| &interface.functions),
        )
        .any(|function| {
            type_uses_json(&function.result)
                || function
                    .parameters
                    .iter()
                    .any(|parameter| type_uses_json(&parameter.ty))
        });
    function_uses_json
        || module
            .types
            .iter()
            .chain(
                module
                    .interfaces
                    .iter()
                    .flat_map(|interface| &interface.types),
            )
            .any(type_definition_uses_json)
}

fn type_definition_uses_json(definition: &TypeDefinitionDescriptor) -> bool {
    match &definition.kind {
        TypeDefinitionKind::Alias(ty) => type_uses_json(ty),
        TypeDefinitionKind::Record(fields) => fields.iter().any(|field| type_uses_json(&field.ty)),
        TypeDefinitionKind::Enum(_) => false,
        TypeDefinitionKind::Variant(cases) => cases
            .iter()
            .filter_map(|case| case.payload.as_ref())
            .any(type_uses_json),
    }
}

fn type_uses_json(ty: &TypeDescriptor) -> bool {
    match ty {
        TypeDescriptor::Json => true,
        TypeDescriptor::List(element) | TypeDescriptor::Option(element) => type_uses_json(element),
        TypeDescriptor::Tuple(elements) => elements.iter().any(type_uses_json),
        TypeDescriptor::Result { ok, error } => type_uses_json(ok) || type_uses_json(error),
        TypeDescriptor::Unit
        | TypeDescriptor::Bool
        | TypeDescriptor::I32
        | TypeDescriptor::I64
        | TypeDescriptor::F64
        | TypeDescriptor::String
        | TypeDescriptor::Named(_) => false,
    }
}

/// Compatibility name for callers that previously parsed a `Program`.
pub type Program = CompiledProgram;

#[derive(Debug, Clone)]
struct Function {
    qualified_name: String,
    hir: HirFunction,
    module_path: Vec<String>,
    imports: HashMap<String, String>,
    test: Option<TestDescriptor>,
    receiver: Option<SelfReceiver>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CellHandle {
    slot: u32,
    generation: u32,
}

#[derive(Debug, Clone)]
struct ClosureObject {
    parameters: Vec<husk_hir::ClosureParameter>,
    body: Expr,
    captures: HashMap<LocalId, CellHandle>,
    plugin: String,
    module_path: Vec<String>,
    imports: HashMap<String, String>,
}

#[derive(Debug, Clone)]
enum HeapObject {
    Cell(Value),
    Closure(Box<ClosureObject>),
}

#[derive(Debug, Clone)]
struct HeapSlot {
    generation: u32,
    marked: bool,
    object: Option<HeapObject>,
}

#[derive(Debug, Clone)]
struct RuntimeHeap {
    slots: Vec<HeapSlot>,
    free: Vec<u32>,
    live_objects: usize,
    live_bytes: usize,
    max_objects: usize,
    max_bytes: usize,
}

impl Default for RuntimeHeap {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            live_objects: 0,
            live_bytes: 0,
            max_objects: 1_000_000,
            max_bytes: 64 * 1024 * 1024,
        }
    }
}

impl RuntimeHeap {
    fn set_limits(&mut self, max_objects: usize, max_bytes: usize) {
        self.max_objects = max_objects;
        self.max_bytes = max_bytes;
    }

    fn allocate(&mut self, object: HeapObject) -> anyhow::Result<CellHandle> {
        if self.live_objects >= self.max_objects {
            anyhow::bail!(
                "Husk heap object limit exceeded (maximum {})",
                self.max_objects
            );
        }
        let object_bytes = heap_object_size(&object);
        let next_bytes = self.live_bytes.saturating_add(object_bytes);
        if next_bytes > self.max_bytes {
            anyhow::bail!(
                "Husk heap byte limit exceeded (requested {object_bytes} bytes with {} of {} bytes live)",
                self.live_bytes,
                self.max_bytes
            );
        }
        self.live_objects += 1;
        self.live_bytes = next_bytes;
        if let Some(slot_index) = self.free.pop() {
            let slot = self
                .slots
                .get_mut(slot_index as usize)
                .expect("free heap slot index is valid");
            debug_assert!(slot.object.is_none());
            slot.object = Some(object);
            return Ok(CellHandle {
                slot: slot_index,
                generation: slot.generation,
            });
        }

        let slot = u32::try_from(self.slots.len())
            .map_err(|_| anyhow::anyhow!("Husk heap cannot address another object"))?;
        self.slots.push(HeapSlot {
            generation: 1,
            marked: false,
            object: Some(object),
        });
        Ok(CellHandle {
            slot,
            generation: 1,
        })
    }

    fn allocate_cell(&mut self, value: Value) -> anyhow::Result<CellHandle> {
        self.allocate(HeapObject::Cell(value))
    }

    fn allocate_closure(
        &mut self,
        instance_generation: u64,
        closure: ClosureObject,
    ) -> anyhow::Result<FunctionHandle> {
        let handle = self.allocate(HeapObject::Closure(Box::new(closure)))?;
        Ok(FunctionHandle {
            instance_generation,
            slot: handle.slot,
            slot_generation: handle.generation,
        })
    }

    fn cell(&self, handle: CellHandle) -> anyhow::Result<&Value> {
        match self.object(handle)? {
            HeapObject::Cell(value) => Ok(value),
            HeapObject::Closure(_) => anyhow::bail!("Husk heap handle does not refer to a cell"),
        }
    }

    fn set_cell(&mut self, handle: CellHandle, value: Value) -> anyhow::Result<()> {
        let old_bytes = match self.object(handle)? {
            HeapObject::Cell(current) => heap_value_size(current),
            HeapObject::Closure(_) => {
                anyhow::bail!("Husk heap handle does not refer to a cell");
            }
        };
        let new_bytes = heap_value_size(&value);
        let next_bytes = self
            .live_bytes
            .saturating_sub(old_bytes)
            .saturating_add(new_bytes);
        if next_bytes > self.max_bytes {
            anyhow::bail!(
                "Husk heap byte limit exceeded (value needs {new_bytes} bytes with {} of {} bytes live)",
                self.live_bytes.saturating_sub(old_bytes),
                self.max_bytes
            );
        }
        let HeapObject::Cell(current) = self.object_mut(handle)? else {
            unreachable!("cell kind was checked before taking a mutable borrow");
        };
        *current = value;
        self.live_bytes = next_bytes;
        Ok(())
    }

    fn closure(
        &self,
        handle: FunctionHandle,
        instance_generation: u64,
    ) -> anyhow::Result<&ClosureObject> {
        if handle.instance_generation != instance_generation {
            anyhow::bail!("stale Husk function handle belongs to another instance generation");
        }
        let raw = CellHandle {
            slot: handle.slot,
            generation: handle.slot_generation,
        };
        match self.object(raw)? {
            HeapObject::Closure(closure) => Ok(closure),
            HeapObject::Cell(_) => {
                anyhow::bail!("Husk function handle does not refer to a closure")
            }
        }
    }

    fn object(&self, handle: CellHandle) -> anyhow::Result<&HeapObject> {
        let Some(slot) = self.slots.get(handle.slot as usize) else {
            anyhow::bail!("stale Husk heap handle has an invalid slot");
        };
        if slot.generation != handle.generation {
            anyhow::bail!("stale Husk heap handle generation");
        }
        slot.object
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("stale Husk heap handle refers to a freed object"))
    }

    fn object_mut(&mut self, handle: CellHandle) -> anyhow::Result<&mut HeapObject> {
        let Some(slot) = self.slots.get_mut(handle.slot as usize) else {
            anyhow::bail!("stale Husk heap handle has an invalid slot");
        };
        if slot.generation != handle.generation {
            anyhow::bail!("stale Husk heap handle generation");
        }
        slot.object
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("stale Husk heap handle refers to a freed object"))
    }

    fn free_cell(&mut self, handle: CellHandle) {
        let Some(slot) = self.slots.get_mut(handle.slot as usize) else {
            return;
        };
        if slot.generation != handle.generation || !matches!(slot.object, Some(HeapObject::Cell(_)))
        {
            return;
        }
        let object_bytes = slot.object.as_ref().map_or(0, heap_object_size);
        slot.object = None;
        slot.generation = slot.generation.wrapping_add(1).max(1);
        self.free.push(handle.slot);
        self.live_objects = self.live_objects.saturating_sub(1);
        self.live_bytes = self.live_bytes.saturating_sub(object_bytes);
    }

    fn reachable_handles(
        &self,
        function_roots: &[FunctionHandle],
        cell_roots: impl IntoIterator<Item = CellHandle>,
        instance_generation: u64,
    ) -> HashSet<CellHandle> {
        let mut reachable = HashSet::new();
        let mut pending = function_roots
            .iter()
            .filter(|handle| handle.instance_generation == instance_generation)
            .map(|handle| CellHandle {
                slot: handle.slot,
                generation: handle.slot_generation,
            })
            .chain(cell_roots)
            .collect::<Vec<_>>();
        while let Some(handle) = pending.pop() {
            if !reachable.insert(handle) {
                continue;
            }
            let Ok(object) = self.object(handle) else {
                continue;
            };
            match object {
                HeapObject::Cell(value) => {
                    collect_heap_handles(value, instance_generation, &mut pending);
                }
                HeapObject::Closure(closure) => {
                    pending.extend(closure.captures.values().copied());
                }
            }
        }
        reachable
    }

    fn collect(&mut self, roots: &[FunctionHandle], instance_generation: u64) {
        let mut pending = roots
            .iter()
            .filter(|handle| handle.instance_generation == instance_generation)
            .map(|handle| CellHandle {
                slot: handle.slot,
                generation: handle.slot_generation,
            })
            .collect::<Vec<_>>();
        while let Some(handle) = pending.pop() {
            let Some(slot) = self.slots.get_mut(handle.slot as usize) else {
                continue;
            };
            if slot.generation != handle.generation || slot.marked || slot.object.is_none() {
                continue;
            }
            slot.marked = true;
            match slot.object.as_ref().expect("checked above") {
                HeapObject::Cell(value) => {
                    collect_heap_handles(value, instance_generation, &mut pending);
                }
                HeapObject::Closure(closure) => {
                    pending.extend(closure.captures.values().copied());
                }
            }
        }

        for (index, slot) in self.slots.iter_mut().enumerate() {
            if slot.object.is_none() {
                continue;
            }
            if slot.marked {
                slot.marked = false;
                continue;
            }
            let object_bytes = slot.object.as_ref().map_or(0, heap_object_size);
            slot.object = None;
            slot.generation = slot.generation.wrapping_add(1).max(1);
            self.free
                .push(u32::try_from(index).expect("allocated heap indices fit in u32"));
            self.live_objects = self.live_objects.saturating_sub(1);
            self.live_bytes = self.live_bytes.saturating_sub(object_bytes);
        }
    }
}

fn heap_object_size(object: &HeapObject) -> usize {
    std::mem::size_of::<HeapObject>().saturating_add(match object {
        HeapObject::Cell(value) => heap_value_size(value),
        HeapObject::Closure(closure) => {
            closure
                .plugin
                .len()
                .saturating_add(closure.module_path.iter().map(String::len).sum::<usize>())
                .saturating_add(
                    closure
                        .imports
                        .iter()
                        .map(|(name, path)| name.len().saturating_add(path.len()))
                        .sum::<usize>(),
                )
                .saturating_add(
                    closure
                        .parameters
                        .len()
                        .saturating_mul(std::mem::size_of::<husk_hir::ClosureParameter>()),
                )
                .saturating_add(
                    closure
                        .captures
                        .len()
                        .saturating_mul(std::mem::size_of::<(LocalId, CellHandle)>()),
                )
                // The HIR body is cloned into the closure today. Count its
                // shallow allocation and rely on the source/object limits
                // for its bounded recursive children.
                .saturating_add(std::mem::size_of_val(&closure.body))
        }
    })
}

fn heap_value_size(value: &Value) -> usize {
    std::mem::size_of::<Value>().saturating_add(match value {
        Value::String(value) => value.len(),
        Value::Array(values) | Value::Tuple(values) => values
            .iter()
            .map(heap_value_size)
            .fold(0, usize::saturating_add),
        Value::Object(values) | Value::Struct { fields: values, .. } => values
            .iter()
            .map(|(name, value)| name.len().saturating_add(heap_value_size(value)))
            .fold(0, usize::saturating_add),
        Value::Variant {
            type_name,
            case,
            fields,
        } => type_name.len().saturating_add(case.len()).saturating_add(
            fields
                .iter()
                .map(heap_value_size)
                .fold(0, usize::saturating_add),
        ),
        Value::Json(value) => heap_json_size(value),
        Value::Callback(callback) => callback
            .plugin
            .len()
            .saturating_add(callback.function.len()),
        Value::Missing(missing) => missing.field.len().saturating_add(
            missing
                .available_fields
                .iter()
                .map(String::len)
                .sum::<usize>(),
        ),
        Value::Unit
        | Value::Null
        | Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::Range { .. }
        | Value::Closure(_) => 0,
    })
}

fn heap_json_size(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            std::mem::size_of::<serde_json::Value>()
        }
        serde_json::Value::String(value) => value.len(),
        serde_json::Value::Array(values) => values
            .iter()
            .map(heap_json_size)
            .fold(0, usize::saturating_add),
        serde_json::Value::Object(values) => values
            .iter()
            .map(|(name, value)| name.len().saturating_add(heap_json_size(value)))
            .fold(0, usize::saturating_add),
    }
}

fn collect_heap_handles(value: &Value, instance_generation: u64, pending: &mut Vec<CellHandle>) {
    match value {
        Value::Closure(handle) if handle.instance_generation == instance_generation => {
            pending.push(CellHandle {
                slot: handle.slot,
                generation: handle.slot_generation,
            });
        }
        Value::Array(values) | Value::Tuple(values) => {
            for value in values.iter() {
                collect_heap_handles(value, instance_generation, pending);
            }
        }
        Value::Object(values) | Value::Struct { fields: values, .. } => {
            for value in values.values() {
                collect_heap_handles(value, instance_generation, pending);
            }
        }
        Value::Variant { fields, .. } => {
            for value in fields.iter() {
                collect_heap_handles(value, instance_generation, pending);
            }
        }
        Value::Unit
        | Value::Null
        | Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::String(_)
        | Value::Range { .. }
        | Value::Json(_)
        | Value::Callback(_)
        | Value::Closure(_)
        | Value::Missing(_) => {}
    }
}

fn collect_function_roots(value: &Value, roots: &mut Vec<FunctionHandle>) {
    match value {
        Value::Closure(handle) => roots.push(*handle),
        Value::Array(values) | Value::Tuple(values) => {
            for value in values.iter() {
                collect_function_roots(value, roots);
            }
        }
        Value::Object(values) | Value::Struct { fields: values, .. } => {
            for value in values.values() {
                collect_function_roots(value, roots);
            }
        }
        Value::Variant { fields, .. } => {
            for value in fields.iter() {
                collect_function_roots(value, roots);
            }
        }
        Value::Unit
        | Value::Null
        | Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::String(_)
        | Value::Range { .. }
        | Value::Json(_)
        | Value::Callback(_)
        | Value::Missing(_) => {}
    }
}

fn test_descriptor(item: &husk_ast::Item, qualified_name: &str) -> Option<TestDescriptor> {
    item.is_test().then(|| TestDescriptor {
        qualified_name: qualified_name.to_string(),
        ignored: item.is_ignored(),
        expectation: if item.should_panic() {
            TestExpectation::Panic {
                expected: item.expected_panic_message().map(str::to_string),
            }
        } else {
            TestExpectation::Pass
        },
    })
}

fn insert_impl_functions(
    functions: &mut HashMap<String, Function>,
    file: &File,
    module_path: &[String],
    imports: &HashMap<String, String>,
    semantic: Option<&SemanticResult>,
) -> anyhow::Result<()> {
    let empty_name_resolution = HashMap::new();
    let name_resolution = semantic
        .map(|result| &result.name_resolution)
        .unwrap_or(&empty_name_resolution);
    let empty_type_resolution = HashMap::new();
    let type_resolution = semantic
        .map(|result| &result.type_resolution)
        .unwrap_or(&empty_type_resolution);
    let empty_variant_calls = HashMap::new();
    let variant_calls = semantic
        .map(|result| &result.variant_calls)
        .unwrap_or(&empty_variant_calls);
    let empty_variant_patterns = HashMap::new();
    let variant_patterns = semantic
        .map(|result| &result.variant_patterns)
        .unwrap_or(&empty_variant_patterns);
    let traits = file
        .items
        .iter()
        .filter_map(|item| {
            let ItemKind::Trait(definition) = &item.kind else {
                return None;
            };
            Some((definition.name.name.as_str(), definition))
        })
        .collect::<HashMap<_, _>>();

    for item in &file.items {
        let ItemKind::Impl(impl_block) = &item.kind else {
            continue;
        };
        let type_name = runtime_type_name(&impl_block.self_ty)?;
        let mut implemented_methods = HashSet::new();
        for impl_item in &impl_block.items {
            let ImplItemKind::Method(method) = &impl_item.kind else {
                continue;
            };
            implemented_methods.insert(method.name.name.as_str());
            if method.is_extern {
                continue;
            }
            let mut parameters = method.params.clone();
            if method.receiver.is_some() {
                parameters.insert(
                    0,
                    Param {
                        attributes: Vec::new(),
                        name: Ident {
                            name: "self".to_string(),
                            span: method.name.span.clone(),
                        },
                        ty: impl_block.self_ty.clone(),
                    },
                );
            }
            let relative_name = format!("{type_name}::{}", method.name.name);
            let key = if module_path.is_empty() {
                relative_name
            } else {
                format!("{}::{relative_name}", module_path.join("::"))
            };
            let function = Function {
                qualified_name: key.clone(),
                hir: lower_function(
                    &parameters,
                    &method.body,
                    name_resolution,
                    type_resolution,
                    variant_calls,
                    variant_patterns,
                ),
                module_path: module_path.to_vec(),
                imports: imports.clone(),
                test: None,
                receiver: method.receiver,
            };
            if functions.insert(key.clone(), function).is_some() {
                anyhow::bail!("duplicate package function or method `{key}`");
            }
        }

        let Some(trait_ref) = &impl_block.trait_ref else {
            continue;
        };
        let trait_name = runtime_type_name(trait_ref)?;
        let Some(trait_definition) = traits.get(trait_name.as_str()) else {
            continue;
        };
        for trait_item in &trait_definition.items {
            let husk_ast::TraitItemKind::Method(method) = &trait_item.kind;
            let Some(default_body) = &method.default_body else {
                continue;
            };
            if implemented_methods.contains(method.name.name.as_str()) {
                continue;
            }
            let mut parameters = method.params.clone();
            if method.receiver.is_some() {
                parameters.insert(
                    0,
                    Param {
                        attributes: Vec::new(),
                        name: Ident {
                            name: "self".to_string(),
                            span: method.name.span.clone(),
                        },
                        ty: impl_block.self_ty.clone(),
                    },
                );
            }
            let relative_name = format!("{type_name}::{}", method.name.name);
            let key = if module_path.is_empty() {
                relative_name
            } else {
                format!("{}::{relative_name}", module_path.join("::"))
            };
            let function = Function {
                qualified_name: key.clone(),
                hir: lower_function(
                    &parameters,
                    default_body,
                    name_resolution,
                    type_resolution,
                    variant_calls,
                    variant_patterns,
                ),
                module_path: module_path.to_vec(),
                imports: imports.clone(),
                test: None,
                receiver: method.receiver,
            };
            if functions.insert(key.clone(), function).is_some() {
                anyhow::bail!("duplicate package function or method `{key}`");
            }
        }
    }
    Ok(())
}

fn runtime_type_name(ty: &TypeExpr) -> anyhow::Result<String> {
    match &ty.kind {
        TypeExprKind::Named(name) | TypeExprKind::Generic { name, .. } => Ok(name.name.clone()),
        TypeExprKind::Array(_) => Ok("Array".to_string()),
        TypeExprKind::Tuple(_) => Ok("Tuple".to_string()),
        TypeExprKind::Function { .. } | TypeExprKind::ImplTrait { .. } => {
            anyhow::bail!("methods cannot be implemented for this runtime type")
        }
    }
}

fn collect_function_imports(file: &File, module_path: &[String]) -> HashMap<String, String> {
    file.items
        .iter()
        .filter_map(|item| {
            let ItemKind::Use {
                path,
                kind: husk_ast::UseKind::Item,
            } = &item.kind
            else {
                return None;
            };
            let alias = path.last()?.name.clone();
            let segments = path
                .iter()
                .map(|segment| segment.name.clone())
                .collect::<Vec<_>>();
            normalize_source_path(&segments, module_path).map(|path| (alias, path))
        })
        .collect()
}

fn normalize_source_path(segments: &[String], module_path: &[String]) -> Option<String> {
    if segments.is_empty() {
        return None;
    }
    let mut segments = segments.to_vec();
    match segments.first().map(String::as_str) {
        Some("crate") => {
            segments.remove(0);
        }
        Some("self") => {
            segments.remove(0);
            let mut resolved = module_path.to_vec();
            resolved.extend(segments);
            segments = resolved;
        }
        Some("super") => {
            let mut resolved = module_path.to_vec();
            while segments.first().is_some_and(|segment| segment == "super") {
                segments.remove(0);
                resolved.pop();
            }
            resolved.extend(segments);
            segments = resolved;
        }
        _ => {}
    }
    (!segments.is_empty()).then(|| segments.join("::"))
}

fn stable_callable_id(namespace: &str, name: &str) -> u64 {
    // FNV-1a is deliberately simple and platform-independent. Compilation
    // rejects the exceedingly unlikely collision instead of silently routing
    // one callable to another.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in namespace
        .bytes()
        .chain(std::iter::once(0))
        .chain(name.bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

struct FinalizedCallTables {
    functions: FunctionTable,
    module_functions: HashMap<ModuleFunctionId, ModuleFunctionTarget>,
    intrinsic_methods: HashMap<IntrinsicMethodId, IntrinsicMethodTarget>,
}

fn finalize_function_table(
    functions: HashMap<String, Function>,
    modules: &[ModuleDescriptor],
    profile: SemanticProfile,
) -> anyhow::Result<FinalizedCallTables> {
    let mut entries = functions.into_iter().collect::<Vec<_>>();
    entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));

    let mut by_name = HashMap::with_capacity(entries.len());
    let mut by_id = HashMap::with_capacity(entries.len());
    for (index, (name, _)) in entries.iter().enumerate() {
        let id = FunctionId::from_raw(stable_callable_id("script", name));
        if let Some(previous) = by_id.insert(id, index) {
            anyhow::bail!(
                "stable Husk function ID collision between `{}` and `{name}`",
                entries[previous].0
            );
        }
        by_name.insert(name.clone(), id);
    }

    let mut module_paths = Vec::new();
    for module in modules {
        for function in &module.functions {
            module_paths.push(format!("{}::{}", module.name, function.name));
        }
        for interface in &module.interfaces {
            for function in &interface.functions {
                module_paths.push(format!(
                    "{}::{}::{}",
                    module.name, interface.name, function.name
                ));
            }
        }
    }
    module_paths.sort_unstable();
    module_paths.dedup();
    let mut module_functions = HashMap::with_capacity(module_paths.len());
    let mut module_ids = HashMap::with_capacity(module_paths.len());
    for path in module_paths {
        let id = ModuleFunctionId::from_raw(stable_callable_id("module", &path));
        if let Some(previous) =
            module_functions.insert(id, ModuleFunctionTarget { path: path.clone() })
        {
            anyhow::bail!(
                "stable Husk module-function ID collision between `{}` and `{path}`",
                previous.path
            );
        }
        module_ids.insert(path, id);
    }

    let mut intrinsic_methods = HashMap::new();
    let mut intrinsic_method_ids = HashMap::new();
    for (_, function) in &mut entries {
        let module_path = function.module_path.clone();
        let imports = function.imports.clone();
        resolve_hir_statements(
            &mut function.hir.body,
            &module_path,
            &imports,
            &by_name,
            &module_ids,
            &mut intrinsic_methods,
            &mut intrinsic_method_ids,
            profile,
        )?;
    }

    Ok(FinalizedCallTables {
        functions: FunctionTable {
            entries: entries.into_iter().map(|(_, function)| function).collect(),
            by_name,
            by_id,
        },
        module_functions,
        intrinsic_methods,
    })
}

// Resolution deliberately threads immutable declaration indexes and mutable
// intrinsic-method indexes through every recursive HIR node.
#[allow(clippy::too_many_arguments)]
fn resolve_hir_statements(
    statements: &mut [Stmt],
    module_path: &[String],
    imports: &HashMap<String, String>,
    functions: &HashMap<String, FunctionId>,
    modules: &HashMap<String, ModuleFunctionId>,
    intrinsic_methods: &mut HashMap<IntrinsicMethodId, IntrinsicMethodTarget>,
    intrinsic_method_ids: &mut HashMap<(String, String), IntrinsicMethodId>,
    profile: SemanticProfile,
) -> anyhow::Result<()> {
    for statement in statements {
        resolve_hir_statement(
            statement,
            module_path,
            imports,
            functions,
            modules,
            intrinsic_methods,
            intrinsic_method_ids,
            profile,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn resolve_hir_statement(
    statement: &mut Stmt,
    module_path: &[String],
    imports: &HashMap<String, String>,
    functions: &HashMap<String, FunctionId>,
    modules: &HashMap<String, ModuleFunctionId>,
    intrinsic_methods: &mut HashMap<IntrinsicMethodId, IntrinsicMethodTarget>,
    intrinsic_method_ids: &mut HashMap<(String, String), IntrinsicMethodId>,
    profile: SemanticProfile,
) -> anyhow::Result<()> {
    let mut resolve_expr = |expr: &mut Expr| {
        resolve_hir_expr(
            expr,
            module_path,
            imports,
            functions,
            modules,
            intrinsic_methods,
            intrinsic_method_ids,
            profile,
        )
    };
    match &mut statement.kind {
        StmtKind::Let {
            value, else_block, ..
        } => {
            if let Some(value) = value {
                resolve_expr(value)?;
            }
            if let Some(block) = else_block {
                resolve_hir_statements(
                    &mut block.statements,
                    module_path,
                    imports,
                    functions,
                    modules,
                    intrinsic_methods,
                    intrinsic_method_ids,
                    profile,
                )?;
            }
        }
        StmtKind::Assign { target, value, .. } => {
            resolve_expr(target)?;
            resolve_expr(value)?;
        }
        StmtKind::Expr(expr) | StmtKind::Semi(expr) => resolve_expr(expr)?,
        StmtKind::Return { value } => {
            if let Some(value) = value {
                resolve_expr(value)?;
            }
        }
        StmtKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            resolve_expr(cond)?;
            resolve_hir_statements(
                &mut then_branch.statements,
                module_path,
                imports,
                functions,
                modules,
                intrinsic_methods,
                intrinsic_method_ids,
                profile,
            )?;
            if let Some(branch) = else_branch {
                resolve_hir_statement(
                    branch,
                    module_path,
                    imports,
                    functions,
                    modules,
                    intrinsic_methods,
                    intrinsic_method_ids,
                    profile,
                )?;
            }
        }
        StmtKind::While { cond, body } => {
            resolve_expr(cond)?;
            resolve_hir_statements(
                &mut body.statements,
                module_path,
                imports,
                functions,
                modules,
                intrinsic_methods,
                intrinsic_method_ids,
                profile,
            )?;
        }
        StmtKind::Loop { body } | StmtKind::Block(body) => {
            resolve_hir_statements(
                &mut body.statements,
                module_path,
                imports,
                functions,
                modules,
                intrinsic_methods,
                intrinsic_method_ids,
                profile,
            )?;
        }
        StmtKind::ForIn { iterable, body, .. } => {
            resolve_expr(iterable)?;
            resolve_hir_statements(
                &mut body.statements,
                module_path,
                imports,
                functions,
                modules,
                intrinsic_methods,
                intrinsic_method_ids,
                profile,
            )?;
        }
        StmtKind::IfLet {
            scrutinee,
            then_branch,
            else_branch,
            ..
        } => {
            resolve_expr(scrutinee)?;
            resolve_hir_statements(
                &mut then_branch.statements,
                module_path,
                imports,
                functions,
                modules,
                intrinsic_methods,
                intrinsic_method_ids,
                profile,
            )?;
            if let Some(branch) = else_branch {
                resolve_hir_statement(
                    branch,
                    module_path,
                    imports,
                    functions,
                    modules,
                    intrinsic_methods,
                    intrinsic_method_ids,
                    profile,
                )?;
            }
        }
        StmtKind::Break | StmtKind::Continue => {}
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn resolve_hir_expr(
    expr: &mut Expr,
    module_path: &[String],
    imports: &HashMap<String, String>,
    functions: &HashMap<String, FunctionId>,
    modules: &HashMap<String, ModuleFunctionId>,
    intrinsic_methods: &mut HashMap<IntrinsicMethodId, IntrinsicMethodTarget>,
    intrinsic_method_ids: &mut HashMap<(String, String), IntrinsicMethodId>,
    profile: SemanticProfile,
) -> anyhow::Result<()> {
    let is_constructor = expr.constructor.is_some();
    match &mut expr.kind {
        ExprKind::Call {
            callee,
            target,
            args,
            ..
        } => {
            resolve_hir_expr(
                callee,
                module_path,
                imports,
                functions,
                modules,
                intrinsic_methods,
                intrinsic_method_ids,
                profile,
            )?;
            for argument in args {
                resolve_hir_expr(
                    argument,
                    module_path,
                    imports,
                    functions,
                    modules,
                    intrinsic_methods,
                    intrinsic_method_ids,
                    profile,
                )?;
            }
            *target = resolve_call_target(
                callee,
                is_constructor,
                module_path,
                imports,
                functions,
                modules,
                profile,
            )?;
        }
        ExprKind::MethodCall {
            receiver,
            method,
            target,
            args,
            ..
        } => {
            resolve_hir_expr(
                receiver,
                module_path,
                imports,
                functions,
                modules,
                intrinsic_methods,
                intrinsic_method_ids,
                profile,
            )?;
            for argument in args {
                resolve_hir_expr(
                    argument,
                    module_path,
                    imports,
                    functions,
                    modules,
                    intrinsic_methods,
                    intrinsic_method_ids,
                    profile,
                )?;
            }
            *target = resolve_method_target(
                receiver,
                method,
                module_path,
                imports,
                functions,
                modules,
                intrinsic_methods,
                intrinsic_method_ids,
                profile,
            )?;
        }
        ExprKind::Field { base, .. }
        | ExprKind::Unary { expr: base, .. }
        | ExprKind::TupleField { base, .. }
        | ExprKind::Try { expr: base }
        | ExprKind::Cast { expr: base, .. } => resolve_hir_expr(
            base,
            module_path,
            imports,
            functions,
            modules,
            intrinsic_methods,
            intrinsic_method_ids,
            profile,
        )?,
        ExprKind::Binary { left, right, .. }
        | ExprKind::Index {
            base: left,
            index: right,
        }
        | ExprKind::Assign {
            target: left,
            value: right,
            ..
        } => {
            resolve_hir_expr(
                left,
                module_path,
                imports,
                functions,
                modules,
                intrinsic_methods,
                intrinsic_method_ids,
                profile,
            )?;
            resolve_hir_expr(
                right,
                module_path,
                imports,
                functions,
                modules,
                intrinsic_methods,
                intrinsic_method_ids,
                profile,
            )?;
        }
        ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            for nested in [cond, then_branch, else_branch] {
                resolve_hir_expr(
                    nested,
                    module_path,
                    imports,
                    functions,
                    modules,
                    intrinsic_methods,
                    intrinsic_method_ids,
                    profile,
                )?;
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            resolve_hir_expr(
                scrutinee,
                module_path,
                imports,
                functions,
                modules,
                intrinsic_methods,
                intrinsic_method_ids,
                profile,
            )?;
            for arm in arms {
                resolve_hir_expr(
                    &mut arm.expr,
                    module_path,
                    imports,
                    functions,
                    modules,
                    intrinsic_methods,
                    intrinsic_method_ids,
                    profile,
                )?;
            }
        }
        ExprKind::Block(block) => resolve_hir_statements(
            &mut block.statements,
            module_path,
            imports,
            functions,
            modules,
            intrinsic_methods,
            intrinsic_method_ids,
            profile,
        )?,
        ExprKind::Struct { fields, .. } => {
            for field in fields {
                resolve_hir_expr(
                    &mut field.value,
                    module_path,
                    imports,
                    functions,
                    modules,
                    intrinsic_methods,
                    intrinsic_method_ids,
                    profile,
                )?;
            }
        }
        ExprKind::FormatPrint { args, .. }
        | ExprKind::Format { args, .. }
        | ExprKind::Array { elements: args }
        | ExprKind::Tuple { elements: args } => {
            for argument in args {
                resolve_hir_expr(
                    argument,
                    module_path,
                    imports,
                    functions,
                    modules,
                    intrinsic_methods,
                    intrinsic_method_ids,
                    profile,
                )?;
            }
        }
        ExprKind::Closure { body, .. } => resolve_hir_expr(
            body,
            module_path,
            imports,
            functions,
            modules,
            intrinsic_methods,
            intrinsic_method_ids,
            profile,
        )?,
        ExprKind::Range { start, end, .. } => {
            if let Some(start) = start {
                resolve_hir_expr(
                    start,
                    module_path,
                    imports,
                    functions,
                    modules,
                    intrinsic_methods,
                    intrinsic_method_ids,
                    profile,
                )?;
            }
            if let Some(end) = end {
                resolve_hir_expr(
                    end,
                    module_path,
                    imports,
                    functions,
                    modules,
                    intrinsic_methods,
                    intrinsic_method_ids,
                    profile,
                )?;
            }
        }
        ExprKind::Ident {
            name,
            local: None,
            function,
        } => {
            *function = resolve_compiled_function_id(name, module_path, imports, functions);
        }
        ExprKind::Path { segments, function } => {
            *function =
                resolve_compiled_function_id(&segments.join("::"), module_path, imports, functions);
        }
        ExprKind::Literal(_)
        | ExprKind::Ident { local: Some(_), .. }
        | ExprKind::JsLiteral { .. } => {}
    }
    Ok(())
}

fn resolve_call_target(
    callee: &Expr,
    is_constructor: bool,
    module_path: &[String],
    imports: &HashMap<String, String>,
    functions: &HashMap<String, FunctionId>,
    modules: &HashMap<String, ModuleFunctionId>,
    profile: SemanticProfile,
) -> anyhow::Result<CallTarget> {
    if is_constructor {
        return Ok(CallTarget::Constructor);
    }
    let spelling = match &callee.kind {
        ExprKind::Ident { local: Some(_), .. } => return Ok(CallTarget::Indirect),
        ExprKind::Ident {
            name, local: None, ..
        } => name.clone(),
        ExprKind::Path { segments, .. } => {
            let path = segments.join("::");
            if enum_constructor_path(&path).is_some() {
                return Ok(CallTarget::Constructor);
            }
            path
        }
        _ => return Ok(CallTarget::Indirect),
    };
    if let Some(function) = resolve_compiled_function_id(&spelling, module_path, imports, functions)
    {
        return Ok(CallTarget::Script(function));
    }
    let resolved = resolve_compiled_path(&spelling, module_path, imports);
    if let Some(module) = modules.get(&resolved).copied() {
        return Ok(CallTarget::Module(module));
    }
    let intrinsic = match spelling.as_str() {
        "println" => Some(IntrinsicFunction::Println),
        "assert" => Some(IntrinsicFunction::Assert),
        "assert_msg" => Some(IntrinsicFunction::AssertMessage),
        _ => None,
    };
    if let Some(intrinsic) = intrinsic {
        return Ok(CallTarget::Intrinsic(intrinsic));
    }
    if profile == SemanticProfile::LegacyJavaScript {
        Ok(CallTarget::LegacyDynamic)
    } else {
        anyhow::bail!("native Husk call `{spelling}` has no resolved callable target")
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_method_target(
    receiver: &Expr,
    method: &str,
    module_path: &[String],
    imports: &HashMap<String, String>,
    functions: &HashMap<String, FunctionId>,
    modules: &HashMap<String, ModuleFunctionId>,
    intrinsic_methods: &mut HashMap<IntrinsicMethodId, IntrinsicMethodTarget>,
    intrinsic_method_ids: &mut HashMap<(String, String), IntrinsicMethodId>,
    profile: SemanticProfile,
) -> anyhow::Result<MethodTarget> {
    if let ExprKind::Ident {
        name, local: None, ..
    } = &receiver.kind
    {
        let module_path = format!("{name}::{method}");
        if let Some(module) = modules.get(&module_path).copied() {
            return Ok(MethodTarget::Module(module));
        }
    }

    let receiver_type = receiver.ty.clone().or_else(|| match &receiver.kind {
        ExprKind::Struct { name, .. } | ExprKind::Path { segments: name, .. } => {
            Some(name.join("::"))
        }
        _ => None,
    });
    if let Some(receiver_type) = &receiver_type {
        let method_name = format!("{receiver_type}::{method}");
        if let Some(function) =
            resolve_compiled_function_id(&method_name, module_path, imports, functions)
        {
            return Ok(MethodTarget::Script(function));
        }
    }
    if profile == SemanticProfile::LegacyJavaScript {
        return Ok(MethodTarget::LegacyDynamic);
    }

    let receiver_type = receiver_type.unwrap_or_else(|| "<inferred>".to_string());
    let key = (receiver_type.clone(), method.to_string());
    let id = if let Some(id) = intrinsic_method_ids.get(&key).copied() {
        id
    } else {
        let id = IntrinsicMethodId::from_raw(stable_callable_id(
            "intrinsic-method",
            &format!("{receiver_type}\0{method}"),
        ));
        if let Some(previous) = intrinsic_methods.insert(
            id,
            IntrinsicMethodTarget {
                receiver_type,
                method: method.to_string(),
            },
        ) {
            anyhow::bail!(
                "stable Husk intrinsic-method ID collision with `{}::{}`",
                previous.receiver_type,
                previous.method
            );
        }
        intrinsic_method_ids.insert(key, id);
        id
    };
    Ok(MethodTarget::Intrinsic(id))
}

fn resolve_compiled_function_id(
    spelling: &str,
    module_path: &[String],
    imports: &HashMap<String, String>,
    functions: &HashMap<String, FunctionId>,
) -> Option<FunctionId> {
    let resolved = resolve_compiled_path(spelling, module_path, imports);
    if let Some(id) = functions.get(&resolved).copied() {
        return Some(id);
    }
    if !spelling.contains("::") && !module_path.is_empty() {
        let local = format!("{}::{spelling}", module_path.join("::"));
        if let Some(id) = functions.get(&local).copied() {
            return Some(id);
        }
    }
    if spelling.contains("::")
        && !matches!(
            spelling.split("::").next(),
            Some("crate" | "self" | "super")
        )
        && !module_path.is_empty()
    {
        let relative = format!("{}::{spelling}", module_path.join("::"));
        if let Some(id) = functions.get(&relative).copied() {
            return Some(id);
        }
    }
    None
}

fn resolve_compiled_path(
    spelling: &str,
    module_path: &[String],
    imports: &HashMap<String, String>,
) -> String {
    imports.get(spelling).cloned().unwrap_or_else(|| {
        let segments = spelling.split("::").map(str::to_string).collect::<Vec<_>>();
        normalize_source_path(&segments, module_path).unwrap_or_else(|| spelling.to_string())
    })
}

/// Embedded Husk VM.
#[derive(Debug, Clone, Default)]
pub struct Vm {
    programs: HashMap<String, Program>,
    program_generations: HashMap<String, u64>,
    next_program_generation: u64,
    instruction_budget: usize,
    call_depth_limit: usize,
    host_call_budget: usize,
    instance_generation: u64,
    heap: RuntimeHeap,
    rooted_functions: HashSet<FunctionHandle>,
    max_function_roots: usize,
}

impl Vm {
    /// Creates an empty VM with the default per-callback instruction budget.
    #[must_use]
    pub fn new() -> Self {
        Self {
            instruction_budget: 10_000,
            call_depth_limit: 512,
            host_call_budget: 10_000,
            max_function_roots: 10_000,
            ..Self::default()
        }
    }

    /// Set the maximum number of statements/expressions run by one callback.
    pub fn set_instruction_budget(&mut self, budget: usize) {
        self.instruction_budget = budget;
    }

    /// Set the maximum nested Husk function/closure depth for one call.
    pub fn set_call_depth_limit(&mut self, limit: usize) {
        self.call_depth_limit = limit;
    }

    /// Set the maximum registered native/Wasm module calls for one call.
    pub fn set_host_call_budget(&mut self, budget: usize) {
        self.host_call_budget = budget;
    }

    /// Set the generation used to reject stale runtime function handles.
    pub fn set_instance_generation(&mut self, generation: u64) {
        self.instance_generation = generation;
    }

    /// Set the maximum number of live cells and closures in this instance.
    pub fn set_heap_object_limit(&mut self, max_objects: usize) {
        self.heap.set_limits(max_objects, self.heap.max_bytes);
    }

    /// Set live heap object and approximate byte limits for this instance.
    pub fn set_heap_limits(&mut self, max_objects: usize, max_bytes: usize) {
        self.heap.set_limits(max_objects, max_bytes);
    }

    /// Set the maximum number of explicitly retained closure handles.
    pub fn set_function_root_limit(&mut self, max_roots: usize) {
        self.max_function_roots = max_roots;
    }

    /// Load and activate a plugin program.
    ///
    /// # Errors
    ///
    /// Returns parser or runtime errors from `activate`.
    pub fn load_plugin<H: Host>(
        &mut self,
        name: impl Into<String>,
        source: &str,
        host: &mut H,
    ) -> anyhow::Result<()> {
        let name = name.into();
        self.load_plugin_at(name.clone(), format!("plugins/{name}.hk"), source, host)
    }

    /// Load and activate a plugin using a specific display path for diagnostics.
    ///
    /// # Errors
    ///
    /// Returns parser or runtime errors from `activate`.
    pub fn load_plugin_at<H: Host>(
        &mut self,
        name: impl Into<String>,
        path: impl Into<String>,
        source: &str,
        host: &mut H,
    ) -> anyhow::Result<()> {
        let name = name.into();
        let program = CompiledProgram::compile_at(
            name.clone(),
            path,
            source,
            &CompileOptions::legacy_runtime_compatibility(),
        )?;
        self.load_compiled_plugin(name, program, host)
    }

    /// Load and activate an already compiled plugin without parsing it again.
    ///
    /// # Errors
    ///
    /// Returns runtime errors from `activate`.
    pub fn load_compiled_plugin<H: Host>(
        &mut self,
        name: impl Into<String>,
        program: CompiledProgram,
        host: &mut H,
    ) -> anyhow::Result<()> {
        let name = name.into();
        let program_generation = self
            .next_program_generation
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Husk program generation space is exhausted"))?;
        self.next_program_generation = program_generation;
        self.program_generations
            .insert(name.clone(), program_generation);
        host.begin_reload_replacement(&name);
        self.programs.insert(name.clone(), program);
        if self.has_function(&name, "activate")
            && let Err(error) =
                self.call_function(&Callback::new(name.clone(), "activate"), Vec::new(), host)
        {
            self.unload_plugin(&name);
            return Err(error);
        }
        Ok(())
    }

    /// Stage parse/activation/state migration and replace a loaded plugin only when all
    /// steps succeed. A failed save leaves the old program and callbacks untouched.
    pub fn reload_plugin_at<H: Host>(
        &mut self,
        name: impl Into<String>,
        path: impl Into<String>,
        source: &str,
        host: &mut H,
    ) -> anyhow::Result<()> {
        let name = name.into();
        let program = CompiledProgram::compile_at(
            name.clone(),
            path,
            source,
            &CompileOptions::legacy_runtime_compatibility(),
        )?;
        self.reload_compiled_plugin(name, program, host)
    }

    /// Transactionally replace a plugin with an already compiled artifact.
    ///
    /// # Errors
    ///
    /// Returns runtime errors from state export, replacement activation,
    /// import, or old-instance deactivation.
    pub fn reload_compiled_plugin<H: Host>(
        &mut self,
        name: impl Into<String>,
        program: CompiledProgram,
        host: &mut H,
    ) -> anyhow::Result<()> {
        let name = name.into();
        if !self.programs.contains_key(&name) {
            return self.load_compiled_plugin(name, program, host);
        }
        let mut previous = self.clone();
        let exported_state = if previous.has_function(&name, "state_export") {
            previous.call_function(
                &Callback::new(name.clone(), "state_export"),
                Vec::new(),
                host,
            )?
        } else {
            Value::Null
        };
        let mut staged = self.clone();
        staged.load_compiled_plugin(name.clone(), program, host)?;
        if staged.has_function(&name, "state_import") {
            staged.call_function(
                &Callback::new(name.clone(), "state_import"),
                vec![exported_state],
                host,
            )?;
        }
        if previous.has_function(&name, "deactivate") {
            host.begin_reload_teardown(&name);
            previous.call_function(&Callback::new(name.clone(), "deactivate"), Vec::new(), host)?;
        }
        *self = staged;
        Ok(())
    }

    /// Run a loaded plugin's `deactivate` hook and remove all of its VM-owned state.
    ///
    /// The plugin is always unloaded, including when teardown fails.
    ///
    /// # Errors
    ///
    /// Returns the error raised by `deactivate`, if any.
    pub fn deactivate_plugin<H: Host>(&mut self, name: &str, host: &mut H) -> anyhow::Result<()> {
        let result = if self.has_function(name, "deactivate") {
            self.call_function(&Callback::new(name, "deactivate"), Vec::new(), host)
                .map(|_| ())
        } else {
            Ok(())
        };
        self.unload_plugin(name);
        result
    }

    /// Removes a plugin and every VM-owned function handle.
    ///
    /// This does not run plugin teardown code.
    pub fn unload_plugin(&mut self, name: &str) {
        self.programs.remove(name);
        let owned_handles = self
            .rooted_functions
            .iter()
            .copied()
            .filter(|handle| {
                self.heap
                    .closure(*handle, self.instance_generation)
                    .is_ok_and(|closure| closure.plugin == name)
            })
            .collect::<Vec<_>>();
        for handle in owned_handles {
            self.rooted_functions.remove(&handle);
        }
    }

    #[must_use]
    /// Returns whether a program with `name` is currently loaded.
    pub fn has_plugin(&self, name: &str) -> bool {
        self.programs.contains_key(name)
    }

    /// Call a named function in a loaded compiled program.
    ///
    /// # Errors
    ///
    /// Returns an error when the export is unknown or execution fails.
    pub fn call_export<H: Host>(
        &mut self,
        plugin: &str,
        function: &str,
        args: Vec<Value>,
        host: &mut H,
    ) -> anyhow::Result<Value> {
        if !self.has_function(plugin, function) {
            anyhow::bail!("unknown Husk export `{plugin}::{function}`");
        }
        self.call_function(&Callback::new(plugin, function), args, host)
    }

    /// Invoke a named callback previously passed to a host module.
    ///
    /// This is a compatibility boundary for hosts migrating from named
    /// callbacks to retained [`FunctionHandle`] values.
    pub fn call_callback<H: Host>(
        &mut self,
        callback: &Callback,
        args: Vec<Value>,
        host: &mut H,
    ) -> anyhow::Result<Value> {
        self.call_function(callback, args, host)
    }

    pub(crate) fn capture_exported_function<H: Host>(
        &mut self,
        plugin: &str,
        function: &str,
        args: Vec<Value>,
        host: &mut H,
    ) -> anyhow::Result<FunctionHandle> {
        let value = self.call_export(plugin, function, args, host)?;
        let Value::Closure(handle) = value else {
            anyhow::bail!(
                "Husk export `{plugin}::{function}` returned {}, not a function",
                value.kind_name()
            );
        };
        self.heap.closure(handle, self.instance_generation)?;
        if !self.rooted_functions.contains(&handle)
            && self.rooted_functions.len() >= self.max_function_roots
        {
            anyhow::bail!(
                "Husk function root limit exceeded (maximum {})",
                self.max_function_roots
            );
        }
        self.rooted_functions.insert(handle);
        Ok(handle)
    }

    pub(crate) fn invoke_function_handle<H: Host>(
        &mut self,
        handle: FunctionHandle,
        args: Vec<Value>,
        host: &mut H,
    ) -> anyhow::Result<Value> {
        if handle.instance_generation != self.instance_generation {
            anyhow::bail!("stale Husk function handle belongs to another instance generation");
        }
        if !self.rooted_functions.contains(&handle) {
            anyhow::bail!("Husk function handle has been released or was never retained");
        }
        self.collect_heap_before_call(&[Value::Closure(handle)]);
        let plugin = self
            .heap
            .closure(handle, self.instance_generation)?
            .plugin
            .clone();
        let mut frame = Frame {
            plugin,
            locals: HashMap::new(),
            owned_cells: Vec::new(),
            module_path: Vec::new(),
            imports: HashMap::new(),
            remaining: self.instruction_budget,
            remaining_host_calls: self.host_call_budget,
            call_depth: 0,
        };
        self.call_closure_in_frame(handle, args, &mut frame, host)
    }

    pub(crate) fn release_function_handle(
        &mut self,
        handle: FunctionHandle,
    ) -> anyhow::Result<bool> {
        if handle.instance_generation != self.instance_generation {
            anyhow::bail!("stale Husk function handle belongs to another instance generation");
        }
        Ok(self.rooted_functions.remove(&handle))
    }

    /// Run `before_exit` on every loaded plugin that defines it.
    ///
    /// # Errors
    ///
    /// Returns the first callback error.
    pub fn before_exit<H: Host>(
        &mut self,
        snapshot: serde_json::Value,
        host: &mut H,
    ) -> anyhow::Result<()> {
        let callbacks = self
            .programs
            .keys()
            .filter(|plugin| self.has_function(plugin, "before_exit"))
            .cloned()
            .map(|plugin| Callback::new(plugin, "before_exit"))
            .collect::<Vec<_>>();
        for callback in callbacks {
            self.call_function(&callback, vec![Value::Json(snapshot.clone())], host)?;
        }
        Ok(())
    }

    /// Run `deactivate` on every loaded plugin that defines it.
    ///
    /// # Errors
    ///
    /// Returns the first callback error.
    pub fn deactivate_all<H: Host>(&mut self, host: &mut H) -> anyhow::Result<()> {
        let callbacks = self
            .programs
            .keys()
            .filter(|plugin| self.has_function(plugin, "deactivate"))
            .cloned()
            .map(|plugin| Callback::new(plugin, "deactivate"))
            .collect::<Vec<_>>();
        for callback in callbacks {
            self.call_function(&callback, Vec::new(), host)?;
        }
        self.rooted_functions.clear();
        Ok(())
    }

    fn has_function(&self, plugin: &str, function: &str) -> bool {
        self.programs
            .get(plugin)
            .is_some_and(|program| program.functions.contains(function))
    }

    fn resolved_callback(&self, plugin: &str, function_id: FunctionId) -> anyhow::Result<Callback> {
        let program = self
            .programs
            .get(plugin)
            .ok_or_else(|| anyhow::anyhow!("unknown Husk program `{plugin}`"))?;
        let function = program.functions.get(function_id).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown Husk function ID {} in program `{plugin}`",
                function_id.raw()
            )
        })?;
        let generation = self
            .program_generations
            .get(plugin)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("Husk program `{plugin}` has no loaded generation"))?;
        Ok(Callback::resolved(
            plugin,
            &function.qualified_name,
            function_id,
            generation,
        ))
    }

    fn collect_heap_before_call(&mut self, args: &[Value]) {
        let mut roots = self.rooted_functions.iter().copied().collect::<Vec<_>>();
        for value in args {
            collect_function_roots(value, &mut roots);
        }
        self.heap.collect(&roots, self.instance_generation);
    }

    fn release_frame_cells(
        &mut self,
        finished: &Frame,
        parent: &mut Frame,
        result: Option<&Value>,
    ) {
        if finished.owned_cells.is_empty() {
            return;
        }
        let mut function_roots = self.rooted_functions.iter().copied().collect::<Vec<_>>();
        if let Some(result) = result {
            collect_function_roots(result, &mut function_roots);
        }
        let reachable = self.heap.reachable_handles(
            &function_roots,
            parent.locals.values().copied(),
            self.instance_generation,
        );
        for handle in &finished.owned_cells {
            if reachable.contains(handle) {
                if !parent.owned_cells.contains(handle) {
                    parent.owned_cells.push(*handle);
                }
            } else {
                self.heap.free_cell(*handle);
            }
        }
    }

    fn bind_local(
        &mut self,
        frame: &mut Frame,
        local: LocalId,
        value: Value,
    ) -> anyhow::Result<CellHandle> {
        if let Some(handle) = frame.locals.get(&local).copied() {
            self.heap.set_cell(handle, value)?;
            return Ok(handle);
        }
        let handle = self.heap.allocate_cell(value)?;
        frame.locals.insert(local, handle);
        frame.owned_cells.push(handle);
        Ok(handle)
    }

    fn bind_locals(
        &mut self,
        frame: &mut Frame,
        bindings: Vec<(LocalId, Value)>,
    ) -> anyhow::Result<()> {
        for (local, value) in bindings {
            self.bind_local(frame, local, value)?;
        }
        Ok(())
    }

    fn local_value(&self, frame: &Frame, local: LocalId) -> anyhow::Result<Value> {
        let Some(handle) = frame.locals.get(&local).copied() else {
            return Ok(Value::Unit);
        };
        self.heap.cell(handle).cloned()
    }

    fn set_local(&mut self, frame: &mut Frame, local: LocalId, value: Value) -> anyhow::Result<()> {
        self.bind_local(frame, local, value).map(|_| ())
    }

    fn call_function<H: Host>(
        &mut self,
        callback: &Callback,
        args: Vec<Value>,
        host: &mut H,
    ) -> anyhow::Result<Value> {
        self.collect_heap_before_call(&args);
        let mut frame = Frame {
            plugin: callback.plugin.clone(),
            locals: HashMap::new(),
            owned_cells: Vec::new(),
            module_path: Vec::new(),
            imports: HashMap::new(),
            remaining: self.instruction_budget,
            remaining_host_calls: self.host_call_budget,
            call_depth: 0,
        };

        self.call_function_in_frame(callback, args, &mut frame, host)
    }

    fn call_function_in_frame<H: Host>(
        &mut self,
        callback: &Callback,
        args: Vec<Value>,
        parent_frame: &mut Frame,
        host: &mut H,
    ) -> anyhow::Result<Value> {
        self.call_function_in_frame_with_receiver(callback, args, None, parent_frame, host)
    }

    fn call_function_in_frame_with_receiver<H: Host>(
        &mut self,
        callback: &Callback,
        args: Vec<Value>,
        receiver_cell: Option<CellHandle>,
        parent_frame: &mut Frame,
        host: &mut H,
    ) -> anyhow::Result<Value> {
        let function = if let Some(function_id) = callback.function_id {
            let callback_generation = callback.program_generation.ok_or_else(|| {
                anyhow::anyhow!(
                    "resolved Husk callback `{}::{}` has no program generation",
                    callback.plugin,
                    callback.function
                )
            })?;
            let current_generation = self
                .program_generations
                .get(&callback.plugin)
                .copied()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "stale Husk callback `{}::{}` refers to an unloaded program",
                        callback.plugin,
                        callback.function
                    )
                })?;
            if callback_generation != current_generation {
                anyhow::bail!(
                    "stale Husk callback `{}::{}` belongs to program generation {}, current generation is {}",
                    callback.plugin,
                    callback.function,
                    callback_generation,
                    current_generation
                );
            }
            let target_exists = self
                .programs
                .get(&callback.plugin)
                .and_then(|program| program.functions.get(function_id))
                .is_some();
            if !target_exists {
                anyhow::bail!(
                    "resolved Husk callback `{}::{}` has unavailable function ID {}",
                    callback.plugin,
                    callback.function,
                    function_id.raw()
                );
            }
            function_id
        } else {
            self.programs
                .get(&callback.plugin)
                .and_then(|program| program.functions.id(&callback.function))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "unknown Husk function `{}::{}`",
                        callback.plugin,
                        callback.function
                    )
                })?
        };
        self.call_function_id_in_frame_with_receiver(
            &callback.plugin,
            function,
            args,
            receiver_cell,
            parent_frame,
            host,
        )
    }

    fn call_function_id_in_frame<H: Host>(
        &mut self,
        plugin: &str,
        function: FunctionId,
        args: Vec<Value>,
        parent_frame: &mut Frame,
        host: &mut H,
    ) -> anyhow::Result<Value> {
        self.call_function_id_in_frame_with_receiver(
            plugin,
            function,
            args,
            None,
            parent_frame,
            host,
        )
    }

    fn call_function_id_in_frame_with_receiver<H: Host>(
        &mut self,
        plugin: &str,
        function_id: FunctionId,
        args: Vec<Value>,
        receiver_cell: Option<CellHandle>,
        parent_frame: &mut Frame,
        host: &mut H,
    ) -> anyhow::Result<Value> {
        if parent_frame.call_depth >= self.call_depth_limit {
            anyhow::bail!("Husk call depth exceeded");
        }

        let function = self
            .programs
            .get(plugin)
            .and_then(|program| program.functions.get(function_id))
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown Husk function ID {}", function_id.raw()))?;
        let callback = Callback::new(plugin, &function.qualified_name);
        if function.receiver == Some(SelfReceiver::RefMut) && receiver_cell.is_none() {
            anyhow::bail!(
                "mutable method receiver `{}::{}` requires a mutable local receiver",
                plugin,
                function.qualified_name
            );
        }
        let legacy_arity = self
            .programs
            .get(plugin)
            .is_some_and(|program| program.semantic_profile == SemanticProfile::LegacyJavaScript);
        if !legacy_arity && args.len() != function.hir.parameters.len() {
            anyhow::bail!(
                "Husk function `{}::{}` expects {} argument{}, got {}",
                plugin,
                function.qualified_name,
                function.hir.parameters.len(),
                if function.hir.parameters.len() == 1 {
                    ""
                } else {
                    "s"
                },
                args.len()
            );
        }
        let mut frame = Frame {
            plugin: plugin.to_string(),
            locals: HashMap::new(),
            owned_cells: Vec::new(),
            module_path: function.module_path.clone(),
            imports: function.imports.clone(),
            remaining: parent_frame.remaining,
            remaining_host_calls: parent_frame.remaining_host_calls,
            call_depth: parent_frame.call_depth + 1,
        };

        for (index, (parameter, value)) in function.hir.parameters.iter().zip(args).enumerate() {
            if index == 0
                && function.receiver == Some(SelfReceiver::RefMut)
                && let Some(receiver_cell) = receiver_cell
            {
                frame.locals.insert(parameter.id, receiver_cell);
            } else {
                self.bind_local(&mut frame, parameter.id, value)?;
            }
        }

        let result = self
            .eval_statements(&function.hir.body, &mut frame, host)
            .or_else(|error| match error.downcast::<EarlyReturn>() {
                Ok(early) => Ok(early.value),
                Err(error) => Err(error),
            })
            .map_err(|error| with_call_frame(error, &callback));
        parent_frame.remaining = frame.remaining;
        parent_frame.remaining_host_calls = frame.remaining_host_calls;
        self.release_frame_cells(&frame, parent_frame, result.as_ref().ok());
        result
    }

    fn eval_statements<H: Host>(
        &mut self,
        statements: &[Stmt],
        frame: &mut Frame,
        host: &mut H,
    ) -> anyhow::Result<Value> {
        let mut value = Value::Unit;
        for statement in statements {
            frame
                .consume()
                .map_err(|error| self.enrich_runtime_error(error, frame, &statement.span))?;
            match self.eval_statement(statement, frame, host)? {
                Flow::Continue(next) => value = next,
                Flow::Return(result) => return Ok(result),
                Flow::Break => {
                    return Err(self.runtime_error(
                        "HUSK-R0006",
                        "Husk `break` escaped a loop",
                        &statement.span,
                        "this `break` is not inside a loop",
                        frame,
                    ));
                }
                Flow::LoopContinue => {
                    return Err(self.runtime_error(
                        "HUSK-R0007",
                        "Husk `continue` escaped a loop",
                        &statement.span,
                        "this `continue` is not inside a loop",
                        frame,
                    ));
                }
            }
        }
        Ok(value)
    }

    fn eval_statement<H: Host>(
        &mut self,
        statement: &Stmt,
        frame: &mut Frame,
        host: &mut H,
    ) -> anyhow::Result<Flow> {
        match &statement.kind {
            StmtKind::Let {
                pattern,
                value,
                else_block,
                ..
            } => {
                if let PatternKind::Binding(local) = &pattern.kind
                    && !frame.locals.contains_key(&local.id)
                {
                    // Reserving the cell before evaluating the initializer
                    // lets a closure capture its own binding safely.
                    self.bind_local(frame, local.id, Value::Unit)?;
                }
                let value = if let Some(value) = value {
                    self.eval_expr(value, frame, host)?
                } else {
                    Value::Unit
                };
                let mut bindings = Vec::new();
                if match_pattern(pattern, &value, &mut bindings) {
                    self.bind_locals(frame, bindings)?;
                } else if let Some(else_block) = else_block {
                    return self.eval_block(else_block, frame, host);
                } else {
                    return Err(self.runtime_error(
                        "HUSK-R0008",
                        "value does not match the `let` pattern",
                        &pattern.span,
                        "pattern did not match",
                        frame,
                    ));
                }
                Ok(Flow::Continue(Value::Unit))
            }
            StmtKind::Assign { target, op, value } => {
                let value = self.eval_assignment(target, *op, value, frame, host)?;
                Ok(Flow::Continue(value))
            }
            StmtKind::Expr(expr) | StmtKind::Semi(expr) => {
                Ok(Flow::Continue(self.eval_expr(expr, frame, host)?))
            }
            StmtKind::Return { value } => {
                let value = if let Some(value) = value {
                    self.eval_expr(value, frame, host)?
                } else {
                    Value::Unit
                };
                Ok(Flow::Return(value))
            }
            StmtKind::Block(block) => self.eval_block(block, frame, host),
            StmtKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                if self.eval_condition(cond, frame, host)? {
                    return self.eval_block(then_branch, frame, host);
                }

                if let Some(else_branch) = else_branch {
                    return self.eval_statement(else_branch, frame, host);
                }

                Ok(Flow::Continue(Value::Unit))
            }
            StmtKind::While { cond, body } => {
                let mut value = Value::Unit;
                while self.eval_condition(cond, frame, host)? {
                    match self.eval_block(body, frame, host)? {
                        Flow::Continue(next) => value = next,
                        Flow::Return(result) => return Ok(Flow::Return(result)),
                        Flow::Break => break,
                        Flow::LoopContinue => continue,
                    }
                }
                Ok(Flow::Continue(value))
            }
            StmtKind::Loop { body } => {
                let mut value = Value::Unit;
                loop {
                    frame
                        .consume()
                        .map_err(|error| self.enrich_runtime_error(error, frame, &body.span))?;
                    match self.eval_block(body, frame, host)? {
                        Flow::Continue(next) => value = next,
                        Flow::Return(result) => return Ok(Flow::Return(result)),
                        Flow::Break => break,
                        Flow::LoopContinue => continue,
                    }
                }
                Ok(Flow::Continue(value))
            }
            StmtKind::ForIn {
                binding,
                iterable,
                body,
            } => {
                let iterable = self.eval_expr(iterable, frame, host)?;
                let mut value = Value::Unit;
                for item in iterable_values(iterable)
                    .map_err(|error| self.enrich_runtime_error(error, frame, &statement.span))?
                {
                    frame.consume().map_err(|error| {
                        self.enrich_runtime_error(error, frame, &statement.span)
                    })?;
                    self.bind_local(frame, binding.id, item)?;
                    match self.eval_block(body, frame, host)? {
                        Flow::Continue(next) => value = next,
                        Flow::Return(result) => return Ok(Flow::Return(result)),
                        Flow::Break => break,
                        Flow::LoopContinue => continue,
                    }
                }
                Ok(Flow::Continue(value))
            }
            StmtKind::Break => Ok(Flow::Break),
            StmtKind::Continue => Ok(Flow::LoopContinue),
            StmtKind::IfLet {
                pattern,
                scrutinee,
                then_branch,
                else_branch,
            } => {
                let value = self.eval_expr(scrutinee, frame, host)?;
                let mut bindings = Vec::new();
                if match_pattern(pattern, &value, &mut bindings) {
                    self.bind_locals(frame, bindings)?;
                    self.eval_block(then_branch, frame, host)
                } else if let Some(else_branch) = else_branch {
                    self.eval_statement(else_branch, frame, host)
                } else {
                    Ok(Flow::Continue(Value::Unit))
                }
            }
        }
    }

    fn eval_block<H: Host>(
        &mut self,
        block: &Block,
        frame: &mut Frame,
        host: &mut H,
    ) -> anyhow::Result<Flow> {
        let mut value = Value::Unit;
        for statement in &block.statements {
            frame
                .consume()
                .map_err(|error| self.enrich_runtime_error(error, frame, &statement.span))?;
            match self.eval_statement(statement, frame, host)? {
                Flow::Continue(next) => value = next,
                Flow::Return(result) => return Ok(Flow::Return(result)),
                Flow::Break => return Ok(Flow::Break),
                Flow::LoopContinue => return Ok(Flow::LoopContinue),
            }
        }
        Ok(Flow::Continue(value))
    }

    fn eval_expr<H: Host>(
        &mut self,
        expr: &Expr,
        frame: &mut Frame,
        host: &mut H,
    ) -> anyhow::Result<Value> {
        frame
            .consume()
            .map_err(|error| self.enrich_runtime_error(error, frame, &expr.span))?;
        let result = match &expr.kind {
            ExprKind::Literal(literal) => match literal {
                LiteralKind::Bool(value) => Ok(Value::Bool(*value)),
                LiteralKind::Int(value) => Ok(Value::Int(*value)),
                LiteralKind::Float(value) => Ok(Value::Float(*value)),
                LiteralKind::String(value) => Ok(Value::String(value.clone())),
            },
            ExprKind::Ident {
                name,
                local,
                function,
            } => {
                if let Some(local) = local
                    && frame.locals.contains_key(local)
                {
                    return self.local_value(frame, *local);
                }
                if name == "None" {
                    return Ok(enum_variant_value("Option", "None", Vec::new()));
                }
                if let Some(function_id) = function {
                    return self
                        .resolved_callback(&frame.plugin, *function_id)
                        .map(Value::Callback);
                }
                if self.has_function(&frame.plugin, name) {
                    return Ok(Value::Callback(Callback::new(
                        frame.plugin.clone(),
                        name.clone(),
                    )));
                }
                Ok(Value::String(name.clone()))
            }
            ExprKind::Path { segments, function } => {
                let path = segments.join("::");
                if let Some((type_name, case)) = enum_constructor_path(&path) {
                    Ok(enum_variant_value(type_name, case, Vec::new()))
                } else if path == "None" {
                    Ok(enum_variant_value("Option", "None", Vec::new()))
                } else if let Some(function_id) = function {
                    self.resolved_callback(&frame.plugin, *function_id)
                        .map(Value::Callback)
                } else {
                    Ok(Value::String(path))
                }
            }
            ExprKind::Call {
                callee,
                target,
                args,
                ..
            } => {
                if let Some(constructor) = &expr.constructor {
                    let args = args
                        .iter()
                        .map(|arg| self.eval_expr(arg, frame, host))
                        .collect::<anyhow::Result<Vec<_>>>()?;
                    return Ok(enum_variant_value(
                        &constructor.type_name,
                        &constructor.case,
                        args,
                    ));
                }
                if let ExprKind::Path { segments, .. } = &callee.kind {
                    let path = segments.join("::");
                    if let Some((type_name, case)) = enum_constructor_path(&path) {
                        let args = args
                            .iter()
                            .map(|arg| self.eval_expr(arg, frame, host))
                            .collect::<anyhow::Result<Vec<_>>>()?;
                        return Ok(enum_variant_value(type_name, case, args));
                    }
                }
                let args = args
                    .iter()
                    .map(|arg| self.eval_expr(arg, frame, host))
                    .collect::<anyhow::Result<Vec<_>>>()?;
                match target {
                    CallTarget::Script(function) => {
                        let plugin = frame.plugin.clone();
                        self.call_function_id_in_frame(&plugin, *function, args, frame, host)
                    }
                    CallTarget::Module(function) => {
                        self.call_module_function(*function, args, frame, host)
                    }
                    CallTarget::Intrinsic(function) => {
                        self.call_intrinsic_function(*function, &args, host)
                    }
                    CallTarget::Indirect | CallTarget::LegacyDynamic => {
                        let callee = self.eval_expr(callee, frame, host)?;
                        self.call_value(callee, args, frame, host)
                    }
                    CallTarget::Constructor => {
                        anyhow::bail!("constructor call is missing resolved variant metadata")
                    }
                    CallTarget::Unresolved => {
                        anyhow::bail!("Husk call reached runtime without a resolved target")
                    }
                }
            }
            ExprKind::Field {
                base,
                member,
                member_span,
            } => {
                let base = self.eval_expr(base, frame, host)?;
                self.field_value(base, member, member_span, frame)
            }
            ExprKind::Array { elements } => {
                let elements = elements
                    .iter()
                    .map(|element| self.eval_expr(element, frame, host))
                    .collect::<anyhow::Result<Vec<_>>>()?;
                Ok(Value::Array(Arc::new(elements)))
            }
            ExprKind::Tuple { elements } if elements.is_empty() => Ok(Value::Unit),
            ExprKind::Tuple { elements } => {
                let elements = elements
                    .iter()
                    .map(|element| self.eval_expr(element, frame, host))
                    .collect::<anyhow::Result<Vec<_>>>()?;
                Ok(Value::Tuple(Arc::new(elements)))
            }
            ExprKind::TupleField { base, index } => {
                let base = self.eval_expr(base, frame, host)?;
                match base {
                    Value::Tuple(values) | Value::Array(values) => values
                        .get(*index)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("tuple field {index} is out of bounds")),
                    value => anyhow::bail!("cannot read tuple field from {value:?}"),
                }
            }
            ExprKind::Struct { name, fields } => {
                let mut object = BTreeMap::new();
                for field in fields {
                    let value = self.eval_expr(&field.value, frame, host)?;
                    object.insert(field.name.clone(), value);
                }
                let type_path = name.join("::");
                if let Some((type_name, case)) = enum_constructor_path(&type_path) {
                    Ok(enum_variant_value(
                        type_name,
                        case,
                        vec![Value::Object(Arc::new(object))],
                    ))
                } else if self.programs.get(&frame.plugin).is_some_and(|program| {
                    program.semantic_profile == SemanticProfile::LegacyJavaScript
                }) {
                    Ok(Value::Object(Arc::new(object)))
                } else {
                    Ok(Value::Struct {
                        type_name: type_path,
                        fields: Arc::new(object),
                    })
                }
            }
            ExprKind::Index { base, index } => {
                let base = self.eval_expr(base, frame, host)?;
                if let ExprKind::Range {
                    start,
                    end,
                    inclusive,
                } = &index.kind
                {
                    let start = start
                        .as_deref()
                        .map(|bound| self.eval_range_bound(bound, frame, host))
                        .transpose()?;
                    let end = end
                        .as_deref()
                        .map(|bound| self.eval_range_bound(bound, frame, host))
                        .transpose()?;
                    return self
                        .slice_value(base, start, end, *inclusive)
                        .map_err(|error| self.enrich_runtime_error(error, frame, &expr.span));
                }
                let index = self.eval_expr(index, frame, host)?;
                self.index_value(base, index)
            }
            ExprKind::Unary { op, expr } => {
                let value = self.eval_expr(expr, frame, host)?;
                self.eval_unary(*op, value)
            }
            ExprKind::Binary { op, left, right } => self.eval_binary(*op, left, right, frame, host),
            ExprKind::Block(block) => self.eval_statements(&block.statements, frame, host),
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                if self.eval_condition(cond, frame, host)? {
                    self.eval_expr(then_branch, frame, host)
                } else {
                    self.eval_expr(else_branch, frame, host)
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                let value = self.eval_expr(scrutinee, frame, host)?;
                let mut matched = None;
                for arm in arms {
                    let mut bindings = Vec::new();
                    if match_pattern(&arm.pattern, &value, &mut bindings) {
                        self.bind_locals(frame, bindings)?;
                        matched = Some(self.eval_expr(&arm.expr, frame, host));
                        break;
                    }
                }
                matched.unwrap_or_else(|| anyhow::bail!("non-exhaustive match reached at runtime"))
            }
            ExprKind::Assign { target, op, value } => {
                self.eval_assignment(target, *op, value, frame, host)
            }
            ExprKind::Range {
                start,
                end,
                inclusive,
            } => self.eval_range(start.as_deref(), end.as_deref(), *inclusive, frame, host),
            ExprKind::Cast { expr, target_ty } => {
                let value = self.eval_expr(expr, frame, host)?;
                cast_value(value, target_ty)
            }
            ExprKind::Try { expr } => {
                let value = self.eval_expr(expr, frame, host)?;
                match variant_value_parts(&value) {
                    Some((_, "Ok" | "Some", [value])) => Ok(value.clone()),
                    Some((_, "Err" | "None", _)) => Err(anyhow::Error::new(EarlyReturn { value })),
                    _ => anyhow::bail!("Husk `?` requires a Result or Option value"),
                }
            }
            ExprKind::Format { format, args } => {
                let values = args
                    .iter()
                    .map(|argument| self.eval_expr(argument, frame, host))
                    .collect::<anyhow::Result<Vec<_>>>()?;
                Ok(Value::String(format_husk_values(format, &values)?))
            }
            ExprKind::FormatPrint {
                format,
                args,
                newline: _,
            } => {
                let values = args
                    .iter()
                    .map(|argument| self.eval_expr(argument, frame, host))
                    .collect::<anyhow::Result<Vec<_>>>()?;
                host.log(&format_husk_values(format, &values)?);
                Ok(Value::Unit)
            }
            ExprKind::Closure {
                params,
                captures,
                body,
                ..
            } => {
                let captures = captures
                    .iter()
                    .map(|local| {
                        frame
                            .locals
                            .get(local)
                            .copied()
                            .map(|handle| (*local, handle))
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "closure capture references unavailable local {}",
                                    local.index()
                                )
                            })
                    })
                    .collect::<anyhow::Result<HashMap<_, _>>>()?;
                self.heap
                    .allocate_closure(
                        self.instance_generation,
                        ClosureObject {
                            parameters: params.clone(),
                            body: (**body).clone(),
                            captures,
                            plugin: frame.plugin.clone(),
                            module_path: frame.module_path.clone(),
                            imports: frame.imports.clone(),
                        },
                    )
                    .map(Value::Closure)
            }
            ExprKind::MethodCall {
                receiver,
                method,
                target,
                args,
                ..
            } => {
                let receiver_value = self.eval_expr(receiver, frame, host)?;
                let argument_values = args
                    .iter()
                    .map(|argument| self.eval_expr(argument, frame, host))
                    .collect::<anyhow::Result<Vec<_>>>()?;
                match target {
                    MethodTarget::Script(function_id) => {
                        let mut values = Vec::with_capacity(argument_values.len() + 1);
                        values.push(receiver_value);
                        values.extend(argument_values);
                        let is_mutable_receiver = self
                            .programs
                            .get(&frame.plugin)
                            .and_then(|program| program.functions.get(*function_id))
                            .is_some_and(|function| {
                                function.receiver == Some(SelfReceiver::RefMut)
                            });
                        let receiver_cell = if is_mutable_receiver {
                            Some(receiver_local_cell(receiver, frame)?)
                        } else {
                            None
                        };
                        let plugin = frame.plugin.clone();
                        self.call_function_id_in_frame_with_receiver(
                            &plugin,
                            *function_id,
                            values,
                            receiver_cell,
                            frame,
                            host,
                        )
                    }
                    MethodTarget::Module(function) => {
                        self.call_module_function(*function, argument_values, frame, host)
                    }
                    MethodTarget::Intrinsic(intrinsic) => self.call_intrinsic_method(
                        *intrinsic,
                        receiver,
                        &receiver_value,
                        &argument_values,
                        frame,
                        host,
                    ),
                    MethodTarget::LegacyDynamic => self.call_legacy_method(
                        receiver,
                        receiver_value,
                        method,
                        argument_values,
                        frame,
                        host,
                    ),
                    MethodTarget::Unresolved => {
                        anyhow::bail!(
                            "Husk method `{method}` reached runtime without a resolved target"
                        )
                    }
                }
            }
            ExprKind::JsLiteral { .. } => {
                anyhow::bail!("unsupported Husk expression in embedded runtime")
            }
        };
        result.map_err(|error| self.enrich_runtime_error(error, frame, &expr.span))
    }

    fn eval_range<H: Host>(
        &mut self,
        start: Option<&Expr>,
        end: Option<&Expr>,
        inclusive: bool,
        frame: &mut Frame,
        host: &mut H,
    ) -> anyhow::Result<Value> {
        let Some(start) = start else {
            anyhow::bail!("an open-start range needs a slicing context");
        };
        let Some(end) = end else {
            anyhow::bail!("an open-ended range needs a slicing context");
        };
        let start_value = self.eval_expr(start, frame, host)?;
        let start = value_to_i64(&start_value)
            .ok_or_else(|| anyhow::anyhow!("range start must be an integer"))?;
        let end_value = self.eval_expr(end, frame, host)?;
        let end = value_to_i64(&end_value)
            .ok_or_else(|| anyhow::anyhow!("range end must be an integer"))?;
        Ok(Value::Range {
            start,
            end,
            inclusive,
        })
    }

    fn eval_range_bound<H: Host>(
        &mut self,
        bound: &Expr,
        frame: &mut Frame,
        host: &mut H,
    ) -> anyhow::Result<i64> {
        let value = self.eval_expr(bound, frame, host)?;
        value_to_i64(&value).ok_or_else(|| anyhow::anyhow!("slice range bound must be an integer"))
    }

    fn eval_assignment<H: Host>(
        &mut self,
        target: &Expr,
        op: AssignOp,
        value: &Expr,
        frame: &mut Frame,
        host: &mut H,
    ) -> anyhow::Result<Value> {
        let right = self.eval_expr(value, frame, host)?;
        match &target.kind {
            ExprKind::Ident {
                local: Some(local), ..
            } => {
                let left = self.local_value(frame, *local)?;
                let assigned = self.assignment_value(op, left, right)?;
                self.set_local(frame, *local, assigned.clone())?;
                Ok(assigned)
            }
            ExprKind::Field { base, member, .. } => {
                let ExprKind::Ident {
                    local: Some(local), ..
                } = &base.kind
                else {
                    anyhow::bail!(
                        "embedded Husk runtime only supports assignment through a local binding"
                    );
                };
                let base = self.local_value(frame, *local)?;
                match base {
                    Value::Object(mut object) => {
                        let assigned = {
                            let fields = Arc::make_mut(&mut object);
                            let left = fields.get(member).cloned().unwrap_or(Value::Unit);
                            let assigned = self.assignment_value(op, left, right)?;
                            fields.insert(member.clone(), assigned.clone());
                            assigned
                        };
                        self.set_local(frame, *local, Value::Object(object))?;
                        Ok(assigned)
                    }
                    Value::Struct {
                        type_name,
                        mut fields,
                    } => {
                        let assigned = {
                            let fields = Arc::make_mut(&mut fields);
                            let left = fields.get(member).cloned().unwrap_or(Value::Unit);
                            let assigned = self.assignment_value(op, left, right)?;
                            fields.insert(member.clone(), assigned.clone());
                            assigned
                        };
                        self.set_local(frame, *local, Value::Struct { type_name, fields })?;
                        Ok(assigned)
                    }
                    Value::Json(serde_json::Value::Object(mut object)) => {
                        let left = object.get(member).map_or(Value::Unit, json_to_value);
                        let assigned = self.assignment_value(op, left, right)?;
                        object.insert(member.clone(), value_to_json(&assigned));
                        self.set_local(
                            frame,
                            *local,
                            Value::Json(serde_json::Value::Object(object)),
                        )?;
                        Ok(assigned)
                    }
                    _ => anyhow::bail!("cannot assign field on a non-object"),
                }
            }
            ExprKind::Index { base, index } => {
                let ExprKind::Ident {
                    local: Some(local), ..
                } = &base.kind
                else {
                    anyhow::bail!(
                        "embedded Husk runtime only supports assignment through a local binding"
                    );
                };
                let index = self.eval_expr(index, frame, host)?;
                let base = self.local_value(frame, *local)?;
                let (updated, assigned) = match (base, index) {
                    (Value::Array(mut values), Value::Int(index)) => {
                        let index = usize::try_from(index).map_err(|_| {
                            anyhow::anyhow!("array assignment index must be non-negative")
                        })?;
                        let assigned = {
                            let items = Arc::make_mut(&mut values);
                            let left = items.get(index).cloned().unwrap_or(Value::Unit);
                            let assigned = self.assignment_value(op, left, right)?;
                            let slot = items.get_mut(index).ok_or_else(|| {
                                anyhow::anyhow!("array assignment index is out of bounds")
                            })?;
                            *slot = assigned.clone();
                            assigned
                        };
                        (Value::Array(values), assigned)
                    }
                    (Value::Object(mut values), Value::String(index)) => {
                        let assigned = {
                            let fields = Arc::make_mut(&mut values);
                            let left = fields.get(&index).cloned().unwrap_or(Value::Unit);
                            let assigned = self.assignment_value(op, left, right)?;
                            fields.insert(index, assigned.clone());
                            assigned
                        };
                        (Value::Object(values), assigned)
                    }
                    (Value::Json(serde_json::Value::Array(mut values)), Value::Int(index)) => {
                        let index = usize::try_from(index).map_err(|_| {
                            anyhow::anyhow!("array assignment index must be non-negative")
                        })?;
                        let left = values.get(index).map_or(Value::Unit, json_to_value);
                        let assigned = self.assignment_value(op, left, right)?;
                        let slot = values.get_mut(index).ok_or_else(|| {
                            anyhow::anyhow!("array assignment index is out of bounds")
                        })?;
                        *slot = value_to_json(&assigned);
                        (Value::Json(serde_json::Value::Array(values)), assigned)
                    }
                    (Value::Json(serde_json::Value::Object(mut values)), Value::String(index)) => {
                        let left = values.get(&index).map_or(Value::Unit, json_to_value);
                        let assigned = self.assignment_value(op, left, right)?;
                        values.insert(index, value_to_json(&assigned));
                        (Value::Json(serde_json::Value::Object(values)), assigned)
                    }
                    (base, index) => {
                        anyhow::bail!("cannot assign Husk value {base:?} with index {index:?}")
                    }
                };
                self.set_local(frame, *local, updated)?;
                Ok(assigned)
            }
            _ => anyhow::bail!("embedded Husk runtime cannot assign to this expression"),
        }
    }

    fn assignment_value(&self, op: AssignOp, left: Value, right: Value) -> anyhow::Result<Value> {
        if matches!(op, AssignOp::Assign) {
            return Ok(right);
        }
        let binary = match op {
            AssignOp::Assign => unreachable!(),
            AssignOp::AddAssign => BinaryOp::Add,
            AssignOp::SubAssign => BinaryOp::Sub,
            AssignOp::ModAssign => BinaryOp::Mod,
        };
        self.eval_binary_values(binary, left, right)
    }

    fn eval_condition<H: Host>(
        &mut self,
        expr: &Expr,
        frame: &mut Frame,
        host: &mut H,
    ) -> anyhow::Result<bool> {
        let value = self.eval_expr(expr, frame, host)?;
        value
            .as_bool()
            .ok_or_else(|| anyhow::anyhow!("Husk condition must evaluate to a bool"))
    }

    fn eval_unary(&self, op: UnaryOp, value: Value) -> anyhow::Result<Value> {
        match op {
            UnaryOp::Not => Ok(Value::Bool(!value.as_bool().ok_or_else(|| {
                anyhow::anyhow!("Husk `!` operator requires a bool operand")
            })?)),
            UnaryOp::Neg => match value {
                Value::Int(value) => Ok(Value::Int(-value)),
                Value::Float(value) => Ok(Value::Float(-value)),
                _ => anyhow::bail!("Husk unary `-` operator requires a number operand"),
            },
        }
    }

    fn eval_binary<H: Host>(
        &mut self,
        op: BinaryOp,
        left: &Expr,
        right: &Expr,
        frame: &mut Frame,
        host: &mut H,
    ) -> anyhow::Result<Value> {
        match op {
            BinaryOp::And => {
                if !self.eval_condition(left, frame, host)? {
                    return Ok(Value::Bool(false));
                }
                Ok(Value::Bool(self.eval_condition(right, frame, host)?))
            }
            BinaryOp::Or => {
                if self.eval_condition(left, frame, host)? {
                    return Ok(Value::Bool(true));
                }
                Ok(Value::Bool(self.eval_condition(right, frame, host)?))
            }
            BinaryOp::Eq | BinaryOp::NotEq => {
                let left = self.eval_expr(left, frame, host)?;
                let right = self.eval_expr(right, frame, host)?;
                let equal = left == right;
                Ok(Value::Bool(if matches!(op, BinaryOp::Eq) {
                    equal
                } else {
                    !equal
                }))
            }
            BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Mod
            | BinaryOp::Lt
            | BinaryOp::Gt
            | BinaryOp::Le
            | BinaryOp::Ge => {
                let left = self.eval_expr(left, frame, host)?;
                let right = self.eval_expr(right, frame, host)?;
                self.eval_binary_values(op, left, right)
            }
        }
    }

    fn eval_binary_values(&self, op: BinaryOp, left: Value, right: Value) -> anyhow::Result<Value> {
        match op {
            BinaryOp::Add
                if matches!(left, Value::String(_)) || matches!(right, Value::String(_)) =>
            {
                Ok(Value::String(format!(
                    "{}{}",
                    value_to_log_string(&left),
                    value_to_log_string(&right)
                )))
            }
            BinaryOp::Add => numeric_binary(left, right, |left, right| left + right),
            BinaryOp::Sub => numeric_binary(left, right, |left, right| left - right),
            BinaryOp::Mul => numeric_binary(left, right, |left, right| left * right),
            BinaryOp::Div => divide_values(left, right),
            BinaryOp::Mod => numeric_binary(left, right, |left, right| left % right),
            BinaryOp::Lt => compare_values(left, right, |ordering| ordering.is_lt()),
            BinaryOp::Gt => compare_values(left, right, |ordering| ordering.is_gt()),
            BinaryOp::Le => compare_values(left, right, |ordering| !ordering.is_gt()),
            BinaryOp::Ge => compare_values(left, right, |ordering| !ordering.is_lt()),
            BinaryOp::Eq | BinaryOp::NotEq | BinaryOp::And | BinaryOp::Or => {
                anyhow::bail!("internal unsupported Husk binary operator")
            }
        }
    }

    fn field_value(
        &self,
        base: Value,
        member: &str,
        span: &Span,
        frame: &Frame,
    ) -> anyhow::Result<Value> {
        match base {
            Value::Object(value) => {
                let Some(field) = value.get(member) else {
                    let available_fields = value.keys().cloned().collect::<Vec<_>>();
                    return Ok(Value::Missing(MissingValue {
                        field: member.to_string(),
                        span: span.clone(),
                        available_fields,
                    }));
                };
                Ok(field.clone())
            }
            Value::Struct { fields, .. } => {
                let Some(field) = fields.get(member) else {
                    let available_fields = fields.keys().cloned().collect::<Vec<_>>();
                    return Ok(Value::Missing(MissingValue {
                        field: member.to_string(),
                        span: span.clone(),
                        available_fields,
                    }));
                };
                Ok(field.clone())
            }
            Value::Json(serde_json::Value::Object(value)) => {
                let Some(field) = value.get(member) else {
                    let mut available_fields = value.keys().cloned().collect::<Vec<_>>();
                    available_fields.sort();
                    return Ok(Value::Missing(MissingValue {
                        field: member.to_string(),
                        span: span.clone(),
                        available_fields,
                    }));
                };
                Ok(json_to_value(field))
            }
            Value::Missing(missing) => Err(self.missing_field_error(&missing, frame)),
            value => Err(self.runtime_error(
                "HUSK-R0005",
                format!("cannot read field `{member}` from {}", value.kind_name()),
                span,
                "field access is not supported for this value",
                frame,
            )),
        }
    }

    fn missing_field_error(&self, missing: &MissingValue, frame: &Frame) -> anyhow::Error {
        let mut diagnostic = self.runtime_diagnostic(
            "HUSK-R0004",
            format!("unknown field `{}`", missing.field),
            &missing.span,
            "unknown field",
            frame,
        );
        if let Some(suggestion) = closest_field(&missing.field, &missing.available_fields) {
            diagnostic =
                diagnostic.with_help(format!("a similarly named field exists: `{suggestion}`"));
        }
        if !missing.available_fields.is_empty() {
            diagnostic = diagnostic.with_note(format!(
                "available fields: {}",
                missing.available_fields.join(", ")
            ));
        }
        anyhow::Error::new(Report::new(diagnostic))
    }

    fn enrich_runtime_error(
        &self,
        error: anyhow::Error,
        frame: &Frame,
        span: &Span,
    ) -> anyhow::Error {
        if error.downcast_ref::<Report>().is_some() || error.downcast_ref::<EarlyReturn>().is_some()
        {
            return error;
        }
        self.runtime_error(
            "HUSK-R0001",
            error.to_string(),
            span,
            "runtime error occurred here",
            frame,
        )
    }

    fn runtime_error(
        &self,
        code: &'static str,
        message: impl Into<String>,
        span: &Span,
        label: impl Into<String>,
        frame: &Frame,
    ) -> anyhow::Error {
        anyhow::Error::new(Report::new(
            self.runtime_diagnostic(code, message, span, label, frame),
        ))
    }

    fn runtime_diagnostic(
        &self,
        code: &'static str,
        message: impl Into<String>,
        span: &Span,
        label: impl Into<String>,
        frame: &Frame,
    ) -> Diagnostic {
        let source = self
            .programs
            .get(&frame.plugin)
            .map(|program| {
                span.file_path()
                    .and_then(|path| program.source_map.get(path))
                    .cloned()
                    .unwrap_or_else(|| program.source.clone())
            })
            .unwrap_or_else(|| SourceFile::new(format!("plugins/{}.hk", frame.plugin), ""));
        Diagnostic::new(code, message, source, span.clone(), label)
    }

    fn index_value(&self, base: Value, index: Value) -> anyhow::Result<Value> {
        match (base, index) {
            (Value::Array(values) | Value::Tuple(values), Value::Int(index)) => {
                let Ok(index) = usize::try_from(index) else {
                    return Ok(Value::Unit);
                };
                Ok(values.get(index).cloned().unwrap_or(Value::Unit))
            }
            (Value::Object(values), Value::String(index)) => {
                Ok(values.get(&index).cloned().unwrap_or(Value::Unit))
            }
            (Value::Json(serde_json::Value::Array(values)), Value::Int(index)) => {
                let Ok(index) = usize::try_from(index) else {
                    return Ok(Value::Unit);
                };
                Ok(values.get(index).map_or(Value::Unit, json_to_value))
            }
            (Value::Json(serde_json::Value::Object(values)), Value::String(index)) => {
                Ok(values.get(&index).map_or(Value::Unit, json_to_value))
            }
            (Value::String(value), Value::Int(index)) => {
                let Ok(index) = usize::try_from(index) else {
                    return Ok(Value::Unit);
                };
                Ok(value
                    .chars()
                    .nth(index)
                    .map_or(Value::Unit, |value| Value::String(value.to_string())))
            }
            (base, index) => anyhow::bail!("cannot index Husk value `{base:?}` with `{index:?}`"),
        }
    }

    fn slice_value(
        &self,
        base: Value,
        start: Option<i64>,
        end: Option<i64>,
        inclusive: bool,
    ) -> anyhow::Result<Value> {
        match base {
            Value::Array(values) => {
                let (start, end) = normalized_slice_bounds(values.len(), start, end, inclusive)?;
                Ok(Value::Array(Arc::new(values[start..end].to_vec())))
            }
            Value::Tuple(values) => {
                let (start, end) = normalized_slice_bounds(values.len(), start, end, inclusive)?;
                Ok(Value::Tuple(Arc::new(values[start..end].to_vec())))
            }
            Value::String(value) => {
                let length = value.chars().count();
                let (start, end) = normalized_slice_bounds(length, start, end, inclusive)?;
                Ok(Value::String(
                    value.chars().skip(start).take(end - start).collect(),
                ))
            }
            Value::Json(serde_json::Value::Array(values)) => {
                let (start, end) = normalized_slice_bounds(values.len(), start, end, inclusive)?;
                Ok(Value::Json(serde_json::Value::Array(
                    values[start..end].to_vec(),
                )))
            }
            value => anyhow::bail!("cannot slice Husk {}", value.kind_name()),
        }
    }

    fn call_module_function<H: Host>(
        &mut self,
        function: ModuleFunctionId,
        args: Vec<Value>,
        frame: &mut Frame,
        host: &mut H,
    ) -> anyhow::Result<Value> {
        let path = self
            .programs
            .get(&frame.plugin)
            .and_then(|program| program.module_functions.get(&function))
            .map(|function| function.path.clone())
            .ok_or_else(|| anyhow::anyhow!("unknown Husk module function ID {}", function.raw()))?;
        frame.consume_host_call()?;
        host.call_module(&frame.plugin, &path, &args)
            .unwrap_or_else(|| {
                Err(anyhow::anyhow!(
                    "registered Husk module function `{path}` has no runtime implementation"
                ))
            })
    }

    fn call_intrinsic_function<H: Host>(
        &mut self,
        function: IntrinsicFunction,
        args: &[Value],
        host: &mut H,
    ) -> anyhow::Result<Value> {
        match function {
            IntrinsicFunction::Println => {
                let value = args
                    .first()
                    .ok_or_else(|| anyhow::anyhow!("println expects one argument"))?;
                host.log(&value_to_log_string(value));
                Ok(Value::Unit)
            }
            IntrinsicFunction::Assert => {
                let condition = args
                    .first()
                    .and_then(Value::as_bool)
                    .ok_or_else(|| anyhow::anyhow!("assert expects a boolean"))?;
                if condition {
                    Ok(Value::Unit)
                } else {
                    anyhow::bail!("assertion failed")
                }
            }
            IntrinsicFunction::AssertMessage => {
                let condition = args
                    .first()
                    .and_then(Value::as_bool)
                    .ok_or_else(|| anyhow::anyhow!("assert_msg expects a boolean"))?;
                if condition {
                    Ok(Value::Unit)
                } else {
                    let message = args
                        .get(1)
                        .and_then(Value::as_str)
                        .unwrap_or("assertion failed");
                    anyhow::bail!("{message}")
                }
            }
        }
    }

    fn call_intrinsic_method<H: Host>(
        &mut self,
        intrinsic: IntrinsicMethodId,
        receiver_expr: &Expr,
        receiver: &Value,
        args: &[Value],
        frame: &mut Frame,
        host: &mut H,
    ) -> anyhow::Result<Value> {
        let target = self
            .programs
            .get(&frame.plugin)
            .and_then(|program| program.intrinsic_methods.get(&intrinsic))
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!("unknown Husk intrinsic method ID {}", intrinsic.raw())
            })?;
        if let Some(value) = self.call_higher_order_builtin_method(
            receiver_expr,
            receiver,
            &target.method,
            args,
            frame,
            host,
        )? {
            return Ok(value);
        }
        if let Some(value) =
            self.call_mutating_builtin_method(receiver_expr, receiver, &target.method, args, frame)?
        {
            return Ok(value);
        }
        if let Some(value) = call_builtin_method(receiver, &target.method, args)? {
            return Ok(value);
        }
        anyhow::bail!(
            "intrinsic method `{}::{}` has no runtime implementation",
            target.receiver_type,
            target.method
        )
    }

    fn call_legacy_method<H: Host>(
        &mut self,
        receiver_expr: &Expr,
        receiver: Value,
        method: &str,
        args: Vec<Value>,
        frame: &mut Frame,
        host: &mut H,
    ) -> anyhow::Result<Value> {
        if let Some(value) = self.call_higher_order_builtin_method(
            receiver_expr,
            &receiver,
            method,
            &args,
            frame,
            host,
        )? {
            return Ok(value);
        }
        if let Some(value) =
            self.call_mutating_builtin_method(receiver_expr, &receiver, method, &args, frame)?
        {
            return Ok(value);
        }
        if let Some(value) = call_builtin_method(&receiver, method, &args)? {
            return Ok(value);
        }
        let type_name = match &receiver {
            Value::Struct { type_name, .. } | Value::Variant { type_name, .. } => type_name.clone(),
            _ => receiver_expr.ty.clone().ok_or_else(|| {
                anyhow::anyhow!("cannot resolve legacy method `{method}` without a receiver type")
            })?,
        };
        let mut values = Vec::with_capacity(args.len() + 1);
        values.push(receiver);
        values.extend(args);
        let method_name = format!("{type_name}::{method}");
        if let Some(function_name) = self.resolve_legacy_script_function_name(&method_name, frame) {
            let function = self
                .programs
                .get(&frame.plugin)
                .and_then(|program| program.functions.id(&function_name))
                .ok_or_else(|| anyhow::anyhow!("unknown legacy method `{function_name}`"))?;
            let is_mutable_receiver = self
                .programs
                .get(&frame.plugin)
                .and_then(|program| program.functions.get(function))
                .is_some_and(|function| function.receiver == Some(SelfReceiver::RefMut));
            if is_mutable_receiver {
                let receiver_cell = receiver_local_cell(receiver_expr, frame)?;
                let plugin = frame.plugin.clone();
                return self.call_function_id_in_frame_with_receiver(
                    &plugin,
                    function,
                    values,
                    Some(receiver_cell),
                    frame,
                    host,
                );
            }
        }
        self.call_legacy_named(&method_name, values, frame, host)
    }

    fn call_value<H: Host>(
        &mut self,
        callee: Value,
        args: Vec<Value>,
        frame: &mut Frame,
        host: &mut H,
    ) -> anyhow::Result<Value> {
        match callee {
            Value::String(name)
                if self.programs.get(&frame.plugin).is_some_and(|program| {
                    program.semantic_profile == SemanticProfile::LegacyJavaScript
                }) =>
            {
                self.call_legacy_named(&name, args, frame, host)
            }
            Value::Callback(callback) => self.call_function_in_frame(&callback, args, frame, host),
            Value::Closure(handle) => self.call_closure_in_frame(handle, args, frame, host),
            _ => anyhow::bail!("value is not callable: {callee:?}"),
        }
    }

    fn call_mutating_builtin_method(
        &mut self,
        receiver_expr: &Expr,
        receiver: &Value,
        method: &str,
        args: &[Value],
        frame: &mut Frame,
    ) -> anyhow::Result<Option<Value>> {
        let Value::Array(values) = receiver else {
            return Ok(None);
        };
        if !matches!(
            method,
            "push" | "sort" | "reverse" | "pop" | "shift" | "unshift"
        ) {
            return Ok(None);
        }
        let receiver_cell = receiver_local_cell(receiver_expr, frame)?;
        let mut values = values.as_ref().clone();
        let result = match method {
            "push" => {
                expect_method_arity("array", method, args, 1)?;
                values.push(args[0].clone());
                Value::Unit
            }
            "sort" => {
                expect_method_arity("array", method, args, 0)?;
                sort_husk_values(&mut values)?;
                Value::Array(Arc::new(values.clone()))
            }
            "reverse" => {
                expect_method_arity("array", method, args, 0)?;
                values.reverse();
                Value::Array(Arc::new(values.clone()))
            }
            "pop" => {
                expect_method_arity("array", method, args, 0)?;
                values
                    .pop()
                    .ok_or_else(|| anyhow::anyhow!("cannot pop an empty Husk array"))?
            }
            "shift" => {
                expect_method_arity("array", method, args, 0)?;
                if values.is_empty() {
                    anyhow::bail!("cannot shift an empty Husk array");
                }
                values.remove(0)
            }
            "unshift" => {
                expect_method_arity("array", method, args, 1)?;
                values.insert(0, args[0].clone());
                Value::Int(saturating_i64(values.len()))
            }
            _ => unreachable!(),
        };
        self.heap
            .set_cell(receiver_cell, Value::Array(Arc::new(values)))?;
        Ok(Some(result))
    }

    fn call_higher_order_builtin_method<H: Host>(
        &mut self,
        receiver_expr: &Expr,
        receiver: &Value,
        method: &str,
        args: &[Value],
        frame: &mut Frame,
        host: &mut H,
    ) -> anyhow::Result<Option<Value>> {
        let Value::Array(values) = receiver else {
            return Ok(None);
        };
        if !matches!(
            method,
            "map"
                | "filter"
                | "some"
                | "every"
                | "reduce"
                | "forEach"
                | "for_each"
                | "find"
                | "findIndex"
                | "find_index"
                | "findLastIndex"
                | "find_last_index"
                | "sortBy"
                | "sort_by"
        ) {
            return Ok(None);
        }
        expect_method_arity("array", method, args, 1)?;
        let callback = args[0].clone();
        let result = match method {
            "map" => {
                let mut mapped = Vec::with_capacity(values.len());
                for value in values.iter() {
                    mapped.push(self.call_value(
                        callback.clone(),
                        vec![value.clone()],
                        frame,
                        host,
                    )?);
                }
                Value::Array(Arc::new(mapped))
            }
            "filter" => {
                let mut filtered = Vec::new();
                for value in values.iter() {
                    if callback_predicate(
                        self.call_value(callback.clone(), vec![value.clone()], frame, host)?,
                        method,
                    )? {
                        filtered.push(value.clone());
                    }
                }
                Value::Array(Arc::new(filtered))
            }
            "some" => {
                let mut found = false;
                for value in values.iter() {
                    if callback_predicate(
                        self.call_value(callback.clone(), vec![value.clone()], frame, host)?,
                        method,
                    )? {
                        found = true;
                        break;
                    }
                }
                Value::Bool(found)
            }
            "every" => {
                let mut all = true;
                for value in values.iter() {
                    if !callback_predicate(
                        self.call_value(callback.clone(), vec![value.clone()], frame, host)?,
                        method,
                    )? {
                        all = false;
                        break;
                    }
                }
                Value::Bool(all)
            }
            "reduce" => {
                let mut values = values.iter();
                let mut accumulator = values
                    .next()
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("cannot reduce an empty Husk array"))?;
                for value in values {
                    accumulator = self.call_value(
                        callback.clone(),
                        vec![accumulator, value.clone()],
                        frame,
                        host,
                    )?;
                }
                accumulator
            }
            "forEach" | "for_each" => {
                for value in values.iter() {
                    self.call_value(callback.clone(), vec![value.clone()], frame, host)?;
                }
                Value::Unit
            }
            "find" => {
                let mut found = None;
                for value in values.iter() {
                    if callback_predicate(
                        self.call_value(callback.clone(), vec![value.clone()], frame, host)?,
                        method,
                    )? {
                        found = Some(value.clone());
                        break;
                    }
                }
                found.map_or_else(
                    || enum_variant_value("Option", "None", Vec::new()),
                    |value| enum_variant_value("Option", "Some", vec![value]),
                )
            }
            "findIndex" | "find_index" | "findLastIndex" | "find_last_index" => {
                let reverse = matches!(method, "findLastIndex" | "find_last_index");
                let indices: Box<dyn Iterator<Item = usize>> = if reverse {
                    Box::new((0..values.len()).rev())
                } else {
                    Box::new(0..values.len())
                };
                let mut found = -1;
                for index in indices {
                    if callback_predicate(
                        self.call_value(
                            callback.clone(),
                            vec![values[index].clone()],
                            frame,
                            host,
                        )?,
                        method,
                    )? {
                        found = saturating_i64(index);
                        break;
                    }
                }
                Value::Int(found)
            }
            "sortBy" | "sort_by" => {
                let receiver_cell = receiver_local_cell(receiver_expr, frame)?;
                let mut sorted = values.as_ref().clone();
                // Stable insertion sort permits a fallible Husk comparator
                // without hiding callback errors inside Rust's `sort_by`.
                for index in 1..sorted.len() {
                    let mut cursor = index;
                    while cursor > 0 {
                        let ordering = self.call_value(
                            callback.clone(),
                            vec![sorted[cursor - 1].clone(), sorted[cursor].clone()],
                            frame,
                            host,
                        )?;
                        let ordering = value_to_i64(&ordering).ok_or_else(|| {
                            anyhow::anyhow!(
                                "array method `{method}` comparator must return an integer"
                            )
                        })?;
                        if ordering <= 0 {
                            break;
                        }
                        sorted.swap(cursor - 1, cursor);
                        cursor -= 1;
                    }
                }
                let result = Value::Array(Arc::new(sorted));
                self.heap.set_cell(receiver_cell, result.clone())?;
                result
            }
            _ => unreachable!(),
        };
        Ok(Some(result))
    }

    fn call_closure_in_frame<H: Host>(
        &mut self,
        handle: FunctionHandle,
        args: Vec<Value>,
        parent_frame: &mut Frame,
        host: &mut H,
    ) -> anyhow::Result<Value> {
        if parent_frame.call_depth >= self.call_depth_limit {
            anyhow::bail!("Husk call depth exceeded");
        }
        let closure = self.heap.closure(handle, self.instance_generation)?.clone();
        let legacy_arity = self
            .programs
            .get(&closure.plugin)
            .is_some_and(|program| program.semantic_profile == SemanticProfile::LegacyJavaScript);
        if !legacy_arity && args.len() != closure.parameters.len() {
            anyhow::bail!(
                "Husk closure expects {} argument{}, got {}",
                closure.parameters.len(),
                if closure.parameters.len() == 1 {
                    ""
                } else {
                    "s"
                },
                args.len()
            );
        }
        let mut frame = Frame {
            plugin: closure.plugin.clone(),
            locals: closure.captures,
            owned_cells: Vec::new(),
            module_path: closure.module_path,
            imports: closure.imports,
            remaining: parent_frame.remaining,
            remaining_host_calls: parent_frame.remaining_host_calls,
            call_depth: parent_frame.call_depth + 1,
        };
        for (parameter, value) in closure.parameters.iter().zip(args) {
            self.bind_local(&mut frame, parameter.local.id, value)?;
        }
        let callback = Callback::new(closure.plugin, "<closure>");
        let result = self
            .eval_expr(&closure.body, &mut frame, host)
            .or_else(|error| match error.downcast::<EarlyReturn>() {
                Ok(early) => Ok(early.value),
                Err(error) => Err(error),
            })
            .map_err(|error| with_call_frame(error, &callback));
        parent_frame.remaining = frame.remaining;
        parent_frame.remaining_host_calls = frame.remaining_host_calls;
        self.release_frame_cells(&frame, parent_frame, result.as_ref().ok());
        result
    }

    fn resolve_legacy_script_function_name(&self, name: &str, frame: &Frame) -> Option<String> {
        let segments = name.split("::").map(str::to_string).collect::<Vec<_>>();
        let resolved = frame
            .imports
            .get(name)
            .cloned()
            .or_else(|| normalize_source_path(&segments, &frame.module_path))
            .unwrap_or_else(|| name.to_string());
        if self.has_function(&frame.plugin, &resolved) {
            return Some(resolved);
        }
        if !name.contains("::") && !frame.module_path.is_empty() {
            let local = format!("{}::{name}", frame.module_path.join("::"));
            if self.has_function(&frame.plugin, &local) {
                return Some(local);
            }
        }
        if name.contains("::")
            && !matches!(
                segments.first().map(String::as_str),
                Some("crate" | "self" | "super")
            )
            && !frame.module_path.is_empty()
        {
            let relative = format!("{}::{name}", frame.module_path.join("::"));
            if self.has_function(&frame.plugin, &relative) {
                return Some(relative);
            }
        }
        None
    }

    fn call_legacy_named<H: Host>(
        &mut self,
        name: &str,
        args: Vec<Value>,
        frame: &mut Frame,
        host: &mut H,
    ) -> anyhow::Result<Value> {
        let path_segments = name.split("::").map(str::to_string).collect::<Vec<_>>();
        let mut resolved_name = frame
            .imports
            .get(name)
            .cloned()
            .or_else(|| normalize_source_path(&path_segments, &frame.module_path))
            .unwrap_or_else(|| name.to_string());
        if !name.contains("::")
            && !frame.imports.contains_key(name)
            && !frame.module_path.is_empty()
        {
            let local = format!("{}::{name}", frame.module_path.join("::"));
            if self.has_function(&frame.plugin, &local) {
                resolved_name = local;
            }
        }
        let module_root = resolved_name.split("::").next().unwrap_or_default();
        let registered_external = self.programs.get(&frame.plugin).is_some_and(|program| {
            program
                .modules
                .iter()
                .any(|module| module.name.as_str() == module_root)
        });
        if registered_external {
            frame.consume_host_call()?;
            return host
                .call_module(&frame.plugin, &resolved_name, &args)
                .unwrap_or_else(|| {
                    Err(anyhow::anyhow!(
                        "registered Husk module `{module_root}` has no runtime implementation"
                    ))
                });
        }
        if let Some(result) = host.call_module(&frame.plugin, &resolved_name, &args) {
            // Compatibility hosts can expose modules without descriptors. They
            // remain charged, although descriptor-backed modules are checked
            // before entering host code above.
            frame.consume_host_call()?;
            return result;
        }
        if name.contains("::")
            && !matches!(
                path_segments.first().map(String::as_str),
                Some("crate" | "self" | "super")
            )
            && !frame.module_path.is_empty()
        {
            let relative_name = format!("{}::{name}", frame.module_path.join("::"));
            if self.has_function(&frame.plugin, &relative_name) {
                return self.call_function_in_frame(
                    &Callback::new(frame.plugin.clone(), relative_name),
                    args,
                    frame,
                    host,
                );
            }
        }
        if let Some((type_name, case)) = match name {
            "Some" | "Option::Some" => Some(("Option", "Some")),
            "Ok" | "Result::Ok" => Some(("Result", "Ok")),
            "Err" | "Result::Err" => Some(("Result", "Err")),
            _ => None,
        } {
            return Ok(enum_variant_value(type_name, case, args));
        }
        if let Some((type_name, case)) = enum_constructor_path(name) {
            return Ok(enum_variant_value(type_name, case, args));
        }
        if self.has_function(&frame.plugin, &resolved_name) {
            self.call_function_in_frame(
                &Callback::new(frame.plugin.clone(), resolved_name),
                args,
                frame,
                host,
            )
        } else {
            anyhow::bail!("unknown Husk function `{name}`")
        }
    }
}

fn iterable_values(value: Value) -> anyhow::Result<Box<dyn Iterator<Item = Value>>> {
    match value {
        Value::Array(values) | Value::Tuple(values) => {
            let length = values.len();
            Ok(Box::new(
                (0..length).map(move |index| values[index].clone()),
            ))
        }
        Value::Json(serde_json::Value::Array(values)) => Ok(Box::new(
            values.into_iter().map(|value| json_to_value(&value)),
        )),
        Value::String(value) => Ok(Box::new(
            value
                .chars()
                .map(|value| Value::String(value.to_string()))
                .collect::<Vec<_>>()
                .into_iter(),
        )),
        Value::Range {
            start,
            end,
            inclusive: true,
        } => Ok(Box::new((start..=end).map(Value::Int))),
        Value::Range {
            start,
            end,
            inclusive: false,
        } => Ok(Box::new((start..end).map(Value::Int))),
        value => anyhow::bail!("Husk `for` requires an iterable value, got {value:?}"),
    }
}

fn enum_variant_value(type_name: &str, case: &str, fields: Vec<Value>) -> Value {
    Value::Variant {
        type_name: type_name.to_string(),
        case: case.to_string(),
        fields: Arc::new(fields),
    }
}

fn enum_constructor_path(path: &str) -> Option<(&str, &str)> {
    let (type_name, case) = path.rsplit_once("::")?;
    let type_name = type_name.rsplit("::").next()?;
    let is_type = type_name.chars().next().is_some_and(char::is_uppercase);
    let is_case = case.chars().next().is_some_and(char::is_uppercase);
    (is_type && is_case).then_some((type_name, case))
}

fn json_to_value(value: &serde_json::Value) -> Value {
    match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(value) => Value::Bool(*value),
        serde_json::Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Value::Int(value)
            } else if let Some(value) = value.as_f64() {
                Value::Float(value)
            } else {
                Value::Json(serde_json::Value::Number(value.clone()))
            }
        }
        serde_json::Value::String(value) => Value::String(value.clone()),
        serde_json::Value::Array(values) => {
            Value::Array(Arc::new(values.iter().map(json_to_value).collect()))
        }
        serde_json::Value::Object(values) => Value::Object(Arc::new(
            values
                .iter()
                .map(|(key, value)| (key.clone(), json_to_value(value)))
                .collect(),
        )),
    }
}

fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Unit | Value::Null | Value::Missing(_) => serde_json::Value::Null,
        Value::Bool(value) => serde_json::Value::Bool(*value),
        Value::Int(value) => serde_json::Value::Number((*value).into()),
        Value::Float(value) => serde_json::Number::from_f64(*value)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::String(value) => serde_json::Value::String(value.clone()),
        Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(value_to_json).collect())
        }
        Value::Tuple(values) => {
            serde_json::Value::Array(values.iter().map(value_to_json).collect())
        }
        Value::Range {
            start,
            end,
            inclusive,
        } => serde_json::json!({
            "$range": {
                "start": start,
                "end": end,
                "inclusive": inclusive,
            }
        }),
        Value::Object(values) => serde_json::Value::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), value_to_json(value)))
                .collect(),
        ),
        Value::Struct { fields, .. } => serde_json::Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), value_to_json(value)))
                .collect(),
        ),
        Value::Variant {
            type_name,
            case,
            fields,
        } => serde_json::Value::Object(
            [
                (
                    "$type".to_string(),
                    serde_json::Value::String(type_name.clone()),
                ),
                ("$case".to_string(), serde_json::Value::String(case.clone())),
                (
                    "$fields".to_string(),
                    serde_json::Value::Array(fields.iter().map(value_to_json).collect()),
                ),
            ]
            .into_iter()
            .collect(),
        ),
        Value::Json(value) => value.clone(),
        Value::Callback(callback) => {
            serde_json::Value::String(format!("{}::{}", callback.plugin, callback.function))
        }
        Value::Closure(_) => serde_json::Value::String("<closure>".to_string()),
    }
}

fn numeric_binary<F>(left: Value, right: Value, op: F) -> anyhow::Result<Value>
where
    F: FnOnce(f64, f64) -> f64,
{
    let Some(left_number) = value_to_f64(&left) else {
        anyhow::bail!("Husk numeric operator requires numbers, got {left:?}");
    };
    let Some(right_number) = value_to_f64(&right) else {
        anyhow::bail!("Husk numeric operator requires numbers, got {right:?}");
    };
    let result = op(left_number, right_number);
    if matches!((&left, &right), (Value::Int(_), Value::Int(_))) && result.fract() == 0.0 {
        Ok(Value::Int(result as i64))
    } else {
        Ok(Value::Float(result))
    }
}

fn divide_values(left: Value, right: Value) -> anyhow::Result<Value> {
    match (left, right) {
        (Value::Int(_), Value::Int(0)) => anyhow::bail!("integer division by zero"),
        (Value::Int(left), Value::Int(right)) => left
            .checked_div(right)
            .map(Value::Int)
            .ok_or_else(|| anyhow::anyhow!("integer division overflow")),
        (left, right) => numeric_binary(left, right, |left, right| left / right),
    }
}

fn compare_values<F>(left: Value, right: Value, pred: F) -> anyhow::Result<Value>
where
    F: FnOnce(std::cmp::Ordering) -> bool,
{
    let ordering = match (&left, &right) {
        (Value::String(left), Value::String(right)) => left.cmp(right),
        _ => {
            let Some(left_number) = value_to_f64(&left) else {
                anyhow::bail!("Husk comparison requires comparable values, got {left:?}");
            };
            let Some(right_number) = value_to_f64(&right) else {
                anyhow::bail!("Husk comparison requires comparable values, got {right:?}");
            };
            left_number
                .partial_cmp(&right_number)
                .ok_or_else(|| anyhow::anyhow!("Husk comparison cannot compare NaN"))?
        }
    };
    Ok(Value::Bool(pred(ordering)))
}

fn value_to_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Int(value) => Some(*value as f64),
        Value::Float(value) => Some(*value),
        _ => None,
    }
}

fn value_to_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Int(value) => Some(*value),
        Value::Float(value) => Some(*value as i64),
        Value::String(value) => value.parse().ok(),
        Value::Json(serde_json::Value::Number(value)) => value.as_i64(),
        Value::Json(serde_json::Value::String(value)) => value.parse().ok(),
        _ => None,
    }
}

fn normalize_string_index(index: i64, len: i64) -> i64 {
    if index < 0 {
        (len + index).clamp(0, len)
    } else {
        index.clamp(0, len)
    }
}

fn normalized_slice_bounds(
    length: usize,
    start: Option<i64>,
    end: Option<i64>,
    inclusive: bool,
) -> anyhow::Result<(usize, usize)> {
    let length_i64 = i64::try_from(length).unwrap_or(i64::MAX);
    let start = normalize_string_index(start.unwrap_or(0), length_i64);
    let mut end = normalize_string_index(end.unwrap_or(length_i64), length_i64);
    if inclusive && end < length_i64 {
        end = end
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("inclusive slice end overflows"))?;
    }
    let start = usize::try_from(start).unwrap_or(0);
    let end = usize::try_from(end).unwrap_or(length).max(start);
    Ok((start.min(length), end.min(length)))
}

#[derive(Debug, Clone)]
struct Frame {
    plugin: String,
    locals: HashMap<LocalId, CellHandle>,
    owned_cells: Vec<CellHandle>,
    module_path: Vec<String>,
    imports: HashMap<String, String>,
    remaining: usize,
    remaining_host_calls: usize,
    call_depth: usize,
}

impl Frame {
    fn consume(&mut self) -> anyhow::Result<()> {
        if self.remaining == 0 {
            anyhow::bail!("Husk instruction budget exhausted");
        }
        self.remaining -= 1;
        Ok(())
    }

    fn consume_host_call(&mut self) -> anyhow::Result<()> {
        if self.remaining_host_calls == 0 {
            anyhow::bail!("Husk host-call budget exhausted");
        }
        self.remaining_host_calls -= 1;
        Ok(())
    }
}

enum Flow {
    Continue(Value),
    Return(Value),
    Break,
    LoopContinue,
}

#[derive(Debug)]
struct EarlyReturn {
    value: Value,
}

impl fmt::Display for EarlyReturn {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("early return from `?`")
    }
}

impl std::error::Error for EarlyReturn {}

fn variant_value_parts(value: &Value) -> Option<(&str, &str, &[Value])> {
    if let Value::Variant {
        type_name,
        case,
        fields,
    } = value
    {
        return Some((type_name, case, fields));
    }
    let Value::Object(fields) = value else {
        return None;
    };
    let (Some(Value::String(type_name)), Some(Value::String(case)), Some(Value::Array(payload))) = (
        fields.get("$type"),
        fields.get("$case"),
        fields.get("$fields"),
    ) else {
        return None;
    };
    Some((type_name, case, payload))
}

fn match_pattern(
    pattern: &husk_hir::Pattern,
    value: &Value,
    bindings: &mut Vec<(LocalId, Value)>,
) -> bool {
    match &pattern.kind {
        PatternKind::Wildcard => true,
        PatternKind::Binding(local) => {
            bindings.push((local.id, value.clone()));
            true
        }
        PatternKind::Tuple { fields } => {
            let values = match value {
                Value::Tuple(values) | Value::Array(values) => values,
                _ => return false,
            };
            fields.len() == values.len()
                && fields
                    .iter()
                    .zip(values.iter())
                    .all(|(pattern, value)| match_pattern(pattern, value, bindings))
        }
        PatternKind::EnumUnit { path } => {
            let Some((type_name, case, fields)) = variant_value_parts(value) else {
                return false;
            };
            fields.is_empty() && pattern_variant_matches(pattern, path, type_name, case)
        }
        PatternKind::EnumTuple { path, fields } => {
            let Some((type_name, case, values)) = variant_value_parts(value) else {
                return false;
            };
            pattern_variant_matches(pattern, path, type_name, case)
                && fields.len() == values.len()
                && fields
                    .iter()
                    .zip(values)
                    .all(|(pattern, value)| match_pattern(pattern, value, bindings))
        }
        PatternKind::EnumStruct { path, fields } => {
            let Some((type_name, case, values)) = variant_value_parts(value) else {
                return false;
            };
            if !pattern_variant_matches(pattern, path, type_name, case) {
                return false;
            }
            let [record] = values else {
                return false;
            };
            let record = match record {
                Value::Object(record) => record,
                Value::Struct { fields, .. } => fields,
                _ => return false,
            };
            fields.iter().all(|(name, pattern)| {
                record
                    .get(name)
                    .is_some_and(|value| match_pattern(pattern, value, bindings))
            })
        }
    }
}

fn pattern_variant_matches(
    pattern: &husk_hir::Pattern,
    path: &[String],
    actual_type: &str,
    actual_case: &str,
) -> bool {
    if let Some(variant) = &pattern.variant {
        return variant.type_name == actual_type && variant.case == actual_case;
    }
    let Some(case) = path.last() else {
        return false;
    };
    if case != actual_case {
        return false;
    }
    path.len() < 2 || path[path.len() - 2] == actual_type
}

fn cast_value(value: Value, target: &str) -> anyhow::Result<Value> {
    match (value, target) {
        (Value::Int(value), "i32") => i32::try_from(value)
            .map(i64::from)
            .map(Value::Int)
            .map_err(|_| anyhow::anyhow!("value {value} is outside the i32 range")),
        (Value::Int(value), "i64") => Ok(Value::Int(value)),
        (Value::Float(value), "i32") => {
            checked_float_to_int(value, i64::from(i32::MIN), i64::from(i32::MAX)).map(Value::Int)
        }
        (Value::Float(value), "i64") => {
            checked_float_to_int(value, i64::MIN, i64::MAX).map(Value::Int)
        }
        (Value::Int(value), "f64") => {
            let converted = value as f64;
            if converted < i64::MAX as f64 && converted as i64 == value {
                Ok(Value::Float(converted))
            } else {
                anyhow::bail!("value {value} cannot be represented exactly as f64")
            }
        }
        (Value::Float(value), "f64") if value.is_finite() => Ok(Value::Float(value)),
        (Value::String(value), "String") => Ok(Value::String(value)),
        (Value::Bool(value), "bool") => Ok(Value::Bool(value)),
        (value, "String") => Ok(Value::String(value_to_log_string(&value))),
        (value, target) => {
            anyhow::bail!("cannot cast Husk {} value to `{target}`", value.kind_name())
        }
    }
}

fn checked_float_to_int(value: f64, minimum: i64, maximum: i64) -> anyhow::Result<i64> {
    if !value.is_finite() || value.fract() != 0.0 {
        anyhow::bail!("value {value} is not a finite whole number");
    }
    let above_maximum = if maximum == i64::MAX {
        value >= i64::MAX as f64
    } else {
        value > maximum as f64
    };
    if value < minimum as f64 || above_maximum {
        anyhow::bail!("value {value} is outside the target integer range");
    }
    let converted = value as i64;
    if converted as f64 != value {
        anyhow::bail!("value {value} cannot be represented exactly as an integer");
    }
    Ok(converted)
}

fn format_husk_values(format: &husk_ast::FormatString, values: &[Value]) -> anyhow::Result<String> {
    let mut output = String::new();
    let mut next_argument = 0usize;
    for segment in &format.segments {
        match segment {
            FormatSegment::Literal(literal) => output.push_str(literal),
            FormatSegment::Placeholder(placeholder) => {
                if let Some(name) = &placeholder.name
                    && placeholder.position.is_none()
                {
                    anyhow::bail!(
                        "named format placeholder `{{{name}}}` is not supported without named arguments"
                    );
                }
                let index = placeholder.position.unwrap_or_else(|| {
                    let index = next_argument;
                    next_argument += 1;
                    index
                });
                let value = values.get(index).ok_or_else(|| {
                    anyhow::anyhow!("format placeholder references missing argument {index}")
                })?;
                output.push_str(&format_husk_value(value, &placeholder.spec)?);
            }
        }
    }
    Ok(output)
}

fn format_husk_value(value: &Value, spec: &FormatSpec) -> anyhow::Result<String> {
    let mut rendered = match spec.ty {
        None => value_to_log_string(value),
        Some('?') => format!("{value:?}"),
        Some('x') => format!("{:x}", format_integer(value)?),
        Some('X') => format!("{:X}", format_integer(value)?),
        Some('b') => format!("{:b}", format_integer(value)?),
        Some('o') => format!("{:o}", format_integer(value)?),
        Some(kind) => anyhow::bail!("unsupported Husk format type `{kind}`"),
    };
    if let Some(precision) = spec.precision {
        rendered = match value {
            Value::Float(value) if spec.ty.is_none() => {
                format!("{value:.precision$}")
            }
            _ => rendered.chars().take(precision).collect(),
        };
    }
    if spec.sign && !rendered.starts_with('-') && matches!(value, Value::Int(_) | Value::Float(_)) {
        rendered.insert(0, '+');
    }
    if spec.alternate {
        let prefix = match spec.ty {
            Some('x' | 'X') => "0x",
            Some('b') => "0b",
            Some('o') => "0o",
            _ => "",
        };
        rendered.insert_str(0, prefix);
    }
    let width = spec.width.unwrap_or(0);
    let length = rendered.chars().count();
    if length >= width {
        return Ok(rendered);
    }
    let fill = if spec.zero_pad {
        '0'
    } else {
        spec.fill.unwrap_or(' ')
    };
    let padding = width - length;
    let (left, right) = match spec.align.unwrap_or('>') {
        '<' => (0, padding),
        '^' => (padding / 2, padding - padding / 2),
        '>' => (padding, 0),
        align => anyhow::bail!("unsupported Husk format alignment `{align}`"),
    };
    Ok(format!(
        "{}{rendered}{}",
        fill.to_string().repeat(left),
        fill.to_string().repeat(right)
    ))
}

fn format_integer(value: &Value) -> anyhow::Result<i64> {
    match value {
        Value::Int(value) => Ok(*value),
        value => anyhow::bail!(
            "integer format specifier requires an integer, got {}",
            value.kind_name()
        ),
    }
}

fn call_builtin_method(
    receiver: &Value,
    method: &str,
    args: &[Value],
) -> anyhow::Result<Option<Value>> {
    let result = match (receiver, method) {
        (Value::String(value), "len") => {
            expect_method_arity("String", method, args, 0)?;
            Ok(Value::Int(saturating_i64(value.chars().count())))
        }
        (Value::String(value), "trim") => {
            expect_method_arity("String", method, args, 0)?;
            Ok(Value::String(value.trim().to_string()))
        }
        (Value::String(value), "split") => {
            expect_method_arity("String", method, args, 1)?;
            let separator = method_string_arg("String", method, args, 0)?;
            let parts = if separator.is_empty() {
                value
                    .chars()
                    .map(|character| Value::String(character.to_string()))
                    .collect()
            } else {
                value
                    .split(separator)
                    .map(|part| Value::String(part.to_string()))
                    .collect()
            };
            Ok(Value::Array(Arc::new(parts)))
        }
        (Value::String(value), "char_at") => {
            expect_method_arity("String", method, args, 1)?;
            let index = method_integer_arg("String", method, args, 0)?;
            let character = usize::try_from(index)
                .ok()
                .and_then(|index| value.chars().nth(index))
                .map_or_else(String::new, |character| character.to_string());
            Ok(Value::String(character))
        }
        (Value::String(value), "slice" | "substring") => {
            expect_method_arity("String", method, args, 2)?;
            let start = method_integer_arg("String", method, args, 0)?;
            let end = method_integer_arg("String", method, args, 1)?;
            Ok(Value::String(slice_string(value, start, end)))
        }
        (Value::String(value), "index_of" | "last_index_of") => {
            expect_method_arity("String", method, args, 1)?;
            let needle = method_string_arg("String", method, args, 0)?;
            let byte_index = if method == "index_of" {
                value.find(needle)
            } else {
                value.rfind(needle)
            };
            let index = byte_index.map_or(-1, |byte_index| {
                saturating_i64(value[..byte_index].chars().count())
            });
            Ok(Value::Int(index))
        }
        (Value::String(value), "starts_with" | "ends_with" | "includes") => {
            expect_method_arity("String", method, args, 1)?;
            let needle = method_string_arg("String", method, args, 0)?;
            let found = match method {
                "starts_with" => value.starts_with(needle),
                "ends_with" => value.ends_with(needle),
                "includes" => value.contains(needle),
                _ => unreachable!(),
            };
            Ok(Value::Bool(found))
        }
        (Value::String(value), "to_upper_case") => {
            expect_method_arity("String", method, args, 0)?;
            Ok(Value::String(value.to_uppercase()))
        }
        (Value::String(value), "to_lower_case") => {
            expect_method_arity("String", method, args, 0)?;
            Ok(Value::String(value.to_lowercase()))
        }
        (Value::String(value), "split_once") => {
            expect_method_arity("String", method, args, 1)?;
            let delimiter = method_string_arg("String", method, args, 0)?;
            Ok(match value.split_once(delimiter) {
                Some((before, after)) => enum_variant_value(
                    "Option",
                    "Some",
                    vec![Value::Tuple(Arc::new(vec![
                        Value::String(before.to_string()),
                        Value::String(after.to_string()),
                    ]))],
                ),
                None => enum_variant_value("Option", "None", Vec::new()),
            })
        }
        (Value::String(value), "iter" | "into_iter") => {
            expect_method_arity("String", method, args, 0)?;
            Ok(Value::Array(Arc::new(
                value
                    .chars()
                    .map(|character| Value::String(character.to_string()))
                    .collect(),
            )))
        }
        (Value::Int(_) | Value::Float(_) | Value::Bool(_), "to_string") => {
            expect_method_arity(receiver.kind_name(), method, args, 0)?;
            Ok(Value::String(value_to_log_string(receiver)))
        }
        (Value::Float(value), "floor" | "ceil" | "round" | "abs") => {
            expect_method_arity("f64", method, args, 0)?;
            let value = match method {
                "floor" => value.floor(),
                "ceil" => value.ceil(),
                "round" => value.round(),
                "abs" => value.abs(),
                _ => unreachable!(),
            };
            Ok(Value::Float(value))
        }
        (Value::Array(values) | Value::Tuple(values), "len") => {
            expect_method_arity(receiver.kind_name(), method, args, 0)?;
            Ok(Value::Int(saturating_i64(values.len())))
        }
        (Value::Array(values), "slice") => {
            expect_method_arity("array", method, args, 2)?;
            let len = saturating_i64(values.len());
            let start = normalize_string_index(method_integer_arg("array", method, args, 0)?, len);
            let end = normalize_string_index(method_integer_arg("array", method, args, 1)?, len);
            let start = usize::try_from(start).unwrap_or(0);
            let end = usize::try_from(end.max(i64::try_from(start).unwrap_or(i64::MAX)))
                .unwrap_or(values.len());
            Ok(Value::Array(Arc::new(values[start..end].to_vec())))
        }
        (Value::Array(values), "join") => {
            expect_method_arity("array", method, args, 1)?;
            let separator = method_string_arg("array", method, args, 0)?;
            Ok(Value::String(
                values
                    .iter()
                    .map(value_to_log_string)
                    .collect::<Vec<_>>()
                    .join(separator),
            ))
        }
        (Value::Array(values), "index_of" | "last_index_of") => {
            expect_method_arity("array", method, args, 1)?;
            let needle = &args[0];
            let index = if method == "index_of" {
                values.iter().position(|value| value == needle)
            } else {
                values.iter().rposition(|value| value == needle)
            };
            Ok(Value::Int(index.map_or(-1, saturating_i64)))
        }
        (Value::Array(values), "includes") => {
            expect_method_arity("array", method, args, 1)?;
            Ok(Value::Bool(values.contains(&args[0])))
        }
        (Value::Array(values), "iter" | "into_iter") => {
            expect_method_arity("array", method, args, 0)?;
            Ok(Value::Array(values.clone()))
        }
        (
            Value::Range {
                start,
                end,
                inclusive,
            },
            "contains",
        ) => {
            expect_method_arity("range", method, args, 1)?;
            let needle = method_integer_arg("range", method, args, 0)?;
            Ok(Value::Bool(if *inclusive {
                *start <= needle && needle <= *end
            } else {
                *start <= needle && needle < *end
            }))
        }
        (
            Value::Range {
                start,
                end,
                inclusive,
            },
            "is_empty",
        ) => {
            expect_method_arity("range", method, args, 0)?;
            Ok(Value::Bool(if *inclusive {
                start > end
            } else {
                start >= end
            }))
        }
        (Value::Range { .. }, "iter" | "into_iter") => {
            expect_method_arity("range", method, args, 0)?;
            Ok(receiver.clone())
        }
        (Value::Array(_), "push" | "sort" | "reverse" | "pop" | "shift" | "unshift") => Err(
            anyhow::anyhow!("mutable array method `{method}` requires the instance heap"),
        ),
        (Value::Object(values) | Value::Struct { fields: values, .. }, "len") => {
            expect_method_arity(receiver.kind_name(), method, args, 0)?;
            Ok(Value::Int(saturating_i64(values.len())))
        }
        (Value::Json(serde_json::Value::Array(values)), "len") => {
            expect_method_arity("array", method, args, 0)?;
            Ok(Value::Int(saturating_i64(values.len())))
        }
        (Value::Json(serde_json::Value::Object(values)), "len") => {
            expect_method_arity("object", method, args, 0)?;
            Ok(Value::Int(saturating_i64(values.len())))
        }
        _ => return Ok(None),
    };
    result.map(Some)
}

fn receiver_local_cell(receiver: &Expr, frame: &Frame) -> anyhow::Result<CellHandle> {
    let ExprKind::Ident {
        local: Some(local), ..
    } = &receiver.kind
    else {
        anyhow::bail!("mutable method receiver must be a local binding");
    };
    frame
        .locals
        .get(local)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("mutable method receiver local is unavailable"))
}

fn sort_husk_values(values: &mut [Value]) -> anyhow::Result<()> {
    let Some(first) = values.first() else {
        return Ok(());
    };
    match first {
        Value::Int(_) => {
            if !values.iter().all(|value| matches!(value, Value::Int(_))) {
                anyhow::bail!("Husk array sort requires elements of one sortable type");
            }
            values.sort_by(|left, right| {
                let (Value::Int(left), Value::Int(right)) = (left, right) else {
                    unreachable!("array element types were checked")
                };
                left.cmp(right)
            });
        }
        Value::Float(_) => {
            if !values
                .iter()
                .all(|value| matches!(value, Value::Float(number) if !number.is_nan()))
            {
                anyhow::bail!("Husk array sort requires finite, non-NaN f64 elements");
            }
            values.sort_by(|left, right| {
                let (Value::Float(left), Value::Float(right)) = (left, right) else {
                    unreachable!("array element types were checked")
                };
                left.partial_cmp(right)
                    .expect("NaN values were rejected before sorting")
            });
        }
        Value::String(_) => {
            if !values.iter().all(|value| matches!(value, Value::String(_))) {
                anyhow::bail!("Husk array sort requires elements of one sortable type");
            }
            values.sort_by(|left, right| {
                let (Value::String(left), Value::String(right)) = (left, right) else {
                    unreachable!("array element types were checked")
                };
                left.cmp(right)
            });
        }
        Value::Bool(_) => {
            if !values.iter().all(|value| matches!(value, Value::Bool(_))) {
                anyhow::bail!("Husk array sort requires elements of one sortable type");
            }
            values.sort_by(|left, right| {
                let (Value::Bool(left), Value::Bool(right)) = (left, right) else {
                    unreachable!("array element types were checked")
                };
                left.cmp(right)
            });
        }
        value => {
            anyhow::bail!(
                "Husk array sort does not support {} elements",
                value.kind_name()
            );
        }
    }
    Ok(())
}

fn callback_predicate(value: Value, method: &str) -> anyhow::Result<bool> {
    value
        .as_bool()
        .ok_or_else(|| anyhow::anyhow!("array method `{method}` predicate must return a bool"))
}

fn expect_method_arity(
    receiver: &str,
    method: &str,
    args: &[Value],
    expected: usize,
) -> anyhow::Result<()> {
    if args.len() == expected {
        return Ok(());
    }
    anyhow::bail!(
        "{receiver} method `{method}` expects {expected} argument{}, got {}",
        if expected == 1 { "" } else { "s" },
        args.len()
    )
}

fn method_string_arg<'a>(
    receiver: &str,
    method: &str,
    args: &'a [Value],
    index: usize,
) -> anyhow::Result<&'a str> {
    args.get(index).and_then(Value::as_str).ok_or_else(|| {
        anyhow::anyhow!("{receiver} method `{method}` argument {index} must be a string")
    })
}

fn method_integer_arg(
    receiver: &str,
    method: &str,
    args: &[Value],
    index: usize,
) -> anyhow::Result<i64> {
    args.get(index).and_then(value_to_i64).ok_or_else(|| {
        anyhow::anyhow!("{receiver} method `{method}` argument {index} must be an integer")
    })
}

fn saturating_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn slice_string(value: &str, start: i64, end: i64) -> String {
    let len = saturating_i64(value.chars().count());
    let start = normalize_string_index(start, len);
    let end = normalize_string_index(end, len);
    let count = end.saturating_sub(start);
    value
        .chars()
        .skip(usize::try_from(start).unwrap_or(0))
        .take(usize::try_from(count).unwrap_or(0))
        .collect()
}

fn value_to_log_string(value: &Value) -> String {
    match value {
        Value::Unit => "()".to_string(),
        Value::Null | Value::Missing(_) => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Int(value) => value.to_string(),
        Value::Float(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(value) | Value::Tuple(value) => {
            serde_json::Value::Array(value.iter().map(value_to_json).collect()).to_string()
        }
        Value::Range {
            start,
            end,
            inclusive,
        } => {
            if *inclusive {
                format!("{start}..={end}")
            } else {
                format!("{start}..{end}")
            }
        }
        Value::Object(value) => serde_json::Value::Object(
            value
                .iter()
                .map(|(key, value)| (key.clone(), value_to_json(value)))
                .collect(),
        )
        .to_string(),
        Value::Struct { type_name, fields } => format!(
            "{type_name} {}",
            serde_json::Value::Object(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), value_to_json(value)))
                    .collect(),
            )
        ),
        Value::Variant {
            type_name,
            case,
            fields,
        } => {
            let payload = fields
                .iter()
                .map(value_to_log_string)
                .collect::<Vec<_>>()
                .join(", ");
            if fields.is_empty() {
                format!("{type_name}::{case}")
            } else {
                format!("{type_name}::{case}({payload})")
            }
        }
        Value::Json(value) => value.to_string(),
        Value::Callback(callback) => format!("{}::{}", callback.plugin, callback.function),
        Value::Closure(_) => "<closure>".to_string(),
    }
}

fn with_call_frame(error: anyhow::Error, callback: &Callback) -> anyhow::Error {
    match error.downcast::<Report>() {
        Ok(report) => anyhow::Error::new(report.with_frame(CallFrame {
            function: callback.function.clone(),
            plugin: callback.plugin.clone(),
        })),
        Err(error) => error,
    }
}

fn closest_field<'a>(field: &str, available: &'a [String]) -> Option<&'a str> {
    let normalized = normalize_field_name(field);
    available
        .iter()
        .map(|candidate| {
            let distance = edit_distance(&normalized, &normalize_field_name(candidate));
            (candidate.as_str(), distance)
        })
        .min_by_key(|(_, distance)| *distance)
        .filter(|(_, distance)| *distance <= 3)
        .map(|(candidate, _)| candidate)
}

fn normalize_field_name(field: &str) -> String {
    field
        .chars()
        .filter(|character| *character != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

fn edit_distance(left: &str, right: &str) -> usize {
    let mut previous = (0..=right.chars().count()).collect::<Vec<_>>();
    for (left_index, left_character) in left.chars().enumerate() {
        let mut current = vec![left_index + 1];
        for (right_index, right_character) in right.chars().enumerate() {
            let insertion = current[right_index] + 1;
            let deletion = previous[right_index + 1] + 1;
            let substitution =
                previous[right_index] + usize::from(left_character != right_character);
            current.push(insertion.min(deletion).min(substitution));
        }
        previous = current;
    }
    previous.last().copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestHost;

    impl Host for TestHost {
        fn log(&mut self, _message: &str) {}
    }

    #[test]
    fn integer_division_truncates_toward_zero() {
        let source = r#"
            pub fn result() {
                return [39 / 4, -39 / 4, 39.0 / 4.0];
            }
        "#;
        let mut host = TestHost::default();
        let mut vm = Vm::new();

        vm.load_plugin("division", source, &mut host).unwrap();
        assert_eq!(
            vm.call_export("division", "result", Vec::new(), &mut host)
                .unwrap(),
            Value::Array(Arc::new(vec![
                Value::Int(9),
                Value::Int(-9),
                Value::Float(9.75),
            ]))
        );
    }

    #[test]
    fn integer_division_by_zero_returns_runtime_error() {
        let source = r#"
            pub fn divide() {
                return 1 / 0;
            }
        "#;
        let mut host = TestHost::default();
        let mut vm = Vm::new();

        vm.load_plugin("division", source, &mut host).unwrap();
        let error = vm
            .call_export("division", "divide", Vec::new(), &mut host)
            .unwrap_err();
        let rendered = error.to_string();

        assert!(rendered.contains("HUSK-R0001"));
        assert!(rendered.contains("integer division by zero"));
    }

    #[test]
    fn integer_division_overflow_returns_error() {
        let error = divide_values(Value::Int(i64::MIN), Value::Int(-1)).unwrap_err();

        assert_eq!(error.to_string(), "integer division overflow");
    }

    #[test]
    fn reload_marks_replacement_and_teardown_effect_boundaries() {
        #[derive(Default)]
        struct ReloadHost {
            effects: Vec<String>,
        }

        impl Host for ReloadHost {
            fn log(&mut self, _message: &str) {}

            fn call_module(
                &mut self,
                _plugin: &str,
                path: &str,
                args: &[Value],
            ) -> Option<anyhow::Result<Value>> {
                (path == "test::effect").then(|| {
                    let effect = args
                        .first()
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow::anyhow!("test::effect expects a string"))?;
                    self.effects.push(effect.to_string());
                    Ok(Value::Unit)
                })
            }

            fn begin_reload_replacement(&mut self, _program: &str) {
                self.effects.push("replacement-boundary".to_string());
            }

            fn begin_reload_teardown(&mut self, _program: &str) {
                self.effects.push("teardown-boundary".to_string());
            }
        }

        let mut host = ReloadHost::default();
        let mut vm = Vm::new();
        vm.load_plugin(
            "stateful",
            r#"
                pub fn activate() {}
                fn state_export() -> Json { return "preserved"; }
                fn deactivate() { test::effect("OldTeardown"); }
            "#,
            &mut host,
        )
        .unwrap();
        host.effects.clear();

        vm.reload_plugin_at(
            "stateful",
            "plugins/stateful.hk",
            r#"
                pub fn activate() { test::effect("NewActivation"); }
                fn state_import(saved: Json) { test::effect("NewImport"); }
            "#,
            &mut host,
        )
        .unwrap();

        assert_eq!(
            host.effects,
            [
                "replacement-boundary",
                "NewActivation",
                "NewImport",
                "teardown-boundary",
                "OldTeardown"
            ]
        );
    }

    #[test]
    fn callback_from_replaced_program_generation_is_rejected() {
        #[derive(Default)]
        struct CallbackHost {
            callback: Option<Callback>,
        }

        impl Host for CallbackHost {
            fn log(&mut self, _message: &str) {}

            fn call_module(
                &mut self,
                _plugin: &str,
                path: &str,
                args: &[Value],
            ) -> Option<anyhow::Result<Value>> {
                (path == "test::capture").then(|| {
                    let Value::Callback(callback) = args.first().cloned().ok_or_else(|| {
                        anyhow::anyhow!("test::capture expects one callback argument")
                    })?
                    else {
                        anyhow::bail!("test::capture expects a callback");
                    };
                    self.callback = Some(callback);
                    Ok(Value::Unit)
                })
            }
        }

        let mut host = CallbackHost::default();
        let mut vm = Vm::new();
        vm.load_plugin(
            "callbacks",
            r#"
                fn answer() { 1 }
                pub fn expose() { test::capture(answer); }
            "#,
            &mut host,
        )
        .unwrap();
        vm.call_export("callbacks", "expose", Vec::new(), &mut host)
            .unwrap();
        let stale = host.callback.take().unwrap();

        vm.reload_plugin_at(
            "callbacks",
            "plugins/callbacks.hk",
            r#"
                fn answer() { 2 }
                pub fn expose() { test::capture(answer); }
            "#,
            &mut host,
        )
        .unwrap();

        let error = vm
            .call_callback(&stale, Vec::new(), &mut host)
            .unwrap_err()
            .to_string();
        assert!(error.contains("stale Husk callback"), "{error}");

        vm.call_export("callbacks", "expose", Vec::new(), &mut host)
            .unwrap();
        let current = host.callback.take().unwrap();
        assert_eq!(
            vm.call_callback(&current, Vec::new(), &mut host).unwrap(),
            Value::Int(2)
        );
    }

    #[test]
    fn recursive_calls_share_instruction_budget() {
        let source = r#"
            pub fn recurse() {
                recurse();
            }
        "#;
        let mut host = TestHost::default();
        let mut vm = Vm::new();
        vm.set_instruction_budget(32);

        vm.load_plugin("test", source, &mut host).unwrap();
        let error = vm
            .call_export("test", "recurse", Vec::new(), &mut host)
            .unwrap_err();

        let rendered = error.to_string();
        assert!(
            rendered.contains("Husk instruction budget exhausted")
                || rendered.contains("Husk call depth exceeded")
        );
    }

    #[test]
    fn renders_parser_errors_with_source_excerpts() {
        let error = Program::parse("broken", "fn activate( {").unwrap_err();
        let rendered = error.to_string();

        assert!(rendered.contains("error[HUSK-P0001]:"));
        assert!(rendered.contains("--> plugins/broken.hk:1:"));
        assert!(rendered.contains("fn activate( {"));
        assert!(rendered.contains("note: while parsing plugin `broken`"));
    }

    #[test]
    fn reports_the_first_missing_field_in_a_chain() {
        let source = r#"
            pub fn inspect(event: Json) {
                header_style(event);
            }

            fn header_style(event: Json) {
                let style = event.theme.uiStyle.popupTitle;
            }
        "#;
        let mut host = TestHost::default();
        let mut vm = Vm::new();
        vm.load_plugin("fidget", source, &mut host).unwrap();

        let error = vm
            .call_export(
                "fidget",
                "inspect",
                vec![Value::from_json(serde_json::json!({
                    "theme": {
                        "ui_style": {
                            "popup_title": {}
                        }
                    }
                }))],
                &mut host,
            )
            .unwrap_err();
        let rendered = error.to_string();

        assert!(rendered.contains("error[HUSK-R0004]: unknown field `uiStyle`"));
        assert!(rendered.contains("plugins/fidget.hk:"));
        assert!(rendered.contains("uiStyle"));
        assert!(rendered.contains("help: a similarly named field exists: `ui_style`"));
        assert!(rendered.contains("while calling `header_style` in plugin `fidget`"));
        assert!(rendered.contains("while calling `inspect` in plugin `fidget`"));
    }

    #[test]
    fn heap_collects_unreachable_recursive_closure_cycles() {
        let mut host = TestHost::default();
        let mut vm = Vm::new();
        vm.set_heap_limits(2, 1024 * 1024);
        vm.load_plugin(
            "cycles",
            r#"
                fn create_cycle() {
                    let cycle = || cycle;
                }
            "#,
            &mut host,
        )
        .unwrap();

        for _ in 0..32 {
            vm.call_export("cycles", "create_cycle", Vec::new(), &mut host)
                .unwrap();
            // The finished frame releases its unreachable local cell
            // immediately. The orphaned closure is swept before the next
            // top-level call.
            assert_eq!(vm.heap.live_objects, 1);
        }
    }

    #[test]
    fn stale_closure_handle_is_rejected_after_collection_and_slot_reuse() {
        let mut host = TestHost::default();
        let mut vm = Vm::new();
        vm.set_instance_generation(7);
        vm.load_plugin(
            "handles",
            r#"
                fn make() { || 42 }
                fn noop() {}
                fn invoke(callback: fn() -> i32) { callback() }
            "#,
            &mut host,
        )
        .unwrap();

        let closure = vm
            .call_export("handles", "make", Vec::new(), &mut host)
            .unwrap();
        assert!(matches!(closure, Value::Closure(_)));
        vm.call_export("handles", "noop", Vec::new(), &mut host)
            .unwrap();
        let error = vm
            .call_export("handles", "invoke", vec![closure], &mut host)
            .unwrap_err()
            .to_string();

        assert!(error.contains("stale Husk heap handle"), "{error}");
    }
}
