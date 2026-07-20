use std::{
    cell::Cell,
    collections::{BTreeMap, HashMap},
    fmt,
    marker::PhantomData,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::{
    CompileOptions, CompiledProgram, Frame, FunctionDescriptor, FunctionHandle, Host,
    ModuleDescriptor, ParameterDescriptor, ResolvedPackage, SemanticProfile, TypeDescriptor, Value,
    Version, Vm,
};
use husk_parser::{ReplFragment, ReplParseResult};
pub use husk_value::OwnedValue;
#[cfg(feature = "wasm-extensions")]
use husk_wasm::{WasmComponent, WasmInstance, WasmLimits};

/// Failed conversion at a typed native boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionError {
    pub argument_index: Option<usize>,
    pub expected: TypeDescriptor,
    pub actual: String,
    pub module: Option<String>,
    pub function: Option<String>,
}

impl ConversionError {
    fn new(expected: TypeDescriptor, actual: &OwnedValue) -> Self {
        Self {
            argument_index: None,
            expected,
            actual: actual.kind_name().to_string(),
            module: None,
            function: None,
        }
    }

    fn at_argument(mut self, index: usize, module: &str, function: &str) -> Self {
        self.argument_index = Some(index);
        self.module = Some(module.to_string());
        self.function = Some(function.to_string());
        self
    }
}

impl fmt::Display for ConversionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let (Some(module), Some(function), Some(index)) =
            (&self.module, &self.function, self.argument_index)
        {
            write!(
                formatter,
                "native call `{module}::{function}` argument {index} expected {:?}, got {}",
                self.expected, self.actual
            )
        } else {
            write!(
                formatter,
                "expected Husk value {:?}, got {}",
                self.expected, self.actual
            )
        }
    }
}

impl std::error::Error for ConversionError {}

/// A host-side failure, distinct from a script-level `Result` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeError {
    message: String,
}

impl NativeError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for NativeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for NativeError {}

impl From<ConversionError> for NativeError {
    fn from(error: ConversionError) -> Self {
        Self::new(error.to_string())
    }
}

/// Script-level success or error returned as a Husk `Result`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptResult<T, E>(pub Result<T, E>);

impl<T, E> From<Result<T, E>> for ScriptResult<T, E> {
    fn from(result: Result<T, E>) -> Self {
        Self(result)
    }
}

/// Static Husk signature for a Rust adapter type.
pub trait HuskType {
    fn husk_type() -> TypeDescriptor;
}

/// Convert one borrowed, detached Husk argument into a Rust value.
pub trait FromHusk<'value>: HuskType + Sized {
    fn from_husk(value: &'value OwnedValue) -> Result<Self, ConversionError>;
}

/// Convert a Rust return value into detached Husk data.
pub trait IntoHusk: HuskType {
    fn into_husk(self) -> Result<OwnedValue, ConversionError>;
}

macro_rules! exact_value_conversion {
    ($rust:ty, $descriptor:expr, $variant:ident) => {
        impl HuskType for $rust {
            fn husk_type() -> TypeDescriptor {
                $descriptor
            }
        }

        impl<'value> FromHusk<'value> for $rust {
            fn from_husk(value: &'value OwnedValue) -> Result<Self, ConversionError> {
                match value {
                    OwnedValue::$variant(value) => Ok(value.clone()),
                    value => Err(ConversionError::new(Self::husk_type(), value)),
                }
            }
        }

        impl IntoHusk for $rust {
            fn into_husk(self) -> Result<OwnedValue, ConversionError> {
                Ok(OwnedValue::$variant(self))
            }
        }
    };
}

impl HuskType for () {
    fn husk_type() -> TypeDescriptor {
        TypeDescriptor::Unit
    }
}

impl<'value> FromHusk<'value> for () {
    fn from_husk(value: &'value OwnedValue) -> Result<Self, ConversionError> {
        match value {
            OwnedValue::Unit => Ok(()),
            value => Err(ConversionError::new(Self::husk_type(), value)),
        }
    }
}

impl IntoHusk for () {
    fn into_husk(self) -> Result<OwnedValue, ConversionError> {
        Ok(OwnedValue::Unit)
    }
}

exact_value_conversion!(bool, TypeDescriptor::Bool, Bool);
exact_value_conversion!(f64, TypeDescriptor::F64, F64);
exact_value_conversion!(String, TypeDescriptor::String, String);

impl HuskType for i32 {
    fn husk_type() -> TypeDescriptor {
        TypeDescriptor::I32
    }
}

impl<'value> FromHusk<'value> for i32 {
    fn from_husk(value: &'value OwnedValue) -> Result<Self, ConversionError> {
        match value {
            OwnedValue::I32(value) => Ok(*value),
            value @ OwnedValue::I64(integer) => {
                i32::try_from(*integer).map_err(|_| ConversionError::new(Self::husk_type(), value))
            }
            value => Err(ConversionError::new(Self::husk_type(), value)),
        }
    }
}

impl IntoHusk for i32 {
    fn into_husk(self) -> Result<OwnedValue, ConversionError> {
        Ok(OwnedValue::I32(self))
    }
}

impl HuskType for i64 {
    fn husk_type() -> TypeDescriptor {
        TypeDescriptor::I64
    }
}

impl<'value> FromHusk<'value> for i64 {
    fn from_husk(value: &'value OwnedValue) -> Result<Self, ConversionError> {
        match value {
            OwnedValue::I32(value) => Ok(i64::from(*value)),
            OwnedValue::I64(value) => Ok(*value),
            value => Err(ConversionError::new(Self::husk_type(), value)),
        }
    }
}

impl IntoHusk for i64 {
    fn into_husk(self) -> Result<OwnedValue, ConversionError> {
        Ok(OwnedValue::I64(self))
    }
}

impl HuskType for &str {
    fn husk_type() -> TypeDescriptor {
        TypeDescriptor::String
    }
}

impl<'value> FromHusk<'value> for &'value str {
    fn from_husk(value: &'value OwnedValue) -> Result<Self, ConversionError> {
        match value {
            OwnedValue::String(value) => Ok(value),
            value => Err(ConversionError::new(Self::husk_type(), value)),
        }
    }
}

impl IntoHusk for &str {
    fn into_husk(self) -> Result<OwnedValue, ConversionError> {
        Ok(OwnedValue::String(self.to_string()))
    }
}

