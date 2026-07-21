//! The public embedding API for the Husk scripting language.
//!
//! Compile source once with [`Engine`], instantiate isolated mutable state,
//! and register statically linked Rust crates through [`NativeModule`].
//!
//! ```
//! use husk::{CallContext, Engine, NativeError, NativeModule, OwnedValue};
//!
//! #[derive(Default)]
//! struct State {
//!     calls: usize,
//! }
//!
//! let math = NativeModule::<State>::builder("math")
//!     .typed_function(
//!         "double",
//!         |context: &mut CallContext<'_, State>, value: i32|
//!             -> Result<i32, NativeError> {
//!             context.data_mut().calls += 1;
//!             Ok(value * 2)
//!         },
//!     )
//!     .build()?;
//! let engine = Engine::builder().register_module(math)?.build()?;
//! let compiled = engine.compile_source(
//!     "example",
//!     "example.hk",
//!     "fn answer() -> i32 { return math::double(21); }",
//! )?;
//! let mut instance = engine.instantiate(compiled, State::default())?;
//! assert_eq!(instance.call("answer", &[])?, OwnedValue::I64(42));
//! assert_eq!(instance.data().calls, 1);
//! # Ok::<(), anyhow::Error>(())
//! ```

pub use husk_runtime::{
    CallContext, CompileLimits, CompileOptions, CompiledModule, CompiledProgram, ConversionError,
    DescriptorError, DescriptorHash, Engine, EngineBuilder, ExtensionSource, FieldDescriptor,
    FromHusk, FunctionDescriptor, FunctionHandle, FunctionId, HirFunctionSummary, HuskType,
    Instance, InterfaceDescriptor, IntoHusk, LOCK_FILE, Limits, LockedExtension, LockedPackage,
    MANIFEST_FILE, MainArguments, MainResult, MainSignature, ModuleDescriptor, ModuleFunctionId,
    ModuleName, NativeError, NativeModule, NativeModuleBuilder, OwnedValue, PackageError,
    PackageLimits, PackageLock, PackageManifest, PackageSection, ParameterDescriptor, ReplOutcome,
    ReplSession, ResolvedExtension, ResolvedPackage, ScriptResult, SemanticProfile, SourceModule,
    TestDescriptor, TestExpectation, TypeDefinitionDescriptor, TypeDefinitionKind, TypeDescriptor,
    VariantCaseDescriptor, Version, discover_manifest,
};
#[cfg(feature = "wasm-extensions")]
pub use husk_runtime::{
    WasmCompileOptions, WasmComponent, WasmDescriptorError, WasmInstance, WasmLimits,
    normalize_wit_name,
};
