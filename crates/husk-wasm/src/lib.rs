//! Dynamic, typed Husk modules backed by WebAssembly Components.
//!
//! This crate deliberately links no WASI implementation. A component gets no
//! ambient filesystem, network, environment, clock, random, or process access.
//! Imports are inspected and capability-checked before instantiation.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use anyhow::Context;
use husk_extension::{Capability, ExtensionBundle, validate_capabilities};
use husk_types::{
    FieldDescriptor, FunctionDescriptor, InterfaceDescriptor, ModuleDescriptor,
    ParameterDescriptor, TypeDefinitionDescriptor, TypeDefinitionKind, TypeDescriptor,
    VariantCaseDescriptor, Version,
};
use husk_value::OwnedValue;
use thiserror::Error;
use wasmtime::{
    Config, Engine, Store, StoreLimits, StoreLimitsBuilder,
    component::{
        Component, ComponentExportIndex, Instance as ComponentInstance, Linker, Val,
        types::{ComponentFunc, ComponentInstance as ComponentInstanceType, ComponentItem, Type},
    },
};

/// Compilation and capability policy for one component.
#[derive(Debug, Clone)]
pub struct WasmCompileOptions {
    pub runtime_version: Version,
    pub requested_capabilities: BTreeSet<Capability>,
    pub granted_capabilities: BTreeSet<Capability>,
}

impl Default for WasmCompileOptions {
    fn default() -> Self {
        Self {
            runtime_version: Version::new(0, 1, 0),
            requested_capabilities: BTreeSet::new(),
            granted_capabilities: BTreeSet::new(),
        }
    }
}

/// Per-instance Wasmtime limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WasmLimits {
    pub fuel_per_call: u64,
    pub max_memory_bytes: usize,
    pub max_table_elements: usize,
    pub max_core_instances: usize,
    pub max_tables: usize,
    pub max_memories: usize,
    pub max_value_bytes: usize,
}

impl Default for WasmLimits {
    fn default() -> Self {
        Self {
            fuel_per_call: 1_000_000,
            max_memory_bytes: 64 * 1024 * 1024,
            max_table_elements: 100_000,
            max_core_instances: 1_000,
            max_tables: 100,
            max_memories: 100,
            max_value_bytes: 16 * 1024 * 1024,
        }
    }
}

/// A component shape or naming rule that cannot be represented safely by the
/// initial Husk/WIT contract.
#[derive(Debug, Error)]
pub enum WasmDescriptorError {
    #[error("unsupported WIT type `{0}`")]
    UnsupportedType(&'static str),
    #[error("unsupported component export `{name}` of kind `{kind}`")]
    UnsupportedExport { name: String, kind: &'static str },
    #[error("WIT name `{original}` cannot be represented as a Husk identifier")]
    InvalidName { original: String },
    #[error("WIT names `{first}` and `{second}` both normalize to Husk name `{normalized}`")]
    NameCollision {
        normalized: String,
        first: String,
        second: String,
    },
    #[error("record, variant, or enum type used by `{context}` has no exported WIT type name")]
    AnonymousNominalType { context: String },
    #[error("component export `{0}` disappeared during indexed lookup")]
    MissingExport(String),
    #[error("component import `{0}` has no version-1 Husk capability mapping")]
    UnsupportedImport(String),
}

#[derive(Debug, Clone)]
struct NamedType {
    original_name: String,
    husk_name: String,
    ty: Type,
}

#[derive(Debug, Clone)]
struct WasmFunction {
    export: ComponentExportIndex,
    parameters: Vec<Type>,
    results: Vec<Type>,
}

struct WasmComponentInner {
    engine: Engine,
    component: Component,
    descriptor: ModuleDescriptor,
    functions: BTreeMap<String, WasmFunction>,
    actual_imports: BTreeSet<Capability>,
    raw_imports: Vec<String>,
}

/// A compiled component shared by every Husk instance in one engine.
#[derive(Clone)]
pub struct WasmComponent {
    inner: Arc<WasmComponentInner>,
}

impl WasmComponent {
    /// Compile and inspect a validated extension bundle.
    pub fn from_bundle(
        bundle: &ExtensionBundle,
        mut options: WasmCompileOptions,
    ) -> anyhow::Result<Self> {
        if bundle.manifest().minimum_husk > options.runtime_version {
            anyhow::bail!(
                "extension `{}` requires Husk {}, but this runtime is {}",
                bundle.manifest().name,
                bundle.manifest().minimum_husk,
                options.runtime_version
            );
        }
        options.requested_capabilities = bundle
            .manifest()
            .capabilities
            .requested
            .iter()
            .cloned()
            .collect();
        Self::compile_bytes(
            bundle.module().as_str(),
            bundle.manifest().version.clone(),
            bundle.component(),
            options,
        )
    }

    /// Compile component bytes and derive the module descriptor entirely from
    /// component exports.
    pub fn compile_bytes(
        module_name: &str,
        module_version: Version,
        bytes: &[u8],
        options: WasmCompileOptions,
    ) -> anyhow::Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true).consume_fuel(true);
        let engine = Engine::new(&config)
            .map_err(anyhow::Error::from)
            .context("configure Wasmtime component engine")?;
        let component = Component::new(&engine, bytes)
            .map_err(anyhow::Error::from)
            .context("compile WebAssembly component")?;

        let component_type = component.component_type();
        let mut actual_imports = BTreeSet::new();
        let mut raw_imports = Vec::new();
        for (name, _) in component_type.imports(&engine) {
            raw_imports.push(name.to_string());
            actual_imports.insert(capability_for_import(name)?);
        }
        raw_imports.sort();
        validate_capabilities(
            &actual_imports,
            &options.requested_capabilities,
            &options.granted_capabilities,
        )
        .context("component capability policy rejected extension")?;

        let inspection = inspect_exports(&engine, &component, module_name, module_version)?;

        Ok(Self {
            inner: Arc::new(WasmComponentInner {
                engine,
                component,
                descriptor: inspection.descriptor,
                functions: inspection.functions,
                actual_imports,
                raw_imports,
            }),
        })
    }

    #[must_use]
    pub fn descriptor(&self) -> &ModuleDescriptor {
        &self.inner.descriptor
    }

    #[must_use]
    pub fn actual_imports(&self) -> &BTreeSet<Capability> {
        &self.inner.actual_imports
    }

    #[must_use]
    pub fn raw_imports(&self) -> &[String] {
        &self.inner.raw_imports
    }