impl<T> HuskType for Vec<T>
where
    T: HuskType,
{
    fn husk_type() -> TypeDescriptor {
        TypeDescriptor::list(T::husk_type())
    }
}

impl<'value, T> FromHusk<'value> for Vec<T>
where
    T: FromHusk<'value>,
{
    fn from_husk(value: &'value OwnedValue) -> Result<Self, ConversionError> {
        let values = match value {
            OwnedValue::List(values) | OwnedValue::Tuple(values) => values,
            value => return Err(ConversionError::new(Self::husk_type(), value)),
        };
        values.iter().map(T::from_husk).collect()
    }
}

impl<T> IntoHusk for Vec<T>
where
    T: IntoHusk,
{
    fn into_husk(self) -> Result<OwnedValue, ConversionError> {
        self.into_iter()
            .map(IntoHusk::into_husk)
            .collect::<Result<Vec<_>, _>>()
            .map(OwnedValue::List)
    }
}

impl<T> HuskType for Option<T>
where
    T: HuskType,
{
    fn husk_type() -> TypeDescriptor {
        TypeDescriptor::option(T::husk_type())
    }
}

impl<'value, T> FromHusk<'value> for Option<T>
where
    T: FromHusk<'value>,
{
    fn from_husk(value: &'value OwnedValue) -> Result<Self, ConversionError> {
        match value {
            OwnedValue::Null => Ok(None),
            OwnedValue::Variant { case, fields, .. } if case == "None" && fields.is_empty() => {
                Ok(None)
            }
            OwnedValue::Variant { case, fields, .. } if case == "Some" && fields.len() == 1 => {
                T::from_husk(&fields[0]).map(Some)
            }
            value => T::from_husk(value).map(Some),
        }
    }
}

impl<T> IntoHusk for Option<T>
where
    T: IntoHusk,
{
    fn into_husk(self) -> Result<OwnedValue, ConversionError> {
        match self {
            Some(value) => Ok(OwnedValue::Variant {
                type_name: "Option".to_string(),
                case: "Some".to_string(),
                fields: vec![value.into_husk()?],
            }),
            None => Ok(OwnedValue::Variant {
                type_name: "Option".to_string(),
                case: "None".to_string(),
                fields: Vec::new(),
            }),
        }
    }
}

impl<A, B> HuskType for (A, B)
where
    A: HuskType,
    B: HuskType,
{
    fn husk_type() -> TypeDescriptor {
        TypeDescriptor::Tuple(vec![A::husk_type(), B::husk_type()])
    }
}

impl<'value, A, B> FromHusk<'value> for (A, B)
where
    A: FromHusk<'value>,
    B: FromHusk<'value>,
{
    fn from_husk(value: &'value OwnedValue) -> Result<Self, ConversionError> {
        let values = match value {
            OwnedValue::Tuple(values) | OwnedValue::List(values) if values.len() == 2 => values,
            value => return Err(ConversionError::new(Self::husk_type(), value)),
        };
        Ok((A::from_husk(&values[0])?, B::from_husk(&values[1])?))
    }
}

impl<A, B> IntoHusk for (A, B)
where
    A: IntoHusk,
    B: IntoHusk,
{
    fn into_husk(self) -> Result<OwnedValue, ConversionError> {
        Ok(OwnedValue::Tuple(vec![
            self.0.into_husk()?,
            self.1.into_husk()?,
        ]))
    }
}

impl<T, E> HuskType for ScriptResult<T, E>
where
    T: HuskType,
    E: HuskType,
{
    fn husk_type() -> TypeDescriptor {
        TypeDescriptor::result(T::husk_type(), E::husk_type())
    }
}

impl<T, E> IntoHusk for ScriptResult<T, E>
where
    T: IntoHusk,
    E: IntoHusk,
{
    fn into_husk(self) -> Result<OwnedValue, ConversionError> {
        match self.0 {
            Ok(value) => Ok(OwnedValue::Variant {
                type_name: "Result".to_string(),
                case: "Ok".to_string(),
                fields: vec![value.into_husk()?],
            }),
            Err(error) => Ok(OwnedValue::Variant {
                type_name: "Result".to_string(),
                case: "Err".to_string(),
                fields: vec![error.into_husk()?],
            }),
        }
    }
}

/// Mutable context supplied to one native function call.
pub struct CallContext<'call, T> {
    data: &'call mut T,
    module: &'call str,
    function: &'call str,
}

impl<T> CallContext<'_, T> {
    #[must_use]
    pub fn data(&self) -> &T {
        self.data
    }

    #[must_use]
    pub fn data_mut(&mut self) -> &mut T {
        self.data
    }

    #[must_use]
    pub fn module(&self) -> &str {
        self.module
    }

    #[must_use]
    pub fn function(&self) -> &str {
        self.function
    }
}

type NativeHandler<T> = dyn for<'call> Fn(&mut CallContext<'call, T>, &[OwnedValue]) -> Result<OwnedValue, NativeError>
    + Send
    + Sync;

/// Internal adapter implemented for supported typed closure arities.
#[doc(hidden)]
pub trait TypedNativeFunction<T, Arguments>: Send + Sync + 'static {
    type Output: IntoHusk;

    fn parameter_types() -> Vec<TypeDescriptor>;

    fn call(
        &self,
        context: &mut CallContext<'_, T>,
        arguments: &[OwnedValue],
    ) -> Result<OwnedValue, NativeError>;
}

impl<T, Function, Output> TypedNativeFunction<T, ()> for Function
where
    Function: for<'call> Fn(&mut CallContext<'call, T>) -> Result<Output, NativeError>
        + Send
        + Sync
        + 'static,
    Output: IntoHusk,
{
    type Output = Output;

    fn parameter_types() -> Vec<TypeDescriptor> {
        Vec::new()
    }

    fn call(
        &self,
        context: &mut CallContext<'_, T>,
        arguments: &[OwnedValue],
    ) -> Result<OwnedValue, NativeError> {
        if !arguments.is_empty() {
            return Err(NativeError::new(format!(
                "expected 0 arguments, got {}",
                arguments.len()
            )));
        }
        self(context)?.into_husk().map_err(Into::into)
    }
}

impl<T, Function, A, Output> TypedNativeFunction<T, (A,)> for Function
where
    Function: for<'call> Fn(&mut CallContext<'call, T>, A) -> Result<Output, NativeError>
        + Send
        + Sync
        + 'static,
    A: for<'value> FromHusk<'value> + 'static,
    Output: IntoHusk,
{
    type Output = Output;

    fn parameter_types() -> Vec<TypeDescriptor> {
        vec![A::husk_type()]
    }

    fn call(
        &self,
        context: &mut CallContext<'_, T>,
        arguments: &[OwnedValue],
    ) -> Result<OwnedValue, NativeError> {
        if arguments.len() != 1 {
            return Err(NativeError::new(format!(
                "expected 1 argument, got {}",
                arguments.len()
            )));
        }
        let value = A::from_husk(&arguments[0]).map_err(|error| {
            NativeError::from(error.at_argument(0, context.module, context.function))
        })?;
        self(context, value)?.into_husk().map_err(Into::into)
    }
}

impl<T, Function, A, B, Output> TypedNativeFunction<T, (A, B)> for Function
where
    Function: for<'call> Fn(&mut CallContext<'call, T>, A, B) -> Result<Output, NativeError>
        + Send
        + Sync
        + 'static,
    A: for<'value> FromHusk<'value> + 'static,
    B: for<'value> FromHusk<'value> + 'static,
    Output: IntoHusk,
{
    type Output = Output;

    fn parameter_types() -> Vec<TypeDescriptor> {
        vec![A::husk_type(), B::husk_type()]
    }

    fn call(
        &self,
        context: &mut CallContext<'_, T>,
        arguments: &[OwnedValue],
    ) -> Result<OwnedValue, NativeError> {
        if arguments.len() != 2 {
            return Err(NativeError::new(format!(
                "expected 2 arguments, got {}",
                arguments.len()
            )));
        }
        let first = A::from_husk(&arguments[0]).map_err(|error| {
            NativeError::from(error.at_argument(0, context.module, context.function))
        })?;
        let second = B::from_husk(&arguments[1]).map_err(|error| {
            NativeError::from(error.at_argument(1, context.module, context.function))
        })?;
        self(context, first, second)?
            .into_husk()
            .map_err(Into::into)
    }
}

/// A statically linked Rust module registered with an [`Engine`].
pub struct NativeModule<T> {
    descriptor: Arc<ModuleDescriptor>,
    handlers: Arc<HashMap<String, Arc<NativeHandler<T>>>>,
}

impl<T> Clone for NativeModule<T> {
    fn clone(&self) -> Self {
        Self {
            descriptor: Arc::clone(&self.descriptor),
            handlers: Arc::clone(&self.handlers),
        }
    }
}

impl<T> NativeModule<T> {
    #[must_use]
    pub fn builder(name: impl Into<String>) -> NativeModuleBuilder<T> {
        NativeModuleBuilder {
            name: name.into(),
            version: Version::new(0, 1, 0),
            functions: Vec::new(),
            handlers: HashMap::new(),
            error: None,
        }
    }

    #[must_use]
    pub fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }

    fn call(
        &self,
        path: &str,
        data: &mut T,
        arguments: &[OwnedValue],
    ) -> Result<OwnedValue, NativeError> {
        let handler = self.handlers.get(path).ok_or_else(|| {
            NativeError::new(format!(
                "module `{}` has no function `{path}`",
                self.descriptor.name
            ))
        })?;
        let mut context = CallContext {
            data,
            module: self.descriptor.name.as_str(),
            function: path,
        };
        handler(&mut context, arguments)
    }
}

/// Builder that derives descriptors and conversion logic from Rust types.
pub struct NativeModuleBuilder<T> {
    name: String,
    version: Version,
    functions: Vec<FunctionDescriptor>,
    handlers: HashMap<String, Arc<NativeHandler<T>>>,
    error: Option<NativeError>,
}

impl<T: 'static> NativeModuleBuilder<T> {
    #[must_use]
    pub fn version(mut self, version: Version) -> Self {
        self.version = version;
        self
    }

    #[must_use]
    pub fn typed_function<Arguments, Function>(
        mut self,
        name: impl Into<String>,
        function: Function,
    ) -> Self
    where
        Function: TypedNativeFunction<T, Arguments>,
    {
        let name = name.into();
        if self.error.is_some() {
            return self;
        }
        let parameters = Function::parameter_types()
            .into_iter()
            .enumerate()
            .map(|(index, ty)| ParameterDescriptor::new(format!("arg{index}"), ty))
            .collect::<Result<Vec<_>, _>>();
        let descriptor = parameters.and_then(|parameters| {
            FunctionDescriptor::new(name.clone(), parameters, Function::Output::husk_type())
        });
        let descriptor = match descriptor {
            Ok(descriptor) => descriptor,
            Err(error) => {
                self.error = Some(NativeError::new(error.to_string()));
                return self;
            }
        };
        if self.handlers.contains_key(&name) {
            self.error = Some(NativeError::new(format!(
                "duplicate native function `{name}`"
            )));
            return self;
        }
        self.functions.push(descriptor);
        self.handlers.insert(
            name,
            Arc::new(move |context, arguments| function.call(context, arguments)),
        );
        self
    }

    /// Register a low-level function with an explicit descriptor.
    #[must_use]
    pub fn function(
        mut self,
        descriptor: FunctionDescriptor,
        function: impl for<'call> Fn(
            &mut CallContext<'call, T>,
            &[OwnedValue],
        ) -> Result<OwnedValue, NativeError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        if self.handlers.contains_key(&descriptor.name) {
            self.error = Some(NativeError::new(format!(
                "duplicate native function `{}`",
                descriptor.name
            )));
            return self;
        }
        self.handlers
            .insert(descriptor.name.clone(), Arc::new(function));
        self.functions.push(descriptor);
        self
    }

    /// Validate and finish the module.
    pub fn build(self) -> anyhow::Result<NativeModule<T>> {
        if let Some(error) = self.error {
            return Err(error.into());
        }
        let descriptor =
            ModuleDescriptor::new(self.name, self.version, self.functions, Vec::new())?;
        Ok(NativeModule {
            descriptor: Arc::new(descriptor),
            handlers: Arc::new(self.handlers),
        })
    }
}