    /// Create one isolated mutable component store and instance.
    pub fn instantiate(&self, limits: WasmLimits) -> anyhow::Result<WasmInstance> {
        let store_limits = StoreLimitsBuilder::new()
            .memory_size(limits.max_memory_bytes)
            .table_elements(limits.max_table_elements)
            .instances(limits.max_core_instances)
            .tables(limits.max_tables)
            .memories(limits.max_memories)
            .trap_on_grow_failure(true)
            .build();
        let mut store = Store::new(&self.inner.engine, StoreData { store_limits });
        store.limiter(|data| &mut data.store_limits);
        store
            .set_fuel(limits.fuel_per_call)
            .map_err(anyhow::Error::from)
            .context("configure extension fuel")?;

        // Intentionally empty: capability grants do not implicitly link WASI
        // or any other host interface.
        let linker = Linker::new(&self.inner.engine);
        let instance = linker
            .instantiate(&mut store, &self.inner.component)
            .map_err(anyhow::Error::from)
            .with_context(|| {
                if self.inner.raw_imports.is_empty() {
                    format!(
                        "instantiate extension module `{}`",
                        self.inner.descriptor.name
                    )
                } else {
                    format!(
                        "instantiate extension module `{}`; no providers are linked for imports: {}",
                        self.inner.descriptor.name,
                        self.inner.raw_imports.join(", ")
                    )
                }
            })?;

        Ok(WasmInstance {
            component: self.clone(),
            store,
            instance,
            limits,
            poisoned: false,
        })
    }
}

struct StoreData {
    store_limits: StoreLimits,
}

/// One mutable, non-shareable WebAssembly Component instance.
pub struct WasmInstance {
    component: WasmComponent,
    store: Store<StoreData>,
    instance: ComponentInstance,
    limits: WasmLimits,
    poisoned: bool,
}