/// Resource limits shared by embedded and standalone execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub instructions_per_call: usize,
    pub max_call_depth: usize,
    pub max_heap_bytes: usize,
    pub max_heap_objects: usize,
    pub max_value_bytes: usize,
    pub max_source_bytes: usize,
    pub max_modules: usize,
    pub native_calls_per_call: usize,
    pub max_callback_roots: usize,
    pub max_extension_instances: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            instructions_per_call: 100_000,
            max_call_depth: 512,
            max_heap_bytes: 64 * 1024 * 1024,
            max_heap_objects: 1_000_000,
            max_value_bytes: 16 * 1024 * 1024,
            max_source_bytes: 1024 * 1024,
            max_modules: 256,
            native_calls_per_call: 10_000,
            max_callback_roots: 10_000,
            max_extension_instances: 64,
        }
    }
}

/// Result of submitting one fragment to an interactive Husk session.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplOutcome {
    /// Whitespace or comments contained nothing to compile or execute.
    Empty,
    /// The fragment is syntactically valid so far and needs another line.
    Incomplete,
    /// A top-level item was compiled and made available to later submissions.
    Defined,
    /// A statement was executed. Unit values are returned explicitly so an
    /// embedder can decide whether to display them.
    Value(OwnedValue),
}

const REPL_PROGRAM_NAME: &str = "__husk_repl";
const REPL_FUNCTION_NAME: &str = "__husk_repl_session";
const REPL_DISPLAY_PATH: &str = "<repl>";

struct EngineInner<T> {
    modules: BTreeMap<String, NativeModule<T>>,
    #[cfg(feature = "wasm-extensions")]
    wasm_components: BTreeMap<String, WasmComponent>,
    compile_options: CompileOptions,
    limits: Limits,
}

/// Immutable, shareable Husk compiler and module registry.
pub struct Engine<T> {
    inner: Arc<EngineInner<T>>,
}

impl<T> Clone for Engine<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T: 'static> Engine<T> {
    #[must_use]
    pub fn builder() -> EngineBuilder<T> {
        EngineBuilder {
            modules: BTreeMap::new(),
            #[cfg(feature = "wasm-extensions")]
            wasm_components: BTreeMap::new(),
            compile_options: CompileOptions::default(),
            limits: Limits::default(),
        }
    }

    /// Compile an in-memory source with a stable display path.
    pub fn compile_source(
        &self,
        name: impl Into<String>,
        path: impl Into<String>,
        source: &str,
    ) -> anyhow::Result<CompiledModule> {
        let name = name.into();
        let options = self.compile_options();
        let program = CompiledProgram::compile_at(name, path, source, &options)?;
        Ok(CompiledModule {
            program: Arc::new(program),
        })
    }

    /// Read and compile one UTF-8 Husk source file.
    pub fn compile_path(&self, path: impl AsRef<Path>) -> anyhow::Result<CompiledModule> {
        let path = path.as_ref();
        let bytes = std::fs::read(path)?;
        if bytes.len() > self.inner.limits.max_source_bytes {
            anyhow::bail!(
                "Husk source is {} bytes; the configured maximum is {}",
                bytes.len(),
                self.inner.limits.max_source_bytes
            );
        }
        let source = String::from_utf8(bytes)
            .map_err(|error| anyhow::anyhow!("Husk source must be UTF-8: {error}"))?;
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("script");
        self.compile_source(name, path.to_string_lossy(), &source)
    }

    /// Compile all source modules in a deterministically resolved package.
    pub fn compile_package(&self, package: &ResolvedPackage) -> anyhow::Result<CompiledModule> {
        if package.modules.len() > self.inner.limits.max_modules {
            anyhow::bail!(
                "package has {} source modules; the configured maximum is {}",
                package.modules.len(),
                self.inner.limits.max_modules
            );
        }
        let options = self.compile_options();
        let program = CompiledProgram::compile_package(package, &options)?;
        Ok(CompiledModule {
            program: Arc::new(program),
        })
    }

    /// Start an interactive session that preserves locals and definitions.
    ///
    /// Each complete fragment is compiled against the full session source
    /// before it executes. A parser or semantic failure does not change the
    /// committed session. Script heap changes are also transactional on a
    /// runtime failure; mutations already performed by native or Wasm modules
    /// remain host-owned and cannot be rolled back.
    pub fn repl(&self, state: T) -> anyhow::Result<ReplSession<T>> {
        let source = render_repl_source("", "");
        let compiled =
            self.compile_source(REPL_PROGRAM_NAME, REPL_DISPLAY_PATH, source.as_str())?;
        let instance = self.instantiate(compiled, state)?;
        let function = instance
            .compiled
            .program
            .functions
            .get_by_name(REPL_FUNCTION_NAME)
            .ok_or_else(|| anyhow::anyhow!("compiled REPL is missing its session function"))?;
        let frame = Frame {
            plugin: instance.program_name.clone(),
            locals: HashMap::new(),
            owned_cells: Vec::new(),
            module_path: function.module_path.clone(),
            imports: function.imports.clone(),
            remaining: self.inner.limits.instructions_per_call,
            remaining_host_calls: self.inner.limits.native_calls_per_call,
            call_depth: 0,
        };
        Ok(ReplSession {
            instance,
            item_source: String::new(),
            statement_source: String::new(),
            executed_statements: 0,
            frame,
        })
    }

    fn compile_options(&self) -> CompileOptions {
        let mut options = self.inner.compile_options.clone();
        options.limits.max_source_bytes = self.inner.limits.max_source_bytes;
        options.modules = self
            .inner
            .modules
            .values()
            .map(|module| module.descriptor().clone())
            .collect::<Vec<_>>();
        #[cfg(feature = "wasm-extensions")]
        options.modules.extend(
            self.inner
                .wasm_components
                .values()
                .map(|component| component.descriptor().clone()),
        );
        options
    }

    /// Create isolated mutable state from a reusable compiled module.
    pub fn instantiate(&self, compiled: CompiledModule, state: T) -> anyhow::Result<Instance<T>> {
        for descriptor in compiled.program.modules() {
            if let Some(module) = self.inner.modules.get(descriptor.name.as_str()) {
                if module.descriptor().stable_hash() != descriptor.stable_hash() {
                    anyhow::bail!(
                        "native module descriptor changed after compilation: `{}`",
                        descriptor.name
                    );
                }
                continue;
            }
            #[cfg(feature = "wasm-extensions")]
            if let Some(component) = self.inner.wasm_components.get(descriptor.name.as_str()) {
                if component.descriptor().stable_hash() != descriptor.stable_hash() {
                    anyhow::bail!(
                        "Wasm module descriptor changed after compilation: `{}`",
                        descriptor.name
                    );
                }
                continue;
            }
            anyhow::bail!(
                "compiled module requires unregistered module `{}`",
                descriptor.name
            );
        }

        #[cfg(feature = "wasm-extensions")]
        let wasm_instances = self
            .inner
            .wasm_components
            .iter()
            .map(|(name, component)| {
                let limits = WasmLimits {
                    fuel_per_call: self.inner.limits.instructions_per_call as u64,
                    max_memory_bytes: self.inner.limits.max_heap_bytes,
                    max_value_bytes: self.inner.limits.max_value_bytes,
                    max_core_instances: self.inner.limits.max_extension_instances.max(1) * 100,
                    ..WasmLimits::default()
                };
                Ok((name.clone(), component.instantiate(limits)?))
            })
            .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
        let generation = NEXT_INSTANCE_GENERATION.fetch_add(1, Ordering::Relaxed);
        let mut vm = Vm::new();
        vm.set_instance_generation(generation);
        vm.set_call_depth_limit(self.inner.limits.max_call_depth);
        vm.set_host_call_budget(self.inner.limits.native_calls_per_call);
        vm.set_heap_limits(
            self.inner.limits.max_heap_objects,
            self.inner.limits.max_heap_bytes,
        );
        vm.set_function_root_limit(self.inner.limits.max_callback_roots);
        let mut instance = Instance {
            engine: self.clone(),
            program_name: compiled.program.name().to_string(),
            compiled,
            vm,
            state,
            #[cfg(feature = "wasm-extensions")]
            wasm_instances,
            generation,
            not_sync: PhantomData,
        };
        instance
            .vm
            .set_instruction_budget(self.inner.limits.instructions_per_call);
        let engine = Arc::clone(&instance.engine.inner);
        let mut host = EngineHost {
            engine: &engine,
            state: &mut instance.state,
            #[cfg(feature = "wasm-extensions")]
            wasm_instances: &mut instance.wasm_instances,
        };
        instance.vm.load_compiled_plugin(
            instance.program_name.clone(),
            (*instance.compiled.program).clone(),
            &mut host,
        )?;
        Ok(instance)
    }
}

/// Builder for an immutable [`Engine`].
pub struct EngineBuilder<T> {
    modules: BTreeMap<String, NativeModule<T>>,
    #[cfg(feature = "wasm-extensions")]
    wasm_components: BTreeMap<String, WasmComponent>,
    compile_options: CompileOptions,
    limits: Limits,
}

impl<T: 'static> EngineBuilder<T> {
    pub fn register_module(mut self, module: NativeModule<T>) -> anyhow::Result<Self> {
        module.descriptor().validate()?;
        let name = module.descriptor().name.as_str().to_string();
        #[cfg(feature = "wasm-extensions")]
        if self.wasm_components.contains_key(&name) {
            anyhow::bail!("module name `{name}` is already registered by a Wasm component");
        }
        if self.modules.insert(name.clone(), module).is_some() {
            anyhow::bail!("duplicate native module `{name}`");
        }
        Ok(self)
    }

    /// Register one already-compiled portable component module.
    #[cfg(feature = "wasm-extensions")]
    pub fn register_wasm_component(mut self, component: WasmComponent) -> anyhow::Result<Self> {
        component.descriptor().validate()?;
        let name = component.descriptor().name.as_str().to_string();
        if self.modules.contains_key(&name) {
            anyhow::bail!("module name `{name}` is already registered by a native module");
        }
        if self
            .wasm_components
            .insert(name.clone(), component)
            .is_some()
        {
            anyhow::bail!("duplicate Wasm component module `{name}`");
        }
        Ok(self)
    }

    #[must_use]
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    #[must_use]
    pub fn semantic_profile(mut self, profile: SemanticProfile) -> Self {
        self.compile_options.semantic_profile = profile;
        self
    }

    #[must_use]
    pub fn typecheck(mut self, enabled: bool) -> Self {
        self.compile_options.typecheck = enabled;
        self
    }

    /// Enable one compile-time `cfg` flag, such as `test`.
    #[must_use]
    pub fn cfg_flag(mut self, flag: impl Into<String>) -> Self {
        self.compile_options.cfg_flags.insert(flag.into());
        self
    }

    pub fn build(self) -> anyhow::Result<Engine<T>> {
        let module_count = self.modules.len() + {
            #[cfg(feature = "wasm-extensions")]
            {
                self.wasm_components.len()
            }
            #[cfg(not(feature = "wasm-extensions"))]
            {
                0
            }
        };
        if module_count > self.limits.max_modules {
            anyhow::bail!(
                "registered {} Husk modules; the configured maximum is {}",
                module_count,
                self.limits.max_modules
            );
        }
        #[cfg(feature = "wasm-extensions")]
        if self.wasm_components.len() > self.limits.max_extension_instances {
            anyhow::bail!(
                "registered {} Wasm extensions; the configured maximum is {}",
                self.wasm_components.len(),
                self.limits.max_extension_instances
            );
        }
        Ok(Engine {
            inner: Arc::new(EngineInner {
                modules: self.modules,
                #[cfg(feature = "wasm-extensions")]
                wasm_components: self.wasm_components,
                compile_options: self.compile_options,
                limits: self.limits,
            }),
        })
    }
}

/// Immutable, reusable output of Husk compilation.
#[derive(Debug, Clone)]
pub struct CompiledModule {
    program: Arc<CompiledProgram>,
}

impl CompiledModule {
    #[must_use]
    pub fn program(&self) -> &CompiledProgram {
        &self.program
    }
}

static NEXT_INSTANCE_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Isolated mutable execution state for one script.
pub struct Instance<T> {
    engine: Engine<T>,
    program_name: String,
    compiled: CompiledModule,
    vm: Vm,
    state: T,
    #[cfg(feature = "wasm-extensions")]
    wasm_instances: BTreeMap<String, WasmInstance>,
    generation: u64,
    not_sync: PhantomData<Cell<()>>,
}

/// Stateful interactive compiler and interpreter session.
///
/// Top-level items and local bindings remain available until the session is
/// dropped. Submit a returned [`ReplOutcome::Incomplete`] fragment again after
/// appending more source.
pub struct ReplSession<T> {
    instance: Instance<T>,
    item_source: String,
    statement_source: String,
    executed_statements: usize,
    frame: Frame,
}