impl WasmInstance {
    #[must_use]
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// Invoke a normalized Husk path such as `api::is_match`.
    pub fn call(&mut self, path: &str, arguments: &[OwnedValue]) -> anyhow::Result<OwnedValue> {
        if self.poisoned {
            anyhow::bail!(
                "extension module `{}` is poisoned after an earlier guest failure",
                self.component.inner.descriptor.name
            );
        }
        let function = self
            .component
            .inner
            .functions
            .get(path)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "extension module `{}` has no function `{path}`",
                    self.component.inner.descriptor.name
                )
            })?
            .clone();
        if arguments.len() != function.parameters.len() {
            anyhow::bail!(
                "extension call `{}::{path}` expected {} arguments, got {}",
                self.component.inner.descriptor.name,
                function.parameters.len(),
                arguments.len()
            );
        }
        ensure_value_size(arguments, self.limits.max_value_bytes)
            .context("extension arguments exceed configured value limit")?;

        let parameters = arguments
            .iter()
            .zip(&function.parameters)
            .enumerate()
            .map(|(index, (value, ty))| {
                owned_to_component(value, ty).with_context(|| {
                    format!(
                        "convert argument {index} for extension call `{}::{path}`",
                        self.component.inner.descriptor.name
                    )
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut results = vec![Val::Bool(false); function.results.len()];
        self.store
            .set_fuel(self.limits.fuel_per_call)
            .map_err(anyhow::Error::from)
            .context("reset extension call fuel")?;
        let func = self
            .instance
            .get_func(&mut self.store, function.export)
            .ok_or_else(|| anyhow::anyhow!("compiled extension export `{path}` is missing"))?;
        if let Err(error) = func.call(&mut self.store, &parameters, &mut results) {
            self.poisoned = true;
            return Err(anyhow::Error::from(error)).with_context(|| {
                format!(
                    "extension call `{}::{path}` trapped or failed",
                    self.component.inner.descriptor.name
                )
            });
        }

        let values = results
            .into_iter()
            .zip(&function.results)
            .enumerate()
            .map(|(index, (value, ty))| {
                component_to_owned(value, ty).with_context(|| {
                    format!(
                        "convert result {index} from extension call `{}::{path}`",
                        self.component.inner.descriptor.name
                    )
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        ensure_value_size(&values, self.limits.max_value_bytes)
            .context("extension results exceed configured value limit")?;
        Ok(match values.len() {
            0 => OwnedValue::Unit,
            1 => values.into_iter().next().expect("one result"),
            _ => OwnedValue::Tuple(values),
        })
    }
}

struct Inspection {
    descriptor: ModuleDescriptor,
    functions: BTreeMap<String, WasmFunction>,
}

fn inspect_exports(
    engine: &Engine,
    component: &Component,
    module_name: &str,
    module_version: Version,
) -> anyhow::Result<Inspection> {
    let component_type = component.component_type();
    let root_named = collect_named_types(
        module_name,
        None,
        component_type
            .exports(engine)
            .filter_map(|(name, export)| match export.ty {
                ComponentItem::Type(ty) => Some((name.to_string(), ty)),
                _ => None,
            }),
    )?;
    let root_types = build_type_definitions(&root_named)?;

    let mut functions = BTreeMap::new();
    let mut function_descriptors = Vec::new();
    let mut interface_descriptors = Vec::new();
    let mut member_names = BTreeMap::new();

    for (original_name, export) in component_type.exports(engine) {
        match export.ty {
            ComponentItem::Type(_) => {}
            ComponentItem::ComponentFunc(function) => {
                let husk_name =
                    normalize_and_track(original_name, &mut member_names, "root component export")?;
                let export_index = component
                    .get_export_index(None, original_name)
                    .ok_or_else(|| WasmDescriptorError::MissingExport(original_name.to_string()))?;
                let (descriptor, wasm_function) = describe_function(
                    &husk_name,
                    function,
                    &root_named,
                    export_index,
                    original_name,
                )?;
                functions.insert(husk_name, wasm_function);
                function_descriptors.push(descriptor);
            }
            ComponentItem::ComponentInstance(instance_type) => {
                let short_name = short_export_name(original_name);
                let interface_name =
                    normalize_and_track(short_name, &mut member_names, "component interface")?;
                let parent_index = component
                    .get_export_index(None, original_name)
                    .ok_or_else(|| WasmDescriptorError::MissingExport(original_name.to_string()))?;
                let interface = inspect_interface(
                    engine,
                    component,
                    module_name,
                    original_name,
                    &interface_name,
                    instance_type,
                    &root_named,
                    parent_index,
                    &mut functions,
                )?;
                interface_descriptors.push(interface);
            }
            other => {
                return Err(WasmDescriptorError::UnsupportedExport {
                    name: original_name.to_string(),
                    kind: component_item_name(&other),
                }
                .into());
            }
        }
    }

    function_descriptors.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    interface_descriptors.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    let descriptor = ModuleDescriptor::new(
        module_name,
        module_version,
        function_descriptors,
        interface_descriptors,
    )?
    .with_types(root_types)?;
    Ok(Inspection {
        descriptor,
        functions,
    })
}

#[allow(clippy::too_many_arguments)]
fn inspect_interface(
    engine: &Engine,
    component: &Component,
    module_name: &str,
    original_interface_name: &str,
    interface_name: &str,
    instance_type: ComponentInstanceType,
    root_named: &[NamedType],
    parent_index: ComponentExportIndex,
    functions: &mut BTreeMap<String, WasmFunction>,
) -> anyhow::Result<InterfaceDescriptor> {
    let interface_named = collect_named_types(
        module_name,
        Some(interface_name),
        instance_type
            .exports(engine)
            .filter_map(|(name, export)| match export.ty {
                ComponentItem::Type(ty) => Some((name.to_string(), ty)),
                _ => None,
            }),
    )?;
    let mut visible_named = root_named.to_vec();
    visible_named.extend(interface_named.clone());
    let type_definitions = build_type_definitions(&interface_named)?;
    let mut descriptors = Vec::new();
    let mut normalized_names = BTreeMap::new();

    for (original_name, export) in instance_type.exports(engine) {
        match export.ty {
            ComponentItem::Type(_) => {}
            ComponentItem::ComponentFunc(function) => {
                let husk_name = normalize_and_track(
                    original_name,
                    &mut normalized_names,
                    "interface function",
                )?;
                let export_index = component
                    .get_export_index(Some(&parent_index), original_name)
                    .ok_or_else(|| {
                        WasmDescriptorError::MissingExport(format!(
                            "{original_interface_name}/{original_name}"
                        ))
                    })?;
                let (descriptor, wasm_function) = describe_function(
                    &husk_name,
                    function,
                    &visible_named,
                    export_index,
                    &format!("{original_interface_name}/{original_name}"),
                )?;
                let path = format!("{interface_name}::{husk_name}");
                if functions.insert(path.clone(), wasm_function).is_some() {
                    anyhow::bail!("duplicate normalized component function path `{path}`");
                }
                descriptors.push(descriptor);
            }
            other => {
                return Err(WasmDescriptorError::UnsupportedExport {
                    name: format!("{original_interface_name}/{original_name}"),
                    kind: component_item_name(&other),
                }
                .into());
            }
        }
    }

    descriptors.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    InterfaceDescriptor::new(interface_name, descriptors)?
        .with_types(type_definitions)
        .map_err(Into::into)
}

fn describe_function(
    husk_name: &str,
    function: ComponentFunc,
    named_types: &[NamedType],
    export: ComponentExportIndex,
    context: &str,
) -> anyhow::Result<(FunctionDescriptor, WasmFunction)> {
    if function.async_() {
        return Err(WasmDescriptorError::UnsupportedType("async function").into());
    }
    let parameters = function.params().collect::<Vec<_>>();
    let result_types = function.results().collect::<Vec<_>>();
    let mut parameter_names = BTreeMap::new();
    let parameter_descriptors = parameters
        .iter()
        .enumerate()
        .map(|(index, (name, ty))| {
            let original = if name.is_empty() {
                format!("arg{index}")
            } else {
                (*name).to_string()
            };
            let normalized =
                normalize_and_track(&original, &mut parameter_names, "function parameter")?;
            let ty = map_type(ty, named_types, context)?;
            ParameterDescriptor::new(normalized, ty).map_err(anyhow::Error::from)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let result = match result_types.as_slice() {
        [] => TypeDescriptor::Unit,
        [result] => map_type(result, named_types, context)?,
        results => TypeDescriptor::Tuple(
            results
                .iter()
                .map(|ty| map_type(ty, named_types, context))
                .collect::<anyhow::Result<Vec<_>>>()?,
        ),
    };
    let descriptor = FunctionDescriptor::new(husk_name, parameter_descriptors, result)?;
    Ok((
        descriptor,
        WasmFunction {
            export,
            parameters: parameters.into_iter().map(|(_, ty)| ty).collect(),
            results: result_types,
        },
    ))
}

fn collect_named_types(
    module_name: &str,
    interface_name: Option<&str>,
    exports: impl IntoIterator<Item = (String, Type)>,
) -> anyhow::Result<Vec<NamedType>> {
    let mut names = BTreeMap::new();
    let mut result = Vec::new();
    for (original_name, ty) in exports {
        let short_name = short_export_name(&original_name);
        let local_name = normalize_and_track(short_name, &mut names, "exported WIT type")?;
        let prefix = match interface_name {
            Some(interface) => format!("{module_name}_{interface}_{local_name}"),
            None => format!("{module_name}_{local_name}"),
        };
        let husk_name = normalize_wit_name(&prefix)?;
        result.push(NamedType {
            original_name,
            husk_name,
            ty,
        });
    }
    result.sort_unstable_by(|left, right| left.husk_name.cmp(&right.husk_name));
    Ok(result)
}

fn build_type_definitions(
    named_types: &[NamedType],
) -> anyhow::Result<Vec<TypeDefinitionDescriptor>> {
    named_types
        .iter()
        .map(|named| {
            let context = format!("exported type `{}`", named.original_name);
            let kind = match &named.ty {
                Type::Record(record) => TypeDefinitionKind::Record(
                    record
                        .fields()
                        .map(|field| {
                            FieldDescriptor::new(
                                normalize_wit_name(field.name)?,
                                map_type_excluding(
                                    &field.ty,
                                    named_types,
                                    &context,
                                    Some(&named.husk_name),
                                )?,
                            )
                            .map_err(anyhow::Error::from)
                        })
                        .collect::<anyhow::Result<Vec<_>>>()?,
                ),
                Type::Enum(enum_type) => {
                    let cases = normalize_unique(enum_type.names(), "enum case")?;
                    TypeDefinitionKind::Enum(cases)
                }
                Type::Variant(variant) => TypeDefinitionKind::Variant(
                    variant
                        .cases()
                        .map(|case| {
                            VariantCaseDescriptor::new(
                                normalize_wit_name(case.name)?,
                                case.ty
                                    .as_ref()
                                    .map(|ty| {
                                        map_type_excluding(
                                            ty,
                                            named_types,
                                            &context,
                                            Some(&named.husk_name),
                                        )
                                    })
                                    .transpose()?,
                            )
                            .map_err(anyhow::Error::from)
                        })
                        .collect::<anyhow::Result<Vec<_>>>()?,
                ),
                ty => TypeDefinitionKind::Alias(map_type_excluding(
                    ty,
                    named_types,
                    &context,
                    Some(&named.husk_name),
                )?),
            };
            TypeDefinitionDescriptor::new(named.husk_name.clone(), kind)
                .map_err(anyhow::Error::from)
        })
        .collect()
}

fn map_type(ty: &Type, named_types: &[NamedType], context: &str) -> anyhow::Result<TypeDescriptor> {
    map_type_excluding(ty, named_types, context, None)
}

fn map_type_excluding(
    ty: &Type,
    named_types: &[NamedType],
    context: &str,
    excluded_name: Option<&str>,
) -> anyhow::Result<TypeDescriptor> {
    if let Some(named) = named_types
        .iter()
        .find(|named| named.husk_name != excluded_name.unwrap_or_default() && named.ty == *ty)
    {
        return Ok(TypeDescriptor::Named(named.husk_name.clone()));
    }
    match ty {
        Type::Bool => Ok(TypeDescriptor::Bool),
        Type::U8 | Type::S32 => Ok(TypeDescriptor::I32),
        Type::S64 => Ok(TypeDescriptor::I64),
        Type::Float64 => Ok(TypeDescriptor::F64),
        Type::String => Ok(TypeDescriptor::String),
        Type::List(list) => Ok(TypeDescriptor::list(map_type_excluding(
            &list.ty(),
            named_types,
            context,
            excluded_name,
        )?)),
        Type::Tuple(tuple) => Ok(TypeDescriptor::Tuple(
            tuple
                .types()
                .map(|ty| map_type_excluding(&ty, named_types, context, excluded_name))
                .collect::<anyhow::Result<Vec<_>>>()?,
        )),
        Type::Option(option) => Ok(TypeDescriptor::option(map_type_excluding(
            &option.ty(),
            named_types,
            context,
            excluded_name,
        )?)),
        Type::Result(result) => Ok(TypeDescriptor::result(
            result
                .ok()
                .map(|ty| map_type_excluding(&ty, named_types, context, excluded_name))
                .transpose()?
                .unwrap_or(TypeDescriptor::Unit),
            result
                .err()
                .map(|ty| map_type_excluding(&ty, named_types, context, excluded_name))
                .transpose()?
                .unwrap_or(TypeDescriptor::Unit),
        )),
        Type::Record(_) | Type::Variant(_) | Type::Enum(_) => {
            Err(WasmDescriptorError::AnonymousNominalType {
                context: context.to_string(),
            }
            .into())
        }
        Type::S8 => Err(WasmDescriptorError::UnsupportedType("s8").into()),
        Type::S16 => Err(WasmDescriptorError::UnsupportedType("s16").into()),
        Type::U16 => Err(WasmDescriptorError::UnsupportedType("u16").into()),
        Type::U32 => Err(WasmDescriptorError::UnsupportedType("u32").into()),
        Type::U64 => Err(WasmDescriptorError::UnsupportedType("u64").into()),
        Type::Float32 => Err(WasmDescriptorError::UnsupportedType("float32").into()),
        Type::Char => Err(WasmDescriptorError::UnsupportedType("char").into()),
        Type::Map(_) => Err(WasmDescriptorError::UnsupportedType("map").into()),
        Type::Flags(_) => Err(WasmDescriptorError::UnsupportedType("flags").into()),
        Type::Own(_) => Err(WasmDescriptorError::UnsupportedType("own resource").into()),
        Type::Borrow(_) => Err(WasmDescriptorError::UnsupportedType("borrowed resource").into()),
        Type::Future(_) => Err(WasmDescriptorError::UnsupportedType("future").into()),
        Type::Stream(_) => Err(WasmDescriptorError::UnsupportedType("stream").into()),
        Type::ErrorContext => Err(WasmDescriptorError::UnsupportedType("error-context").into()),
    }
}

/// Convert a WIT kebab-case member name into one valid Husk identifier.
pub fn normalize_wit_name(original: &str) -> Result<String, WasmDescriptorError> {
    let mut normalized = String::with_capacity(original.len());
    for character in original.chars() {
        if character == '-' {
            normalized.push('_');
        } else if character.is_ascii_alphanumeric() || character == '_' {
            normalized.push(character);
        } else {
            return Err(WasmDescriptorError::InvalidName {
                original: original.to_string(),
            });
        }
    }
    let valid_start = normalized
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || character == '_');
    if !valid_start || normalized.is_empty() {
        return Err(WasmDescriptorError::InvalidName {
            original: original.to_string(),
        });
    }
    Ok(normalized)
}

fn normalize_and_track(
    original: &str,
    names: &mut BTreeMap<String, String>,
    _kind: &'static str,
) -> Result<String, WasmDescriptorError> {
    let normalized = normalize_wit_name(original)?;
    if let Some(first) = names.insert(normalized.clone(), original.to_string()) {
        return Err(WasmDescriptorError::NameCollision {
            normalized,
            first,
            second: original.to_string(),
        });
    }
    Ok(normalized)
}

fn normalize_unique<'a>(
    values: impl IntoIterator<Item = &'a str>,
    kind: &'static str,
) -> Result<Vec<String>, WasmDescriptorError> {
    let mut names = BTreeMap::new();
    values
        .into_iter()
        .map(|name| normalize_and_track(name, &mut names, kind))
        .collect()
}

fn short_export_name(name: &str) -> &str {
    let without_version = name.split('@').next().unwrap_or(name);
    let without_package = without_version
        .rsplit_once('/')
        .map_or(without_version, |(_, short)| short);
    without_package
        .rsplit_once(':')
        .map_or(without_package, |(_, short)| short)
}

fn component_item_name(item: &ComponentItem) -> &'static str {
    match item {
        ComponentItem::ComponentFunc(_) => "component function",
        ComponentItem::CoreFunc(_) => "core function",
        ComponentItem::Module(_) => "core module",
        ComponentItem::Component(_) => "nested component",
        ComponentItem::ComponentInstance(_) => "component instance",
        ComponentItem::Type(_) => "type",
        ComponentItem::Resource(_) => "resource",
    }
}

fn capability_for_import(name: &str) -> Result<Capability, WasmDescriptorError> {
    let capability = if name.starts_with("wasi:filesystem/") {
        "filesystem"
    } else if name.starts_with("wasi:sockets/") || name.starts_with("wasi:http/") {
        "network"
    } else if name.starts_with("wasi:clocks/") {
        "clock"
    } else if name.starts_with("wasi:random/") {
        "random"
    } else if name.starts_with("wasi:cli/environment") {
        "environment"
    } else if name.starts_with("wasi:cli/exit") {
        "process"
    } else if name.starts_with("wasi:io/") {
        "io"
    } else if name.starts_with("husk:log/") {
        "log"
    } else {
        return Err(WasmDescriptorError::UnsupportedImport(name.to_string()));
    };
    Capability::new(capability)
        .map_err(|_| WasmDescriptorError::UnsupportedImport(name.to_string()))
}

fn owned_to_component(value: &OwnedValue, ty: &Type) -> anyhow::Result<Val> {
    match (value, ty) {
        (OwnedValue::Bool(value), Type::Bool) => Ok(Val::Bool(*value)),
        (OwnedValue::I32(value), Type::S32) => Ok(Val::S32(*value)),
        (OwnedValue::I64(value), Type::S32) => Ok(Val::S32(
            i32::try_from(*value).context("i64 is outside s32 range")?,
        )),
        (OwnedValue::I32(value), Type::U8) => Ok(Val::U8(
            u8::try_from(*value).context("i32 is outside u8 range")?,
        )),
        (OwnedValue::I64(value), Type::U8) => Ok(Val::U8(
            u8::try_from(*value).context("i64 is outside u8 range")?,
        )),
        (OwnedValue::I64(value), Type::S64) => Ok(Val::S64(*value)),
        (OwnedValue::I32(value), Type::S64) => Ok(Val::S64(i64::from(*value))),
        (OwnedValue::F64(value), Type::Float64) => Ok(Val::Float64(*value)),
        (OwnedValue::String(value), Type::String) => Ok(Val::String(value.clone())),
        (OwnedValue::Bytes(values), Type::List(list)) if list.ty() == Type::U8 => {
            Ok(Val::List(values.iter().copied().map(Val::U8).collect()))
        }
        (OwnedValue::List(values), Type::List(list)) => Ok(Val::List(
            values
                .iter()
                .map(|value| owned_to_component(value, &list.ty()))
                .collect::<anyhow::Result<Vec<_>>>()?,
        )),
        (OwnedValue::Tuple(values), Type::Tuple(tuple)) => {
            let types = tuple.types().collect::<Vec<_>>();
            if values.len() != types.len() {
                anyhow::bail!(
                    "tuple expected {} fields, got {}",
                    types.len(),
                    values.len()
                );
            }
            Ok(Val::Tuple(
                values
                    .iter()
                    .zip(&types)
                    .map(|(value, ty)| owned_to_component(value, ty))
                    .collect::<anyhow::Result<Vec<_>>>()?,
            ))
        }
        (
            OwnedValue::Record(values) | OwnedValue::Struct { fields: values, .. },
            Type::Record(record),
        ) => {
            let fields = record.fields().collect::<Vec<_>>();
            if values.len() != fields.len() {
                anyhow::bail!(
                    "record expected {} fields, got {}",
                    fields.len(),
                    values.len()
                );
            }
            Ok(Val::Record(
                fields
                    .into_iter()
                    .map(|field| {
                        let husk_name = normalize_wit_name(field.name)?;
                        let value = values.get(&husk_name).ok_or_else(|| {
                            anyhow::anyhow!("record field `{husk_name}` is missing")
                        })?;
                        Ok((
                            field.name.to_string(),
                            owned_to_component(value, &field.ty)?,
                        ))
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?,
            ))
        }
        (OwnedValue::Variant { case, fields, .. }, Type::Variant(variant)) => {
            let matching = variant
                .cases()
                .find(|candidate| {
                    normalize_wit_name(candidate.name).is_ok_and(|name| name == *case)
                })
                .ok_or_else(|| anyhow::anyhow!("unknown variant case `{case}`"))?;
            let payload = match (&matching.ty, fields.as_slice()) {
                (None, []) => None,
                (Some(ty), [field]) => Some(Box::new(owned_to_component(field, ty)?)),
                (None, _) => anyhow::bail!("unit variant `{case}` does not take a payload"),
                (Some(_), _) => anyhow::bail!("variant `{case}` requires exactly one payload"),
            };
            Ok(Val::Variant(matching.name.to_string(), payload))
        }
        (OwnedValue::Variant { case, fields, .. }, Type::Enum(enum_type)) => {
            if !fields.is_empty() {
                anyhow::bail!("enum case `{case}` does not take a payload");
            }
            let matching = enum_type
                .names()
                .find(|candidate| normalize_wit_name(candidate).is_ok_and(|name| name == *case))
                .ok_or_else(|| anyhow::anyhow!("unknown enum case `{case}`"))?;
            Ok(Val::Enum(matching.to_string()))
        }
        (OwnedValue::Variant { case, fields, .. }, Type::Option(option)) => match case.as_str() {
            "None" if fields.is_empty() => Ok(Val::Option(None)),
            "Some" if fields.len() == 1 => Ok(Val::Option(Some(Box::new(owned_to_component(
                &fields[0],
                &option.ty(),
            )?)))),
            "None" => anyhow::bail!("Option::None does not take a payload"),
            "Some" => anyhow::bail!("Option::Some requires exactly one payload"),
            _ => anyhow::bail!("expected Option::Some or Option::None, got `{case}`"),
        },
        (OwnedValue::Variant { case, fields, .. }, Type::Result(result)) => {
            let payload = |ty: Option<Type>, fields: &[OwnedValue]| -> anyhow::Result<_> {
                match (ty, fields) {
                    (None, []) => Ok(None),
                    (Some(ty), [field]) => Ok(Some(Box::new(owned_to_component(field, &ty)?))),
                    (None, _) => anyhow::bail!("unit result case does not take a payload"),
                    (Some(_), _) => anyhow::bail!("result case requires exactly one payload"),
                }
            };
            match case.as_str() {
                "Ok" => Ok(Val::Result(Ok(payload(result.ok(), fields)?))),
                "Err" => Ok(Val::Result(Err(payload(result.err(), fields)?))),
                _ => anyhow::bail!("expected Result::Ok or Result::Err, got `{case}`"),
            }
        }
        _ => anyhow::bail!(
            "Husk {} value is incompatible with WIT {}",
            value.kind_name(),
            wit_type_name(ty)
        ),
    }
}

fn component_to_owned(value: Val, ty: &Type) -> anyhow::Result<OwnedValue> {
    match (value, ty) {
        (Val::Bool(value), Type::Bool) => Ok(OwnedValue::Bool(value)),
        (Val::S32(value), Type::S32) => Ok(OwnedValue::I32(value)),
        (Val::U8(value), Type::U8) => Ok(OwnedValue::I32(i32::from(value))),
        (Val::S64(value), Type::S64) => Ok(OwnedValue::I64(value)),
        (Val::Float64(value), Type::Float64) => Ok(OwnedValue::F64(value)),
        (Val::String(value), Type::String) => Ok(OwnedValue::String(value)),
        (Val::List(values), Type::List(list)) if list.ty() == Type::U8 => values
            .into_iter()
            .map(|value| match value {
                Val::U8(value) => Ok(value),
                other => anyhow::bail!("expected u8 list element, got {other:?}"),
            })
            .collect::<anyhow::Result<Vec<_>>>()
            .map(OwnedValue::Bytes),
        (Val::List(values), Type::List(list)) => values
            .into_iter()
            .map(|value| component_to_owned(value, &list.ty()))
            .collect::<anyhow::Result<Vec<_>>>()
            .map(OwnedValue::List),
        (Val::Tuple(values), Type::Tuple(tuple)) => {
            let types = tuple.types().collect::<Vec<_>>();
            if values.len() != types.len() {
                anyhow::bail!(
                    "component returned tuple with {} fields, expected {}",
                    values.len(),
                    types.len()
                );
            }
            values
                .into_iter()
                .zip(&types)
                .map(|(value, ty)| component_to_owned(value, ty))
                .collect::<anyhow::Result<Vec<_>>>()
                .map(OwnedValue::Tuple)
        }
        (Val::Record(values), Type::Record(record)) => {
            let fields = record.fields().collect::<Vec<_>>();
            if values.len() != fields.len() {
                anyhow::bail!(
                    "component returned record with {} fields, expected {}",
                    values.len(),
                    fields.len()
                );
            }
            values
                .into_iter()
                .zip(fields)
                .map(|((actual_name, value), field)| {
                    if actual_name != field.name {
                        anyhow::bail!(
                            "component returned record field `{actual_name}`, expected `{}`",
                            field.name
                        );
                    }
                    Ok((
                        normalize_wit_name(field.name)?,
                        component_to_owned(value, &field.ty)?,
                    ))
                })
                .collect::<anyhow::Result<BTreeMap<_, _>>>()
                .map(OwnedValue::Record)
        }
        (Val::Variant(case, payload), Type::Variant(variant)) => {
            let definition = variant
                .cases()
                .find(|candidate| candidate.name == case)
                .ok_or_else(|| {
                    anyhow::anyhow!("component returned unknown variant case `{case}`")
                })?;
            let fields = match (payload, definition.ty) {
                (None, None) => Vec::new(),
                (Some(value), Some(ty)) => vec![component_to_owned(*value, &ty)?],
                _ => anyhow::bail!("component returned the wrong payload shape for `{case}`"),
            };
            Ok(OwnedValue::Variant {
                type_name: "WitVariant".to_string(),
                case: normalize_wit_name(&case)?,
                fields,
            })
        }
        (Val::Enum(case), Type::Enum(enum_type)) => {
            if !enum_type.names().any(|candidate| candidate == case) {
                anyhow::bail!("component returned unknown enum case `{case}`");
            }
            Ok(OwnedValue::Variant {
                type_name: "WitEnum".to_string(),
                case: normalize_wit_name(&case)?,
                fields: Vec::new(),
            })
        }
        (Val::Option(payload), Type::Option(option)) => Ok(OwnedValue::Variant {
            type_name: "Option".to_string(),
            case: if payload.is_some() { "Some" } else { "None" }.to_string(),
            fields: payload
                .map(|value| component_to_owned(*value, &option.ty()))
                .transpose()?
                .into_iter()
                .collect(),
        }),
        (Val::Result(result_value), Type::Result(result_type)) => match result_value {
            Ok(payload) => Ok(OwnedValue::Variant {
                type_name: "Result".to_string(),
                case: "Ok".to_string(),
                fields: payload
                    .map(|value| {
                        result_type
                            .ok()
                            .ok_or_else(|| anyhow::anyhow!("unexpected Result::Ok payload"))
                            .and_then(|ty| component_to_owned(*value, &ty))
                    })
                    .transpose()?
                    .into_iter()
                    .collect(),
            }),
            Err(payload) => Ok(OwnedValue::Variant {
                type_name: "Result".to_string(),
                case: "Err".to_string(),
                fields: payload
                    .map(|value| {
                        result_type
                            .err()
                            .ok_or_else(|| anyhow::anyhow!("unexpected Result::Err payload"))
                            .and_then(|ty| component_to_owned(*value, &ty))
                    })
                    .transpose()?
                    .into_iter()
                    .collect(),
            }),
        },
        (value, ty) => anyhow::bail!(
            "component returned {value:?}, which is incompatible with WIT {}",
            wit_type_name(ty)
        ),
    }
}

fn wit_type_name(ty: &Type) -> &'static str {
    match ty {
        Type::Bool => "bool",
        Type::S8 => "s8",
        Type::U8 => "u8",
        Type::S16 => "s16",
        Type::U16 => "u16",
        Type::S32 => "s32",
        Type::U32 => "u32",
        Type::S64 => "s64",
        Type::U64 => "u64",
        Type::Float32 => "float32",
        Type::Float64 => "float64",
        Type::Char => "char",
        Type::String => "string",
        Type::List(_) => "list",
        Type::Map(_) => "map",
        Type::Record(_) => "record",
        Type::Tuple(_) => "tuple",
        Type::Variant(_) => "variant",
        Type::Enum(_) => "enum",
        Type::Option(_) => "option",
        Type::Result(_) => "result",
        Type::Flags(_) => "flags",
        Type::Own(_) => "own",
        Type::Borrow(_) => "borrow",
        Type::Future(_) => "future",
        Type::Stream(_) => "stream",
        Type::ErrorContext => "error-context",
    }
}

fn ensure_value_size(values: &[OwnedValue], limit: usize) -> anyhow::Result<()> {
    let mut remaining = limit;
    for value in values {
        charge_value(value, &mut remaining)?;
    }
    Ok(())
}

fn charge_value(value: &OwnedValue, remaining: &mut usize) -> anyhow::Result<()> {
    fn charge(remaining: &mut usize, amount: usize) -> anyhow::Result<()> {
        *remaining = remaining
            .checked_sub(amount)
            .ok_or_else(|| anyhow::anyhow!("detached value exceeds configured byte limit"))?;
        Ok(())
    }
    charge(remaining, std::mem::size_of::<OwnedValue>())?;
    match value {
        OwnedValue::String(value) => charge(remaining, value.len()),
        OwnedValue::Bytes(value) => charge(remaining, value.len()),
        OwnedValue::List(values) | OwnedValue::Tuple(values) => {
            for value in values {
                charge_value(value, remaining)?;
            }
            Ok(())
        }
        OwnedValue::Record(values) | OwnedValue::Struct { fields: values, .. } => {
            for (name, value) in values {
                charge(remaining, name.len())?;
                charge_value(value, remaining)?;
            }
            if let OwnedValue::Struct { type_name, .. } = value {
                charge(remaining, type_name.len())?;
            }
            Ok(())
        }
        OwnedValue::Variant {
            type_name,
            case,
            fields,
        } => {
            charge(remaining, type_name.len() + case.len())?;
            for field in fields {
                charge_value(field, remaining)?;
            }
            Ok(())
        }
        OwnedValue::Json(value) => {
            charge(remaining, serde_json_size(value))?;
            Ok(())
        }
        OwnedValue::Unit
        | OwnedValue::Null
        | OwnedValue::Bool(_)
        | OwnedValue::I32(_)
        | OwnedValue::I64(_)
        | OwnedValue::F64(_)
        | OwnedValue::Range { .. } => Ok(()),
    }
}

fn serde_json_size(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => 8,
        serde_json::Value::String(value) => value.len(),
        serde_json::Value::Array(values) => values.iter().map(serde_json_size).sum(),
        serde_json::Value::Object(values) => values
            .iter()
            .map(|(key, value)| key.len() + serde_json_size(value))
            .sum(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADD_COMPONENT: &str = r#"
        (component
            (core module $m
                (func (export "add") (param i32 i32) (result i32)
                    local.get 0
                    local.get 1
                    i32.add))
            (core instance $i (instantiate $m))
            (func (export "add")
                (param "left" s32)
                (param "right" s32)
                (result s32)
                (canon lift (core func $i "add"))))
    "#;

    const COMPOSITE_TYPES_COMPONENT: &str = r#"
        (component
            (type $bytes (list u8))
            (export "bytes" (type $bytes))
            (type $pair (tuple s32 string))
            (export "pair" (type $pair))
            (type $maybe (option s64))
            (export "maybe" (type $maybe))
            (type $outcome (result bool (error string)))
            (export "outcome" (type $outcome))
            (type $details (record
                (field "display-name" string)
                (field "enabled" bool)))
            (export "details" (type $details))
            (type $state (enum "ready" "not-ready"))
            (export "state" (type $state))
            (type $choice (variant
                (case "nothing")
                (case "number" s32)))
            (export "choice" (type $choice)))
    "#;

    #[test]
    fn discovers_and_dynamically_calls_unknown_function() {
        let component = WasmComponent::compile_bytes(
            "math",
            Version::new(1, 0, 0),
            ADD_COMPONENT.as_bytes(),
            WasmCompileOptions::default(),
        )
        .unwrap();
        let function = &component.descriptor().functions[0];
        assert_eq!(function.name, "add");
        assert_eq!(function.parameters[0].ty, TypeDescriptor::I32);
        assert!(component.raw_imports().is_empty());

        let mut first = component.instantiate(WasmLimits::default()).unwrap();
        let mut second = component.instantiate(WasmLimits::default()).unwrap();
        assert_eq!(
            first
                .call("add", &[OwnedValue::I32(20), OwnedValue::I32(22)])
                .unwrap(),
            OwnedValue::I32(42)
        );
        assert_eq!(
            second
                .call("add", &[OwnedValue::I32(1), OwnedValue::I32(2)])
                .unwrap(),
            OwnedValue::I32(3)
        );
    }

    #[test]
    fn normalizes_names_and_rejects_collisions() {
        assert_eq!(normalize_wit_name("is-match").unwrap(), "is_match");
        let component = r#"
            (component
                (instance $first)
                (instance $second)
                (export "one:pkg/api@1.0.0" (instance $first))
                (export "two:pkg/api@1.0.0" (instance $second)))
        "#;
        let error = WasmComponent::compile_bytes(
            "collision",
            Version::new(1, 0, 0),
            component.as_bytes(),
            WasmCompileOptions::default(),
        )
        .err()
        .unwrap();
        let error = format!("{error:#}");
        assert!(error.contains("both normalize"), "{error}");
    }

    #[test]
    fn rejects_unsupported_unsigned_types() {
        let component = r#"
            (component
                (core module $m
                    (func (export "value") (result i32) i32.const 1))
                (core instance $i (instantiate $m))
                (func (export "value") (result u32)
                    (canon lift (core func $i "value"))))
        "#;
        let error = WasmComponent::compile_bytes(
            "unsigned",
            Version::new(1, 0, 0),
            component.as_bytes(),
            WasmCompileOptions::default(),
        )
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("unsupported WIT type `u32`"), "{error}");
    }

    #[test]
    fn maps_all_version_one_composite_type_shapes() {
        let component = WasmComponent::compile_bytes(
            "shapes",
            Version::new(1, 0, 0),
            COMPOSITE_TYPES_COMPONENT.as_bytes(),
            WasmCompileOptions::default(),
        )
        .unwrap();
        let definitions = &component.descriptor().types;
        assert_eq!(definitions.len(), 7);
        let details = definitions
            .iter()
            .find(|definition| definition.name == "shapes_details")
            .unwrap();
        let TypeDefinitionKind::Record(fields) = &details.kind else {
            panic!("expected record definition");
        };
        assert_eq!(fields[0].name, "display_name");
        let state = definitions
            .iter()
            .find(|definition| definition.name == "shapes_state")
            .unwrap();
        assert_eq!(
            state.kind,
            TypeDefinitionKind::Enum(vec!["ready".to_string(), "not_ready".to_string()])
        );
        let choice = definitions
            .iter()
            .find(|definition| definition.name == "shapes_choice")
            .unwrap();
        assert!(matches!(choice.kind, TypeDefinitionKind::Variant(_)));
    }

    #[test]
    fn round_trips_all_supported_dynamic_value_shapes() {
        let component = WasmComponent::compile_bytes(
            "shapes",
            Version::new(1, 0, 0),
            COMPOSITE_TYPES_COMPONENT.as_bytes(),
            WasmCompileOptions::default(),
        )
        .unwrap();
        let export_type = |name: &str| {
            let (item, _) = component.inner.component.get_export(None, name).unwrap();
            let ComponentItem::Type(ty) = item else {
                panic!("expected type export");
            };
            ty
        };
        let round_trip = |name: &str, value: OwnedValue| {
            let ty = export_type(name);
            let component_value = owned_to_component(&value, &ty).unwrap();
            component_to_owned(component_value, &ty).unwrap()
        };

        assert_eq!(
            round_trip("bytes", OwnedValue::Bytes(vec![0, 127, 255])),
            OwnedValue::Bytes(vec![0, 127, 255])
        );
        assert_eq!(
            round_trip(
                "pair",
                OwnedValue::Tuple(vec![
                    OwnedValue::I32(7),
                    OwnedValue::String("seven".to_string())
                ])
            ),
            OwnedValue::Tuple(vec![
                OwnedValue::I32(7),
                OwnedValue::String("seven".to_string())
            ])
        );
        let some = OwnedValue::Variant {
            type_name: "Option".to_string(),
            case: "Some".to_string(),
            fields: vec![OwnedValue::I64(9)],
        };
        assert_eq!(round_trip("maybe", some.clone()), some);
        let error = OwnedValue::Variant {
            type_name: "Result".to_string(),
            case: "Err".to_string(),
            fields: vec![OwnedValue::String("bad".to_string())],
        };
        assert_eq!(round_trip("outcome", error.clone()), error);
        let details = OwnedValue::Record(BTreeMap::from([
            (
                "display_name".to_string(),
                OwnedValue::String("Husk".to_string()),
            ),
            ("enabled".to_string(), OwnedValue::Bool(true)),
        ]));
        assert_eq!(round_trip("details", details.clone()), details);
        assert_eq!(
            round_trip(
                "state",
                OwnedValue::Variant {
                    type_name: "shapes_state".to_string(),
                    case: "not_ready".to_string(),
                    fields: Vec::new(),
                }
            ),
            OwnedValue::Variant {
                type_name: "WitEnum".to_string(),
                case: "not_ready".to_string(),
                fields: Vec::new(),
            }
        );
        assert_eq!(
            round_trip(
                "choice",
                OwnedValue::Variant {
                    type_name: "shapes_choice".to_string(),
                    case: "number".to_string(),
                    fields: vec![OwnedValue::I32(42)],
                }
            ),
            OwnedValue::Variant {
                type_name: "WitVariant".to_string(),
                case: "number".to_string(),
                fields: vec![OwnedValue::I32(42)],
            }
        );

        let bytes = export_type("bytes");
        let Type::List(list) = bytes else {
            panic!("expected list");
        };
        assert!(
            owned_to_component(&OwnedValue::I32(256), &list.ty())
                .unwrap_err()
                .to_string()
                .contains("outside u8 range")
        );
    }

    #[test]
    fn fuel_stops_and_poisons_runaway_guest() {
        let component = r#"
            (component
                (core module $m
                    (func (export "spin")
                        (loop $again
                            br $again)))
                (core instance $i (instantiate $m))
                (func (export "spin")
                    (canon lift (core func $i "spin"))))
        "#;
        let component = WasmComponent::compile_bytes(
            "runaway",
            Version::new(1, 0, 0),
            component.as_bytes(),
            WasmCompileOptions::default(),
        )
        .unwrap();
        let mut instance = component
            .instantiate(WasmLimits {
                fuel_per_call: 1_000,
                ..WasmLimits::default()
            })
            .unwrap();
        let error = instance.call("spin", &[]).unwrap_err().to_string();
        assert!(error.contains("trapped or failed"), "{error}");
        assert!(instance.is_poisoned());
        assert!(
            instance
                .call("spin", &[])
                .unwrap_err()
                .to_string()
                .contains("poisoned")
        );
    }

    #[test]
    fn initial_memory_is_limited() {
        let component = r#"
            (component
                (core module $m
                    (memory 2))
                (core instance (instantiate $m)))
        "#;
        let component = WasmComponent::compile_bytes(
            "memory",
            Version::new(1, 0, 0),
            component.as_bytes(),
            WasmCompileOptions::default(),
        )
        .unwrap();
        let error = component
            .instantiate(WasmLimits {
                max_memory_bytes: 64 * 1024,
                ..WasmLimits::default()
            })
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("instantiate extension"), "{error}");
    }

    #[test]
    fn imports_are_capability_checked_and_never_implicitly_linked() {
        let component = r#"
            (component
                (import "wasi:filesystem/types@0.2.0"
                    (func $filesystem (param "descriptor" s32))))
        "#;
        let error = WasmComponent::compile_bytes(
            "files",
            Version::new(1, 0, 0),
            component.as_bytes(),
            WasmCompileOptions::default(),
        )
        .err()
        .unwrap();
        assert!(
            format!("{error:#}").contains("missing from its manifest"),
            "{error:#}"
        );

        let filesystem = Capability::new("filesystem").unwrap();
        let component = WasmComponent::compile_bytes(
            "files",
            Version::new(1, 0, 0),
            component.as_bytes(),
            WasmCompileOptions {
                requested_capabilities: BTreeSet::from([filesystem.clone()]),
                granted_capabilities: BTreeSet::from([filesystem]),
                ..WasmCompileOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            component
                .actual_imports()
                .iter()
                .map(Capability::as_str)
                .collect::<Vec<_>>(),
            vec!["filesystem"]
        );
        let error = component.instantiate(WasmLimits::default()).err().unwrap();
        assert!(
            format!("{error:#}").contains("no providers are linked"),
            "{error:#}"
        );
    }
}