impl<T: 'static> ReplSession<T> {
    /// Host state shared by native modules registered on this session's engine.
    #[must_use]
    pub fn data(&self) -> &T {
        self.instance.data()
    }

    /// Mutable host state shared by native modules registered on this session's engine.
    #[must_use]
    pub fn data_mut(&mut self) -> &mut T {
        self.instance.data_mut()
    }

    /// Submit one item or statement.
    ///
    /// A complete statement may reference locals and items from earlier
    /// submissions. Incomplete input is not committed. Invalid syntax,
    /// semantic failures, limit failures, and runtime failures are returned as
    /// errors without committing script-owned state.
    pub fn submit(&mut self, source: &str) -> anyhow::Result<ReplOutcome> {
        let fragment = match husk_parser::parse_repl_fragment(source) {
            ReplParseResult::Empty => return Ok(ReplOutcome::Empty),
            ReplParseResult::Incomplete { .. } => return Ok(ReplOutcome::Incomplete),
            ReplParseResult::Invalid { errors } => {
                return Err(invalid_repl_fragment(errors));
            }
            ReplParseResult::Complete(fragment) => fragment,
        };

        match *fragment {
            ReplFragment::Item(_) => self.define_item(source),
            ReplFragment::Statement(_) => self.execute_statement(source),
        }
    }

    fn define_item(&mut self, source: &str) -> anyhow::Result<ReplOutcome> {
        let item_source = append_repl_fragment(&self.item_source, source);
        let complete_source = render_repl_source(&item_source, &self.statement_source);
        let compiled = self.instance.engine.compile_source(
            REPL_PROGRAM_NAME,
            REPL_DISPLAY_PATH,
            &complete_source,
        )?;
        let function = compiled
            .program
            .functions
            .get_by_name(REPL_FUNCTION_NAME)
            .ok_or_else(|| anyhow::anyhow!("compiled REPL is missing its session function"))?;
        if function.hir.body.len() != self.executed_statements {
            anyhow::bail!("REPL item changed the previously compiled statement sequence");
        }
        self.frame.module_path = function.module_path.clone();
        self.frame.imports = function.imports.clone();
        self.instance.vm.programs.insert(
            self.instance.program_name.clone(),
            compiled.program.as_ref().clone().into(),
        );
        self.instance.compiled = compiled;
        self.item_source = item_source;
        Ok(ReplOutcome::Defined)
    }

    fn execute_statement(&mut self, source: &str) -> anyhow::Result<ReplOutcome> {
        let statement_source = append_repl_fragment(&self.statement_source, source);
        let complete_source = render_repl_source(&self.item_source, &statement_source);
        let compiled = self.instance.engine.compile_source(
            REPL_PROGRAM_NAME,
            REPL_DISPLAY_PATH,
            &complete_source,
        )?;
        let function = compiled
            .program
            .functions
            .get_by_name(REPL_FUNCTION_NAME)
            .ok_or_else(|| anyhow::anyhow!("compiled REPL is missing its session function"))?;
        let statement_count = function.hir.body.len();
        let new_statements = function
            .hir
            .body
            .get(self.executed_statements..)
            .ok_or_else(|| {
                anyhow::anyhow!("REPL compilation changed the existing statement sequence")
            })?
            .to_vec();
        if new_statements.is_empty() {
            anyhow::bail!("REPL fragment did not produce an executable statement");
        }

        let mut staged_vm = self.instance.vm.clone();
        staged_vm.programs.insert(
            self.instance.program_name.clone(),
            compiled.program.as_ref().clone().into(),
        );
        let mut staged_frame = self.frame.clone();
        staged_frame.module_path = function.module_path.clone();
        staged_frame.imports = function.imports.clone();
        staged_frame.remaining = self.instance.engine.inner.limits.instructions_per_call;
        staged_frame.remaining_host_calls = self.instance.engine.inner.limits.native_calls_per_call;
        staged_frame.call_depth = 0;

        let engine = Arc::clone(&self.instance.engine.inner);
        let mut host = EngineHost {
            engine: &engine,
            state: &mut self.instance.state,
            #[cfg(feature = "wasm-extensions")]
            wasm_instances: &mut self.instance.wasm_instances,
        };
        let value = staged_vm
            .eval_statements(&new_statements, &mut staged_frame, &mut host)
            .or_else(|error| match error.downcast::<crate::EarlyReturn>() {
                Ok(early) => Ok(early.value),
                Err(error) => Err(error),
            })?;
        let value = runtime_to_owned(value)?;
        ensure_owned_value_size(
            std::slice::from_ref(&value),
            self.instance.engine.inner.limits.max_value_bytes,
        )?;

        self.instance.vm = staged_vm;
        self.instance.compiled = compiled;
        self.frame = staged_frame;
        self.statement_source = statement_source;
        self.executed_statements = statement_count;
        Ok(ReplOutcome::Value(value))
    }
}

impl<T: 'static> Instance<T> {
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn data(&self) -> &T {
        &self.state
    }

    #[must_use]
    pub fn data_mut(&mut self) -> &mut T {
        &mut self.state
    }

    /// Call one named script function and detach its result.
    pub fn call(&mut self, function: &str, arguments: &[OwnedValue]) -> anyhow::Result<OwnedValue> {
        ensure_owned_value_size(arguments, self.engine.inner.limits.max_value_bytes)?;
        let arguments = arguments
            .iter()
            .cloned()
            .map(owned_to_runtime)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let engine = Arc::clone(&self.engine.inner);
        let mut host = EngineHost {
            engine: &engine,
            state: &mut self.state,
            #[cfg(feature = "wasm-extensions")]
            wasm_instances: &mut self.wasm_instances,
        };
        let result = self
            .vm
            .call_export(&self.program_name, function, arguments, &mut host)?;
        let result = runtime_to_owned(result)?;
        ensure_owned_value_size(
            std::slice::from_ref(&result),
            self.engine.inner.limits.max_value_bytes,
        )?;
        Ok(result)
    }

    /// Call a script factory function and retain the closure it returns.
    ///
    /// The returned handle is valid only for this instance generation and
    /// remains a garbage-collection root until [`Self::release_function`] is
    /// called or the instance is dropped.
    pub fn capture_function(
        &mut self,
        function: &str,
        arguments: &[OwnedValue],
    ) -> anyhow::Result<FunctionHandle> {
        ensure_owned_value_size(arguments, self.engine.inner.limits.max_value_bytes)?;
        let arguments = arguments
            .iter()
            .cloned()
            .map(owned_to_runtime)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let engine = Arc::clone(&self.engine.inner);
        let mut host = EngineHost {
            engine: &engine,
            state: &mut self.state,
            #[cfg(feature = "wasm-extensions")]
            wasm_instances: &mut self.wasm_instances,
        };
        self.vm
            .capture_exported_function(&self.program_name, function, arguments, &mut host)
    }

    /// Invoke a retained closure in this instance.
    pub fn invoke_function(
        &mut self,
        handle: FunctionHandle,
        arguments: &[OwnedValue],
    ) -> anyhow::Result<OwnedValue> {
        ensure_owned_value_size(arguments, self.engine.inner.limits.max_value_bytes)?;
        let arguments = arguments
            .iter()
            .cloned()
            .map(owned_to_runtime)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let engine = Arc::clone(&self.engine.inner);
        let mut host = EngineHost {
            engine: &engine,
            state: &mut self.state,
            #[cfg(feature = "wasm-extensions")]
            wasm_instances: &mut self.wasm_instances,
        };
        let value = self
            .vm
            .invoke_function_handle(handle, arguments, &mut host)?;
        let value = runtime_to_owned(value)?;
        ensure_owned_value_size(
            std::slice::from_ref(&value),
            self.engine.inner.limits.max_value_bytes,
        )?;
        Ok(value)
    }

    /// Release a retained closure handle.
    ///
    /// Returns `true` when this call removed a live root and `false` when this
    /// instance had already released the handle.
    pub fn release_function(&mut self, handle: FunctionHandle) -> anyhow::Result<bool> {
        self.vm.release_function_handle(handle)
    }
}

fn append_repl_fragment(existing: &str, fragment: &str) -> String {
    let mut combined = String::with_capacity(existing.len() + fragment.len() + 1);
    combined.push_str(existing);
    combined.push_str(fragment);
    if !fragment.ends_with('\n') {
        combined.push('\n');
    }
    combined
}

fn render_repl_source(items: &str, statements: &str) -> String {
    format!("{items}\nfn {REPL_FUNCTION_NAME}() {{\n{statements}}}\n")
}

fn invalid_repl_fragment(errors: Vec<husk_parser::ParseError>) -> anyhow::Error {
    let details = errors
        .into_iter()
        .map(|error| {
            format!(
                "byte {}: {}",
                error.span.range.start.min(error.span.range.end),
                error.message
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    anyhow::anyhow!("invalid Husk REPL fragment:\n{details}")
}

struct EngineHost<'engine, 'state, T> {
    engine: &'engine EngineInner<T>,
    state: &'state mut T,
    #[cfg(feature = "wasm-extensions")]
    wasm_instances: &'state mut BTreeMap<String, WasmInstance>,
}

impl<T: 'static> Host for EngineHost<'_, '_, T> {
    fn log(&mut self, _message: &str) {}

    fn call_module(
        &mut self,
        _plugin: &str,
        path: &str,
        args: &[Value],
    ) -> Option<anyhow::Result<Value>> {
        let mut segments = path.split("::");
        let module_name = segments.next()?;
        let function = segments.collect::<Vec<_>>().join("::");
        let owns_module = self.engine.modules.contains_key(module_name) || {
            #[cfg(feature = "wasm-extensions")]
            {
                self.wasm_instances.contains_key(module_name)
            }
            #[cfg(not(feature = "wasm-extensions"))]
            {
                false
            }
        };
        if !owns_module {
            return None;
        }
        if function.is_empty() {
            return Some(Err(anyhow::anyhow!(
                "module `{module_name}` is not directly callable"
            )));
        }
        if let Some(module) = self.engine.modules.get(module_name) {
            return Some((|| {
                let arguments = args
                    .iter()
                    .cloned()
                    .map(runtime_to_owned)
                    .collect::<anyhow::Result<Vec<_>>>()?;
                ensure_owned_value_size(&arguments, self.engine.limits.max_value_bytes)?;
                let value = module
                    .call(&function, self.state, &arguments)
                    .map_err(|error| {
                        anyhow::anyhow!("native module call `{path}` failed: {error}")
                    })?;
                ensure_owned_value_size(
                    std::slice::from_ref(&value),
                    self.engine.limits.max_value_bytes,
                )?;
                owned_to_runtime(value)
            })());
        }
        #[cfg(feature = "wasm-extensions")]
        if let Some(instance) = self.wasm_instances.get_mut(module_name) {
            return Some((|| {
                let arguments = args
                    .iter()
                    .cloned()
                    .map(runtime_to_owned)
                    .collect::<anyhow::Result<Vec<_>>>()?;
                ensure_owned_value_size(&arguments, self.engine.limits.max_value_bytes)?;
                let value = instance.call(&function, &arguments)?;
                ensure_owned_value_size(
                    std::slice::from_ref(&value),
                    self.engine.limits.max_value_bytes,
                )?;
                owned_to_runtime(value)
            })());
        }
        None
    }
}

fn runtime_to_owned(value: Value) -> anyhow::Result<OwnedValue> {
    match value {
        Value::Unit => Ok(OwnedValue::Unit),
        Value::Null | Value::Missing(_) => Ok(OwnedValue::Null),
        Value::Bool(value) => Ok(OwnedValue::Bool(value)),
        Value::Int(value) => Ok(OwnedValue::I64(value)),
        Value::Float(value) => Ok(OwnedValue::F64(value)),
        Value::String(value) => Ok(OwnedValue::String(value)),
        Value::Array(values) => values
            .iter()
            .cloned()
            .map(runtime_to_owned)
            .collect::<anyhow::Result<Vec<_>>>()
            .map(OwnedValue::List),
        Value::Tuple(values) => values
            .iter()
            .cloned()
            .map(runtime_to_owned)
            .collect::<anyhow::Result<Vec<_>>>()
            .map(OwnedValue::Tuple),
        Value::Range {
            start,
            end,
            inclusive,
        } => Ok(OwnedValue::Range {
            start,
            end,
            inclusive,
        }),
        Value::Object(values) => {
            if let (
                Some(Value::String(type_name)),
                Some(Value::String(case)),
                Some(Value::Array(fields)),
            ) = (
                values.get("$type"),
                values.get("$case"),
                values.get("$fields"),
            ) {
                return Ok(OwnedValue::Variant {
                    type_name: type_name.clone(),
                    case: case.clone(),
                    fields: fields
                        .iter()
                        .cloned()
                        .map(runtime_to_owned)
                        .collect::<anyhow::Result<Vec<_>>>()?,
                });
            }
            values
                .iter()
                .map(|(key, value)| Ok((key.clone(), runtime_to_owned(value.clone())?)))
                .collect::<anyhow::Result<BTreeMap<_, _>>>()
                .map(OwnedValue::Record)
        }
        Value::Struct { type_name, fields } => Ok(OwnedValue::Struct {
            type_name,
            fields: fields
                .iter()
                .map(|(key, value)| Ok((key.clone(), runtime_to_owned(value.clone())?)))
                .collect::<anyhow::Result<BTreeMap<_, _>>>()?,
        }),
        Value::Variant {
            type_name,
            case,
            fields,
        } => Ok(OwnedValue::Variant {
            type_name,
            case,
            fields: fields
                .iter()
                .cloned()
                .map(runtime_to_owned)
                .collect::<anyhow::Result<Vec<_>>>()?,
        }),
        Value::Json(value) => Ok(OwnedValue::Json(value)),
        Value::Callback(callback) => anyhow::bail!(
            "function `{}` from instance `{}` cannot be detached as ordinary data",
            callback.function,
            callback.plugin
        ),
        Value::Closure(_) => {
            anyhow::bail!("a Husk closure cannot be detached as ordinary data")
        }
    }
}

fn owned_to_runtime(value: OwnedValue) -> anyhow::Result<Value> {
    match value {
        OwnedValue::Unit => Ok(Value::Unit),
        OwnedValue::Null => Ok(Value::Null),
        OwnedValue::Bool(value) => Ok(Value::Bool(value)),
        OwnedValue::I32(value) => Ok(Value::Int(i64::from(value))),
        OwnedValue::I64(value) => Ok(Value::Int(value)),
        OwnedValue::F64(value) => Ok(Value::Float(value)),
        OwnedValue::String(value) => Ok(Value::String(value)),
        OwnedValue::Bytes(values) => Ok(Value::Array(Arc::new(
            values
                .into_iter()
                .map(|value| Value::Int(i64::from(value)))
                .collect(),
        ))),
        OwnedValue::List(values) => values
            .into_iter()
            .map(owned_to_runtime)
            .collect::<anyhow::Result<Vec<_>>>()
            .map(Arc::new)
            .map(Value::Array),
        OwnedValue::Tuple(values) => values
            .into_iter()
            .map(owned_to_runtime)
            .collect::<anyhow::Result<Vec<_>>>()
            .map(Arc::new)
            .map(Value::Tuple),
        OwnedValue::Range {
            start,
            end,
            inclusive,
        } => Ok(Value::Range {
            start,
            end,
            inclusive,
        }),
        OwnedValue::Record(values) => values
            .into_iter()
            .map(|(key, value)| Ok((key, owned_to_runtime(value)?)))
            .collect::<anyhow::Result<BTreeMap<_, _>>>()
            .map(Arc::new)
            .map(Value::Object),
        OwnedValue::Struct { type_name, fields } => Ok(Value::Struct {
            type_name,
            fields: Arc::new(
                fields
                    .into_iter()
                    .map(|(key, value)| Ok((key, owned_to_runtime(value)?)))
                    .collect::<anyhow::Result<BTreeMap<_, _>>>()?,
            ),
        }),
        OwnedValue::Variant {
            type_name,
            case,
            fields,
        } => {
            let fields = fields
                .into_iter()
                .map(owned_to_runtime)
                .collect::<anyhow::Result<Vec<_>>>()?;
            Ok(Value::Variant {
                type_name,
                case,
                fields: Arc::new(fields),
            })
        }
        OwnedValue::Json(value) => Ok(Value::from_json(value)),
    }
}

fn ensure_owned_value_size(values: &[OwnedValue], limit: usize) -> anyhow::Result<()> {
    let mut total = 0usize;
    let mut pending = values.iter().collect::<Vec<_>>();
    while let Some(value) = pending.pop() {
        total = total.saturating_add(std::mem::size_of::<OwnedValue>());
        match value {
            OwnedValue::String(value) => total = total.saturating_add(value.len()),
            OwnedValue::Bytes(value) => total = total.saturating_add(value.len()),
            OwnedValue::List(values) | OwnedValue::Tuple(values) => {
                pending.extend(values);
            }
            OwnedValue::Record(values) => {
                for (name, value) in values {
                    total = total.saturating_add(name.len());
                    pending.push(value);
                }
            }
            OwnedValue::Struct { type_name, fields } => {
                total = total.saturating_add(type_name.len());
                for (name, value) in fields {
                    total = total.saturating_add(name.len());
                    pending.push(value);
                }
            }
            OwnedValue::Variant {
                type_name,
                case,
                fields,
            } => {
                total = total
                    .saturating_add(type_name.len())
                    .saturating_add(case.len());
                pending.extend(fields);
            }
            OwnedValue::Json(value) => {
                let mut json = vec![value];
                while let Some(value) = json.pop() {
                    total = total.saturating_add(std::mem::size_of::<serde_json::Value>());
                    match value {
                        serde_json::Value::String(value) => {
                            total = total.saturating_add(value.len());
                        }
                        serde_json::Value::Array(values) => json.extend(values),
                        serde_json::Value::Object(values) => {
                            for (name, value) in values {
                                total = total.saturating_add(name.len());
                                json.push(value);
                            }
                        }
                        serde_json::Value::Null
                        | serde_json::Value::Bool(_)
                        | serde_json::Value::Number(_) => {}
                    }
                }
            }
            OwnedValue::Unit
            | OwnedValue::Null
            | OwnedValue::Bool(_)
            | OwnedValue::I32(_)
            | OwnedValue::I64(_)
            | OwnedValue::F64(_)
            | OwnedValue::Range { .. } => {}
        }
        if total > limit {
            anyhow::bail!("Husk value exceeds the configured {limit}-byte boundary limit");
        }
    }
    Ok(())
}
