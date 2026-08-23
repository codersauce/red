//! Red-specific Husk VM host, request translation, snapshots, timers, and reload staging.
//!
//! [`Runtime`] wraps the Red-agnostic `husk_runtime::Vm` with a host that translates Husk calls
//! into [`PluginRequest`] values. The editor consumes those
//! requests and remains the sole mutator of buffers and UI state. Snapshot requests read
//! editor-produced JSON captured at defined service points rather than borrowing editor
//! state from the VM.
//!
//! Reload staging records host effects until the replacement has activated and the old
//! plugin has torn down successfully. Committing reorders replacement effects ahead of
//! teardown where required; rollback discards every staged request, log, and timer.
//! Each callback also runs under an instruction budget so a plugin cannot monopolize the
//! editor loop indefinitely.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

use husk_ast::{
    EnumVariantFields, File as HuskFile, ItemKind, StructField, TypeExpr, TypeExprKind,
};
use husk_runtime::{
    AnnotatedFunction, Callback, CompileOptions, CompiledProgram, Host, PackageLimits,
    ResolvedPackage, SemanticProfile, Value,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    assets::RuntimeAssetKind,
    config::{Config, PluginPermissions},
    dispatcher::Dispatcher,
    editor::{
        Action, ComposerCallback, ComposerCallbackKind, PickerCallback, PickerCallbackKind,
        PluginRequest, PluginResponse, ACTION_DISPATCHER,
    },
    log,
    plugin::process::{ProcessManager, ProcessSpawnOptions},
    ui::{PickerItem, PickerOptions},
};

use super::{
    Decoration, GutterSign, OverlayConfig, PanelConfig, PanelRow, TextPanelBlock, TextPanelStatus,
    TreePanelModel, WindowBarConfig, WindowBarSegment,
};
use super::{WorkspaceConfig, WorkspaceModel};

#[derive(Debug)]
struct PendingTimeout {
    id: String,
    expires_at: Instant,
}

const PLUGIN_INSTRUCTION_BUDGET: usize = 100_000;
static NEXT_PLUGIN_VM_GENERATION: AtomicU64 = AtomicU64::new(1);
static GIT_CORE_PROGRAM: OnceLock<Result<CompiledProgram, String>> = OnceLock::new();
static NEOTREE_CORE_PROGRAM: OnceLock<Result<CompiledProgram, String>> = OnceLock::new();

/// User-facing metadata attached to a registered Red plugin command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommandMetadata {
    pub title: Option<String>,
    pub category: Option<String>,
    pub description: Option<String>,
    pub aliases: Vec<String>,
    pub visible: bool,
    pub scope: CommandScope,
    /// Opt in to receiving one `CommandInvocation` callback argument.
    pub arguments: bool,
    /// Optional literal completion choices, indexed by argument position.
    pub completions: Vec<Vec<String>>,
}

impl Default for CommandMetadata {
    fn default() -> Self {
        Self {
            title: None,
            category: None,
            description: None,
            aliases: Vec::new(),
            visible: true,
            scope: CommandScope::Editor,
            arguments: false,
            completions: Vec::new(),
        }
    }
}

pub(crate) fn validate_command_arguments(
    arguments: bool,
    completions: &[Vec<String>],
) -> anyhow::Result<()> {
    anyhow::ensure!(
        arguments || completions.is_empty(),
        "command completions require arguments = true"
    );
    anyhow::ensure!(
        completions
            .iter()
            .flatten()
            .all(|value| !value.is_empty() && !value.chars().any(char::is_whitespace)),
        "command completion choices must be nonempty single arguments"
    );
    Ok(())
}

/// Surfaces from which a registered plugin command may be invoked by keymap.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandScope {
    /// Only dispatch the command through the keymap owned by the active editor surface.
    #[default]
    Editor,
    /// Allow configured normal-mode bindings to invoke the command from focused panels too.
    Global,
}

/// Opaque identifier for a one-shot request issued by a Red plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId(i64);

impl RequestId {
    #[must_use]
    pub const fn from_raw(value: i64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }
}

/// Opaque host-generated identity for a callback-scoped picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PickerHandle(i32);

impl PickerHandle {
    #[must_use]
    pub const fn from_raw(value: i32) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> i32 {
        self.0
    }
}

/// Opaque host-generated identity for a callback-scoped composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ComposerHandle(i32);

impl ComposerHandle {
    #[must_use]
    pub const fn from_raw(value: i32) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> i32 {
        self.0
    }
}

pub(super) const RED_HOST_DECLARATIONS: &str = r#"
type Json = JsValue;
struct EmptyEvent {}
struct CommandMetadata {
    title: String,
    category: String,
    description: String,
    aliases: [String],
    visible: bool,
    scope: String,
    arguments: bool,
    completions: [[String]],
}
struct CommandInvocation {
    name: String,
    args: [String],
    raw_args: String,
}
struct Position {
    line: i32,
    character: i32,
}
struct Range {
    start: Position,
    end: Position,
}
struct TimerEvent {
    timer_id: String,
}
struct Style {
    fg: Json,
    bg: Json,
    bold: bool,
    italic: bool,
}
enum ProcessEvent {
    Stdout { plugin_name: String, process_id: String, line: String },
    Stderr { plugin_name: String, process_id: String, line: String },
    Exit { plugin_name: String, process_id: String, code: Option<i32> },
    Error { plugin_name: String, process_id: String, message: String },
}
struct PanelSegment {
    text: String,
    style: Style,
}
struct PickerItem {
    id: String,
    label: String,
    data: Json,
}
struct PickerCancelled {}
struct PickerActionEvent {
    action: String,
    item: Json,
    query: String,
}
struct PickerHandlers {
    selected: fn(PickerItem),
    cancelled: fn(PickerCancelled),
    changed: fn(PickerItem),
    query: fn(String),
    action: fn(PickerActionEvent),
}
struct ComposerCancelled {}
struct ComposerHandlers {
    submitted: fn(String),
    cancelled: fn(ComposerCancelled),
}
struct RuntimeAssetEntry {
    file: String,
    name: String,
    source: String,
    shadows: [String],
}
extern "red" {
    mod global red {
        fn add_command();
        fn on();
        fn execute() -> JsValue;
        fn request() -> JsValue;
        fn viewport_layout() -> JsValue;
        fn windows() -> JsValue;
        fn editor_info() -> JsValue;
        fn log();
        fn state_bool() -> bool;
        fn state_set();
        fn state_patch();
        fn state() -> JsValue;
        fn push() -> JsValue;
        fn extend() -> JsValue;
        fn unshift() -> JsValue;
        fn contains() -> bool;
        fn remove() -> JsValue;
        fn reverse() -> JsValue;
        fn join() -> String;
        fn range() -> [i32];
        fn len() -> i32;
        fn int() -> i32;
        fn bool() -> bool;
        fn string() -> String;
        fn text_field() -> String;
        fn utf8_byte_to_char_index() -> i32;
        fn blend_color() -> String;
        fn is_light_color() -> bool;
        fn char_at() -> String;
        fn trim() -> String;
        fn lower() -> String;
        fn split() -> [String];
        fn starts_with() -> bool;
        fn ends_with() -> bool;
        fn replace_all() -> String;
        fn trim_line_end() -> String;
        fn slice() -> String;
        fn is_whitespace() -> bool;
        fn char() -> String;
        fn null() -> JsValue;
        fn parse_json() -> JsValue;
        fn document_symbol_chain() -> JsValue;
        fn git_core() -> JsValue;
        fn neotree_core() -> JsValue;
    }
}
"#;

static RED_HOST_AST: OnceLock<husk_ast::File> = OnceLock::new();
static RED_HOST_PAYLOAD_SCHEMA: OnceLock<PluginPayloadSchema> = OnceLock::new();

struct RedHost {
    dispatcher: Arc<Dispatcher<PluginRequest, PluginResponse>>,
    process_manager: ProcessManager,
    pending_timeouts: Vec<PendingTimeout>,
    next_timeout_at: Option<Instant>,
    snapshots: HashMap<String, Value>,
    policy: RedPluginPolicy,
    staged_policy: Option<RedPluginPolicy>,
    teardown_policy: Option<RedPluginPolicy>,
    policy_phase: PolicyPhase,
    staged_effects: Option<Vec<StagedHostEffect>>,
    staged_replacement_start: Option<usize>,
    staged_teardown_start: Option<usize>,
    git_core: Option<husk_runtime::Vm>,
    neotree_core: Option<husk_runtime::Vm>,
}

#[derive(Debug, Clone)]
struct RedCommand {
    callback: Callback,
    metadata: CommandMetadata,
}

#[derive(Debug, Clone)]
enum PayloadTypeDefinition {
    Record(Vec<(String, TypeExpr)>),
    Enum(Vec<PayloadVariant>),
}

#[derive(Debug, Clone)]
struct PayloadVariant {
    name: String,
    fields: PayloadVariantFields,
}

#[derive(Debug, Clone)]
enum PayloadVariantFields {
    Unit,
    Tuple(Vec<TypeExpr>),
    Record(Vec<(String, TypeExpr)>),
}

#[derive(Debug, Clone, Default)]
struct PluginPayloadSchema {
    definitions: HashMap<String, PayloadTypeDefinition>,
    callback_parameters: HashMap<String, Vec<TypeExpr>>,
}

pub(super) struct PreparedStartupPlugin {
    program: CompiledProgram,
    payload_schema: PluginPayloadSchema,
}

impl PluginPayloadSchema {
    fn for_source(source: &HuskFile) -> Self {
        let mut schema = Self::default();
        schema.add_module(source, &[]);
        schema
    }

    fn for_package(package: &ResolvedPackage) -> Self {
        let mut schema = Self::default();
        for module in &package.modules {
            schema.add_module(&module.syntax, &module.module_path);
        }
        schema
    }

    fn definition(&self, name: &str) -> Option<&PayloadTypeDefinition> {
        self.definitions
            .get(name)
            .or_else(|| red_host_payload_schema().definitions.get(name))
    }

    fn add_module(&mut self, syntax: &HuskFile, module_path: &[String]) {
        let qualify = |name: &str| {
            if module_path.is_empty() {
                name.to_string()
            } else {
                format!("{}::{name}", module_path.join("::"))
            }
        };
        for item in &syntax.items {
            match &item.kind {
                ItemKind::Struct { name, fields, .. } => {
                    self.definitions.insert(
                        qualify(&name.name),
                        PayloadTypeDefinition::Record(payload_record_fields(fields)),
                    );
                }
                ItemKind::Enum { name, variants, .. } => {
                    let variants = variants
                        .iter()
                        .map(|variant| PayloadVariant {
                            name: variant.name.name.clone(),
                            fields: match &variant.fields {
                                EnumVariantFields::Unit => PayloadVariantFields::Unit,
                                EnumVariantFields::Tuple(fields) => {
                                    PayloadVariantFields::Tuple(fields.clone())
                                }
                                EnumVariantFields::Struct(fields) => {
                                    PayloadVariantFields::Record(payload_record_fields(fields))
                                }
                            },
                        })
                        .collect();
                    self.definitions
                        .insert(qualify(&name.name), PayloadTypeDefinition::Enum(variants));
                }
                ItemKind::Fn { name, params, .. } => {
                    self.callback_parameters.insert(
                        qualify(&name.name),
                        params
                            .iter()
                            .map(|parameter| parameter.ty.clone())
                            .collect(),
                    );
                }
                _ => {}
            }
        }
    }

    fn callback_argument(
        &self,
        callback: &Callback,
        index: usize,
        payload: &serde_json::Value,
    ) -> anyhow::Result<Value> {
        let Some(parameter) = self
            .callback_parameters
            .get(callback.function())
            .and_then(|parameters| parameters.get(index))
        else {
            return Ok(Value::from_json(payload.clone()));
        };
        let module = callback
            .function()
            .rsplit_once("::")
            .map_or("", |(module, _)| module);
        self.decode(parameter, module, payload, callback.function(), 0)
    }

    fn named_record(&self, name: &str, payload: &serde_json::Value) -> anyhow::Result<Value> {
        let Some(PayloadTypeDefinition::Record(fields)) = self.definition(name) else {
            return Ok(Value::from_json(payload.clone()));
        };
        self.decode_record(name, fields, "", payload, name, 0)
    }

    fn decode(
        &self,
        expected: &TypeExpr,
        module: &str,
        payload: &serde_json::Value,
        path: &str,
        depth: usize,
    ) -> anyhow::Result<Value> {
        anyhow::ensure!(
            depth < 64,
            "host payload nesting exceeds the limit at `{path}`"
        );
        match &expected.kind {
            TypeExprKind::Generic { name, args } if name.name == "Option" && args.len() == 1 => {
                if payload.is_null() {
                    Ok(option_payload(None))
                } else {
                    Ok(option_payload(Some(self.decode(
                        &args[0],
                        module,
                        payload,
                        path,
                        depth + 1,
                    )?)))
                }
            }
            TypeExprKind::Array(element) => {
                let Some(values) = payload.as_array() else {
                    return Ok(Value::from_json(payload.clone()));
                };
                Ok(Value::Array(Arc::new(
                    values
                        .iter()
                        .enumerate()
                        .map(|(index, value)| {
                            self.decode(
                                element,
                                module,
                                value,
                                &format!("{path}[{index}]"),
                                depth + 1,
                            )
                        })
                        .collect::<anyhow::Result<Vec<_>>>()?,
                )))
            }
            TypeExprKind::Tuple(elements) => {
                let Some(values) = payload.as_array() else {
                    return Ok(Value::from_json(payload.clone()));
                };
                if values.len() != elements.len() {
                    return Ok(Value::from_json(payload.clone()));
                }
                Ok(Value::Tuple(Arc::new(
                    elements
                        .iter()
                        .zip(values)
                        .enumerate()
                        .map(|(index, (element, value))| {
                            self.decode(
                                element,
                                module,
                                value,
                                &format!("{path}[{index}]"),
                                depth + 1,
                            )
                        })
                        .collect::<anyhow::Result<Vec<_>>>()?,
                )))
            }
            TypeExprKind::Named(name) => {
                let qualified = if module.is_empty() {
                    name.name.clone()
                } else {
                    format!("{module}::{}", name.name)
                };
                let (type_name, definition) = self
                    .definition(&qualified)
                    .map(|definition| (qualified.as_str(), definition))
                    .or_else(|| {
                        self.definition(&name.name)
                            .map(|definition| (name.name.as_str(), definition))
                    })
                    .map_or((name.name.as_str(), None), |(name, definition)| {
                        (name, Some(definition))
                    });
                match definition {
                    Some(PayloadTypeDefinition::Record(fields)) => {
                        self.decode_record(type_name, fields, module, payload, path, depth + 1)
                    }
                    Some(PayloadTypeDefinition::Enum(variants)) => {
                        self.decode_enum(type_name, variants, module, payload, path, depth + 1)
                    }
                    None => Ok(Value::from_json(payload.clone())),
                }
            }
            _ => Ok(Value::from_json(payload.clone())),
        }
    }

    fn decode_record(
        &self,
        type_name: &str,
        declared: &[(String, TypeExpr)],
        module: &str,
        payload: &serde_json::Value,
        path: &str,
        depth: usize,
    ) -> anyhow::Result<Value> {
        let Some(object) = payload.as_object() else {
            return Ok(Value::from_json(payload.clone()));
        };
        let mut fields = object
            .iter()
            .map(|(name, value)| (name.clone(), Value::from_json(value.clone())))
            .collect::<BTreeMap<_, _>>();
        for (name, expected) in declared {
            let field_path = format!("{path}.{name}");
            if let Some(value) = object.get(name) {
                fields.insert(
                    name.clone(),
                    self.decode(expected, module, value, &field_path, depth + 1)?,
                );
            } else if is_option_type(expected) {
                fields.insert(name.clone(), option_payload(None));
            }
        }
        Ok(Value::Struct {
            type_name: type_name.to_string(),
            fields: Arc::new(fields),
        })
    }

    fn decode_enum(
        &self,
        type_name: &str,
        variants: &[PayloadVariant],
        module: &str,
        payload: &serde_json::Value,
        path: &str,
        depth: usize,
    ) -> anyhow::Result<Value> {
        let tag = payload.as_str().or_else(|| {
            payload.as_object().and_then(|object| {
                ["$case", "type", "session_update", "kind"]
                    .iter()
                    .find_map(|name| object.get(*name).and_then(serde_json::Value::as_str))
            })
        });
        let variant = tag.and_then(|tag| {
            variants
                .iter()
                .find(|variant| variant.name == tag || payload_variant_name(&variant.name) == tag)
        });
        let Some(variant) =
            variant.or_else(|| variants.iter().find(|variant| variant.name == "Unknown"))
        else {
            anyhow::bail!(
                "unknown host event variant `{}` for `{type_name}` at `{path}`",
                tag.unwrap_or("<missing>")
            );
        };
        let fields = match &variant.fields {
            PayloadVariantFields::Unit => Vec::new(),
            PayloadVariantFields::Tuple(types) => {
                if let Some(values) = payload.get("$fields").and_then(serde_json::Value::as_array) {
                    anyhow::ensure!(
                        values.len() == types.len(),
                        "host variant `{type_name}::{}` has the wrong tuple arity at `{path}`",
                        variant.name
                    );
                    types
                        .iter()
                        .zip(values)
                        .enumerate()
                        .map(|(index, (expected, value))| {
                            self.decode(
                                expected,
                                module,
                                value,
                                &format!("{path}[{index}]"),
                                depth + 1,
                            )
                        })
                        .collect::<anyhow::Result<Vec<_>>>()?
                } else if types.len() == 1 {
                    vec![self.decode(&types[0], module, payload, path, depth + 1)?]
                } else {
                    anyhow::bail!(
                        "host variant `{type_name}::{}` has no tuple payload at `{path}`",
                        variant.name
                    );
                }
            }
            PayloadVariantFields::Record(fields) => vec![self.decode_record(
                &format!("{type_name}::{}", variant.name),
                fields,
                module,
                payload,
                path,
                depth + 1,
            )?],
        };
        Ok(Value::Variant {
            type_name: type_name.to_string(),
            case: variant.name.clone(),
            fields: Arc::new(fields),
        })
    }
}

fn payload_record_fields(fields: &[StructField]) -> Vec<(String, TypeExpr)> {
    fields
        .iter()
        .map(|field| (field.name.name.clone(), field.ty.clone()))
        .collect()
}

fn is_option_type(ty: &TypeExpr) -> bool {
    matches!(
        &ty.kind,
        TypeExprKind::Generic { name, args } if name.name == "Option" && args.len() == 1
    )
}

fn option_payload(value: Option<Value>) -> Value {
    Value::Variant {
        type_name: "Option".to_string(),
        case: if value.is_some() { "Some" } else { "None" }.to_string(),
        fields: Arc::new(value.into_iter().collect()),
    }
}

fn payload_variant_name(name: &str) -> String {
    let mut result = String::new();
    for (index, character) in name.chars().enumerate() {
        if character.is_ascii_uppercase() {
            if index > 0 {
                result.push('_');
            }
            result.extend(character.to_lowercase());
        } else {
            result.push(character);
        }
    }
    result
}

#[derive(Debug, Clone)]
struct RedPluginPolicy {
    commands: HashMap<String, RedCommand>,
    event_listeners: HashMap<String, Arc<Vec<Callback>>>,
    pending_requests: HashMap<RequestId, Callback>,
    picker_handlers: HashMap<PickerHandle, PickerRegistration>,
    composer_handlers: HashMap<ComposerHandle, ComposerRegistration>,
    plugin_states: HashMap<String, HashMap<String, Value>>,
    typed_states: HashMap<String, Value>,
    state_initializers: HashMap<String, Callback>,
    state_record_types: HashMap<String, String>,
    payload_schemas: HashMap<String, Arc<PluginPayloadSchema>>,
    lifecycle_callbacks: HashMap<String, HashMap<String, Callback>>,
    config_bindings: HashMap<String, HashSet<Option<String>>>,
    next_request_id: i64,
    next_picker_handle: i32,
    next_composer_handle: i32,
}

impl Default for RedPluginPolicy {
    fn default() -> Self {
        Self {
            commands: HashMap::new(),
            event_listeners: HashMap::new(),
            pending_requests: HashMap::new(),
            picker_handlers: HashMap::new(),
            composer_handlers: HashMap::new(),
            plugin_states: HashMap::new(),
            typed_states: HashMap::new(),
            state_initializers: HashMap::new(),
            state_record_types: HashMap::new(),
            payload_schemas: HashMap::new(),
            lifecycle_callbacks: HashMap::new(),
            config_bindings: HashMap::new(),
            next_request_id: 1,
            next_picker_handle: 1,
            next_composer_handle: 1,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct PickerHandlers {
    selected: Option<Callback>,
    cancelled: Option<Callback>,
    changed: Option<Callback>,
    query: Option<Callback>,
    action: Option<Callback>,
}

impl PickerHandlers {
    fn callback(&self, kind: PickerCallbackKind) -> Option<&Callback> {
        match kind {
            PickerCallbackKind::Selected => self.selected.as_ref(),
            PickerCallbackKind::Cancelled => self.cancelled.as_ref(),
            PickerCallbackKind::Changed => self.changed.as_ref(),
            PickerCallbackKind::Query => self.query.as_ref(),
            PickerCallbackKind::Action => self.action.as_ref(),
        }
    }

    fn is_empty(&self) -> bool {
        self.selected.is_none()
            && self.cancelled.is_none()
            && self.changed.is_none()
            && self.query.is_none()
            && self.action.is_none()
    }
}

#[derive(Debug, Clone)]
struct PickerRegistration {
    plugin: String,
    handlers: PickerHandlers,
}

#[derive(Debug, Clone, Default)]
struct ComposerHandlers {
    submitted: Option<Callback>,
    cancelled: Option<Callback>,
}

impl ComposerHandlers {
    fn callback(&self, kind: ComposerCallbackKind) -> Option<&Callback> {
        match kind {
            ComposerCallbackKind::Submitted => self.submitted.as_ref(),
            ComposerCallbackKind::Cancelled => self.cancelled.as_ref(),
        }
    }

    fn is_empty(&self) -> bool {
        self.submitted.is_none() && self.cancelled.is_none()
    }
}

#[derive(Debug, Clone)]
struct ComposerRegistration {
    plugin: String,
    handlers: ComposerHandlers,
}

impl RedPluginPolicy {
    fn remove_plugin(&mut self, plugin: &str) {
        self.commands
            .retain(|_, command| command.callback.plugin() != plugin);
        self.event_listeners.retain(|_, callbacks| {
            Arc::make_mut(callbacks).retain(|callback| callback.plugin() != plugin);
            !callbacks.is_empty()
        });
        self.pending_requests
            .retain(|_, callback| callback.plugin() != plugin);
        self.picker_handlers
            .retain(|_, registration| registration.plugin != plugin);
        self.composer_handlers
            .retain(|_, registration| registration.plugin != plugin);
        self.plugin_states.remove(plugin);
        self.typed_states.remove(plugin);
        self.state_initializers.remove(plugin);
        self.state_record_types.remove(plugin);
        self.payload_schemas.remove(plugin);
        self.lifecycle_callbacks.remove(plugin);
        self.config_bindings.remove(plugin);
    }

    fn allocate_request_id(&mut self) -> RequestId {
        loop {
            let request_id = RequestId::from_raw(self.next_request_id);
            self.next_request_id = if self.next_request_id == i64::MAX {
                1
            } else {
                self.next_request_id + 1
            };
            if !self.pending_requests.contains_key(&request_id) {
                return request_id;
            }
        }
    }

    fn allocate_picker_handle(&mut self) -> PickerHandle {
        loop {
            let handle = PickerHandle::from_raw(self.next_picker_handle);
            self.next_picker_handle = if self.next_picker_handle == i32::MAX {
                1
            } else {
                self.next_picker_handle + 1
            };
            if !self.picker_handlers.contains_key(&handle) {
                return handle;
            }
        }
    }

    fn allocate_composer_handle(&mut self) -> ComposerHandle {
        loop {
            let handle = ComposerHandle::from_raw(self.next_composer_handle);
            self.next_composer_handle = if self.next_composer_handle == i32::MAX {
                1
            } else {
                self.next_composer_handle + 1
            };
            if !self.composer_handlers.contains_key(&handle) {
                return handle;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum PolicyPhase {
    #[default]
    Active,
    Replacement,
    Teardown,
}

enum StagedHostEffect {
    Request(Box<PluginRequest>),
    Log(String),
    ScheduleTimeout { id: String, delay_ms: u64 },
    CancelTimeout(String),
}

impl RedHost {
    #[cfg(test)]
    fn new(process_permissions: HashMap<String, PluginPermissions>) -> Self {
        Self::with_dispatcher(process_permissions, Arc::new(Dispatcher::new()))
    }

    fn with_dispatcher(
        process_permissions: HashMap<String, PluginPermissions>,
        dispatcher: Arc<Dispatcher<PluginRequest, PluginResponse>>,
    ) -> Self {
        Self {
            dispatcher,
            process_manager: ProcessManager::new(process_permissions),
            pending_timeouts: Vec::new(),
            next_timeout_at: None,
            snapshots: HashMap::new(),
            policy: RedPluginPolicy::default(),
            staged_policy: None,
            teardown_policy: None,
            policy_phase: PolicyPhase::Active,
            staged_effects: None,
            staged_replacement_start: None,
            staged_teardown_start: None,
            git_core: None,
            neotree_core: None,
        }
    }

    fn set_snapshot(&mut self, name: impl Into<String>, value: serde_json::Value) {
        self.snapshots.insert(name.into(), Value::from_json(value));
    }

    fn update_viewport_cursor(&mut self, cursor: serde_json::Value) -> bool {
        let Some(Value::Object(viewport)) = self.snapshots.get_mut("viewport_layout") else {
            return false;
        };
        if !viewport.contains_key("cursor") {
            return false;
        }
        Arc::make_mut(viewport).insert("cursor".to_string(), Value::from_json(cursor));
        true
    }

    fn poll_process_events(&mut self) -> Vec<serde_json::Value> {
        self.process_manager
            .poll_events()
            .into_iter()
            .filter_map(|event| serde_json::to_value(event).ok())
            .collect()
    }

    fn begin_initial_activation(&mut self) {
        self.staged_policy = Some(self.policy.clone());
        self.teardown_policy = None;
        self.policy_phase = PolicyPhase::Teardown;
        self.staged_effects = Some(Vec::new());
        self.staged_replacement_start = None;
        self.staged_teardown_start = None;
    }

    fn begin_reload(&mut self) {
        self.begin_initial_activation();
        // State export runs against a cloned previous-policy snapshot, just as
        // the compatibility VM evaluates it on a cloned previous VM. This
        // keeps export-time state mutations transactional when export fails.
        self.teardown_policy = Some(self.policy.clone());
    }

    fn commit_reload(&mut self) {
        if let Some(policy) = self.staged_policy.take() {
            self.policy = policy;
        }
        self.teardown_policy = None;
        self.policy_phase = PolicyPhase::Active;
        let mut effects = self.staged_effects.take().unwrap_or_default();
        if let (Some(replacement), Some(teardown)) = (
            self.staged_replacement_start.take(),
            self.staged_teardown_start.take(),
        ) {
            if replacement <= teardown && teardown <= effects.len() {
                effects[replacement..].rotate_left(teardown - replacement);
            }
        }
        for effect in effects {
            match effect {
                StagedHostEffect::Request(request) => self.dispatcher.send_request(*request),
                StagedHostEffect::Log(message) => log!("[PLUGIN:HUSK] {}", message),
                StagedHostEffect::ScheduleTimeout { id, delay_ms } => {
                    self.schedule_timeout_with_id(id, delay_ms);
                }
                StagedHostEffect::CancelTimeout(id) => self.cancel_timeout(&id),
            }
        }
    }

    fn rollback_reload(&mut self) {
        self.staged_policy = None;
        self.teardown_policy = None;
        self.policy_phase = PolicyPhase::Active;
        self.staged_effects = None;
        self.staged_replacement_start = None;
        self.staged_teardown_start = None;
    }

    fn policy(&self) -> &RedPluginPolicy {
        match self.policy_phase {
            PolicyPhase::Active => &self.policy,
            PolicyPhase::Replacement => self.staged_policy.as_ref().unwrap_or(&self.policy),
            PolicyPhase::Teardown => self.teardown_policy.as_ref().unwrap_or(&self.policy),
        }
    }

    fn policy_mut(&mut self) -> &mut RedPluginPolicy {
        match self.policy_phase {
            PolicyPhase::Active => &mut self.policy,
            PolicyPhase::Replacement => self.staged_policy.as_mut().unwrap_or(&mut self.policy),
            PolicyPhase::Teardown => self.teardown_policy.as_mut().unwrap_or(&mut self.policy),
        }
    }

    fn remove_plugin(&mut self, plugin: &str) {
        self.policy.remove_plugin(plugin);
        if let Some(policy) = &mut self.staged_policy {
            policy.remove_plugin(plugin);
        }
        if let Some(policy) = &mut self.teardown_policy {
            policy.remove_plugin(plugin);
        }
    }

    fn clear_policy(&mut self) {
        self.policy = RedPluginPolicy::default();
        self.staged_policy = None;
        self.teardown_policy = None;
        self.policy_phase = PolicyPhase::Active;
    }

    fn send_request(&mut self, request: PluginRequest) {
        if let Some(effects) = &mut self.staged_effects {
            effects.push(StagedHostEffect::Request(Box::new(request)));
        } else {
            self.dispatcher.send_request(request);
        }
    }

    fn schedule_timeout(&mut self, delay_ms: u64) -> String {
        let id = Uuid::new_v4().to_string();
        if let Some(effects) = &mut self.staged_effects {
            effects.push(StagedHostEffect::ScheduleTimeout {
                id: id.clone(),
                delay_ms,
            });
        } else {
            self.schedule_timeout_with_id(id.clone(), delay_ms);
        }
        id
    }

    fn cancel_timeout(&mut self, timer_id: &str) {
        if let Some(effects) = &mut self.staged_effects {
            effects.push(StagedHostEffect::CancelTimeout(timer_id.to_string()));
        } else {
            self.cancel_timeout_now(timer_id);
        }
    }

    fn schedule_timeout_with_id(&mut self, id: String, delay_ms: u64) {
        let expires_at = Instant::now() + Duration::from_millis(delay_ms);
        self.next_timeout_at = Some(
            self.next_timeout_at
                .map_or(expires_at, |next| next.min(expires_at)),
        );
        self.pending_timeouts
            .push(PendingTimeout { id, expires_at });
    }

    fn cancel_timeout_now(&mut self, timer_id: &str) {
        let mut removed_earliest = false;
        self.pending_timeouts.retain(|timeout| {
            let keep = timeout.id != timer_id;
            removed_earliest |= !keep && Some(timeout.expires_at) == self.next_timeout_at;
            keep
        });
        if removed_earliest {
            self.next_timeout_at = self
                .pending_timeouts
                .iter()
                .map(|timeout| timeout.expires_at)
                .min();
        }
    }

    fn poll_timer_callbacks(&mut self) -> Vec<PluginRequest> {
        let now = Instant::now();
        if self.next_timeout_at.is_none_or(|next| next > now) {
            return Vec::new();
        }

        let mut requests = Vec::new();
        let mut next_timeout_at: Option<Instant> = None;
        self.pending_timeouts.retain(|timeout| {
            if timeout.expires_at <= now {
                requests.push(PluginRequest::TimeoutCallback {
                    timer_id: timeout.id.clone(),
                });
                false
            } else {
                next_timeout_at = Some(
                    next_timeout_at.map_or(timeout.expires_at, |next| next.min(timeout.expires_at)),
                );
                true
            }
        });
        self.next_timeout_at = next_timeout_at;
        requests
    }
}

impl RedHost {
    fn log(&mut self, message: &str) {
        if let Some(effects) = &mut self.staged_effects {
            effects.push(StagedHostEffect::Log(message.to_string()));
        } else {
            log!("[PLUGIN:HUSK] {}", message);
        }
    }

    fn begin_reload_replacement(&mut self, plugin: &str) {
        self.staged_replacement_start = self.staged_effects.as_ref().map(Vec::len);
        let staged = self
            .staged_policy
            .get_or_insert_with(|| self.policy.clone());
        let payload_schema = staged.payload_schemas.remove(plugin);
        staged.remove_plugin(plugin);
        if let Some(schema) = payload_schema {
            staged.payload_schemas.insert(plugin.to_string(), schema);
        }
        self.policy_phase = PolicyPhase::Replacement;
    }

    fn begin_reload_teardown(&mut self, _plugin: &str) {
        self.staged_teardown_start = self.staged_effects.as_ref().map(Vec::len);
        self.teardown_policy = Some(self.policy.clone());
        self.policy_phase = PolicyPhase::Teardown;
    }

    fn ensure_picker_owner(&self, plugin: &str, id: i32, action: &str) -> anyhow::Result<()> {
        let handle = PickerHandle::from_raw(id);
        if let Some(registration) = self.policy().picker_handlers.get(&handle) {
            anyhow::ensure!(
                registration.plugin == plugin,
                "`{action}` cannot mutate picker {id} owned by plugin `{}`",
                registration.plugin
            );
        }
        Ok(())
    }

    fn execute(&mut self, plugin: &str, action: &str, args: &[Value]) -> anyhow::Result<Value> {
        match action {
            "Print" => {
                let message = args.first().map(value_to_string).unwrap_or_default();
                self.send_request(PluginRequest::Action(Action::Print(message)));
            }
            "PrintWarning" => {
                let message = args.first().map(value_to_string).unwrap_or_default();
                self.send_request(PluginRequest::Action(Action::PrintWarning(message)));
            }
            "FilePicker" => {
                self.send_request(PluginRequest::Action(Action::FilePicker));
            }
            "ClearSearchHighlight" => {
                self.send_request(PluginRequest::Action(Action::ClearSearchHighlight));
            }
            "RefreshDiagnostics" => {
                self.send_request(PluginRequest::Action(Action::RefreshDiagnostics));
            }
            "Refresh" => {
                self.send_request(PluginRequest::Action(Action::Refresh));
            }
            "ShowDialog" => {
                self.send_request(PluginRequest::Action(Action::ShowDialog));
            }
            "CloseDialog" => {
                self.send_request(PluginRequest::Action(Action::CloseDialog));
            }
            "GoToDefinition" => {
                self.send_request(PluginRequest::Action(Action::GoToDefinition));
            }
            "Hover" => {
                self.send_request(PluginRequest::Action(Action::Hover));
            }
            "ShowLineDiagnostics" => {
                self.send_request(PluginRequest::Action(Action::ShowLineDiagnostics));
            }
            "ViewLogs" => {
                self.send_request(PluginRequest::Action(Action::ViewLogs));
            }
            "ListPlugins" => {
                self.send_request(PluginRequest::Action(Action::ListPlugins));
            }
            "PreviewTheme" => {
                let theme_name = args.first().map(value_to_string).unwrap_or_default();
                self.send_request(PluginRequest::Action(Action::PreviewTheme(theme_name)));
            }
            "SetTheme" => {
                let theme_name = args.first().map(value_to_string).unwrap_or_default();
                self.send_request(PluginRequest::Action(Action::SetTheme(theme_name)));
            }
            "AgentNewSession" => {
                let cwd = args
                    .first()
                    .and_then(Value::as_str)
                    .map_or_else(|| PathBuf::from("."), PathBuf::from);
                self.send_request(PluginRequest::AgentNewSession { cwd });
            }
            "AgentResumeSession" => {
                let cwd = args
                    .first()
                    .and_then(Value::as_str)
                    .map_or_else(|| PathBuf::from("."), PathBuf::from);
                let session_id = args
                    .get(1)
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("AgentResumeSession requires a session id"))?
                    .to_string();
                self.send_request(PluginRequest::AgentResumeSession { cwd, session_id });
            }
            "AgentPrompt" => {
                let session_id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("AgentPrompt requires a session id"))?
                    .to_string();
                let text = args.get(1).map(value_to_string).unwrap_or_default();
                self.send_request(PluginRequest::AgentPrompt { session_id, text });
            }
            "AgentPromptWithContext" => {
                let session_id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("AgentPromptWithContext requires a session id"))?
                    .to_string();
                let text = args.get(1).map(value_to_string).unwrap_or_default();
                let context = args
                    .get(2)
                    .map(value_to_json)
                    .unwrap_or(serde_json::Value::Null);
                let uri = context
                    .get("uri")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("red-buffer://active")
                    .to_string();
                let context = context
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.send_request(PluginRequest::AgentPromptWithContext {
                    session_id,
                    text,
                    uri,
                    context,
                });
            }
            "AgentCancel" => {
                let session_id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("AgentCancel requires a session id"))?
                    .to_string();
                self.send_request(PluginRequest::AgentCancel { session_id });
            }
            "AgentCloseSession" => {
                let session_id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("AgentCloseSession requires a session id"))?
                    .to_string();
                self.send_request(PluginRequest::AgentCloseSession { session_id });
            }
            "AgentArchiveSession" => {
                let session_id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("AgentArchiveSession requires a session id"))?
                    .to_string();
                self.send_request(PluginRequest::AgentArchiveSession { session_id });
            }
            "AgentForgetSession" => {
                let session_id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("AgentForgetSession requires a session id"))?
                    .to_string();
                self.send_request(PluginRequest::AgentForgetSession { session_id });
            }
            "AgentPermissionResponse" => {
                let request_id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        anyhow::anyhow!("AgentPermissionResponse requires a request id")
                    })?
                    .to_string();
                let option_id = args
                    .get(/*index*/ 1)
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);
                self.send_request(PluginRequest::AgentPermissionResponse {
                    request_id,
                    option_id,
                });
            }
            "RevertTransaction" => {
                let transaction_id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("RevertTransaction requires an id"))?
                    .to_string();
                self.send_request(PluginRequest::Action(Action::RevertTransaction(
                    transaction_id,
                )));
            }
            "SetCursorPosition" => {
                let x = args.first().and_then(value_to_u64).unwrap_or(0) as usize;
                let y = args.get(1).and_then(value_to_u64).unwrap_or(0) as usize;
                let jump = args.get(2).and_then(Value::as_bool).unwrap_or(false);
                self.send_request(PluginRequest::SetCursorPosition { x, y, jump });
            }
            "CloseScratchBuffer" => {
                let buffer_index = args
                    .first()
                    .and_then(value_to_u64)
                    .and_then(|index| usize::try_from(index).ok())
                    .ok_or_else(|| anyhow::anyhow!("CloseScratchBuffer requires a buffer index"))?;
                self.send_request(PluginRequest::CloseScratchBuffer { buffer_index });
            }
            "SetStorage" => {
                let key = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("SetStorage requires a storage key"))?
                    .to_string();
                let value = args
                    .get(1)
                    .map(value_to_json)
                    .unwrap_or(serde_json::Value::Null);
                self.send_request(PluginRequest::SetPluginStorage {
                    plugin: plugin.to_string(),
                    key,
                    value,
                });
            }
            "SetDecorations" => {
                let namespace = args
                    .first()
                    .and_then(Value::as_str)
                    .unwrap_or("default")
                    .to_string();
                let decorations = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value::<Vec<Decoration>>)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::SetDecorations {
                    namespace,
                    decorations,
                });
            }
            "ClearDecorations" => {
                let namespace = args
                    .first()
                    .and_then(Value::as_str)
                    .map_or_else(|| "default".to_string(), str::to_string);
                self.send_request(PluginRequest::ClearDecorations { namespace });
            }
            "SetGutterSigns" => {
                let namespace = args
                    .first()
                    .and_then(Value::as_str)
                    .unwrap_or("default")
                    .to_string();
                let signs = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value::<Vec<GutterSign>>)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::SetGutterSigns { namespace, signs });
            }
            "ClearGutterSigns" => {
                let namespace = args
                    .first()
                    .and_then(Value::as_str)
                    .map_or_else(|| "default".to_string(), str::to_string);
                self.send_request(PluginRequest::ClearGutterSigns { namespace });
            }
            "OpenPicker" => {
                let title = Some(red_required_string(args, 0, "OpenPicker")?.to_string());
                let values = red_required_value_array(args, 1, "OpenPicker")?;
                let items = values
                    .iter()
                    .map(value_to_json)
                    .map(serde_json::from_value::<PickerItem>)
                    .collect::<Result<Vec<_>, _>>()?;
                let options = args
                    .get(2)
                    .map(value_to_json)
                    .map(serde_json::from_value::<PickerOptions>)
                    .transpose()?
                    .unwrap_or_default();
                let handlers = args
                    .get(3)
                    .ok_or_else(|| anyhow::anyhow!("OpenPicker requires PickerHandlers"))
                    .and_then(|value| red_picker_handlers(plugin, value, "OpenPicker"))?;
                let handle = self.policy_mut().allocate_picker_handle();
                self.policy_mut().picker_handlers.insert(
                    handle,
                    PickerRegistration {
                        plugin: plugin.to_string(),
                        handlers,
                    },
                );
                self.send_request(PluginRequest::OpenCallbackPicker {
                    owner: plugin.to_string(),
                    handle,
                    title,
                    items,
                    options,
                });
                return Ok(Value::Int(i64::from(handle.get())));
            }
            "OpenConfirm" => {
                let title = red_required_string(args, 0, "OpenConfirm")?.to_string();
                let message = red_required_string(args, 1, "OpenConfirm")?.to_string();
                let handlers = args
                    .get(2)
                    .ok_or_else(|| anyhow::anyhow!("OpenConfirm requires PickerHandlers"))
                    .and_then(|value| red_picker_handlers(plugin, value, "OpenConfirm"))?;
                let options = args
                    .get(3)
                    .map(value_to_json)
                    .map(serde_json::from_value::<crate::ui::ConfirmationOptions>)
                    .transpose()?
                    .unwrap_or_default();
                let handle = self.policy_mut().allocate_picker_handle();
                self.policy_mut().picker_handlers.insert(
                    handle,
                    PickerRegistration {
                        plugin: plugin.to_string(),
                        handlers,
                    },
                );
                self.send_request(PluginRequest::OpenCallbackConfirmation {
                    owner: plugin.to_string(),
                    handle,
                    title,
                    message,
                    options,
                });
                return Ok(Value::Int(i64::from(handle.get())));
            }
            "OpenDynamicPicker" => {
                let title = args.first().and_then(Value::as_str).map(str::to_string);
                let id = args.get(1).and_then(value_to_i32).unwrap_or(1);
                let items = args
                    .get(2)
                    .map(value_to_json)
                    .map(serde_json::from_value::<Vec<PickerItem>>)
                    .transpose()?
                    .unwrap_or_default();
                let options = args
                    .get(3)
                    .map(value_to_json)
                    .map(serde_json::from_value::<PickerOptions>)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::OpenDynamicPicker {
                    title,
                    id,
                    items,
                    options,
                });
            }
            "OpenAgentComposer" => {
                let title = args.first().and_then(Value::as_str).map(str::to_string);
                let id = args.get(1).and_then(value_to_i32).unwrap_or(1);
                let query = args.get(2).map(value_to_string).unwrap_or_default();
                let history = args
                    .get(3)
                    .map(value_to_json)
                    .map(serde_json::from_value::<Vec<String>>)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::OpenAgentComposer {
                    owner: plugin.to_string(),
                    title,
                    id,
                    query,
                    history,
                });
            }
            "OpenComposer" => {
                let title = Some(red_required_string(args, 0, "OpenComposer")?.to_string());
                let query = red_required_string(args, 1, "OpenComposer")?.to_string();
                let history = args
                    .get(2)
                    .map(value_to_json)
                    .map(serde_json::from_value::<Vec<String>>)
                    .transpose()?
                    .unwrap_or_default();
                let handlers = args
                    .get(3)
                    .ok_or_else(|| anyhow::anyhow!("OpenComposer requires ComposerHandlers"))
                    .and_then(|value| red_composer_handlers(plugin, value, "OpenComposer"))?;
                let handle = self.policy_mut().allocate_composer_handle();
                self.policy_mut().composer_handlers.insert(
                    handle,
                    ComposerRegistration {
                        plugin: plugin.to_string(),
                        handlers,
                    },
                );
                self.send_request(PluginRequest::OpenCallbackComposer {
                    owner: plugin.to_string(),
                    handle,
                    title,
                    query,
                    history,
                });
                return Ok(Value::Int(i64::from(handle.get())));
            }
            "OpenInput" => {
                let title = red_required_string(args, 0, "OpenInput")?.to_string();
                let initial = red_required_string(args, 1, "OpenInput")?.to_string();
                let handlers = args
                    .get(2)
                    .ok_or_else(|| anyhow::anyhow!("OpenInput requires ComposerHandlers"))
                    .and_then(|value| red_composer_handlers(plugin, value, "OpenInput"))?;
                let handle = self.policy_mut().allocate_composer_handle();
                self.policy_mut().composer_handlers.insert(
                    handle,
                    ComposerRegistration {
                        plugin: plugin.to_string(),
                        handlers,
                    },
                );
                self.send_request(PluginRequest::OpenCallbackInput {
                    owner: plugin.to_string(),
                    handle,
                    title,
                    initial,
                });
                return Ok(Value::Int(i64::from(handle.get())));
            }
            "UpdatePickerItems" => {
                let id = args.first().and_then(value_to_i32).unwrap_or(1);
                self.ensure_picker_owner(plugin, id, "UpdatePickerItems")?;
                let items = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value::<Vec<PickerItem>>)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::UpdatePickerItems { id, items });
            }
            "UpdatePickerQuery" => {
                let id = args.first().and_then(value_to_i32).unwrap_or(1);
                self.ensure_picker_owner(plugin, id, "UpdatePickerQuery")?;
                let query = args.get(1).map(value_to_string).unwrap_or_default();
                self.send_request(PluginRequest::UpdatePickerQuery { id, query });
            }
            "UpdatePickerSelection" => {
                let id = args.first().and_then(value_to_i32).unwrap_or(1);
                self.ensure_picker_owner(plugin, id, "UpdatePickerSelection")?;
                let selection = args.get(1).map(value_to_string).unwrap_or_default();
                self.send_request(PluginRequest::UpdatePickerSelection { id, selection });
            }
            "UpdatePickerStatus" => {
                let id = args.first().and_then(value_to_i32).unwrap_or(1);
                self.ensure_picker_owner(plugin, id, "UpdatePickerStatus")?;
                let status = args.get(1).map(value_to_string);
                self.send_request(PluginRequest::UpdatePickerStatus { id, status });
            }
            "UpdatePickerBusy" => {
                let id = args.first().and_then(value_to_i32).unwrap_or(1);
                self.ensure_picker_owner(plugin, id, "UpdatePickerBusy")?;
                let busy = args.get(1).and_then(Value::as_bool).unwrap_or(false);
                self.send_request(PluginRequest::UpdatePickerBusy { id, busy });
            }
            "ClosePicker" => {
                let id = args.first().and_then(value_to_i32).unwrap_or(1);
                self.ensure_picker_owner(plugin, id, "ClosePicker")?;
                self.send_request(PluginRequest::ClosePicker { id });
            }
            "OpenLocation" => {
                let location = args
                    .first()
                    .map(value_to_json)
                    .map(serde_json::from_value)
                    .transpose()?
                    .ok_or_else(|| anyhow::anyhow!("OpenLocation requires a location object"))?;
                let target = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::OpenLocation { location, target });
            }
            "OpenBuffer" => {
                let name = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("OpenBuffer requires a buffer name"))?
                    .to_string();
                self.send_request(PluginRequest::Action(Action::OpenBuffer(name)));
            }
            "OpenBufferById" => {
                let id = args
                    .first()
                    .and_then(value_to_u64)
                    .ok_or_else(|| anyhow::anyhow!("OpenBufferById requires a buffer ID"))?;
                self.send_request(PluginRequest::Action(Action::OpenBufferById(id)));
            }
            "WatchDirectory" => {
                let path = args
                    .first()
                    .and_then(Value::as_str)
                    .unwrap_or(".")
                    .to_string();
                let watch_id = args.get(1).and_then(value_to_i32).unwrap_or(1);
                let recursive = args.get(2).and_then(Value::as_bool).unwrap_or(false);
                let interval_ms = args.get(3).and_then(value_to_u64).unwrap_or(250);
                self.send_request(PluginRequest::WatchDirectory {
                    path,
                    watch_id,
                    recursive,
                    interval_ms,
                });
            }
            "UnwatchDirectory" => {
                let watch_id = args.first().and_then(value_to_i32).unwrap_or(1);
                self.send_request(PluginRequest::UnwatchDirectory { watch_id });
            }
            "CreateOverlay" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("CreateOverlay requires an overlay id"))?
                    .to_string();
                let config = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value::<OverlayConfig>)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::CreateOverlay { id, config });
            }
            "UpdateOverlay" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("UpdateOverlay requires an overlay id"))?
                    .to_string();
                let lines = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::UpdateOverlay { id, lines });
            }
            "UpdateOverlayBusy" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("UpdateOverlayBusy requires an overlay id"))?
                    .to_string();
                let busy = args.get(1).and_then(Value::as_bool).unwrap_or(false);
                self.send_request(PluginRequest::UpdateOverlayBusy { id, busy });
            }
            "RemoveOverlay" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("RemoveOverlay requires an overlay id"))?
                    .to_string();
                self.send_request(PluginRequest::RemoveOverlay { id });
            }
            "CreateWindowBar" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("CreateWindowBar requires a bar id"))?
                    .to_string();
                let config = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value::<WindowBarConfig>)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::CreateWindowBar { id, config });
            }
            "UpdateWindowBar" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("UpdateWindowBar requires a bar id"))?
                    .to_string();
                let window_id = args
                    .get(1)
                    .and_then(value_to_u64)
                    .ok_or_else(|| anyhow::anyhow!("UpdateWindowBar requires a window id"))?;
                let segments = args
                    .get(2)
                    .map(value_to_json)
                    .map(serde_json::from_value::<Vec<WindowBarSegment>>)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::UpdateWindowBar {
                    id,
                    window_id,
                    segments,
                });
            }
            "CloseWindowBar" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("CloseWindowBar requires a bar id"))?
                    .to_string();
                let window_id = args.get(1).and_then(value_to_u64);
                self.send_request(PluginRequest::CloseWindowBar { id, window_id });
            }
            "OpenWorkspace" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("OpenWorkspace requires a workspace id"))?
                    .to_string();
                let config = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value::<WorkspaceConfig>)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::OpenWorkspace { id, config });
            }
            "UpdateWorkspace" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("UpdateWorkspace requires a workspace id"))?
                    .to_string();
                let model = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value::<WorkspaceModel>)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::UpdateWorkspace { id, model });
            }
            "CloseWorkspace" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("CloseWorkspace requires a workspace id"))?
                    .to_string();
                self.send_request(PluginRequest::CloseWorkspace { id });
            }
            "CreatePanel" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("CreatePanel requires a panel id"))?
                    .to_string();
                let config = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value::<PanelConfig>)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::CreatePanel { id, config });
            }
            "UpdatePanel" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("UpdatePanel requires a panel id"))?
                    .to_string();
                let rows = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value::<Vec<PanelRow>>)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::UpdatePanel { id, rows });
            }
            "CreateTextPanel" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("CreateTextPanel requires a panel id"))?
                    .to_string();
                let config = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value::<PanelConfig>)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::CreateTextPanel { id, config });
            }
            "UpdateTextPanel" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("UpdateTextPanel requires a panel id"))?
                    .to_string();
                let blocks = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value::<Vec<TextPanelBlock>>)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::UpdateTextPanel { id, blocks });
            }
            "AppendTextPanel" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("AppendTextPanel requires a panel id"))?
                    .to_string();
                let block_id = args
                    .get(1)
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("AppendTextPanel requires a block id"))?
                    .to_string();
                let delta = args.get(2).map(value_to_string).unwrap_or_default();
                self.send_request(PluginRequest::AppendTextPanel {
                    id,
                    block_id,
                    delta,
                });
            }
            "FocusTextPanelComposer" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("FocusTextPanelComposer requires a panel id"))?
                    .to_string();
                self.send_request(PluginRequest::FocusTextPanelComposer { id });
            }
            "SetTextPanelComposerState" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        anyhow::anyhow!("SetTextPanelComposerState requires a panel id")
                    })?
                    .to_string();
                let enabled = args.get(1).and_then(Value::as_bool).unwrap_or(true);
                let status = args.get(2).and_then(Value::as_str).map(str::to_string);
                self.send_request(PluginRequest::SetTextPanelComposerState {
                    id,
                    enabled,
                    status,
                });
            }
            "SetTextPanelComposerHistory" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        anyhow::anyhow!("SetTextPanelComposerHistory requires a panel id")
                    })?
                    .to_string();
                let history = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value::<Vec<String>>)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::SetTextPanelComposerHistory { id, history });
            }
            "SetTextPanelHeaderDetail" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("SetTextPanelHeaderDetail requires a panel id"))?
                    .to_string();
                let detail = match args.get(1).map(value_to_json) {
                    None | Some(serde_json::Value::Null) => None,
                    Some(value) => Some(serde_json::from_value(value)?),
                };
                self.send_request(PluginRequest::SetTextPanelHeaderDetail { id, detail });
            }
            "SetTextPanelStatus" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("SetTextPanelStatus requires a panel id"))?
                    .to_string();
                let status = match args.get(1).map(value_to_json) {
                    None | Some(serde_json::Value::Null) => None,
                    Some(value) => Some(serde_json::from_value::<TextPanelStatus>(value)?),
                };
                self.send_request(PluginRequest::SetTextPanelStatus { id, status });
            }
            "ClearTextPanelComposer" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("ClearTextPanelComposer requires a panel id"))?
                    .to_string();
                self.send_request(PluginRequest::ClearTextPanelComposer { id });
            }
            "SelectPanelRow" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("SelectPanelRow requires a panel id"))?
                    .to_string();
                let row_id = args
                    .get(1)
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("SelectPanelRow requires a row id"))?
                    .to_string();
                self.send_request(PluginRequest::SelectPanelRow { id, row_id });
            }
            "FocusPanel" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("FocusPanel requires a panel id"))?
                    .to_string();
                self.send_request(PluginRequest::FocusPanel { id });
            }
            "RestorePanelFocus" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("RestorePanelFocus requires a panel id"))?
                    .to_string();
                self.send_request(PluginRequest::RestorePanelFocus { id });
            }
            "FocusEditor" => {
                self.send_request(PluginRequest::FocusEditor);
            }
            "SetPanelVisible" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("SetPanelVisible requires a panel id"))?
                    .to_string();
                let visible = args.get(1).and_then(Value::as_bool).unwrap_or(true);
                self.send_request(PluginRequest::SetPanelVisible { id, visible });
            }
            "ClosePanel" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("ClosePanel requires a panel id"))?
                    .to_string();
                self.send_request(PluginRequest::ClosePanel { id });
            }
            "SpawnProcess" => {
                anyhow::ensure!(
                    self.staged_effects.is_none(),
                    "SpawnProcess is not allowed while a plugin reload is being staged"
                );
                let options = args
                    .first()
                    .map(value_to_json)
                    .map(serde_json::from_value::<ProcessSpawnOptions>)
                    .transpose()?
                    .ok_or_else(|| anyhow::anyhow!("SpawnProcess requires process options"))?;
                return self
                    .process_manager
                    .spawn(plugin, options)
                    .map(Value::String);
            }
            "KillProcess" => {
                anyhow::ensure!(
                    self.staged_effects.is_none(),
                    "KillProcess is not allowed while a plugin reload is being staged"
                );
                let process_id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("KillProcess requires a process id"))?;
                self.process_manager.kill(plugin, process_id)?;
            }
            "RecordCursorMoved" => {
                let event = first_json(args)?;
                let message = format!(
                    "cursor:{}:{},{}->{},{}:{}",
                    json_str(&event, "cause"),
                    json_usize_at(&event, &["from", "x"]),
                    json_usize_at(&event, &["from", "y"]),
                    json_usize_at(&event, &["to", "x"]),
                    json_usize_at(&event, &["to", "y"]),
                    json_str(&event, "mode")
                );
                self.send_request(PluginRequest::Action(Action::Print(message)));
            }
            "RecordModeChanged" => {
                let event = first_json(args)?;
                let message = format!(
                    "mode:{}:{}->{}",
                    json_str(&event, "cause"),
                    json_str(&event, "from"),
                    json_str(&event, "to")
                );
                self.send_request(PluginRequest::Action(Action::Print(message)));
            }
            "RecordSearchHighlighted" => {
                let event = first_json(args)?;
                let message = format!(
                    "search:{}:{}:{}",
                    json_str(&event, "source"),
                    json_str(&event, "term"),
                    json_str(&event, "direction")
                );
                self.send_request(PluginRequest::Action(Action::Print(message)));
            }
            "RecordSearchCleared" => {
                let event = first_json(args)?;
                let message = format!("cleared:{}", json_str(&event, "term"));
                self.send_request(PluginRequest::Action(Action::Print(message)));
            }
            "SetTimeout" => {
                let delay_ms = args.first().and_then(value_to_u64).unwrap_or(0);
                let id = self.schedule_timeout(delay_ms);
                return Ok(Value::String(id));
            }
            "CancelTimeout" => {
                let timer_id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("CancelTimeout requires a timer id"))?;
                self.cancel_timeout(timer_id);
            }
            other => {
                anyhow::bail!("unsupported Red host action `{other}`");
            }
        }

        Ok(Value::Unit)
    }

    fn request(
        &mut self,
        plugin: &str,
        request_id: RequestId,
        action: &str,
        args: &[Value],
    ) -> anyhow::Result<()> {
        let request = match action {
            "GetViewportLayout" => PluginRequest::GetViewportLayout { request_id },
            "InlayHints" => {
                let range = args
                    .first()
                    .map(value_to_json)
                    .map(serde_json::from_value)
                    .transpose()?;
                PluginRequest::InlayHints { request_id, range }
            }
            "GetEditorInfo" => PluginRequest::EditorInfo(request_id),
            "EditHistory" => PluginRequest::EditHistory { request_id },
            "AgentReadDefaultModel" => PluginRequest::AgentModelRequest {
                request_id,
                request: crate::codex::ModelRequest::ReadDefault {
                    cwd: crate::utils::get_workspace_path(),
                },
            },
            "AgentListModels" => PluginRequest::AgentModelRequest {
                request_id,
                request: crate::codex::ModelRequest::List,
            },
            "AgentSetModel" => {
                let session_id = args
                    .first()
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let selection = args
                    .get(1)
                    .map(value_to_json)
                    .ok_or_else(|| anyhow::anyhow!("AgentSetModel requires a model selection"))?;
                PluginRequest::AgentModelRequest {
                    request_id,
                    request: crate::codex::ModelRequest::Set {
                        session_id,
                        selection: serde_json::from_value(selection)?,
                    },
                }
            }
            "GenerateCommitMessage" => {
                let cwd = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("GenerateCommitMessage requires a cwd"))?;
                let context = args
                    .get(1)
                    .map(value_to_json)
                    .ok_or_else(|| anyhow::anyhow!("GenerateCommitMessage requires context"))?;
                PluginRequest::GenerateCommitMessage {
                    request_id,
                    cwd: PathBuf::from(cwd),
                    branch: json_str(&context, "branch").to_string(),
                    staged_diff: json_str(&context, "staged_diff").to_string(),
                    recent_commits: json_str(&context, "recent_commits").to_string(),
                }
            }
            "GetCursorPosition" => PluginRequest::GetCursorPosition { request_id },
            "GetCursorDisplayColumn" => PluginRequest::GetCursorDisplayColumn { request_id },
            "GetBufferText" => {
                let start_line = args
                    .first()
                    .and_then(value_to_u64)
                    .and_then(|line| usize::try_from(line).ok());
                let end_line = args
                    .get(1)
                    .and_then(value_to_u64)
                    .and_then(|line| usize::try_from(line).ok());
                PluginRequest::GetBufferText {
                    request_id,
                    start_line,
                    end_line,
                }
            }
            "GetSelection" => PluginRequest::GetSelection { request_id },
            "GetAgentContext" => PluginRequest::GetAgentContext { request_id },
            "OpenScratchBuffer" => {
                let commands = args
                    .get(2)
                    .map(value_to_json)
                    .unwrap_or(serde_json::Value::Null);
                PluginRequest::OpenScratchBuffer {
                    request_id,
                    name: args.first().map(value_to_string).unwrap_or_default(),
                    text: args.get(1).map(value_to_string).unwrap_or_default(),
                    syntax: commands
                        .get("syntax")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    submit_command: commands
                        .get("submit")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    cancel_command: commands
                        .get("cancel")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                }
            }
            "GetConfig" => PluginRequest::GetConfig {
                request_id,
                key: args.first().and_then(Value::as_str).map(str::to_string),
            },
            "GetStorage" => {
                let key = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("GetStorage requires a storage key"))?
                    .to_string();
                PluginRequest::GetPluginStorage {
                    plugin: plugin.to_string(),
                    key,
                    request_id,
                }
            }
            "GetEditorState" => PluginRequest::GetEditorState { request_id },
            "RestoreEditorState" => {
                let snapshot = args
                    .first()
                    .map(value_to_json)
                    .map(serde_json::from_value)
                    .transpose()?
                    .ok_or_else(|| anyhow::anyhow!("RestoreEditorState requires a snapshot"))?;
                PluginRequest::RestoreEditorState {
                    request_id,
                    snapshot,
                }
            }
            "GetWindows" => PluginRequest::GetWindows { request_id },
            "DocumentSymbols" => {
                let buffer_index = args
                    .first()
                    .and_then(value_to_u64)
                    .and_then(|index| usize::try_from(index).ok());
                PluginRequest::DocumentSymbols {
                    request_id,
                    buffer_index,
                }
            }
            "WorkspaceSymbols" => PluginRequest::WorkspaceSymbols {
                request_id,
                query: args.first().map(value_to_query_string).unwrap_or_default(),
            },
            "References" => PluginRequest::References {
                request_id,
                include_declaration: args.first().and_then(Value::as_bool).unwrap_or(true),
            },
            "ResolveThemeStyle" => {
                let spec = args
                    .first()
                    .map(value_to_json)
                    .map(serde_json::from_value)
                    .transpose()?
                    .ok_or_else(|| anyhow::anyhow!("ResolveThemeStyle requires a style spec"))?;
                PluginRequest::ResolveThemeStyle { request_id, spec }
            }
            "ListRuntimeAssets" => {
                let kind = match args.first().and_then(Value::as_str).unwrap_or("themes") {
                    "plugin" | "plugins" => RuntimeAssetKind::Plugin,
                    "theme" | "themes" => RuntimeAssetKind::Theme,
                    other => anyhow::bail!("unsupported runtime asset kind: {other}"),
                };
                PluginRequest::ListRuntimeAssets { kind, request_id }
            }
            "GetTextDisplayWidth" => PluginRequest::GetTextDisplayWidth {
                request_id,
                text: args.first().map(value_to_string).unwrap_or_default(),
            },
            "CharIndexToDisplayColumn" => PluginRequest::CharIndexToDisplayColumn {
                request_id,
                x: args.first().and_then(value_to_u64).unwrap_or(0) as usize,
                y: args.get(1).and_then(value_to_u64).unwrap_or(0) as usize,
            },
            "DisplayColumnToCharIndex" => PluginRequest::DisplayColumnToCharIndex {
                request_id,
                column: args.first().and_then(value_to_u64).unwrap_or(0) as usize,
                y: args.get(1).and_then(value_to_u64).unwrap_or(0) as usize,
            },
            "ListDirectory" => PluginRequest::ListDirectory {
                path: args
                    .first()
                    .and_then(Value::as_str)
                    .unwrap_or(".")
                    .to_string(),
                request_id,
            },
            "GetGitStatus" => PluginRequest::GetGitStatus {
                path: args
                    .first()
                    .and_then(Value::as_str)
                    .unwrap_or(".")
                    .to_string(),
                request_id,
            },
            "FileOperation" => PluginRequest::FileOperation {
                operation: args
                    .first()
                    .map(value_to_json)
                    .unwrap_or(serde_json::Value::Null),
                request_id,
            },
            "CompanionCall" => PluginRequest::CompanionCall {
                owner: plugin.to_string(),
                method: args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("CompanionCall requires a method"))?
                    .to_string(),
                params: args
                    .get(1)
                    .map(value_to_json)
                    .unwrap_or(serde_json::Value::Null),
                timeout_ms: args.get(2).and_then(value_to_u64),
                request_id,
            },
            "DocumentSnapshot" => PluginRequest::DocumentSnapshot {
                path: args.first().and_then(Value::as_str).map(str::to_string),
                request_id,
            },
            "DocumentApply" => {
                let options = args
                    .first()
                    .map(value_to_json)
                    .ok_or_else(|| anyhow::anyhow!("DocumentApply requires options"))?;
                PluginRequest::DocumentApply {
                    owner: plugin.to_string(),
                    path: options
                        .get("path")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    expected_revision: options
                        .get("expected_revision")
                        .and_then(serde_json::Value::as_u64)
                        .ok_or_else(|| {
                            anyhow::anyhow!("DocumentApply requires expected_revision")
                        })?,
                    label: options
                        .get("label")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("plugin edit")
                        .to_string(),
                    edits: serde_json::from_value(
                        options
                            .get("edits")
                            .cloned()
                            .ok_or_else(|| anyhow::anyhow!("DocumentApply requires edits"))?,
                    )?,
                    request_id,
                }
            }
            "DocumentUndo" => {
                let options = args
                    .first()
                    .map(value_to_json)
                    .ok_or_else(|| anyhow::anyhow!("DocumentUndo requires options"))?;
                PluginRequest::DocumentUndo {
                    owner: plugin.to_string(),
                    path: options
                        .get("path")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    transaction_id: options
                        .get("transaction_id")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| anyhow::anyhow!("DocumentUndo requires transaction_id"))?
                        .to_string(),
                    request_id,
                }
            }
            other => anyhow::bail!("unsupported Red host request: {other}"),
        };
        self.send_request(request);
        Ok(())
    }

    fn query(&mut self, _plugin: &str, query: &str) -> anyhow::Result<Value> {
        self.snapshots
            .get(query)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Husk host snapshot `{query}` is unavailable"))
    }

    fn call_module(
        &mut self,
        plugin: &str,
        path: &str,
        args: &[Value],
    ) -> Option<anyhow::Result<Value>> {
        if !path.starts_with("red::") {
            return None;
        }
        Some((|| match path {
            "red::add_command" => {
                let command = red_required_string(args, 0, path)?;
                let callback = red_required_callback(args, 1, path)?.clone();
                let metadata = args
                    .get(2)
                    .map(Value::to_json)
                    .map(serde_json::from_value::<CommandMetadata>)
                    .transpose()
                    .map_err(|error| {
                        anyhow::anyhow!("invalid metadata for command `{command}`: {error}")
                    })?
                    .unwrap_or_default();
                validate_command_registration(self, &callback, &metadata)?;
                if let Some(existing) = self.policy().commands.get(command) {
                    if existing.callback.plugin() != plugin {
                        anyhow::bail!(
                            "command `{command}` is already registered by plugin `{}`",
                            existing.callback.plugin()
                        );
                    }
                }
                self.policy_mut()
                    .commands
                    .insert(command.to_string(), RedCommand { callback, metadata });
                Ok(Value::Unit)
            }
            "red::on" => {
                let event = red_required_string(args, 0, path)?;
                let callback = red_required_callback(args, 1, path)?.clone();
                let listeners = self
                    .policy_mut()
                    .event_listeners
                    .entry(event.to_string())
                    .or_default();
                Arc::make_mut(listeners).push(callback);
                Ok(Value::Unit)
            }
            "red::execute" => {
                let action = red_required_string(args, 0, path)?;
                self.execute(plugin, action, &args[1..])
            }
            "red::request" => {
                let action = red_required_string(args, 0, path)?;
                let callback = red_required_callback(args, 1, path)?.clone();
                let request_id = self.policy_mut().allocate_request_id();
                self.policy_mut()
                    .pending_requests
                    .insert(request_id, callback);
                if let Err(error) = self.request(plugin, request_id, action, &args[2..]) {
                    self.policy_mut().pending_requests.remove(&request_id);
                    return Err(error);
                }
                Ok(Value::Int(request_id.get()))
            }
            "red::viewport_layout" => self.query(plugin, "viewport_layout"),
            "red::windows" => self.query(plugin, "windows"),
            "red::editor_info" => self.query(plugin, "editor_info"),
            "red::log" => {
                let message = args
                    .iter()
                    .map(red_value_to_log_string)
                    .collect::<Vec<_>>()
                    .join(" ");
                self.log(&message);
                Ok(Value::Unit)
            }
            "red::state_bool" => {
                let key = red_required_string(args, 0, path)?;
                Ok(Value::Bool(
                    self.policy()
                        .plugin_states
                        .get(plugin)
                        .and_then(|state| state.get(key))
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                ))
            }
            "red::state_set" => {
                if args.len() == 1 {
                    let value = args.first().cloned().unwrap_or(Value::Unit);
                    let Value::Struct { type_name, .. } = &value else {
                        anyhow::bail!("`red::state_set(state)` requires a state record");
                    };
                    let Some(expected_type) = self.policy().state_record_types.get(plugin) else {
                        anyhow::bail!(
                            "`red::state_set(state)` requires a `#[red::state]` initializer"
                        );
                    };
                    anyhow::ensure!(
                        type_name == expected_type,
                        "state type `{type_name}` does not match plugin state `{expected_type}`"
                    );
                    self.policy_mut()
                        .typed_states
                        .insert(plugin.to_string(), value);
                } else {
                    let key = red_required_string(args, 0, path)?.to_string();
                    let value = args.get(1).cloned().unwrap_or(Value::Unit);
                    self.policy_mut()
                        .plugin_states
                        .entry(plugin.to_string())
                        .or_default()
                        .insert(key, value);
                }
                Ok(Value::Unit)
            }
            "red::state_patch" => {
                let Some(Value::Struct {
                    type_name: patch_type,
                    fields: patch,
                }) = args.first()
                else {
                    anyhow::bail!("`red::state_patch(patch)` requires a typed state record");
                };
                let Some(Value::Struct {
                    type_name: state_type,
                    fields,
                }) = self.policy_mut().typed_states.get_mut(plugin)
                else {
                    anyhow::bail!(
                        "`red::state_patch(patch)` requires an initialized `#[red::state]` record"
                    );
                };
                anyhow::ensure!(
                    patch_type == state_type,
                    "state patch type `{patch_type}` does not match plugin state `{state_type}`"
                );
                for name in patch.keys() {
                    anyhow::ensure!(
                        fields.contains_key(name),
                        "state `{state_type}` has no field named `{name}`"
                    );
                }
                let fields = Arc::make_mut(fields);
                for (name, value) in patch.iter() {
                    fields.insert(name.clone(), value.clone());
                }
                Ok(Value::Unit)
            }
            "red::state" => {
                if args.is_empty() {
                    self.policy()
                        .typed_states
                        .get(plugin)
                        .cloned()
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "`red::state()` requires an initialized `#[red::state]` record"
                            )
                        })
                } else {
                    let key = red_required_string(args, 0, path)?;
                    Ok(self
                        .policy()
                        .plugin_states
                        .get(plugin)
                        .and_then(|state| state.get(key))
                        .cloned()
                        .unwrap_or(Value::Unit))
                }
            }
            "red::push" => {
                let mut values = red_required_value_array(args, 0, path)?;
                Arc::make_mut(&mut values).push(args.get(1).cloned().unwrap_or(Value::Null));
                Ok(Value::Array(values))
            }
            "red::extend" => {
                let mut values = red_required_value_array(args, 0, path)?;
                let additional = red_required_value_array(args, 1, path)?;
                Arc::make_mut(&mut values).extend(additional.iter().cloned());
                Ok(Value::Array(values))
            }
            "red::unshift" => {
                let mut values = red_required_value_array(args, 0, path)?;
                Arc::make_mut(&mut values).insert(0, args.get(1).cloned().unwrap_or(Value::Null));
                Ok(Value::Array(values))
            }
            "red::contains" => {
                let values = red_required_value_array(args, 0, path)?;
                let needle = args.get(1).cloned().unwrap_or(Value::Null);
                Ok(Value::Bool(values.contains(&needle)))
            }
            "red::remove" => {
                let values = red_required_value_array(args, 0, path)?;
                let needle = args.get(1).cloned().unwrap_or(Value::Null);
                Ok(Value::Array(Arc::new(
                    values
                        .iter()
                        .filter(|value| **value != needle)
                        .cloned()
                        .collect(),
                )))
            }
            "red::reverse" => {
                let values = red_required_value_array(args, 0, path)?;
                Ok(Value::Array(Arc::new(
                    values.iter().rev().cloned().collect(),
                )))
            }
            "red::join" => {
                let values = red_required_value_array(args, 0, path)?;
                let separator = args.get(1).and_then(Value::as_str).unwrap_or("");
                Ok(Value::String(
                    values
                        .iter()
                        .map(red_value_to_log_string)
                        .collect::<Vec<_>>()
                        .join(separator),
                ))
            }
            "red::range" => {
                let end = args.first().and_then(red_value_to_i64).unwrap_or(0).max(0);
                Ok(Value::Array(Arc::new((0..end).map(Value::Int).collect())))
            }
            "red::len" => {
                let length = match args.first() {
                    Some(Value::String(value)) => value.chars().count(),
                    Some(Value::Array(values)) => values.len(),
                    Some(Value::Object(values)) => values.len(),
                    Some(Value::Json(serde_json::Value::Array(values))) => values.len(),
                    Some(Value::Json(serde_json::Value::Object(values))) => values.len(),
                    Some(Value::Unit | Value::Null | Value::Missing(_)) | None => 0,
                    Some(value) => {
                        anyhow::bail!("`{path}` argument 0 has no length: {value:?}")
                    }
                };
                Ok(Value::Int(i64::try_from(length).unwrap_or(i64::MAX)))
            }
            "red::int" => {
                let fallback = args.get(1).and_then(red_value_to_i64).unwrap_or(0);
                Ok(Value::Int(
                    args.first().and_then(red_value_to_i64).unwrap_or(fallback),
                ))
            }
            "red::bool" => {
                let fallback = args.get(1).and_then(Value::as_bool).unwrap_or(false);
                Ok(Value::Bool(
                    args.first().and_then(red_value_to_bool).unwrap_or(fallback),
                ))
            }
            "red::string" => {
                let fallback = args.get(1).map(red_value_to_log_string).unwrap_or_default();
                Ok(Value::String(
                    args.first()
                        .and_then(red_value_to_plain_string)
                        .unwrap_or(fallback),
                ))
            }
            "red::text_field" => {
                let text = args
                    .first()
                    .and_then(red_text_field_value)
                    .unwrap_or_default();
                Ok(Value::String(text))
            }
            "red::utf8_byte_to_char_index" => {
                let text = red_required_string(args, 0, path)?;
                let offset = args.get(1).and_then(red_value_to_i64).unwrap_or(0);
                let offset = usize::try_from(offset).unwrap_or(0);
                let index = text
                    .char_indices()
                    .take_while(|(byte_index, _)| *byte_index < offset)
                    .count();
                Ok(Value::Int(i64::try_from(index).unwrap_or(i64::MAX)))
            }
            "red::blend_color" => {
                let foreground = args.first().and_then(red_color_channels);
                let background = args.get(1).and_then(red_color_channels);
                let opacity = args.get(2).and_then(red_value_to_f64).unwrap_or(0.42);
                let Some((fr, fg, fb)) = foreground else {
                    return Ok(args.first().cloned().unwrap_or(Value::Unit));
                };
                let Some((br, bg, bb)) = background else {
                    return Ok(args.first().cloned().unwrap_or(Value::Unit));
                };
                let opacity = opacity.clamp(0.0, 1.0);
                let blend = |foreground: u8, background: u8| {
                    (f64::from(background)
                        + (f64::from(foreground) - f64::from(background)) * opacity)
                        .round()
                        .clamp(0.0, 255.0) as u8
                };
                Ok(Value::Json(serde_json::json!({
                    "Rgb": {
                        "r": blend(fr, br),
                        "g": blend(fg, bg),
                        "b": blend(fb, bb),
                    }
                })))
            }
            "red::is_light_color" => {
                let Some((red, green, blue)) = args.first().and_then(red_color_channels) else {
                    return Ok(Value::Bool(false));
                };
                Ok(Value::Bool(
                    crate::color::Color::Rgb {
                        r: red,
                        g: green,
                        b: blue,
                    }
                    .is_light(),
                ))
            }
            "red::char_at" => {
                let value = red_required_string(args, 0, path)?;
                let index = args.get(1).and_then(red_value_to_i64).unwrap_or(0);
                let character = usize::try_from(index)
                    .ok()
                    .and_then(|index| value.chars().nth(index))
                    .map_or_else(String::new, |character| character.to_string());
                Ok(Value::String(character))
            }
            "red::trim" => {
                let value = red_required_string(args, 0, path)?;
                Ok(Value::String(value.trim().to_string()))
            }
            "red::lower" => {
                let value = red_required_string(args, 0, path)?;
                Ok(Value::String(value.to_lowercase()))
            }
            "red::split" => {
                let value = red_required_string(args, 0, path)?;
                let delimiter = red_required_string(args, 1, path)?;
                Ok(Value::Json(serde_json::Value::Array(
                    value
                        .split(delimiter)
                        .map(|part| serde_json::Value::String(part.to_string()))
                        .collect(),
                )))
            }
            "red::starts_with" => {
                let value = red_required_string(args, 0, path)?;
                let prefix = red_required_string(args, 1, path)?;
                Ok(Value::Bool(value.starts_with(prefix)))
            }
            "red::ends_with" => {
                let value = red_required_string(args, 0, path)?;
                let suffix = red_required_string(args, 1, path)?;
                Ok(Value::Bool(value.ends_with(suffix)))
            }
            "red::replace_all" => {
                let value = red_required_string(args, 0, path)?;
                let from = red_required_string(args, 1, path)?;
                let to = red_required_string(args, 2, path)?;
                Ok(Value::String(value.replace(from, to)))
            }
            "red::trim_line_end" => {
                let value = red_required_string(args, 0, path)?;
                Ok(Value::String(
                    value
                        .strip_suffix("\r\n")
                        .or_else(|| value.strip_suffix('\n'))
                        .unwrap_or(value)
                        .to_string(),
                ))
            }
            "red::slice" => {
                let value = red_required_string(args, 0, path)?;
                let len = i64::try_from(value.chars().count()).unwrap_or(i64::MAX);
                let start = args.get(1).and_then(red_value_to_i64).unwrap_or(0);
                let end = args.get(2).and_then(red_value_to_i64).unwrap_or(len);
                let start = red_normalize_string_index(start, len);
                let end = red_normalize_string_index(end, len);
                let count = end.saturating_sub(start);
                Ok(Value::String(
                    value
                        .chars()
                        .skip(usize::try_from(start).unwrap_or(0))
                        .take(usize::try_from(count).unwrap_or(0))
                        .collect(),
                ))
            }
            "red::is_whitespace" => {
                let value = red_required_string(args, 0, path)?;
                Ok(Value::Bool(value.chars().all(char::is_whitespace)))
            }
            "red::char" => {
                let codepoint = args.first().and_then(red_value_to_i64).unwrap_or(0);
                let value = u32::try_from(codepoint)
                    .ok()
                    .and_then(char::from_u32)
                    .map_or_else(String::new, |character| character.to_string());
                Ok(Value::String(value))
            }
            "red::null" => Ok(Value::Null),
            "red::parse_json" => {
                let value = red_required_string(args, 0, path)?;
                Ok(serde_json::from_str(value)
                    .map(Value::Json)
                    .unwrap_or(Value::Unit))
            }
            "red::git_core" => {
                let operation = red_required_string(args, 0, path)?;
                let value = self.call_git_core(operation, &args[1..])?;
                let record = match operation {
                    "parse_status" => Some("GitState"),
                    "detail_document" => Some("GitWorkspaceDocument"),
                    _ => None,
                };
                if let Some((schema, record)) =
                    self.policy().payload_schemas.get(plugin).zip(record)
                {
                    return schema.named_record(record, &value_to_json(&value));
                }
                Ok(value)
            }
            "red::document_symbol_chain" => {
                let symbols = red_required_value_array(args, 0, path)?;
                let cursor = args
                    .get(1)
                    .ok_or_else(|| anyhow::anyhow!("`{path}` requires a position"))?;
                let file = red_required_string(args, 2, path)?;
                Ok(Value::Array(Arc::new(super::document_symbols::chain(
                    &symbols, cursor, file,
                ))))
            }
            "red::neotree_core" => {
                let operation = red_required_string(args, 0, path)?;
                self.call_neotree_core(operation, &args[1..])
            }
            _ => anyhow::bail!("unknown Red host function `{path}`"),
        })())
    }

    fn call_git_core(&mut self, operation: &str, args: &[Value]) -> anyhow::Result<Value> {
        // The full patch remains in plugin state for staging and the scratch view.
        // Only the bounded preview enters the instruction-metered document parser.
        let preview = if operation == "detail_document" {
            Some(git_detail_preview(args)?)
        } else {
            None
        };
        let args = preview.as_ref().map_or(args, |(args, _)| args.as_slice());
        let function = match operation {
            "parse_status" => "status::parse_status",
            "display_entries" => "status::display_entries",
            "sign_hunks" => "patch::sign_hunks",
            "detail_document" => "patch::detail_document",
            "dashboard_hunk" => "patch::dashboard_hunk",
            "dashboard_range" => "patch::dashboard_range",
            "hunk_starts" => "patch::hunk_starts",
            "editor_hunk" => "patch::editor_hunk",
            "editor_range" => "patch::editor_range",
            "normalize_unsaved" => "patch::normalize_unsaved",
            "apply_hunk_args" => "commands::apply_hunk_args",
            "status_args" => "commands::status_args",
            "sign_diff_args" => "commands::sign_diff_args",
            "detail_diff_args" => "commands::detail_diff_args",
            "hunk_diff_args" => "commands::hunk_diff_args",
            "commit_args" => "commands::commit_args",
            "commit_history_args" => "commands::commit_history_args",
            _ => anyhow::bail!("unknown Git core operation `{operation}`"),
        };

        if self.git_core.is_none() {
            let program = git_core_program()?;
            let mut vm = new_plugin_vm();
            let mut host = NativeCoreHost;
            vm.load_compiled_plugin("red-git-core", program, &mut host)?;
            self.git_core = Some(vm);
        }
        let vm = self
            .git_core
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Git core VM did not initialize"))?;
        let mut host = NativeCoreHost;
        // A structured preview emits several typed records per line. Give only
        // this trusted, 1,000-line-bounded core call enough instructions to do so.
        if preview.is_some() {
            vm.set_instruction_budget(PLUGIN_INSTRUCTION_BUDGET * 4);
        }
        let result = vm.call_export("red-git-core", function, args.to_vec(), &mut host);
        vm.set_instruction_budget(PLUGIN_INSTRUCTION_BUDGET);
        let result = result?;
        let result = normalize_native_core_value(result);
        if let Some((_, summary)) = preview {
            let mut document = result.to_json();
            document["added"] = summary.added.into();
            document["removed"] = summary.removed.into();
            document["total_lines"] = summary.total_lines.into();
            document["truncated"] = (summary.total_lines > summary.preview_lines).into();
            return Ok(Value::Json(document));
        }
        Ok(result)
    }

    fn call_neotree_core(&mut self, operation: &str, args: &[Value]) -> anyhow::Result<Value> {
        if operation == "status_entries" {
            return neotree_status_entries(args.first());
        }
        if operation == "update_panel" {
            const EAGER_TREE_ROWS: usize = 192;
            let id = args
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("Neo-tree panel update requires an id"))?
                .to_string();
            let model = TreePanelModel::from_husk_values(&args[1..])?;
            crate::editor::perf::gauge_max("neotree:total_rows", model.len() as u64);
            if model.len() <= EAGER_TREE_ROWS {
                let rows = (0..model.len())
                    .filter_map(|index| model.row(index))
                    .collect::<Vec<_>>();
                self.send_request(PluginRequest::UpdatePanel { id, rows });
            } else {
                self.send_request(PluginRequest::UpdateTreePanel { id, model });
            }
            return Ok(Value::Unit);
        }

        let function = match operation {
            "normalize_path" => "path::normalize",
            "path_name" => "path::name",
            "path_parent" => "path::parent",
            "path_join" => "path::join",
            "workspace_path" => "path::workspace",
            "name_extension" => "path::extension",
            "basename" => "path::basename",
            "tree_path" => "path::tree_path",
            "reveal_parts" => "path::reveal_parts",
            "build_rows" => "tree::build_rows",
            _ => anyhow::bail!("unknown Neo-tree core operation `{operation}`"),
        };

        if self.neotree_core.is_none() {
            let program = neotree_core_program()?;
            let mut vm = new_plugin_vm();
            let mut host = NativeCoreHost;
            vm.load_compiled_plugin("red-neotree-core", program, &mut host)?;
            self.neotree_core = Some(vm);
        }
        let vm = self
            .neotree_core
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Neo-tree core VM did not initialize"))?;
        let mut host = NativeCoreHost;
        // A deep, decorated tree can emit several segments per row. Only this
        // trusted core operation receives extra fuel; its output is capped at 200 rows.
        if operation == "build_rows" {
            vm.set_instruction_budget(PLUGIN_INSTRUCTION_BUDGET * 4);
        }
        let result = vm.call_export("red-neotree-core", function, args.to_vec(), &mut host);
        vm.set_instruction_budget(PLUGIN_INSTRUCTION_BUDGET);
        let result = result?;
        Ok(normalize_native_core_value(result))
    }
}

#[derive(Default)]
struct GitDiffSummary {
    added: usize,
    removed: usize,
    total_lines: usize,
    preview_lines: usize,
}

fn git_detail_preview(args: &[Value]) -> anyhow::Result<(Vec<Value>, GitDiffSummary)> {
    const PREVIEW_LINES: usize = 1000;
    let text = red_required_string(args, 0, "Git detail document")?;
    let mut summary = GitDiffSummary::default();
    let mut preview = String::new();
    let mut in_hunk = false;
    for line in text.lines() {
        if summary.preview_lines < PREVIEW_LINES {
            if summary.preview_lines > 0 {
                preview.push('\n');
            }
            preview.push_str(line);
            summary.preview_lines += 1;
        }
        summary.total_lines += 1;
        if line.starts_with("diff --git ") {
            in_hunk = false;
        } else if line.starts_with("@@ ") {
            in_hunk = true;
        } else if in_hunk {
            summary.added += usize::from(line.starts_with('+'));
            summary.removed += usize::from(line.starts_with('-'));
        }
    }
    let mut args = args.to_vec();
    args[0] = Value::String(preview);
    Ok((args, summary))
}

struct NativeCoreHost;

impl Host for NativeCoreHost {
    fn log(&mut self, _message: &str) {}
}

fn git_core_program() -> anyhow::Result<CompiledProgram> {
    let compiled = GIT_CORE_PROGRAM.get_or_init(|| {
        let package = ResolvedPackage::from_sources(
            "plugins/git_core",
            include_str!("../../plugins/git_core/Husk.toml"),
            &[
                (
                    "src/main.hk",
                    include_str!("../../plugins/git_core/src/main.hk"),
                ),
                (
                    "src/status.hk",
                    include_str!("../../plugins/git_core/src/status.hk"),
                ),
                (
                    "src/patch.hk",
                    include_str!("../../plugins/git_core/src/patch.hk"),
                ),
                (
                    "src/commands.hk",
                    include_str!("../../plugins/git_core/src/commands.hk"),
                ),
            ],
            PackageLimits::default(),
        )
        .map_err(|error| format!("failed to resolve embedded Git core: {error}"))?;
        CompiledProgram::compile_package(&package, &CompileOptions::default())
            .map_err(|error| format!("failed to compile embedded Git core: {error}"))
    });
    compiled
        .as_ref()
        .cloned()
        .map_err(|error| anyhow::anyhow!("{error}"))
}

fn neotree_core_program() -> anyhow::Result<CompiledProgram> {
    let compiled = NEOTREE_CORE_PROGRAM.get_or_init(|| {
        let package = ResolvedPackage::from_sources(
            "plugins/neotree_core",
            include_str!("../../plugins/neotree_core/Husk.toml"),
            &[
                (
                    "src/main.hk",
                    include_str!("../../plugins/neotree_core/src/main.hk"),
                ),
                (
                    "src/path.hk",
                    include_str!("../../plugins/neotree_core/src/path.hk"),
                ),
                (
                    "src/status.hk",
                    include_str!("../../plugins/neotree_core/src/status.hk"),
                ),
                (
                    "src/tree.hk",
                    include_str!("../../plugins/neotree_core/src/tree.hk"),
                ),
            ],
            PackageLimits::default(),
        )
        .map_err(|error| format!("failed to resolve embedded Neo-tree core: {error}"))?;
        CompiledProgram::compile_package(&package, &CompileOptions::default())
            .map_err(|error| format!("failed to compile embedded Neo-tree core: {error}"))
    });
    compiled
        .as_ref()
        .cloned()
        .map_err(|error| anyhow::anyhow!("{error}"))
}

fn neotree_status_entries(value: Option<&Value>) -> anyhow::Result<Value> {
    let mut statuses = match value {
        Some(Value::Object(entries)) => entries
            .iter()
            .filter_map(|(path, status)| status.as_str().map(|status| (path.clone(), status)))
            .collect::<Vec<_>>(),
        Some(Value::Json(serde_json::Value::Object(entries))) => entries
            .iter()
            .filter_map(|(path, status)| status.as_str().map(|status| (path.clone(), status)))
            .collect::<Vec<_>>(),
        Some(Value::Unit | Value::Null | Value::Missing(_)) | None => Vec::new(),
        Some(_) => anyhow::bail!("Neo-tree status index must be an object"),
    };
    statuses.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
    Ok(Value::Array(Arc::new(
        statuses
            .into_iter()
            .map(|(path, status)| Value::Struct {
                type_name: "PathStatus".to_string(),
                fields: Arc::new(BTreeMap::from([
                    ("path".to_string(), Value::String(path)),
                    ("status".to_string(), Value::String(status.to_string())),
                ])),
            })
            .collect(),
    )))
}

fn normalize_native_core_value(value: Value) -> Value {
    match value {
        Value::Variant {
            type_name,
            case,
            fields,
        } if type_name == "Option" => match (case.as_str(), fields.as_slice()) {
            ("None", []) => Value::Null,
            ("Some", [value]) => normalize_native_core_value(value.clone()),
            _ => Value::Null,
        },
        Value::Array(values) => Value::Array(Arc::new(
            values
                .iter()
                .cloned()
                .map(normalize_native_core_value)
                .collect(),
        )),
        Value::Tuple(values) => Value::Tuple(Arc::new(
            values
                .iter()
                .cloned()
                .map(normalize_native_core_value)
                .collect(),
        )),
        Value::Object(fields) => Value::Object(Arc::new(
            fields
                .iter()
                .map(|(name, value)| (name.clone(), normalize_native_core_value(value.clone())))
                .collect(),
        )),
        Value::Struct { fields, .. } => Value::Object(Arc::new(
            fields
                .iter()
                .map(|(name, value)| (name.clone(), normalize_native_core_value(value.clone())))
                .collect(),
        )),
        Value::Variant {
            type_name,
            case,
            fields,
        } => Value::Variant {
            type_name,
            case,
            fields: Arc::new(
                fields
                    .iter()
                    .cloned()
                    .map(normalize_native_core_value)
                    .collect(),
            ),
        },
        value => value,
    }
}

impl Host for RedHost {
    fn log(&mut self, message: &str) {
        RedHost::log(self, message);
    }

    fn call_module(
        &mut self,
        plugin: &str,
        path: &str,
        args: &[Value],
    ) -> Option<anyhow::Result<Value>> {
        RedHost::call_module(self, plugin, path, args)
    }

    fn register_annotated_function(
        &mut self,
        plugin: &str,
        function: &AnnotatedFunction,
    ) -> anyhow::Result<()> {
        let annotations = super::api::red_function_annotations(
            function.attributes(),
            function.parameter_count(),
            function.return_type(),
        )
        .map_err(|error| {
            anyhow::anyhow!(
                "invalid Red plugin annotation at bytes {}..{}: {}",
                error.span.range.start,
                error.span.range.end,
                error.message
            )
        })?;
        for annotation in annotations {
            match annotation {
                super::api::RedFunctionAnnotation::Command {
                    name,
                    title,
                    category,
                    description,
                    aliases,
                    visible,
                    scope,
                    arguments,
                    completions,
                } => {
                    if let Some(existing) = self.policy().commands.get(&name) {
                        if existing.callback.plugin() == plugin {
                            anyhow::bail!("duplicate command annotation `{name}`");
                        }
                        anyhow::bail!(
                            "command `{name}` is already registered by plugin `{}`",
                            existing.callback.plugin()
                        );
                    }
                    let metadata = CommandMetadata {
                        title,
                        category,
                        description,
                        aliases,
                        visible,
                        scope,
                        arguments,
                        completions,
                    };
                    validate_command_registration(self, function.callback(), &metadata)?;
                    self.policy_mut().commands.insert(
                        name,
                        RedCommand {
                            callback: function.callback().clone(),
                            metadata,
                        },
                    );
                }
                super::api::RedFunctionAnnotation::Event { name } => {
                    let listeners = self.policy_mut().event_listeners.entry(name).or_default();
                    Arc::make_mut(listeners).push(function.callback().clone());
                }
                super::api::RedFunctionAnnotation::StateInitializer => {
                    let Some(husk_ast::TypeExpr {
                        kind: husk_ast::TypeExprKind::Named(name),
                        ..
                    }) = function.return_type()
                    else {
                        anyhow::bail!("state initializer has no named state record type");
                    };
                    if self
                        .policy_mut()
                        .state_initializers
                        .insert(plugin.to_string(), function.callback().clone())
                        .is_some()
                    {
                        anyhow::bail!("duplicate `#[red::state]` initializer for `{plugin}`");
                    }
                    self.policy_mut()
                        .state_record_types
                        .insert(plugin.to_string(), name.name.clone());
                }
                super::api::RedFunctionAnnotation::Config { key } => {
                    if !self
                        .policy_mut()
                        .config_bindings
                        .entry(plugin.to_string())
                        .or_default()
                        .insert(key.clone())
                    {
                        anyhow::bail!("duplicate `#[red::config]` binding for `{plugin}`");
                    }
                    let request_id = self.policy_mut().allocate_request_id();
                    self.policy_mut()
                        .pending_requests
                        .insert(request_id, function.callback().clone());
                    let arguments = key.into_iter().map(Value::String).collect::<Vec<_>>();
                    if let Err(error) = self.request(plugin, request_id, "GetConfig", &arguments) {
                        self.policy_mut().pending_requests.remove(&request_id);
                        return Err(error);
                    }
                }
                super::api::RedFunctionAnnotation::Lifecycle { hook } => {
                    if self
                        .policy_mut()
                        .lifecycle_callbacks
                        .entry(plugin.to_string())
                        .or_default()
                        .insert(hook.clone(), function.callback().clone())
                        .is_some()
                    {
                        anyhow::bail!("duplicate lifecycle annotation `{hook}` for `{plugin}`");
                    }
                }
            }
        }
        Ok(())
    }

    fn pre_activation_callbacks(&self, plugin: &str) -> Vec<Callback> {
        self.policy()
            .state_initializers
            .get(plugin)
            .cloned()
            .into_iter()
            .collect()
    }

    fn complete_pre_activation_callback(
        &mut self,
        plugin: &str,
        callback: &Callback,
        value: Value,
    ) -> anyhow::Result<()> {
        if self.policy().state_initializers.get(plugin) != Some(callback) {
            anyhow::bail!("unknown state initializer for plugin `{plugin}`");
        }
        let Value::Struct { type_name, .. } = &value else {
            anyhow::bail!("`#[red::state]` initializer must return a state record");
        };
        let expected_type = self
            .policy()
            .state_record_types
            .get(plugin)
            .ok_or_else(|| {
                anyhow::anyhow!("state initializer has no record type for `{plugin}`")
            })?;
        anyhow::ensure!(
            type_name == expected_type,
            "state initializer returned `{type_name}` instead of `{expected_type}`"
        );
        self.policy_mut()
            .typed_states
            .insert(plugin.to_string(), value);
        Ok(())
    }

    fn lifecycle_callback(&self, plugin: &str, hook: &str) -> Option<Callback> {
        self.policy()
            .lifecycle_callbacks
            .get(plugin)
            .and_then(|callbacks| callbacks.get(hook))
            .cloned()
    }

    fn preserves_nominal_record(&self, plugin: &str, type_name: &str) -> bool {
        self.policy()
            .state_record_types
            .get(plugin)
            .is_some_and(|expected| expected == type_name)
    }

    fn begin_reload_replacement(&mut self, plugin: &str) {
        RedHost::begin_reload_replacement(self, plugin);
    }

    fn begin_reload_teardown(&mut self, plugin: &str) {
        RedHost::begin_reload_teardown(self, plugin);
    }
}

fn red_required_string<'a>(
    args: &'a [Value],
    index: usize,
    function: &str,
) -> anyhow::Result<&'a str> {
    args.get(index)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("`{function}` argument {index} must be a string"))
}

fn red_required_callback<'a>(
    args: &'a [Value],
    index: usize,
    function: &str,
) -> anyhow::Result<&'a Callback> {
    match args.get(index) {
        Some(Value::Callback(callback)) => Ok(callback),
        _ => anyhow::bail!("`{function}` argument {index} must be a function callback"),
    }
}

fn red_picker_handlers(
    plugin: &str,
    value: &Value,
    function: &str,
) -> anyhow::Result<PickerHandlers> {
    let fields = match value {
        Value::Struct { type_name, fields } => {
            anyhow::ensure!(
                type_name == "PickerHandlers",
                "`{function}` handlers must be PickerHandlers, found {type_name}"
            );
            fields
        }
        // Red still compiles plugins under the legacy semantic profile, which erases
        // nominal struct identity at runtime. Static checking has already established
        // the PickerHandlers type before this adapter receives the native object.
        Value::Object(fields) => fields,
        _ => anyhow::bail!("`{function}` handlers must be a PickerHandlers value"),
    };

    let callback = |name: &str| -> anyhow::Result<Option<Callback>> {
        match fields.get(name) {
            None | Some(Value::Unit | Value::Null | Value::Missing(_)) => Ok(None),
            Some(Value::Callback(callback)) => {
                anyhow::ensure!(
                    callback.plugin() == plugin,
                    "`{function}` handler `{name}` belongs to plugin `{}`, not `{plugin}`",
                    callback.plugin()
                );
                Ok(Some(callback.clone()))
            }
            Some(_) => anyhow::bail!("`{function}` handler `{name}` must be a function callback"),
        }
    };

    let handlers = PickerHandlers {
        selected: callback("selected")?,
        cancelled: callback("cancelled")?,
        changed: callback("changed")?,
        query: callback("query")?,
        action: callback("action")?,
    };
    anyhow::ensure!(
        !handlers.is_empty(),
        "`{function}` requires at least one picker handler"
    );
    Ok(handlers)
}

fn red_composer_handlers(
    plugin: &str,
    value: &Value,
    function: &str,
) -> anyhow::Result<ComposerHandlers> {
    let fields = match value {
        Value::Struct { type_name, fields } => {
            anyhow::ensure!(
                type_name == "ComposerHandlers",
                "`{function}` handlers must be ComposerHandlers, found {type_name}"
            );
            fields
        }
        Value::Object(fields) => fields,
        _ => anyhow::bail!("`{function}` handlers must be a ComposerHandlers value"),
    };

    let callback = |name: &str| -> anyhow::Result<Option<Callback>> {
        match fields.get(name) {
            None | Some(Value::Unit | Value::Null | Value::Missing(_)) => Ok(None),
            Some(Value::Callback(callback)) => {
                anyhow::ensure!(
                    callback.plugin() == plugin,
                    "`{function}` handler `{name}` belongs to plugin `{}`, not `{plugin}`",
                    callback.plugin()
                );
                Ok(Some(callback.clone()))
            }
            Some(_) => anyhow::bail!("`{function}` handler `{name}` must be a function callback"),
        }
    };

    let handlers = ComposerHandlers {
        submitted: callback("submitted")?,
        cancelled: callback("cancelled")?,
    };
    anyhow::ensure!(
        !handlers.is_empty(),
        "`{function}` requires at least one composer handler"
    );
    Ok(handlers)
}

fn red_required_value_array(
    args: &[Value],
    index: usize,
    function: &str,
) -> anyhow::Result<Arc<Vec<Value>>> {
    match args.get(index) {
        Some(Value::Array(values)) => Ok(values.clone()),
        Some(Value::Json(serde_json::Value::Array(values))) => Ok(Arc::new(
            values
                .iter()
                .cloned()
                .map(Value::from_json)
                .collect::<Vec<_>>(),
        )),
        _ => anyhow::bail!("`{function}` argument {index} must be an array"),
    }
}

fn red_value_to_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Int(value) => Some(*value as f64),
        Value::Float(value) => Some(*value),
        _ => None,
    }
}

fn red_value_to_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Int(value) => Some(*value),
        Value::Float(value) => Some(*value as i64),
        Value::String(value) => value.parse().ok(),
        Value::Json(serde_json::Value::Number(value)) => value.as_i64(),
        Value::Json(serde_json::Value::String(value)) => value.parse().ok(),
        _ => None,
    }
}

fn red_value_to_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(value) => Some(*value),
        Value::Json(serde_json::Value::Bool(value)) => Some(*value),
        _ => None,
    }
}

fn red_value_to_plain_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Json(serde_json::Value::String(value)) => Some(value.clone()),
        _ => None,
    }
}

fn red_text_field_value(value: &Value) -> Option<String> {
    let object = value.to_json();
    object
        .get("text")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            object
                .get("bytes")
                .and_then(serde_json::Value::as_str)
                .and_then(red_decode_base64)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        })
}

fn red_decode_base64(encoded: &str) -> Option<Vec<u8>> {
    let mut output = Vec::new();
    let mut quartet = [0_u8; 4];
    let mut count = 0;
    for byte in encoded.bytes().filter(|byte| !byte.is_ascii_whitespace()) {
        if byte == b'=' {
            break;
        }
        quartet[count] = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        count += 1;
        if count == 4 {
            output.push((quartet[0] << 2) | (quartet[1] >> 4));
            output.push((quartet[1] << 4) | (quartet[2] >> 2));
            output.push((quartet[2] << 6) | quartet[3]);
            count = 0;
        }
    }
    match count {
        0 => Some(output),
        2 => {
            output.push((quartet[0] << 2) | (quartet[1] >> 4));
            Some(output)
        }
        3 => {
            output.push((quartet[0] << 2) | (quartet[1] >> 4));
            output.push((quartet[1] << 4) | (quartet[2] >> 2));
            Some(output)
        }
        _ => None,
    }
}

fn red_color_channels(value: &Value) -> Option<(u8, u8, u8)> {
    if let Value::String(value) = value {
        let hex = value.strip_prefix('#')?;
        if hex.len() < 6 {
            return None;
        }
        return Some((
            u8::from_str_radix(&hex[0..2], 16).ok()?,
            u8::from_str_radix(&hex[2..4], 16).ok()?,
            u8::from_str_radix(&hex[4..6], 16).ok()?,
        ));
    }
    let value = value.to_json();
    let channels = value.get("Rgb").or_else(|| value.get("Rgba"))?;
    Some((
        u8::try_from(channels.get("r")?.as_u64()?).ok()?,
        u8::try_from(channels.get("g")?.as_u64()?).ok()?,
        u8::try_from(channels.get("b")?.as_u64()?).ok()?,
    ))
}

fn red_normalize_string_index(index: i64, len: i64) -> i64 {
    if index < 0 {
        (len + index).clamp(0, len)
    } else {
        index.clamp(0, len)
    }
}

fn red_value_to_log_string(value: &Value) -> String {
    match value {
        Value::Unit => "()".to_string(),
        Value::Null | Value::Missing(_) => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Int(value) => value.to_string(),
        Value::Float(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(value) | Value::Tuple(value) => {
            serde_json::Value::Array(value.iter().map(Value::to_json).collect()).to_string()
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
                .map(|(key, value)| (key.clone(), value.to_json()))
                .collect(),
        )
        .to_string(),
        Value::Struct { type_name, fields } => format!(
            "{type_name} {}",
            serde_json::Value::Object(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), value.to_json()))
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
                .map(red_value_to_log_string)
                .collect::<Vec<_>>()
                .join(", ");
            if fields.is_empty() {
                format!("{type_name}::{case}")
            } else {
                format!("{type_name}::{case}({payload})")
            }
        }
        Value::Resource { type_name, .. } => format!("<resource:{type_name}>"),
        Value::Json(value) => value.to_string(),
        Value::Callback(callback) => {
            format!("{}::{}", callback.plugin(), callback.function())
        }
        Value::Closure(_) => "<closure>".to_string(),
    }
}

fn first_json(args: &[Value]) -> anyhow::Result<serde_json::Value> {
    match args.first() {
        Some(value) => Ok(value_to_json(value)),
        _ => anyhow::bail!("host action expected a JSON event payload"),
    }
}

fn json_str<'a>(value: &'a serde_json::Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

fn json_usize_at(value: &serde_json::Value, path: &[&str]) -> usize {
    let mut cursor = value;
    for key in path {
        let Some(next) = cursor.get(key) else {
            return 0;
        };
        cursor = next;
    }
    cursor.as_u64().map_or(0, |value| value as usize)
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::Unit | Value::Null | Value::Missing(_) => String::new(),
        Value::Bool(value) => value.to_string(),
        Value::Int(value) => value.to_string(),
        Value::Float(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(_)
        | Value::Tuple(_)
        | Value::Range { .. }
        | Value::Object(_)
        | Value::Struct { .. }
        | Value::Variant { .. } => value.to_json().to_string(),
        Value::Resource { type_name, .. } => format!("<resource:{type_name}>"),
        Value::Json(value) => value.to_string(),
        Value::Callback(_) => "<callback>".to_string(),
        Value::Closure(_) => "<closure>".to_string(),
    }
}

fn value_to_query_string(value: &Value) -> String {
    match value {
        Value::Json(value) => value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_string),
        value => value_to_string(value),
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
        Value::Array(values) | Value::Tuple(values) => {
            serde_json::Value::Array(values.iter().map(value_to_json).collect())
        }
        Value::Object(fields) | Value::Struct { fields, .. } => serde_json::Value::Object(
            fields
                .iter()
                .map(|(name, value)| (name.clone(), value_to_json(value)))
                .collect(),
        ),
        Value::Variant {
            type_name,
            case,
            fields,
        } if type_name == "Option" => match (case.as_str(), fields.as_slice()) {
            ("None", []) => serde_json::Value::Null,
            ("Some", [value]) => value_to_json(value),
            _ => serde_json::Value::Null,
        },
        Value::Variant {
            type_name,
            case,
            fields,
        } => serde_json::json!({
            "$type": type_name,
            "$case": case,
            "$fields": fields.iter().map(value_to_json).collect::<Vec<_>>(),
        }),
        Value::Range { .. } => value.to_json(),
        Value::Resource { .. } => serde_json::Value::Null,
        Value::Json(value) => value.clone(),
        Value::Callback(_) | Value::Closure(_) => serde_json::Value::Null,
    }
}

fn value_to_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Int(value) => u64::try_from(*value).ok(),
        Value::Float(value) if *value >= 0.0 => Some(*value as u64),
        Value::String(value) => value.parse().ok(),
        _ => None,
    }
}

fn value_to_i32(value: &Value) -> Option<i32> {
    match value {
        Value::Int(value) => i32::try_from(*value).ok(),
        Value::Float(value) if *value >= 0.0 && *value <= f64::from(i32::MAX) => {
            Some(*value as i32)
        }
        Value::String(value) => value.parse().ok(),
        _ => None,
    }
}

#[derive(Clone)]
pub struct Runtime {
    inner: Arc<Mutex<RuntimeInner>>,
    dispatcher: Arc<Dispatcher<PluginRequest, PluginResponse>>,
}

/// A command currently registered by an active Husk plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredPluginCommand {
    /// Exact, case-sensitive command name.
    pub name: String,
    /// Plugin that owns the command.
    pub plugin: String,
    /// User-facing command information supplied during registration.
    pub metadata: CommandMetadata,
}

struct RuntimeInner {
    plugins: HashMap<String, husk_runtime::Vm>,
    host: RedHost,
    anonymous_module_count: usize,
    typecheck_enabled: bool,
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl Runtime {
    pub fn new() -> Self {
        Self::try_new().expect("failed to initialize plugin runtime")
    }

    pub fn try_new() -> anyhow::Result<Self> {
        Self::try_new_with_permissions(HashMap::new())
    }

    pub fn new_with_permissions(process_permissions: HashMap<String, PluginPermissions>) -> Self {
        Self::try_new_with_permissions(process_permissions)
            .expect("failed to initialize plugin runtime")
    }

    pub fn try_new_with_permissions(
        process_permissions: HashMap<String, PluginPermissions>,
    ) -> anyhow::Result<Self> {
        let dispatcher = Arc::new(Dispatcher::new());
        ACTION_DISPATCHER.bind(dispatcher.clone());
        Ok(Self {
            inner: Arc::new(Mutex::new(RuntimeInner {
                plugins: HashMap::new(),
                host: RedHost::with_dispatcher(process_permissions, dispatcher.clone()),
                anonymous_module_count: 0,
                typecheck_enabled: true,
            })),
            dispatcher,
        })
    }

    pub(crate) fn send_request(&self, request: PluginRequest) {
        self.dispatcher.send_request(request);
    }

    pub(crate) fn try_recv_request(&self) -> Option<PluginRequest> {
        self.dispatcher.try_recv_request()
    }

    pub fn set_typecheck_enabled(&mut self, enabled: bool) {
        self.inner.lock().unwrap().typecheck_enabled = enabled;
    }

    pub(super) fn typecheck_enabled(&self) -> bool {
        self.inner.lock().unwrap().typecheck_enabled
    }

    pub async fn load_plugin(&mut self, name: &str, source: &str) -> anyhow::Result<()> {
        self.load_plugin_at(name, format!("plugins/{name}.hk"), source)
            .await
    }

    pub async fn load_plugin_at(
        &mut self,
        name: &str,
        path: impl Into<String>,
        source: &str,
    ) -> anyhow::Result<()> {
        let _span = crate::editor::perf::PerfSpan::with_detail("husk:load", name);
        let mut inner = self.inner.lock().unwrap();
        let path = path.into();
        let program = if inner.typecheck_enabled {
            compile_plugin_source(name, &path, source)?
        } else {
            CompiledProgram::compile_at(
                name,
                &path,
                source,
                &CompileOptions::legacy_runtime_compatibility(),
            )?
        };
        let payload_schema = PluginPayloadSchema::for_source(program.syntax());
        Self::activate_compiled_plugin(&mut inner, name, program, payload_schema)
    }

    pub(super) fn load_precompiled_plugin(
        &mut self,
        name: &str,
        prepared: PreparedStartupPlugin,
    ) -> anyhow::Result<()> {
        let _span = crate::editor::perf::PerfSpan::with_detail("husk:load", name);
        let mut inner = self.inner.lock().unwrap();
        Self::activate_compiled_plugin(&mut inner, name, prepared.program, prepared.payload_schema)
    }

    fn activate_compiled_plugin(
        inner: &mut RuntimeInner,
        name: &str,
        program: CompiledProgram,
        payload_schema: PluginPayloadSchema,
    ) -> anyhow::Result<()> {
        let RuntimeInner { plugins, host, .. } = inner;
        let was_loaded = plugins.contains_key(name);
        if was_loaded {
            host.begin_reload();
        } else {
            host.begin_initial_activation();
        }
        host.staged_policy
            .as_mut()
            .expect("reload staging must be active")
            .payload_schemas
            .insert(name.to_string(), Arc::new(payload_schema));
        let vm = plugins
            .entry(name.to_string())
            .or_insert_with(new_plugin_vm);
        let result = vm.reload_compiled_plugin(name, program, host);
        if result.is_ok() {
            host.commit_reload();
        } else {
            host.rollback_reload();
            if !was_loaded {
                plugins.remove(name);
            }
        }
        result
    }

    /// Resolves and transactionally loads a multi-file Husk package.
    pub async fn load_plugin_package(
        &mut self,
        name: &str,
        manifest_path: &std::path::Path,
    ) -> anyhow::Result<()> {
        let _span = crate::editor::perf::PerfSpan::with_detail("husk:load_package", name);
        let package = ResolvedPackage::open(manifest_path, PackageLimits::default())?;
        let mut inner = self.inner.lock().unwrap();
        let program = if inner.typecheck_enabled {
            compile_plugin_package(name, &package)?
        } else {
            CompiledProgram::compile_package(
                &package,
                &CompileOptions::legacy_runtime_compatibility(),
            )?
        };
        let payload_schema = PluginPayloadSchema::for_package(&package);
        let RuntimeInner { plugins, host, .. } = &mut *inner;
        let was_loaded = plugins.contains_key(name);
        if was_loaded {
            host.begin_reload();
        } else {
            host.begin_initial_activation();
        }
        host.staged_policy
            .as_mut()
            .expect("reload staging must be active")
            .payload_schemas
            .insert(name.to_string(), Arc::new(payload_schema));
        let vm = plugins
            .entry(name.to_string())
            .or_insert_with(new_plugin_vm);
        let result = vm.reload_compiled_plugin(name, program, host);
        if result.is_ok() {
            host.commit_reload();
        } else {
            host.rollback_reload();
            if !was_loaded {
                plugins.remove(name);
            }
        }
        result
    }

    pub fn unload_plugin(&mut self, name: &str) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let RuntimeInner { plugins, host, .. } = &mut *inner;
        let result = plugins
            .remove(name)
            .map_or(Ok(()), |mut vm| vm.deactivate_plugin(name, host));
        host.remove_plugin(name);
        host.process_manager.shutdown_plugin(name);
        result
    }

    #[must_use]
    pub fn command_plugin(&self, command: &str) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        resolve_command(&inner.host.policy().commands, command)
            .map(|(_, command, _)| command.callback.plugin().to_string())
    }

    /// Returns the key-dispatch scope declared by an active plugin command.
    #[must_use]
    pub fn command_scope(&self, command: &str) -> Option<CommandScope> {
        let inner = self.inner.lock().unwrap();
        resolve_command(&inner.host.policy().commands, command)
            .map(|(_, command, _)| command.metadata.scope)
    }

    /// Returns the active plugin commands in a stable order for discovery UI.
    #[must_use]
    pub fn registered_commands(&self) -> Vec<RegisteredPluginCommand> {
        let inner = self.inner.lock().unwrap();
        let mut commands = inner
            .host
            .policy()
            .commands
            .iter()
            .map(|(name, command)| RegisteredPluginCommand {
                name: name.clone(),
                plugin: command.callback.plugin().to_string(),
                metadata: command.metadata.clone(),
            })
            .collect::<Vec<_>>();
        commands.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        commands
    }

    pub async fn add_module(&mut self, code: &str) -> anyhow::Result<()> {
        let name = {
            let mut inner = self.inner.lock().unwrap();
            inner.anonymous_module_count += 1;
            format!("module-{}", inner.anonymous_module_count)
        };
        self.load_plugin(&name, code).await
    }

    pub async fn run(&mut self, code: &str) -> anyhow::Result<()> {
        self.add_module(code).await
    }

    pub async fn execute_command(&mut self, command: &str) -> anyhow::Result<()> {
        let _span = crate::editor::perf::PerfSpan::with_detail("husk:command", command);
        let mut inner = self.inner.lock().unwrap();
        let RuntimeInner { plugins, host, .. } = &mut *inner;
        let (name, registered, raw_args) = resolve_command(&host.policy().commands, command)
            .ok_or_else(|| anyhow::anyhow!("unknown Husk plugin command `{command}`"))?;
        let callback = registered.callback.clone();
        let args = if registered.metadata.arguments {
            let payload = command_invocation_payload(name, raw_args);
            vec![decoded_callback_payload(host, &callback, 0, &payload)?]
        } else {
            Vec::new()
        };
        call_plugin_callback(plugins, host, &callback, args).map(drop)
    }

    pub async fn notify(&mut self, event: &str, args: serde_json::Value) -> anyhow::Result<()> {
        let _span = crate::editor::perf::PerfSpan::with_detail("husk:notify", event);
        let mut inner = self.inner.lock().unwrap();
        let RuntimeInner { plugins, host, .. } = &mut *inner;
        let Some(callbacks) = host.policy().event_listeners.get(event).cloned() else {
            return Ok(());
        };
        for callback in callbacks.iter() {
            let argument = decoded_callback_payload(host, callback, 0, &args)?;
            call_plugin_callback(plugins, host, callback, vec![argument])?;
        }
        Ok(())
    }

    pub fn notify_isolated(
        &mut self,
        event: &str,
        args: serde_json::Value,
    ) -> Vec<(String, anyhow::Error)> {
        let mut inner = self.inner.lock().unwrap();
        let RuntimeInner { plugins, host, .. } = &mut *inner;
        let Some(callbacks) = host.policy().event_listeners.get(event).cloned() else {
            return Vec::new();
        };
        callbacks
            .iter()
            .filter_map(|callback| {
                decoded_callback_payload(host, callback, 0, &args)
                    .and_then(|argument| {
                        call_plugin_callback(plugins, host, callback, vec![argument])
                    })
                    .err()
                    .map(|error| (callback.plugin().to_string(), error))
            })
            .collect()
    }

    pub fn notify_plugin_isolated(
        &mut self,
        plugin: &str,
        event: &str,
        args: serde_json::Value,
    ) -> Vec<(String, anyhow::Error)> {
        let mut inner = self.inner.lock().unwrap();
        let RuntimeInner { plugins, host, .. } = &mut *inner;
        let Some(callbacks) = host.policy().event_listeners.get(event).cloned() else {
            return Vec::new();
        };
        callbacks
            .iter()
            .filter(|callback| callback.plugin() == plugin)
            .filter_map(|callback| {
                decoded_callback_payload(host, callback, 0, &args)
                    .and_then(|argument| {
                        call_plugin_callback(plugins, host, callback, vec![argument])
                    })
                    .err()
                    .map(|error| (plugin.to_string(), error))
            })
            .collect()
    }

    #[must_use]
    pub fn picker_plugin(&self, handle: PickerHandle) -> Option<String> {
        self.inner
            .lock()
            .unwrap()
            .host
            .policy()
            .picker_handlers
            .get(&handle)
            .map(|registration| registration.plugin.clone())
    }

    pub fn notify_picker(
        &mut self,
        handle: PickerHandle,
        event: PickerCallback,
    ) -> anyhow::Result<bool> {
        let mut inner = self.inner.lock().unwrap();
        let RuntimeInner { plugins, host, .. } = &mut *inner;
        let kind = event.kind();
        let registration = if kind.is_terminal() {
            host.policy_mut().picker_handlers.remove(&handle)
        } else {
            host.policy().picker_handlers.get(&handle).cloned()
        };
        let Some(registration) = registration else {
            return Ok(false);
        };
        let Some(callback) = registration.handlers.callback(kind).cloned() else {
            return Ok(true);
        };
        call_plugin_callback(plugins, host, &callback, vec![picker_callback_value(event)])?;
        Ok(true)
    }

    pub fn release_picker(&mut self, handle: PickerHandle) -> bool {
        self.inner
            .lock()
            .unwrap()
            .host
            .policy_mut()
            .picker_handlers
            .remove(&handle)
            .is_some()
    }

    #[must_use]
    pub fn composer_plugin(&self, handle: ComposerHandle) -> Option<String> {
        self.inner
            .lock()
            .unwrap()
            .host
            .policy()
            .composer_handlers
            .get(&handle)
            .map(|registration| registration.plugin.clone())
    }

    pub fn notify_composer(
        &mut self,
        handle: ComposerHandle,
        event: ComposerCallback,
    ) -> anyhow::Result<bool> {
        let mut inner = self.inner.lock().unwrap();
        let RuntimeInner { plugins, host, .. } = &mut *inner;
        let registration = host.policy_mut().composer_handlers.remove(&handle);
        let Some(registration) = registration else {
            return Ok(false);
        };
        let Some(callback) = registration.handlers.callback(event.kind()).cloned() else {
            return Ok(true);
        };
        call_plugin_callback(
            plugins,
            host,
            &callback,
            vec![composer_callback_value(event)],
        )?;
        Ok(true)
    }

    pub fn release_composer(&mut self, handle: ComposerHandle) -> bool {
        self.inner
            .lock()
            .unwrap()
            .host
            .policy_mut()
            .composer_handlers
            .remove(&handle)
            .is_some()
    }

    pub async fn resolve_request(
        &mut self,
        request_id: RequestId,
        payload: serde_json::Value,
    ) -> anyhow::Result<bool> {
        let mut inner = self.inner.lock().unwrap();
        let RuntimeInner { plugins, host, .. } = &mut *inner;
        let Some(callback) = host.policy_mut().pending_requests.remove(&request_id) else {
            return Ok(false);
        };
        let payload = decoded_callback_payload(host, &callback, 0, &payload)?;
        call_plugin_callback(
            plugins,
            host,
            &callback,
            vec![payload, Value::Int(request_id.get())],
        )?;
        Ok(true)
    }

    #[must_use]
    pub fn request_plugin(&self, request_id: RequestId) -> Option<String> {
        self.inner
            .lock()
            .unwrap()
            .host
            .policy()
            .pending_requests
            .get(&request_id)
            .map(|callback| callback.plugin().to_string())
    }

    pub fn set_snapshot(&mut self, name: impl Into<String>, value: serde_json::Value) {
        let mut inner = self.inner.lock().unwrap();
        inner.host.set_snapshot(name, value);
    }

    /// Replaces only the cursor in an existing viewport snapshot.
    ///
    /// Shared row and metadata values remain untouched, and previously cloned
    /// snapshots continue to observe their original cursor.
    #[must_use]
    pub fn update_viewport_cursor(&mut self, cursor: serde_json::Value) -> bool {
        self.inner
            .lock()
            .unwrap()
            .host
            .update_viewport_cursor(cursor)
    }

    pub fn poll_process_events(&mut self) -> Vec<serde_json::Value> {
        let mut inner = self.inner.lock().unwrap();
        inner.host.poll_process_events()
    }

    pub fn poll_timer_callbacks(&mut self) -> Vec<PluginRequest> {
        self.inner.lock().unwrap().host.poll_timer_callbacks()
    }

    #[cfg(test)]
    fn pending_timeout_count(&self) -> usize {
        self.inner.lock().unwrap().host.pending_timeouts.len()
    }

    #[cfg(test)]
    fn schedule_test_timeout(&mut self, delay_ms: u64) -> String {
        self.inner.lock().unwrap().host.schedule_timeout(delay_ms)
    }

    #[cfg(test)]
    fn cancel_test_timeout(&mut self, timer_id: &str) {
        self.inner.lock().unwrap().host.cancel_timeout(timer_id);
    }

    pub async fn before_exit(&mut self, snapshot: serde_json::Value) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let RuntimeInner { plugins, host, .. } = &mut *inner;
        let mut names = plugins.keys().cloned().collect::<Vec<_>>();
        names.sort_unstable();
        for name in names {
            if let Some(vm) = plugins.get_mut(&name) {
                vm.before_exit(snapshot.clone(), host)?;
            }
        }
        Ok(())
    }

    pub async fn deactivate_all(&mut self) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let RuntimeInner { plugins, host, .. } = &mut *inner;
        let mut names = plugins.keys().cloned().collect::<Vec<_>>();
        names.sort_unstable();
        let mut first_error = None;
        for name in names {
            let Some(mut vm) = plugins.remove(&name) else {
                continue;
            };
            if let Err(error) = vm.deactivate_all(host) {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        host.clear_policy();
        first_error.map_or(Ok(()), Err)
    }
}

fn new_plugin_vm() -> husk_runtime::Vm {
    let mut vm = husk_runtime::Vm::new();
    vm.set_instruction_budget(PLUGIN_INSTRUCTION_BUDGET);
    vm.set_instance_generation(NEXT_PLUGIN_VM_GENERATION.fetch_add(1, Ordering::Relaxed));
    vm
}

fn call_plugin_callback(
    plugins: &mut HashMap<String, husk_runtime::Vm>,
    host: &mut RedHost,
    callback: &Callback,
    args: Vec<Value>,
) -> anyhow::Result<Value> {
    let vm = plugins.get_mut(callback.plugin()).ok_or_else(|| {
        anyhow::anyhow!(
            "Husk callback references unloaded plugin `{}`",
            callback.plugin()
        )
    })?;
    vm.call_callback(callback, args, host)
}

/// Exact legacy command names win over argument-aware prefix resolution.
fn resolve_command<'a>(
    commands: &'a HashMap<String, RedCommand>,
    input: &'a str,
) -> Option<(&'a str, &'a RedCommand, &'a str)> {
    if let Some((name, command)) = commands.get_key_value(input) {
        return Some((name, command, ""));
    }
    let (name, raw_args) = crate::command::split_invocation(input);
    let (name, command) = commands.get_key_value(name)?;
    command
        .metadata
        .arguments
        .then_some((name, command, raw_args))
}

fn command_invocation_payload(name: &str, raw_args: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "args": raw_args.split_whitespace().collect::<Vec<_>>(),
        "raw_args": raw_args,
    })
}

fn validate_command_registration(
    host: &RedHost,
    callback: &Callback,
    metadata: &CommandMetadata,
) -> anyhow::Result<()> {
    validate_command_arguments(metadata.arguments, &metadata.completions)?;
    if metadata.arguments {
        if let Some(parameters) = host
            .policy()
            .payload_schemas
            .get(callback.plugin())
            .and_then(|schema| schema.callback_parameters.get(callback.function()))
        {
            anyhow::ensure!(
                matches!(parameters.as_slice(), [TypeExpr { kind: TypeExprKind::Named(name), .. }]
                    if matches!(name.name.as_str(), "CommandInvocation" | "Json" | "JsValue")),
                "argument-aware command callback must take one CommandInvocation or Json parameter"
            );
        }
        decoded_callback_payload(host, callback, 0, &command_invocation_payload("", ""))?;
    }
    Ok(())
}

fn decoded_callback_payload(
    host: &RedHost,
    callback: &Callback,
    index: usize,
    payload: &serde_json::Value,
) -> anyhow::Result<Value> {
    host.policy()
        .payload_schemas
        .get(callback.plugin())
        .map_or_else(
            || Ok(Value::from_json(payload.clone())),
            |schema| schema.callback_argument(callback, index, payload),
        )
}

fn picker_callback_value(event: PickerCallback) -> Value {
    match event {
        PickerCallback::Selected(item) | PickerCallback::Changed(item) => {
            typed_json_value("PickerItem", serde_json::to_value(item).unwrap_or_default())
        }
        PickerCallback::Cancelled => Value::Struct {
            type_name: "PickerCancelled".to_string(),
            fields: Arc::new(BTreeMap::new()),
        },
        PickerCallback::Query(query) => Value::String(query),
        PickerCallback::Action {
            action,
            item,
            query,
        } => {
            let mut fields = BTreeMap::new();
            fields.insert("action".to_string(), Value::String(action));
            fields.insert(
                "item".to_string(),
                item.map_or(Value::Null, |item| {
                    typed_json_value("PickerItem", serde_json::to_value(item).unwrap_or_default())
                }),
            );
            fields.insert("query".to_string(), Value::String(query));
            Value::Struct {
                type_name: "PickerActionEvent".to_string(),
                fields: Arc::new(fields),
            }
        }
    }
}

fn composer_callback_value(event: ComposerCallback) -> Value {
    match event {
        ComposerCallback::Submitted(prompt) => Value::String(prompt),
        ComposerCallback::Cancelled => Value::Struct {
            type_name: "ComposerCancelled".to_string(),
            fields: Arc::new(BTreeMap::new()),
        },
    }
}

fn typed_json_value(type_name: &str, value: serde_json::Value) -> Value {
    let fields = value.as_object().map_or_else(BTreeMap::new, |object| {
        object
            .iter()
            .map(|(name, value)| (name.clone(), Value::from_json(value.clone())))
            .collect()
    });
    Value::Struct {
        type_name: type_name.to_string(),
        fields: Arc::new(fields),
    }
}

fn red_host_ast() -> &'static HuskFile {
    RED_HOST_AST.get_or_init(|| {
        let parsed = husk_parser::parse_str(RED_HOST_DECLARATIONS);
        assert!(
            parsed.errors.is_empty(),
            "Red host declarations must parse: {:?}",
            parsed.errors
        );
        parsed
            .file
            .expect("Red host declarations must produce an AST")
    })
}

fn red_host_payload_schema() -> &'static PluginPayloadSchema {
    RED_HOST_PAYLOAD_SCHEMA.get_or_init(|| {
        let mut schema = PluginPayloadSchema::default();
        schema.add_module(red_host_ast(), &[]);
        schema
    })
}

pub(super) fn compile_startup_plugin(
    name: &str,
    path: &str,
    source: &str,
    typecheck_enabled: bool,
) -> anyhow::Result<PreparedStartupPlugin> {
    let mut program = if typecheck_enabled {
        compile_plugin_source(name, path, source)
    } else {
        CompiledProgram::compile_at(
            name,
            path,
            source,
            &CompileOptions::legacy_runtime_compatibility(),
        )
    }?;
    let payload_schema = PluginPayloadSchema::for_source(program.syntax());
    program.discard_analysis();
    Ok(PreparedStartupPlugin {
        program,
        payload_schema,
    })
}

fn compile_plugin_source(name: &str, path: &str, source: &str) -> anyhow::Result<CompiledProgram> {
    let host = red_host_ast();
    let options = CompileOptions::legacy_runtime_compatibility()
        .with_typecheck(true)
        .with_profile(SemanticProfile::LegacyJavaScript)
        .with_declaration(host.clone());
    let program = CompiledProgram::compile_at(name, path, source, &options)?;
    super::api::validate_parsed_source(name, path, source, program.syntax())?;
    Ok(program)
}

fn compile_plugin_package(
    name: &str,
    package: &ResolvedPackage,
) -> anyhow::Result<CompiledProgram> {
    let host = red_host_ast();
    let options = CompileOptions::legacy_runtime_compatibility()
        .with_typecheck(true)
        .with_profile(SemanticProfile::LegacyJavaScript)
        .with_declaration(host.clone());
    let program = CompiledProgram::compile_package(package, &options)?;
    for module in &package.modules {
        super::api::validate_parsed_source(
            name,
            &module.display_path.to_string_lossy(),
            &module.source,
            &module.syntax,
        )?;
    }
    Ok(program)
}

#[cfg(test)]
fn validate_plugin_source(name: &str, path: &str, source: &str) -> anyhow::Result<()> {
    compile_plugin_source(name, path, source).map(drop)
}

#[allow(dead_code)]
fn _keep_config_used(_: &Config) {}

#[cfg(test)]
mod tests {
    use std::{
        path::{Path, PathBuf},
        process::Command,
        time::{Duration, Instant},
    };

    #[cfg(not(windows))]
    use std::fs;

    use super::*;
    use crate::{color::Color, editor::PluginRequest, ui::PickerPresentation};

    fn drain_requests() {
        while ACTION_DISPATCHER.try_recv_request().is_some() {}
    }

    #[test]
    fn plugin_payload_schemas_reuse_shared_host_type_definitions() {
        let parsed = husk_parser::parse_str("pub fn activate() {}");
        let schema = PluginPayloadSchema::for_source(parsed.file.as_ref().unwrap());

        assert!(!schema.definitions.contains_key("PickerItem"));
        assert!(matches!(
            schema.definition("PickerItem"),
            Some(PayloadTypeDefinition::Record(_))
        ));
    }

    #[tokio::test]
    async fn cloned_reload_policies_share_immutable_plugin_payload_schemas() {
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("schema-sharing", "pub fn activate() {}")
            .await
            .unwrap();
        let inner = runtime.inner.lock().unwrap();
        let cloned = inner.host.policy.clone();

        assert!(Arc::ptr_eq(
            inner
                .host
                .policy
                .payload_schemas
                .get("schema-sharing")
                .unwrap(),
            cloned.payload_schemas.get("schema-sharing").unwrap(),
        ));
    }

    #[tokio::test]
    async fn first_plugin_activation_does_not_clone_an_unused_teardown_policy() {
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("existing", "pub fn activate() {}")
            .await
            .unwrap();
        let mut inner = runtime.inner.lock().unwrap();

        inner.host.begin_initial_activation();

        assert!(inner.host.staged_policy.is_some());
        assert!(inner.host.teardown_policy.is_none());
        assert!(inner.host.policy.payload_schemas.contains_key("existing"));
        inner.host.rollback_reload();
        assert!(inner.host.policy.payload_schemas.contains_key("existing"));
    }

    #[tokio::test]
    async fn scratch_buffer_requests_preserve_syntax_submit_and_cancel_commands() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "scratch_owner",
                r#"
                    struct ScratchCommands { syntax: String, submit: String, cancel: String }
                    pub fn activate() {
                        red::request(
                            "OpenScratchBuffer",
                            opened,
                            "[Prompt].txt",
                            "draft",
                            ScratchCommands {
                                syntax: "gitcommit",
                                submit: "SubmitPrompt",
                                cancel: "CancelPrompt"
                            }
                        );
                    }
                    fn opened(event: Json) {}
                "#,
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenScratchBuffer {
                name,
                text,
                syntax,
                submit_command,
                cancel_command,
                ..
            } => {
                assert_eq!(name, "[Prompt].txt");
                assert_eq!(text, "draft");
                assert_eq!(syntax.as_deref(), Some("gitcommit"));
                assert_eq!(submit_command.as_deref(), Some("SubmitPrompt"));
                assert_eq!(cancel_command.as_deref(), Some("CancelPrompt"));
            }
            _ => panic!("expected a managed scratch-buffer request"),
        }
    }

    #[tokio::test]
    async fn husk_can_replace_text_panel_composer_history() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "history_owner",
                r#"
                    pub fn activate() {
                        red::execute(
                            "SetTextPanelComposerHistory",
                            "agent",
                            ["newest", "older"]
                        );
                    }
                "#,
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::SetTextPanelComposerHistory { id, history } => {
                assert_eq!(id, "agent");
                assert_eq!(history, ["newest", "older"]);
            }
            _ => panic!("expected text-panel composer history update"),
        }
    }

    fn recv_agent_composer() -> (ComposerHandle, Option<String>, String, Vec<String>) {
        loop {
            match ACTION_DISPATCHER.recv_request() {
                PluginRequest::OpenCallbackComposer {
                    owner,
                    handle,
                    title,
                    query,
                    history,
                } => {
                    assert_eq!(owner, "agent");
                    return (handle, title, query, history);
                }
                PluginRequest::SetTextPanelComposerHistory { id, .. } => {
                    assert_eq!(id, "agent-conversation");
                }
                _ => panic!("expected callback-scoped agent composer"),
            }
        }
    }

    fn recv_agent_picker(expected_title: &str) -> (PickerHandle, Vec<PickerItem>) {
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker {
                owner,
                handle,
                title,
                items,
                ..
            } => {
                assert_eq!(owner, "agent");
                assert_eq!(title.as_deref(), Some(expected_title));
                (handle, items)
            }
            _ => panic!("expected callback-scoped agent picker"),
        }
    }

    fn expect_agent_model_header() -> RequestId {
        let catalog_request = expect_model_catalog_request();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::SetTextPanelHeaderDetail {
                id,
                detail: Some(detail),
            } => {
                assert_eq!(id, "agent-conversation");
                assert_eq!(detail.text, "Codex");
                assert!(detail.secondary.is_empty());
                assert_eq!(detail.action.as_deref(), Some("model"));
                assert_eq!(detail.shortcut.as_deref(), Some("Alt-m"));
            }
            _ => panic!("expected initial Agent model header"),
        }
        catalog_request
    }

    async fn resolve_prompt_history(runtime: &mut Runtime, history: serde_json::Value) {
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage {
                plugin,
                key,
                request_id,
            } => {
                assert_eq!(plugin, "agent");
                assert_eq!(key, "prompt_history");
                request_id
            }
            _ => panic!("expected prompt-history request"),
        };
        runtime
            .resolve_request(request_id, serde_json::json!({ "value": history }))
            .await
            .unwrap();
    }

    fn recv_optimistic_agent_start(
        prompt: &str,
        expected_history: serde_json::Value,
        expect_panel_creation: bool,
    ) -> RequestId {
        let mut created = false;
        let mut focused = false;
        let mut rendered = false;
        let mut busy = false;
        let mut refreshed = false;
        let mut history_saved = false;

        loop {
            match ACTION_DISPATCHER.recv_request() {
                PluginRequest::CreateTextPanel { id, .. } => {
                    assert_eq!(id, "agent-conversation");
                    created = true;
                }
                PluginRequest::SetTextPanelComposerHistory { id, history } => {
                    assert_eq!(id, "agent-conversation");
                    assert_eq!(serde_json::json!(history), expected_history);
                }
                PluginRequest::SetTextPanelHeaderDetail {
                    id,
                    detail: Some(detail),
                } => {
                    assert_eq!(id, "agent-conversation");
                    assert_eq!(detail.text, "Codex");
                }
                PluginRequest::AgentModelRequest {
                    request:
                        crate::codex::ModelRequest::ReadDefault { .. }
                        | crate::codex::ModelRequest::List,
                    ..
                } => {}
                PluginRequest::UpdateTextPanel { id, blocks } => {
                    assert_eq!(id, "agent-conversation");
                    assert_eq!(
                        blocks
                            .iter()
                            .filter(|block| {
                                block.kind == crate::plugin::TextPanelBlockKind::User
                                    && block.text == prompt
                            })
                            .count(),
                        1,
                        "an optimistic prompt must appear exactly once"
                    );
                    rendered = true;
                }
                PluginRequest::FocusTextPanelComposer { id } => {
                    assert_eq!(id, "agent-conversation");
                    focused = true;
                }
                PluginRequest::SetTextPanelStatus {
                    id,
                    status: Some(status),
                } => {
                    assert_eq!(id, "agent-conversation");
                    assert!(status.busy);
                    assert_eq!(status.label, "Starting agent…");
                    busy = true;
                }
                PluginRequest::Action(Action::Refresh) => {
                    assert!(rendered, "the prompt must render before refresh");
                    assert!(busy, "the startup status must render before refresh");
                    refreshed = true;
                }
                PluginRequest::SetPluginStorage { plugin, key, value } => {
                    assert_eq!(plugin, "agent");
                    assert_eq!(key, "prompt_history");
                    assert_eq!(value, expected_history);
                    assert!(refreshed, "paint the prompt before persisting history");
                    history_saved = true;
                }
                PluginRequest::GetConfig { request_id, key } => {
                    assert_eq!(key.as_deref(), Some("cwd"));
                    assert!(refreshed, "paint the prompt before starting Codex");
                    assert!(history_saved, "save prompt history before requesting Codex");
                    assert_eq!(created, expect_panel_creation);
                    assert_eq!(focused, expect_panel_creation);
                    return request_id;
                }
                _ => panic!("unexpected request before optimistic agent startup"),
            }
        }
    }

    async fn submit_agent_prompt(runtime: &mut Runtime, prompt: &str) {
        runtime.execute_command("AgentPrompt").await.unwrap();
        loop {
            match ACTION_DISPATCHER.recv_request() {
                PluginRequest::SetTextPanelComposerState {
                    id,
                    enabled,
                    status,
                } => {
                    assert_eq!(id, "agent-conversation");
                    assert!(enabled);
                    assert!(status
                        .as_deref()
                        .is_some_and(|status| status.contains("Archived conversation")));
                }
                PluginRequest::FocusTextPanelComposer { id } => {
                    assert_eq!(id, "agent-conversation");
                    runtime
                        .notify(
                            "panel:event:agent-conversation",
                            serde_json::json!({ "action": "submit", "text": prompt }),
                        )
                        .await
                        .unwrap();
                    break;
                }
                PluginRequest::GetPluginStorage {
                    plugin,
                    key,
                    request_id,
                } => {
                    assert_eq!(plugin, "agent");
                    assert_eq!(key, "prompt_history");
                    runtime
                        .resolve_request(request_id, serde_json::json!({ "value": [] }))
                        .await
                        .unwrap();
                    let handle = recv_agent_composer().0;
                    assert!(runtime
                        .notify_composer(handle, ComposerCallback::Submitted(prompt.to_string()))
                        .unwrap());
                    break;
                }
                _ => panic!("expected docked or floating agent composer"),
            }
        }
    }

    async fn open_agent_setup_picker(runtime: &mut Runtime) -> (PickerHandle, Vec<PickerItem>) {
        runtime
            .notify(
                "agent:error",
                serde_json::json!({ "message": "Codex login required" }),
            )
            .await
            .unwrap();
        loop {
            if let PluginRequest::OpenCallbackPicker {
                owner,
                handle,
                title,
                items,
                ..
            } = ACTION_DISPATCHER.recv_request()
            {
                assert_eq!(owner, "agent");
                assert_eq!(title.as_deref(), Some("Retry Codex"));
                return (handle, items);
            }
        }
    }

    fn sample_indent_layout() -> serde_json::Value {
        serde_json::json!({
            "buffer_index": 3,
            "revision": 1,
            "vtop": 0,
            "width": 80,
            "height": 24,
            "cursor": { "x": 0, "y": 2 },
            "indentation": {
                "shift_width": 4,
                "tab_width": 4,
            },
            "rows": [
                { "line": 0, "text": "fn main() {", "first_segment": true },
                { "line": 1, "text": "    if ok {", "first_segment": true },
                { "line": 2, "text": "        call();", "first_segment": true },
                { "line": 3, "text": "    }", "first_segment": true },
                { "line": 4, "text": "}", "first_segment": true }
            ]
        })
    }

    fn non_tabstop_indent_layout() -> serde_json::Value {
        let mut layout = sample_indent_layout();
        layout["cursor"]["y"] = serde_json::json!(1);
        layout["rows"] = serde_json::json!([
            { "line": 0, "text": "fn main() {", "first_segment": true },
            {
                "line": 1,
                "text": format!("{}call();", " ".repeat(39)),
                "first_segment": true
            },
            { "line": 2, "text": "}", "first_segment": true }
        ]);
        layout
    }

    fn sample_indent_editor_info(normal: Color, active: Color) -> serde_json::Value {
        serde_json::json!({
            "theme": {
                "colors": {
                    "editorIndentGuide.background": normal,
                    "editorIndentGuide.activeBackground": active,
                    "editor.foreground": Color::Rgb { r: 220, g: 220, b: 220 },
                    "editor.background": Color::Rgb { r: 16, g: 16, b: 16 },
                },
                "style": {
                    "fg": Color::Rgb { r: 220, g: 220, b: 220 },
                    "bg": Color::Rgb { r: 16, g: 16, b: 16 },
                },
                "gutter_style": { "fg": null },
            }
        })
    }

    fn sample_symbol_payload() -> serde_json::Value {
        serde_json::json!({
            "ok": true,
            "symbols": [{
                "name": "main",
                "detail": "fn()",
                "kind": 12,
                "kind_name": "Function",
                "file": "src/main.rs",
                "range": {
                    "start": { "line": 4, "character": 0 },
                    "end": { "line": 6, "character": 1 }
                },
                "selection_range": {
                    "start": { "line": 4, "character": 3 },
                    "end": { "line": 4, "character": 7 }
                },
                "depth": 0
            }]
        })
    }

    fn sample_symbol_payload_with_count(count: usize) -> serde_json::Value {
        let symbols = (0..count)
            .map(|index| {
                serde_json::json!({
                    "name": format!("symbol_{index}"),
                    "detail": "fn()",
                    "kind": 12,
                    "kind_name": "Function",
                    "file": "src/editor.rs",
                    "range": {
                        "start": { "line": index, "character": 0 },
                        "end": { "line": index, "character": 10 }
                    },
                    "selection_range": {
                        "start": { "line": index, "character": 3 },
                        "end": { "line": index, "character": 9 }
                    },
                    "depth": 0
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "ok": true,
            "symbols": symbols,
        })
    }

    fn sample_reference_payload_with_count(count: usize) -> serde_json::Value {
        let references = (0..count)
            .map(|index| {
                serde_json::json!({
                    "file": format!("src/reference_{index}.rs"),
                    "range": {
                        "start": { "line": index, "character": 1 },
                        "end": { "line": index, "character": 4 }
                    }
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "ok": true,
            "file": "src/main.rs",
            "position": { "line": 0, "character": 0 },
            "references": references,
        })
    }

    async fn load_lsp_symbols(runtime: &mut Runtime) {
        runtime
            .load_plugin("lsp_symbols", include_str!("../../plugins/lsp_symbols.hk"))
            .await
            .unwrap();
        let config_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("plugin_config"));
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        runtime
            .resolve_request(
                config_request_id,
                serde_json::json!({
                    "value": {
                        "lsp_symbols": {
                            "icons": {
                                "enabled": true,
                                "overrides": {}
                            }
                        }
                    }
                }),
            )
            .await
            .unwrap();
    }

    async fn notify_lsp_symbols_progress(runtime: &mut Runtime, kind: &str) {
        runtime
            .notify(
                "lsp:progress",
                serde_json::json!({
                    "token": "index",
                    "kind": kind,
                    "lsp_client": {
                        "name": "rust_analyzer",
                        "workspace_root": "/repo",
                    },
                }),
            )
            .await
            .unwrap();
    }

    async fn pump_process_events(runtime: &mut Runtime) -> anyhow::Result<()> {
        for event in runtime.poll_process_events() {
            let Some(process_id) = event
                .get("process_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
            else {
                continue;
            };
            runtime
                .notify(&format!("process:{process_id}"), event)
                .await?;
        }
        Ok(())
    }

    async fn open_project_search_picker(runtime: &mut Runtime) -> PickerHandle {
        runtime.execute_command("ProjectSearch").await.unwrap();

        let cwd_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("cwd"));
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        runtime
            .resolve_request(cwd_request_id, serde_json::json!({ "value": "." }))
            .await
            .unwrap();
        let storage_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage {
                plugin,
                key,
                request_id,
            } => {
                assert_eq!(plugin, "project_search");
                assert_eq!(key, "history_by_cwd");
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        runtime
            .resolve_request(storage_request_id, serde_json::json!({ "value": {} }))
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker {
                owner,
                handle,
                title,
                items,
                options,
            } => {
                assert_eq!(owner, "project_search");
                assert_eq!(title.as_deref(), Some("Find in Files"));
                assert!(items.is_empty());
                assert!(options.external_filter);
                assert!(options
                    .actions
                    .iter()
                    .any(|action| action.action == "export"));
                handle
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    async fn load_git_runtime(root: &Path) -> Runtime {
        let mut runtime = Runtime::new_with_permissions(HashMap::from([(
            "git".to_string(),
            PluginPermissions {
                process: vec!["git".to_string()],
            },
        )]));
        runtime
            .load_plugin("git", include_str!("../../plugins/git.hk"))
            .await
            .unwrap();
        let mut cwd_request_id = None;
        let mut config_request_id = None;
        let mut info_request_id = None;
        for _ in 0..3 {
            match ACTION_DISPATCHER.recv_request() {
                PluginRequest::GetConfig { request_id, key } if key.as_deref() == Some("cwd") => {
                    cwd_request_id = Some(request_id);
                }
                PluginRequest::GetConfig {
                    request_id,
                    key: None,
                } => config_request_id = Some(request_id),
                PluginRequest::EditorInfo(request_id) => info_request_id = Some(request_id),
                _ => panic!("unexpected Git plugin startup request"),
            }
        }
        runtime
            .resolve_request(
                cwd_request_id.unwrap(),
                serde_json::json!({ "value": root.display().to_string() }),
            )
            .await
            .unwrap();
        runtime
            .resolve_request(
                config_request_id.unwrap(),
                serde_json::json!({ "value": { "executable": "red", "plugin_config": {} } }),
            )
            .await
            .unwrap();
        runtime
            .resolve_request(
                info_request_id.unwrap(),
                serde_json::json!({
                    "theme": {
                        "style": { "fg": null, "bg": null, "bold": false, "italic": false },
                        "ui_style": {
                            "muted": { "fg": null, "bg": null, "bold": false, "italic": false },
                            "popup_title": { "fg": null, "bg": null, "bold": false, "italic": false }
                        },
                        "colors": {}
                    }
                }),
            )
            .await
            .unwrap();
        runtime
    }

    #[test]
    fn embedded_git_core_compiles_as_a_native_multi_file_package_and_normalizes_options() {
        let program = git_core_program().unwrap();
        assert_eq!(program.semantic_profile(), SemanticProfile::Native);
        assert_eq!(program.source_map().sources().len(), 4);
        assert_eq!(program.module_semantic_results().len(), 4);

        let mut host = RedHost::new(HashMap::new());
        let status = host
            .call_git_core(
                "parse_status",
                &[Value::String(
                    "# branch.head native\0? src/new file.rs\0".to_string(),
                )],
            )
            .unwrap()
            .to_json();
        assert_eq!(status["head"], "native");
        assert_eq!(status["untracked"][0]["path"], "src/new file.rs");
        assert!(status["untracked"][0]["original_path"].is_null());

        let document = host
            .call_git_core(
                "detail_document",
                &[
                    Value::String(
                        "diff --git a/file.rs b/file.rs\n--- a/file.rs\n+++ b/file.rs\n@@ -1 +1 @@\n-old\n+new"
                            .to_string(),
                    ),
                    Value::String("file.rs".to_string()),
                    Value::String("unstaged".to_string()),
                ],
            )
            .unwrap()
            .to_json();
        assert!(document["lines"][3]["old_line"].is_null());
        assert_eq!(document["lines"][4]["old_line"], 1);
        assert!(document["lines"][4]["new_line"].is_null());
        assert_eq!(document["lines"][5]["new_line"], 1);

        let error = host.call_git_core("not_an_operation", &[]).unwrap_err();
        assert!(
            error.to_string().contains("unknown Git core operation"),
            "{error}"
        );
    }

    #[test]
    fn git_core_detail_bounds_large_previews_and_reports_full_counts() {
        let patch = format!("diff --git a/large.txt b/large.txt\n--- a/large.txt\n+++ b/large.txt\n@@ -1 +1,15001 @@\n-old\n{}", "+new\n".repeat(15001));
        let mut host = RedHost::new(HashMap::new());
        let document = host
            .call_git_core(
                "detail_document",
                &[
                    Value::String(patch),
                    Value::String("large.txt".to_string()),
                    Value::String("unstaged".to_string()),
                ],
            )
            .unwrap()
            .to_json();
        assert_eq!(document["lines"].as_array().unwrap().len(), 1000);
        assert_eq!(document["added"], 15001);
        assert_eq!(document["removed"], 1);
        assert_eq!(document["total_lines"], 15006);
        assert_eq!(document["truncated"], true);
    }

    #[test]
    fn embedded_neotree_core_compiles_as_a_native_multi_file_package_and_renders_typed_rows() {
        let program = neotree_core_program().unwrap();
        assert_eq!(program.semantic_profile(), SemanticProfile::Native);
        assert_eq!(program.source_map().sources().len(), 4);
        assert_eq!(program.module_semantic_results().len(), 4);

        let mut host = RedHost::new(HashMap::new());
        let statuses = host
            .call_neotree_core(
                "status_entries",
                &[Value::from_json(serde_json::json!({
                    "/repo/src": "conflict",
                    "/repo": "modified",
                    "/repo/main.rs": "modified",
                    "/repo/invalid": 42
                }))],
            )
            .unwrap();
        assert_eq!(
            statuses.to_json(),
            serde_json::json!([
                { "path": "/repo", "status": "modified" },
                { "path": "/repo/main.rs", "status": "modified" },
                { "path": "/repo/src", "status": "conflict" }
            ])
        );

        let rows = host
            .call_neotree_core(
                "build_rows",
                &[
                    Value::String("/repo".to_string()),
                    Value::from_json(serde_json::json!([{
                        "path": ".",
                        "entries": [
                            { "name": "src", "path": "./src", "kind": "directory" },
                            { "name": "main.rs", "path": "./main.rs", "kind": "file" }
                        ],
                        "truncated": true
                    }])),
                    Value::from_json(serde_json::json!([".", "./src"])),
                    Value::from_json(serde_json::json!(["./main.rs"])),
                    Value::from_json(serde_json::json!([{
                        "path": "./src",
                        "action": "move"
                    }])),
                    Value::String("/repo".to_string()),
                    statuses,
                ],
            )
            .unwrap()
            .to_json();
        assert!(rows[0]["right_segments"].as_array().unwrap().is_empty());
        assert_eq!(rows[1]["segments"][0]["text"], "✂ ");
        assert!(rows[1]["right_segments"].as_array().unwrap().is_empty());
        assert_eq!(
            rows[1]["segments"][2]["semantic"]["foreground"][0],
            "symbolIcon.folderForeground"
        );
        assert_eq!(
            rows[1]["segments"][3]["semantic"]["foreground"][0],
            "gitDecoration.conflictingResourceForeground"
        );
        assert_eq!(rows[2]["segments"][0]["text"], "✓ ");
        assert_eq!(rows[2]["segments"][2]["text"], " ");
        assert_eq!(
            rows[2]["segments"][2]["semantic"]["foreground"][0],
            "terminal.ansiBrightYellow"
        );
        assert_eq!(
            rows[2]["segments"][3]["semantic"]["foreground"][0],
            "gitDecoration.modifiedResourceForeground"
        );
        assert!(rows[3]["path"].is_null());

        let error = host
            .call_neotree_core("status_entries", &[Value::String("invalid".to_string())])
            .unwrap_err();
        assert!(
            error.to_string().contains("status index must be an object"),
            "{error}"
        );
        let error = host.call_neotree_core("not_an_operation", &[]).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unknown Neo-tree core operation"),
            "{error}"
        );
    }

    #[test]
    fn neotree_renders_deep_mostly_clean_workspaces_within_the_instruction_budget() {
        let workspace = "/Users/developer/code/workspace-with-a-realistic-deep-monorepo";
        let root_entries = std::iter::once(serde_json::json!({
            "name": "codex-rs",
            "path": "./codex-rs",
            "kind": "directory",
        }))
        .chain((0..47).map(|index| {
            serde_json::json!({
                "name": format!("workspace-directory-{index:03}"),
                "path": format!("./workspace-directory-{index:03}"),
                "kind": "directory",
            })
        }))
        .collect::<Vec<_>>();
        let crate_entries = (0..118)
            .map(|index| {
                serde_json::json!({
                    "name": format!("crate-{index:03}"),
                    "path": format!("./codex-rs/crate-{index:03}"),
                    "kind": "directory",
                })
            })
            .chain(std::iter::once(serde_json::json!({
                "name": "tui",
                "path": "./codex-rs/tui",
                "kind": "directory",
            })))
            .collect::<Vec<_>>();
        let source_entries = std::iter::once(serde_json::json!({
            "name": "tui",
            "path": "./codex-rs/tui/src/tui",
            "kind": "directory",
        }))
        .chain((0..149).map(|index| {
            serde_json::json!({
                "name": format!("source-file-{index:03}.rs"),
                "path": format!("./codex-rs/tui/src/source-file-{index:03}.rs"),
                "kind": "file",
            })
        }))
        .collect::<Vec<_>>();
        let tui_entries = std::iter::once(serde_json::json!({
            "name": "screen_size.rs",
            "path": "./codex-rs/tui/src/tui/screen_size.rs",
            "kind": "file",
        }))
        .chain((0..14).map(|index| {
            serde_json::json!({
                "name": format!("tui-file-{index:03}.rs"),
                "path": format!("./codex-rs/tui/src/tui/tui-file-{index:03}.rs"),
                "kind": "file",
            })
        }))
        .collect::<Vec<_>>();
        let statuses = std::iter::once(serde_json::json!({
            "path": "codex-rs/tui/src/tui/screen_size.rs",
            "absolute_path": format!("{workspace}/codex-rs/tui/src/tui/screen_size.rs"),
            "status": "modified",
        }))
        .chain((0..15).map(|index| {
            serde_json::json!({
                "path": format!("codex-rs/tui/src/source-file-{index:03}.rs"),
                "absolute_path": format!(
                    "{workspace}/codex-rs/tui/src/source-file-{index:03}.rs"
                ),
                "status": "modified",
            })
        }))
        .collect::<Vec<_>>();
        let mut host = RedHost::new(HashMap::new());
        let status_entries = host
            .call_neotree_core(
                "status_entries",
                &[Value::from_json(crate::editor::git_status_index(
                    &statuses, workspace,
                ))],
            )
            .unwrap();

        let rows = host
            .call_neotree_core(
                "build_rows",
                &[
                    Value::String(workspace.to_string()),
                    Value::from_json(serde_json::json!([
                        { "path": ".", "entries": root_entries, "truncated": false },
                        {
                            "path": "./codex-rs",
                            "entries": crate_entries,
                            "truncated": false,
                        },
                        {
                            "path": "./codex-rs/tui",
                            "entries": [{
                                "name": "src",
                                "path": "./codex-rs/tui/src",
                                "kind": "directory",
                            }],
                            "truncated": false,
                        },
                        {
                            "path": "./codex-rs/tui/src",
                            "entries": source_entries,
                            "truncated": false,
                        },
                        {
                            "path": "./codex-rs/tui/src/tui",
                            "entries": tui_entries,
                            "truncated": false,
                        }
                    ])),
                    Value::from_json(serde_json::json!([
                        ".",
                        "./codex-rs",
                        "./codex-rs/tui",
                        "./codex-rs/tui/src",
                        "./codex-rs/tui/src/tui",
                    ])),
                    Value::from_json(serde_json::json!([])),
                    Value::from_json(serde_json::json!([])),
                    Value::String(workspace.to_string()),
                    status_entries,
                ],
            )
            .expect("deep mostly-clean Neo-tree rendering must stay within its instruction budget")
            .to_json();
        let rows = rows.as_array().unwrap();

        assert_eq!(rows.len(), 334, "expanded trees must not be capped");
        assert!(
            rows.iter().all(|row| row["path"].is_string()),
            "complete directory listings must not contain a truncation row"
        );
        let modified_row = rows
            .iter()
            .find(|row| row["id"] == "./codex-rs/tui/src/tui/screen_size.rs")
            .expect("the active file should remain visible while its ancestors are expanded");
        assert_eq!(modified_row["right_segments"][0]["text"], "");
    }

    #[test]
    fn neotree_only_decorates_the_exact_ignored_path() {
        let statuses = [
            serde_json::json!({
                "path": "src/.DS_Store",
                "absolute_path": "/repo/src/.DS_Store",
                "status": "ignored",
            }),
            serde_json::json!({
                "path": "src/lsp/.DS_Store",
                "absolute_path": "/repo/src/lsp/.DS_Store",
                "status": "ignored",
            }),
            serde_json::json!({
                "path": "target/",
                "absolute_path": "/repo/target/",
                "status": "ignored",
            }),
        ];
        let status_index = crate::editor::git_status_index(&statuses, "/repo");
        let mut host = RedHost::new(HashMap::new());
        let status_entries = host
            .call_neotree_core("status_entries", &[Value::from_json(status_index)])
            .unwrap();

        let rows = host
            .call_neotree_core(
                "build_rows",
                &[
                    Value::String("/repo".to_string()),
                    Value::from_json(serde_json::json!([
                        {
                            "path": ".",
                            "entries": [
                                { "name": "src", "path": "./src", "kind": "directory" },
                                { "name": "target", "path": "./target", "kind": "directory" }
                            ],
                            "truncated": false
                        },
                        {
                            "path": "./src",
                            "entries": [
                                { "name": "lsp", "path": "./src/lsp", "kind": "directory" }
                            ],
                            "truncated": false
                        }
                    ])),
                    Value::from_json(serde_json::json!([".", "./src"])),
                    Value::from_json(serde_json::json!([])),
                    Value::from_json(serde_json::json!([])),
                    Value::String("/repo".to_string()),
                    status_entries,
                ],
            )
            .unwrap()
            .to_json();

        assert!(rows[0]["right_segments"].as_array().unwrap().is_empty());
        assert!(rows[1]["right_segments"].as_array().unwrap().is_empty());
        assert!(rows[2]["right_segments"].as_array().unwrap().is_empty());
        assert_eq!(
            rows[2]["segments"].as_array().unwrap().last().unwrap()["semantic"]["foreground"][0],
            "symbolIcon.folderForeground"
        );
        assert_eq!(rows[3]["right_segments"][0]["text"], "");
        assert_eq!(
            rows[3]["segments"].as_array().unwrap().last().unwrap()["semantic"]["foreground"][0],
            "gitDecoration.ignoredResourceForeground"
        );
    }

    #[test]
    fn runtimes_own_independent_request_queues() {
        let first = Runtime::new();
        let second = Runtime::new();

        first.send_request(PluginRequest::Action(Action::Print("first".to_string())));
        second.send_request(PluginRequest::Action(Action::Print("second".to_string())));

        assert!(matches!(
            first.try_recv_request(),
            Some(PluginRequest::Action(Action::Print(message))) if message == "first"
        ));
        assert!(matches!(
            second.try_recv_request(),
            Some(PluginRequest::Action(Action::Print(message))) if message == "second"
        ));
        assert!(first.try_recv_request().is_none());
        assert!(second.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn cancelled_timeout_never_reaches_the_editor_queue() {
        let mut runtime = Runtime::new();
        let timer_id = runtime.schedule_test_timeout(0);

        runtime.cancel_test_timeout(&timer_id);

        assert!(!runtime.poll_timer_callbacks().into_iter().any(|request| {
            matches!(
                request,
                PluginRequest::TimeoutCallback { timer_id: id } if id == timer_id
            )
        }));
    }

    #[tokio::test]
    async fn polling_due_timeouts_preserves_order_and_pending_timers() {
        let mut runtime = Runtime::new();
        let due = (0..128)
            .map(|_| runtime.schedule_test_timeout(0))
            .collect::<Vec<_>>();
        let pending = runtime.schedule_test_timeout(60_000);

        let callbacks = runtime
            .poll_timer_callbacks()
            .into_iter()
            .filter_map(|request| match request {
                PluginRequest::TimeoutCallback { timer_id } if due.contains(&timer_id) => {
                    Some(timer_id)
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(callbacks, due);
        assert_eq!(runtime.pending_timeout_count(), 1);
        runtime.cancel_test_timeout(&pending);
        assert_eq!(runtime.pending_timeout_count(), 0);
    }

    #[test]
    fn viewport_cursor_updates_share_rows_without_mutating_previous_snapshots() {
        let mut host = RedHost::new(HashMap::new());
        host.set_snapshot(
            "viewport_layout",
            serde_json::json!({
                "cursor": { "x": 1, "y": 2 },
                "rows": [{ "line": 0, "text": "unchanged" }],
            }),
        );
        let previous = host.snapshots.get("viewport_layout").unwrap().clone();

        assert!(host.update_viewport_cursor(serde_json::json!({ "x": 4, "y": 5 })));

        let Value::Object(previous) = previous else {
            panic!("expected the previous viewport object");
        };
        let Some(Value::Object(current)) = host.snapshots.get("viewport_layout") else {
            panic!("expected the updated viewport object");
        };
        assert_eq!(
            previous["cursor"].to_json(),
            serde_json::json!({ "x": 1, "y": 2 })
        );
        assert_eq!(
            current["cursor"].to_json(),
            serde_json::json!({ "x": 4, "y": 5 })
        );
        let (Value::Array(previous_rows), Value::Array(current_rows)) =
            (&previous["rows"], &current["rows"])
        else {
            panic!("expected shared viewport rows");
        };
        assert!(Arc::ptr_eq(previous_rows, current_rows));
    }

    #[test]
    fn viewport_cursor_updates_require_an_existing_cursor_snapshot() {
        let mut host = RedHost::new(HashMap::new());
        assert!(!host.update_viewport_cursor(serde_json::json!({ "x": 1 })));

        host.set_snapshot("viewport_layout", serde_json::json!({ "rows": [] }));
        assert!(!host.update_viewport_cursor(serde_json::json!({ "x": 1 })));
    }

    #[tokio::test]
    async fn cancelling_the_earliest_timeout_recomputes_the_next_deadline() {
        let mut runtime = Runtime::new();
        let earliest = runtime.schedule_test_timeout(0);
        let pending = runtime.schedule_test_timeout(60_000);

        runtime.cancel_test_timeout(&earliest);

        assert!(runtime.poll_timer_callbacks().is_empty());
        assert_eq!(runtime.pending_timeout_count(), 1);
        assert!(runtime.inner.lock().unwrap().host.next_timeout_at.is_some());

        runtime.cancel_test_timeout(&pending);
        assert!(runtime.inner.lock().unwrap().host.next_timeout_at.is_none());
    }

    #[tokio::test]
    async fn executes_husk_command_through_host() {
        drain_requests();

        let source = r#"
            pub fn activate() {
                red::add_command("Hello", hello);
            }

            fn hello() {
                red::execute("Print", "hello from husk");
            }
        "#;
        let mut runtime = Runtime::new();

        runtime.load_plugin("test", source).await.unwrap();
        runtime.execute_command("Hello").await.unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::Action(Action::Print(message)) => {
                assert_eq!(message, "hello from husk");
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[test]
    fn host_array_extend_appends_json_values_without_mutating_the_source() {
        let mut host = RedHost::new(HashMap::new());
        let source = Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2)]));

        let extended = host
            .call_module(
                "array-test",
                "red::extend",
                &[
                    source.clone(),
                    Value::Json(serde_json::json!([{ "value": 3 }, { "value": 4 }])),
                ],
            )
            .unwrap()
            .unwrap();

        assert_eq!(source.to_json(), serde_json::json!([1, 2]));
        assert_eq!(
            extended.to_json(),
            serde_json::json!([1, 2, { "value": 3 }, { "value": 4 }])
        );

        let error = host
            .call_module("array-test", "red::extend", &[source, Value::Null])
            .unwrap()
            .unwrap_err()
            .to_string();
        assert!(error.contains("argument 1 must be an array"), "{error}");
    }

    #[tokio::test]
    async fn registered_commands_include_owner_and_discovery_metadata() {
        let source = r#"
            pub fn activate() {
                red::add_command("ProjectSearch", search, Json {
                    title: "Search project",
                    category: "Search",
                    aliases: ["ripgrep"],
                    scope: "global",
                });
                red::add_command("BufferPicker", buffers);
            }

            fn search() {}
            fn buffers() {}
        "#;
        let mut runtime = Runtime::new();

        runtime.load_plugin("navigation", source).await.unwrap();

        let commands = runtime.registered_commands();
        assert_eq!(
            commands
                .iter()
                .map(|command| command.name.as_str())
                .collect::<Vec<_>>(),
            vec!["BufferPicker", "ProjectSearch"]
        );
        assert_eq!(commands[1].plugin, "navigation");
        assert_eq!(
            commands[1].metadata.title.as_deref(),
            Some("Search project")
        );
        assert_eq!(commands[1].metadata.category.as_deref(), Some("Search"));
        assert_eq!(commands[1].metadata.aliases, vec!["ripgrep"]);
        assert_eq!(commands[0].metadata.scope, CommandScope::Editor);
        assert_eq!(commands[1].metadata.scope, CommandScope::Global);
        assert_eq!(
            runtime.command_scope("BufferPicker"),
            Some(CommandScope::Editor)
        );
        assert_eq!(
            runtime.command_scope("ProjectSearch"),
            Some(CommandScope::Global)
        );
    }

    #[tokio::test]
    async fn command_arguments_dispatch_typed_payloads_and_keep_legacy_commands() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime.load_plugin("command_arguments", r#"
            #[red::command(name = "Service", arguments = true,
                completions = [["enable", "disable"], ["local", "workspace"]], scope = "global")]
            fn service(command: CommandInvocation) {
                red::execute("Print", format("{}|{}|{}", command.name, command.args.len(), command.raw_args));
            }
            fn imperative(command: CommandInvocation) {
                red::execute("Print", command.args[0]);
            }
            fn legacy() { red::execute("Print", "legacy"); }
            pub fn activate() {
                red::add_command("Imperative", imperative,
                    Json { arguments: true, completions: [["status"]] });
                red::add_command("Legacy", legacy);
                red::add_command("Service exact", legacy);
            }
        "#).await.unwrap();
        let commands = runtime.registered_commands();
        let service = commands
            .iter()
            .find(|command| command.name == "Service")
            .unwrap();
        assert!(service.metadata.arguments);
        assert_eq!(
            service.metadata.completions,
            [vec!["enable", "disable"], vec!["local", "workspace"]]
        );
        assert_eq!(
            runtime.command_scope("Service enable"),
            Some(CommandScope::Global)
        );
        for (input, expected) in [
            ("Service", "Service|0|"),
            (
                "Service  enable   workspace",
                "Service|2|enable   workspace",
            ),
            ("Imperative status", "status"),
            ("Legacy", "legacy"),
            ("Service exact", "legacy"),
        ] {
            runtime.execute_command(input).await.unwrap();
            assert!(
                matches!(ACTION_DISPATCHER.recv_request(),
                PluginRequest::Action(Action::Print(message)) if message == expected),
                "{input}"
            );
        }
        assert!(runtime.execute_command("Legacy extra").await.is_err());
        assert!(runtime.command_plugin("Legacy extra").is_none());
        assert!(runtime.command_plugin("Legacy").is_some());
    }

    #[tokio::test]
    async fn command_arguments_reject_invalid_metadata_before_activation() {
        for source in [
            r#"#[red::command(name = "Bad", arguments = true)] fn bad() {}"#,
            r#"#[red::command(name = "Bad", completions = [["one"]])] fn bad() {}"#,
            r#"#[red::command(name = "Bad", arguments = true, completions = [["two words"]])] fn bad(command: CommandInvocation) {}"#,
            r#"pub fn activate() { red::add_command("Bad", bad, Json { arguments: true }); } fn bad() {}"#,
            r#"pub fn activate() { red::add_command("Bad", bad, Json { arguments: true, completions: [[""]] }); } fn bad(command: CommandInvocation) {}"#,
            r#"#[red::command(name = "Bad", arguments = true)] fn bad(value: i32) {}"#,
        ] {
            let mut runtime = Runtime::new();
            assert!(
                runtime
                    .load_plugin("invalid_command_arguments", source)
                    .await
                    .is_err(),
                "{source}"
            );
            assert!(runtime.command_plugin("Bad").is_none());
        }
    }

    #[tokio::test]
    async fn annotated_commands_and_events_register_before_activation() {
        drain_requests();
        let source = r#"
            pub fn activate() {
                red::state_set("ready", true);
            }

            #[red::command(
                name = "OpenSymbols",
                title = "Open symbols",
                category = "LSP",
                description = "Browse document symbols",
                aliases = ["outline", "symbols"],
                scope = "global",
            )]
            fn open() {
                red::execute("Print", "command");
            }

            #[red::on("editor:changed")]
            #[red::on("timeout:callback")]
            fn changed(event: Json) {
                red::execute("Print", "event");
            }
        "#;
        let mut runtime = Runtime::new();
        runtime.load_plugin("annotated", source).await.unwrap();

        assert_eq!(
            runtime.command_plugin("OpenSymbols").as_deref(),
            Some("annotated")
        );
        let command = runtime
            .registered_commands()
            .into_iter()
            .find(|command| command.name == "OpenSymbols")
            .unwrap();
        assert_eq!(command.metadata.title.as_deref(), Some("Open symbols"));
        assert_eq!(command.metadata.category.as_deref(), Some("LSP"));
        assert_eq!(
            command.metadata.description.as_deref(),
            Some("Browse document symbols")
        );
        assert_eq!(command.metadata.aliases, ["outline", "symbols"]);
        assert_eq!(command.metadata.scope, CommandScope::Global);

        runtime.execute_command("OpenSymbols").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "command"
        ));
        runtime
            .notify("editor:changed", serde_json::json!({}))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "event"
        ));
        runtime
            .notify("timeout:callback", serde_json::json!({}))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "event"
        ));
    }

    #[tokio::test]
    async fn typed_state_configuration_lifecycle_and_hidden_commands_work_together() {
        drain_requests();
        let source = r#"
            struct PluginState { count: i32 }

            #[red::state]
            fn initial_state() -> PluginState {
                return PluginState { count: 1 };
            }

            #[red::lifecycle("activate")]
            fn initialize() {
                let state: PluginState = red::state();
                state.count = 2;
                red::state_set(state);
                red::state_set("legacy", 5);
            }

            #[red::config("plugin_config")]
            fn configured(event: Json) {
                let state: PluginState = red::state();
                red::state_patch(PluginState {
                    count: state.count + event.value.increment,
                });
            }

            #[red::command(name = "Internal", visible = false)]
            fn inspect() {
                let state: PluginState = red::state();
                red::execute("Print", state.count + ":" + red::state("legacy"));
            }

            #[red::lifecycle("before_exit")]
            fn capture(snapshot: Json) {
                red::execute("Print", snapshot.label);
            }

            #[red::lifecycle("deactivate")]
            fn shutdown() {
                red::execute("Print", "shutdown");
            }
        "#;
        let mut runtime = Runtime::new();
        runtime.load_plugin("typed", source).await.unwrap();

        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("plugin_config"));
                request_id
            }
            _ => panic!("expected configuration request"),
        };
        runtime
            .resolve_request(
                request_id,
                serde_json::json!({ "value": { "increment": 3 } }),
            )
            .await
            .unwrap();

        let command = runtime
            .registered_commands()
            .into_iter()
            .find(|command| command.name == "Internal")
            .unwrap();
        assert!(!command.metadata.visible);
        runtime.execute_command("Internal").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "5:5"
        ));

        runtime
            .before_exit(serde_json::json!({ "label": "saving" }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "saving"
        ));

        runtime.deactivate_all().await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "shutdown"
        ));
    }

    #[tokio::test]
    async fn failed_typed_initializer_discards_staged_commands_and_config_requests() {
        drain_requests();
        let mut runtime = Runtime::new();
        let error = runtime
            .load_plugin(
                "invalid-state",
                r#"
                    struct PluginState { count: i32 }

                    #[red::state]
                    fn initial_state() -> PluginState {
                        return PluginState { count: 1 / 0 };
                    }

                    #[red::config("plugin_config")]
                    fn configured(event: Json) {}

                    #[red::command(name = "Leaked")]
                    fn leaked() {}
                "#,
            )
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("integer division by zero"), "{error}");
        assert!(runtime.registered_commands().is_empty());
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn typed_state_patches_reject_wrong_records_and_unknown_fields() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "patches",
                r#"
                    struct PluginState {
                        count: i32,
                        values: [String],
                    }
                    struct OtherState { count: i32 }

                    #[red::state]
                    fn initial_state() -> PluginState {
                        return PluginState { count: 1, values: ["retained"] };
                    }

                    #[red::command(name = "WrongType")]
                    fn wrong_type() {
                        red::state_patch(OtherState { count: 2 });
                    }

                    #[red::command(name = "UnknownField")]
                    fn unknown_field() {
                        red::state_patch(PluginState { count: 99, missing: 2 });
                    }

                    #[red::command(name = "Update")]
                    fn update() {
                        red::state_patch(PluginState { count: 2 });
                    }

                    #[red::command(name = "Inspect")]
                    fn inspect() {
                        let state: PluginState = red::state();
                        red::execute("Print", state.count + ":" + state.values[0]);
                    }
                "#,
            )
            .await
            .unwrap();

        let wrong_type = runtime
            .execute_command("WrongType")
            .await
            .unwrap_err()
            .to_string();
        assert!(
            wrong_type.contains("requires a typed state record"),
            "{wrong_type}"
        );

        let unknown_field = runtime
            .execute_command("UnknownField")
            .await
            .unwrap_err()
            .to_string();
        assert!(
            unknown_field.contains("no field named `missing`"),
            "{unknown_field}"
        );

        runtime.execute_command("Inspect").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "1:retained"
        ));

        runtime.execute_command("Update").await.unwrap();
        runtime.execute_command("Inspect").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "2:retained"
        ));
    }

    #[tokio::test]
    async fn event_callbacks_decode_nested_records_arrays_and_optional_fields() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "typed-payload",
                r#"
                    struct Child { label: Option<String> }
                    struct Event {
                        child: Child,
                        items: [Child],
                        missing: Option<i32>,
                    }

                    #[red::on("typed:record")]
                    fn received(event: Event) {
                        let label = option_label(event.child.label);
                        let item = option_label(event.items[0].label);
                        let missing = -1;
                        if let Some(value) = event.missing {
                            missing = value;
                        }
                        red::execute("Print", label + ":" + item + ":" + missing);
                    }

                    fn option_label(value: Option<String>) -> String {
                        if let Some(label) = value {
                            return label;
                        }
                        return "none";
                    }
                "#,
            )
            .await
            .unwrap();

        runtime
            .notify(
                "typed:record",
                serde_json::json!({
                    "child": { "label": "root" },
                    "items": [{ "label": null }],
                }),
            )
            .await
            .unwrap();

        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "root:none:-1"
        ));
    }

    #[tokio::test]
    async fn event_callbacks_decode_tagged_variants_and_preserve_unknown_payloads() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "typed-variant",
                r#"
                    enum Event {
                        ItemUpdated { id: String, detail: Option<String> },
                        Message(String),
                        Pair(String, Option<i32>),
                        Unknown(Json),
                    }

                    #[red::on("typed:variant")]
                    fn received(event: Event) {
                        match event {
                            Event::ItemUpdated { id, detail } => {
                                let label = "none";
                                if let Some(value) = detail {
                                    label = value;
                                }
                                red::execute("Print", id + ":" + label);
                            }
                            Event::Unknown(value) => {
                                red::execute("Print", "unknown:" + value["type"]);
                            }
                            Event::Message(value) => {
                                red::execute("Print", "message:" + value);
                            }
                            Event::Pair(label, detail) => {
                                let value = -1;
                                if let Some(number) = detail {
                                    value = number;
                                }
                                red::execute("Print", label + ":" + value);
                            }
                        }
                    }
                "#,
            )
            .await
            .unwrap();

        runtime
            .notify(
                "typed:variant",
                serde_json::json!({ "type": "item_updated", "id": "42" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "42:none"
        ));

        runtime
            .notify(
                "typed:variant",
                serde_json::json!({ "$case": "Message", "$fields": ["ready"] }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "message:ready"
        ));

        runtime
            .notify(
                "typed:variant",
                serde_json::json!({ "$case": "Pair", "$fields": ["count", null] }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "count:-1"
        ));

        runtime
            .notify(
                "typed:variant",
                serde_json::json!({ "type": "future_event" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "unknown:future_event"
        ));
    }

    #[tokio::test]
    async fn process_callbacks_decode_host_variants_and_optional_exit_codes() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "typed-process",
                r#"
                    #[red::on("typed:process")]
                    fn received(event: ProcessEvent) {
                        match event {
                            ProcessEvent::Stdout { process_id, line, plugin_name } => {
                                red::execute("Print", process_id + ":out:" + line);
                            }
                            ProcessEvent::Stderr { process_id, line, plugin_name } => {
                                red::execute("Print", process_id + ":err:" + line);
                            }
                            ProcessEvent::Exit { process_id, code, plugin_name } => {
                                let exit_code = "none";
                                if let Some(value) = code {
                                    exit_code = "" + value;
                                }
                                red::execute("Print", process_id + ":exit:" + exit_code);
                            }
                            ProcessEvent::Error { process_id, message, plugin_name } => {
                                red::execute("Print", process_id + ":error:" + message);
                            }
                        }
                    }
                "#,
            )
            .await
            .unwrap();

        for (payload, expected) in [
            (
                serde_json::json!({
                    "type": "stdout",
                    "plugin_name": "typed-process",
                    "process_id": "7",
                    "line": "ready",
                }),
                "7:out:ready",
            ),
            (
                serde_json::json!({
                    "type": "exit",
                    "plugin_name": "typed-process",
                    "process_id": "7",
                    "code": 0,
                }),
                "7:exit:0",
            ),
            (
                serde_json::json!({
                    "type": "exit",
                    "plugin_name": "typed-process",
                    "process_id": "8",
                    "code": null,
                }),
                "8:exit:none",
            ),
            (
                serde_json::json!({
                    "type": "exit",
                    "plugin_name": "typed-process",
                    "process_id": "9",
                }),
                "9:exit:none",
            ),
        ] {
            runtime.notify("typed:process", payload).await.unwrap();
            match ACTION_DISPATCHER.recv_request() {
                PluginRequest::Action(Action::Print(message)) => assert_eq!(message, expected),
                _ => panic!("expected typed process event response"),
            }
        }
    }

    #[tokio::test]
    async fn request_callbacks_decode_optional_record_responses() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "typed-request",
                r#"
                    struct Snapshot { path: String }

                    #[red::command(name = "ReadTypedSnapshot")]
                    fn read() {
                        red::request("GetConfig", received, "snapshot");
                    }

                    fn received(snapshot: Option<Snapshot>) {
                        match snapshot {
                            Some(value) => red::execute("Print", value.path),
                            None => red::execute("Print", "missing"),
                        }
                    }
                "#,
            )
            .await
            .unwrap();

        for (payload, expected) in [
            (serde_json::Value::Null, "missing"),
            (
                serde_json::json!({ "path": "/repo/main.rs" }),
                "/repo/main.rs",
            ),
        ] {
            runtime.execute_command("ReadTypedSnapshot").await.unwrap();
            let request_id = match ACTION_DISPATCHER.recv_request() {
                PluginRequest::GetConfig { request_id, .. } => request_id,
                _ => panic!("expected typed snapshot request"),
            };
            runtime.resolve_request(request_id, payload).await.unwrap();
            match ACTION_DISPATCHER.recv_request() {
                PluginRequest::Action(Action::Print(message)) => assert_eq!(message, expected),
                _ => panic!("expected typed snapshot response"),
            }
        }
    }

    #[test]
    fn host_json_serialization_unwraps_nested_options_and_preserves_other_variants() {
        let record = Value::Struct {
            type_name: "OptionalRecord".to_string(),
            fields: Arc::new(BTreeMap::from([
                ("present".to_string(), option_payload(Some(Value::Int(7)))),
                ("missing".to_string(), option_payload(None)),
                (
                    "nested".to_string(),
                    Value::Array(Arc::new(vec![option_payload(Some(Value::String(
                        "nested".to_string(),
                    )))])),
                ),
                (
                    "variant".to_string(),
                    Value::Variant {
                        type_name: "Status".to_string(),
                        case: "Ready".to_string(),
                        fields: Arc::new(vec![option_payload(None)]),
                    },
                ),
            ])),
        };

        assert_eq!(
            value_to_json(&record),
            serde_json::json!({
                "present": 7,
                "missing": null,
                "nested": ["nested"],
                "variant": {
                    "$type": "Status",
                    "$case": "Ready",
                    "$fields": [null],
                },
            })
        );
    }

    #[tokio::test]
    async fn git_core_status_preserves_typed_rename_options_across_host_boundaries() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "typed-git-status",
                r##"
                    struct GitEntry {
                        path: String,
                        original_path: Option<String>,
                        x: String,
                        y: String,
                        kind: String,
                    }

                    struct GitState {
                        head: String,
                        oid: String,
                        upstream: String,
                        ahead: i32,
                        behind: i32,
                        stash_count: i32,
                        staged: [GitEntry],
                        unstaged: [GitEntry],
                        untracked: [GitEntry],
                        conflicted: [GitEntry],
                        truncated: bool,
                    }

                    #[red::command(name = "ReadTypedGitStatus")]
                    fn read() {
                        let output = "# branch.head typed\02 R. N... 100644 100644 100644 abc def R100 src/new.rs\0src/old.rs\0? notes.txt\0";
                        let status: GitState = red::git_core("parse_status", output);
                        let rename = status.staged[0];
                        if let Some(original_path) = rename.original_path {
                            red::execute("Print", rename.path + "←" + original_path);
                        }
                        let untracked = status.untracked[0];
                        if let None = untracked.original_path {
                            red::execute("Print", "untracked:none");
                        }
                        red::execute("SetStorage", "status", status);
                    }
                "##,
            )
            .await
            .unwrap();

        runtime.execute_command("ReadTypedGitStatus").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message == "src/new.rs←src/old.rs"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "untracked:none"
        ));
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::SetPluginStorage { plugin, key, value } => {
                assert_eq!(plugin, "typed-git-status");
                assert_eq!(key, "status");
                assert_eq!(value["staged"][0]["original_path"], "src/old.rs");
                assert!(value["untracked"][0]["original_path"].is_null());
            }
            _ => panic!("expected typed Git status snapshot"),
        }
    }

    #[tokio::test]
    async fn git_core_diff_preserves_typed_optional_line_and_hunk_metadata() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "typed-git-diff",
                r##"
                    struct GitWorkspaceLineData {
                        path: String,
                        section: String,
                        patch_index: i32,
                        hunk_id: Option<String>,
                    }

                    struct GitWorkspaceDocumentLine {
                        id: String,
                        text: String,
                        kind: String,
                        old_line: Option<i32>,
                        new_line: Option<i32>,
                        hunk_id: Option<String>,
                        data: GitWorkspaceLineData,
                    }

                    struct GitWorkspaceDocument {
                        path: String,
                        lines: [GitWorkspaceDocumentLine],
                    }

                    #[red::command(name = "ReadTypedGitDiff")]
                    fn read() {
                        let patch = "diff --git a/file.rs b/file.rs\n--- a/file.rs\n+++ b/file.rs\n@@ -1 +1 @@\n-old\n+new";
                        let document: GitWorkspaceDocument = red::git_core(
                            "detail_document",
                            patch,
                            "file.rs",
                            "unstaged"
                        );
                        let removed = document.lines[4];
                        if let Some(line) = removed.old_line {
                            red::execute("Print", "old:" + line);
                        }
                        if let None = removed.new_line {
                            red::execute("Print", "old:new-none");
                        }
                        let added = document.lines[5];
                        if let None = added.old_line {
                            red::execute("Print", "new:old-none");
                        }
                        if let Some(line) = added.new_line {
                            red::execute("Print", "new:" + line);
                        }
                        if let Some(hunk) = added.hunk_id {
                            red::execute("Print", "hunk:" + hunk);
                        }
                        red::execute("SetStorage", "document", document);
                    }
                "##,
            )
            .await
            .unwrap();

        runtime.execute_command("ReadTypedGitDiff").await.unwrap();
        for expected in ["old:1", "old:new-none", "new:old-none", "new:1"] {
            assert!(matches!(
                ACTION_DISPATCHER.recv_request(),
                PluginRequest::Action(Action::Print(message)) if message == expected
            ));
        }
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message.starts_with("hunk:")
        ));
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::SetPluginStorage { plugin, key, value } => {
                assert_eq!(plugin, "typed-git-diff");
                assert_eq!(key, "document");
                assert_eq!(value["lines"][4]["old_line"], 1);
                assert!(value["lines"][4]["new_line"].is_null());
                assert!(value["lines"][5]["old_line"].is_null());
                assert_eq!(value["lines"][5]["new_line"], 1);
                assert!(value["lines"][5]["hunk_id"].is_string());
            }
            _ => panic!("expected typed Git diff snapshot"),
        }
    }

    #[tokio::test]
    async fn annotated_lifecycle_state_migration_preserves_reload_order() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "migration",
                r#"
                    #[red::lifecycle("state_export")]
                    fn save() -> Json { return "preserved"; }

                    #[red::lifecycle("deactivate")]
                    fn stop() { red::execute("Print", "old teardown"); }
                "#,
            )
            .await
            .unwrap();

        runtime
            .load_plugin(
                "migration",
                r#"
                    #[red::lifecycle("activate")]
                    fn start() { red::execute("Print", "new activation"); }

                    #[red::lifecycle("state_import")]
                    fn restore(saved: Json) { red::execute("Print", saved); }
                "#,
            )
            .await
            .unwrap();

        let messages = (0..3)
            .map(|_| match ACTION_DISPATCHER.recv_request() {
                PluginRequest::Action(Action::Print(message)) => message,
                _ => panic!("expected lifecycle print"),
            })
            .collect::<Vec<_>>();
        assert_eq!(messages, ["old teardown", "new activation", "preserved"]);
    }

    #[tokio::test]
    async fn annotated_command_collisions_preserve_the_existing_owner() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "first",
                r#"
                    #[red::command(name = "Shared")]
                    fn shared() { red::execute("Print", "first"); }
                "#,
            )
            .await
            .unwrap();
        let error = runtime
            .load_plugin(
                "second",
                r#"
                    #[red::command(name = "Shared")]
                    fn shared() { red::execute("Print", "second"); }
                    pub fn activate() { red::execute("Print", "leaked"); }
                "#,
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("already registered by plugin `first`"),
            "{error}"
        );
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        assert_eq!(runtime.command_plugin("Shared").as_deref(), Some("first"));
        runtime.execute_command("Shared").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "first"
        ));
    }

    #[tokio::test]
    async fn failed_annotated_reload_preserves_previous_commands_and_events() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "annotated-reload",
                r#"
                    struct StableEvent { label: Option<String> }
                    #[red::command(name = "Stable")]
                    fn stable() { red::execute("Print", "stable"); }
                    #[red::on("editor:changed")]
                    fn changed(event: StableEvent) {
                        if let Some(label) = event.label {
                            red::execute("Print", label);
                        }
                    }
                "#,
            )
            .await
            .unwrap();

        let error = runtime
            .load_plugin(
                "annotated-reload",
                r#"
                    struct ReplacementEvent { count: Option<i32> }
                    #[red::command(name = "Leaked")]
                    fn leaked() { red::execute("Print", "replacement"); }
                    #[red::on("editor:changed")]
                    fn changed(event: ReplacementEvent) {
                        red::execute("Print", "replacement event");
                    }
                    pub fn activate() { red::execute("Print", 1 / 0); }
                "#,
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("integer division by zero"), "{error}");
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        assert_eq!(runtime.command_plugin("Leaked"), None);
        runtime.execute_command("Stable").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "stable"
        ));
        runtime
            .notify(
                "editor:changed",
                serde_json::json!({ "label": "original event" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "original event"
        ));
    }

    #[tokio::test]
    async fn annotation_validation_remains_safe_without_static_typechecking() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime.set_typecheck_enabled(false);
        let error = runtime
            .load_plugin(
                "unchecked",
                r#"
                    #[red::command(title = "Missing name")]
                    fn broken() {}
                    pub fn activate() { red::execute("Print", "leaked"); }
                "#,
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("requires a nonempty `name`"), "{error}");
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        assert!(runtime.registered_commands().is_empty());
    }

    #[tokio::test]
    async fn annotated_package_module_commands_register_and_execute() {
        drain_requests();
        let directory = tempfile::tempdir().unwrap();
        let source_directory = directory.path().join("src");
        std::fs::create_dir(&source_directory).unwrap();
        std::fs::write(
            directory.path().join("Husk.toml"),
            "[package]\nname = \"module-annotations\"\nversion = \"0.1.0\"\nentry = \"src/main.hk\"\n",
        )
        .unwrap();
        std::fs::write(
            source_directory.join("main.hk"),
            "mod commands; pub fn activate() {}",
        )
        .unwrap();
        std::fs::write(
            source_directory.join("commands.hk"),
            r#"
                struct ModuleState { opened: i32 }
                struct ModuleEvent { label: Option<String> }

                #[red::state]
                pub fn initial_state() -> ModuleState {
                    return ModuleState { opened: 0 };
                }

                #[red::command(name = "FromModule", title = "Package command")]
                pub fn open() {
                    let state: ModuleState = red::state();
                    red::state_patch(ModuleState { opened: state.opened + 1 });
                    red::execute("Print", "package module: " + red::state().opened);
                }

                #[red::on("package:typed")]
                pub fn received(event: ModuleEvent) {
                    if let Some(label) = event.label {
                        red::execute("Print", "package event: " + label);
                    }
                }
            "#,
        )
        .unwrap();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin_package("module-annotations", &directory.path().join("Husk.toml"))
            .await
            .unwrap();
        runtime.execute_command("FromModule").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "package module: 1"
        ));

        runtime
            .notify("package:typed", serde_json::json!({ "label": "decoded" }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "package event: decoded"
        ));
    }

    #[tokio::test]
    async fn external_hello_package_registers_its_declarative_command() {
        drain_requests();
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("examples/external-hello-plugin/Husk.toml");
        let mut runtime = Runtime::new();
        runtime
            .load_plugin_package("hello-panel", &manifest)
            .await
            .unwrap();

        assert_eq!(
            runtime.command_plugin("HelloPanel").as_deref(),
            Some("hello-panel")
        );
        runtime.execute_command("HelloPanel").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::CreateTextPanel { id, .. } if id == "external-hello"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, .. } if id == "external-hello"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::FocusPanel { id } if id == "external-hello"
        ));
    }

    #[tokio::test]
    async fn husk_can_drive_the_native_agent_bridge() {
        drain_requests();

        let source = r#"
            pub fn activate() {
                red::add_command("AgentStart", start);
                red::add_command("AgentAsk", ask);
                red::add_command("AgentStop", stop);
                red::add_command("AgentClose", close);
            }

            fn start() { red::execute("AgentNewSession", "/workspace"); }
            fn ask() { red::execute("AgentPrompt", "session-1", "hello"); }
            fn stop() { red::execute("AgentCancel", "session-1"); }
            fn close() { red::execute("AgentCloseSession", "session-1"); }
        "#;
        let mut runtime = Runtime::new();
        runtime.load_plugin("test", source).await.unwrap();

        runtime.execute_command("AgentStart").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));

        runtime.execute_command("AgentAsk").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentPrompt { session_id, text }
                if session_id == "session-1" && text == "hello"
        ));

        runtime.execute_command("AgentStop").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentCancel { session_id } if session_id == "session-1"
        ));

        runtime.execute_command("AgentClose").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentCloseSession { session_id } if session_id == "session-1"
        ));
    }

    #[tokio::test]
    async fn bundled_agent_command_opens_prompt_and_lazily_starts_session() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        runtime.execute_command("Agent").await.unwrap();
        let history_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage {
                plugin,
                key,
                request_id,
            } => {
                assert_eq!(plugin, "agent");
                assert_eq!(key, "prompt_history");
                request_id
            }
            _ => panic!("expected agent prompt-history request"),
        };
        runtime
            .resolve_request(history_request_id, serde_json::json!({ "value": [] }))
            .await
            .unwrap();
        let composer = recv_agent_composer().0;
        runtime
            .notify_composer(
                composer,
                ComposerCallback::Submitted("explain the workspace".to_string()),
            )
            .unwrap();
        let request_id = recv_optimistic_agent_start(
            "explain the workspace",
            serde_json::json!(["explain the workspace"]),
            true,
        );
        runtime
            .resolve_request(request_id, serde_json::json!({ "value": "/workspace" }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd }
                if cwd.as_path() == std::path::Path::new("/workspace")
        ));

        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-lazy" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, .. } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "Agent session started"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, blocks }
                if id == "agent-conversation"
                    && blocks.len() == 1
                    && blocks[0].text == "explain the workspace"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelStatus { id, status: Some(status) }
                if id == "agent-conversation"
                    && status.busy
                    && status.label == "Waiting for agent…"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Refresh)
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, .. }
                if plugin == "agent" && key == "transcript"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentPrompt { session_id, text }
                if session_id == "session-lazy" && text == "explain the workspace"
        ));
    }

    #[tokio::test]
    async fn bundled_agent_progress_keeps_one_flush_deadline_and_separate_messages() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({"session_id":"progress"}),
            )
            .await
            .unwrap();
        drain_requests();
        let state = |runtime: &Runtime| {
            let inner = runtime.inner.lock().unwrap();
            value_to_json(inner.host.policy().typed_states.get("agent").unwrap())
        };
        runtime
            .notify(
                "agent:update",
                serde_json::json!({"session_id":"progress","text":"first"}),
            )
            .await
            .unwrap();
        let timer = state(&runtime)["stream_timer"].clone();
        assert_ne!(timer, "");
        drain_requests();
        for text in [" second", " third"] {
            runtime
                .notify(
                    "agent:update",
                    serde_json::json!({"session_id":"progress","text":text}),
                )
                .await
                .unwrap();
            assert_eq!(
                state(&runtime)["stream_timer"],
                timer,
                "a busy stream must not postpone its flush"
            );
        }
        runtime
            .notify("timeout:callback", serde_json::json!({"timer_id":timer}))
            .await
            .unwrap();
        let mut appended = String::new();
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::AppendTextPanel { delta, .. } = request {
                appended.push_str(&delta);
            }
        }
        assert_eq!(appended, "first second third");

        runtime
            .notify(
                "agent:update",
                serde_json::json!({"session_id":"progress","text":" fourth"}),
            )
            .await
            .unwrap();
        let timer = state(&runtime)["stream_timer"].clone();
        runtime
            .notify(
                "panel:event:agent-conversation",
                serde_json::json!({"action":"activity"}),
            )
            .await
            .unwrap();
        assert_eq!(state(&runtime)["stream_delta"], "");
        drain_requests();
        runtime
            .notify("timeout:callback", serde_json::json!({"timer_id":timer}))
            .await
            .unwrap();
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            assert!(
                !matches!(request, PluginRequest::AppendTextPanel { .. }),
                "full render already contains pending text"
            );
        }
        runtime
            .notify(
                "agent:message_completed",
                serde_json::json!({"session_id":"progress","text":"First message."}),
            )
            .await
            .unwrap();
        runtime
            .notify(
                "agent:update",
                serde_json::json!({"session_id":"progress","text":"Final"}),
            )
            .await
            .unwrap();
        runtime
            .notify(
                "agent:message_completed",
                serde_json::json!({"session_id":"progress","text":"Final answer."}),
            )
            .await
            .unwrap();
        let current = state(&runtime);
        let messages: Vec<_> = current["transcript_blocks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|block| block["kind"] == "agent")
            .map(|block| block["text"].as_str().unwrap())
            .collect();
        assert_eq!(messages, ["First message.", "Final answer."]);
        assert_eq!(current["streaming"], false);
        drain_requests();
    }

    #[tokio::test]
    async fn bundled_agent_progress_groups_actions_and_retains_failure_details() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({"session_id":"progress"}),
            )
            .await
            .unwrap();
        drain_requests();
        let state = |runtime: &Runtime| {
            let inner = runtime.inner.lock().unwrap();
            value_to_json(inner.host.policy().typed_states.get("agent").unwrap())
        };
        for (id, title, status, detail) in [
            ("a", "Reading first.rs", "completed", "2 ms"),
            ("b", "Reading second.rs", "completed", "3 ms"),
            ("c", "Reading missing.rs", "failed", "File not found"),
        ] {
            // Exercise completion without start as well as bounded output details.
            runtime
                .notify(
                    "agent:activity",
                    serde_json::json!({"session_id":"progress","update":{
                        "session_update":"tool_call_update","tool_call_id":id,"title":title,
                        "kind":"read","status":status,"detail":detail
                    }}),
                )
                .await
                .unwrap();
        }
        let current = state(&runtime);
        let compact = current["transcript_blocks"][0]["text"].as_str().unwrap();
        assert_eq!(compact, "▸ Activity · 3 actions · 1 issue");
        assert!(!compact.contains("File not found"));
        assert!(!compact.contains("first.rs"));
        let activity_id = current["transcript_blocks"][0]["id"].clone();
        drain_requests();
        runtime
            .notify(
                "panel:event:agent-conversation",
                serde_json::json!({"action":"activity"}),
            )
            .await
            .unwrap();
        let mut expanded = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::UpdateTextPanel { blocks, .. } = request {
                expanded |= blocks.iter().any(|block| {
                    block.text.contains("✓ Read first.rs")
                        && block.text.contains("View all details…")
                });
            }
        }
        assert!(expanded);
        runtime.execute_command("AgentActivity").await.unwrap();
        runtime
            .notify(
                "agent:message_completed",
                serde_json::json!({"session_id":"progress","text":"Done."}),
            )
            .await
            .unwrap();
        runtime.notify("agent:completed", serde_json::json!({"session_id":"progress","stop_reason":"completed","elapsed_ms":1200})).await.unwrap();
        let current = state(&runtime);
        assert_eq!(current["activity_rows"], serde_json::json!([]));
        assert_eq!(current["activity_history"].as_array().unwrap().len(), 1);
        assert_eq!(current["transcript_blocks"][0]["id"], activity_id);
        assert_eq!(
            current["transcript_blocks"][0]["text"],
            "▸ Activity · 3 actions · 1 issue"
        );
        assert_eq!(current["transcript_blocks"][1]["text"], "Done.");
        assert_eq!(current["transcript_blocks"][2]["text"], "Worked for 1s");
        assert!(current["activity_history"][0]["details"]
            .as_str()
            .unwrap()
            .contains("File not found"));
        assert!(!current["transcript"]
            .as_str()
            .unwrap()
            .contains("File not found"));
        assert!(!current["transcript"]
            .as_str()
            .unwrap()
            .contains("Reading first.rs"));
        assert_eq!(
            current["transcript"]
                .as_str()
                .unwrap()
                .matches("Activity:")
                .count(),
            2
        );
        assert!(current["transcript"]
            .as_str()
            .unwrap()
            .ends_with("Agent: Done.\nActivity: Worked for 1s\n"));
        drain_requests();
        runtime.execute_command("AgentActivity").await.unwrap();
        let mut reopened = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::UpdateTextPanel { blocks, .. } = request {
                reopened |= blocks.len() == 4
                    && blocks[1].text.contains("missing.rs — not found")
                    && blocks[2].text == "Done."
                    && blocks[3].text == "Worked for 1s";
            }
        }
        assert!(
            reopened,
            "completed errors remain available above the answer"
        );
        drain_requests();
    }

    #[tokio::test]
    async fn bundled_agent_activity_disclosures_are_per_turn_and_details_are_on_demand() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({"session_id":"activity"}),
            )
            .await
            .unwrap();
        let state = |runtime: &Runtime| {
            let inner = runtime.inner.lock().unwrap();
            value_to_json(inner.host.policy().typed_states.get("agent").unwrap())
        };
        for turn in 0..2 {
            drain_requests();
            submit_agent_prompt(&mut runtime, &format!("turn {turn}")).await;
            for index in 0..8 {
                runtime.notify("agent:activity", serde_json::json!({"session_id":"activity","update":{
                    "session_update":"tool_call_update","tool_call_id":format!("{turn}:{index}"),
                    "title":format!("Reading file{index}.rs"),"full_title":format!("Reading /workspace/src/file{index}.rs"),
                    "kind":"read","status":"failed","detail":"full diagnostic outside workspace"
                }})).await.unwrap();
            }
            runtime
                .notify(
                    "agent:message_completed",
                    serde_json::json!({"session_id":"activity","text":"Answer"}),
                )
                .await
                .unwrap();
            runtime.notify("agent:completed", serde_json::json!({"session_id":"activity","stop_reason":"completed","elapsed_ms":2000})).await.unwrap();
        }
        let current = state(&runtime);
        let first_id = current["activity_history"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let second_id = current["activity_history"][1]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        drain_requests();
        runtime
            .notify(
                "panel:event:agent-conversation",
                serde_json::json!({"action":"activate_block","text":first_id}),
            )
            .await
            .unwrap();
        let mut found_preview = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::UpdateTextPanel { blocks, .. } = request {
                assert!(blocks
                    .iter()
                    .any(|b| b.id == second_id && b.text.starts_with("▸")));
                let preview = blocks
                    .iter()
                    .find(|b| b.id == format!("activity-details:{first_id}"))
                    .unwrap();
                assert_eq!(preview.text.lines().count(), 6);
                assert!(!preview.text.contains("/workspace"));
                assert!(!preview.text.contains("full diagnostic"));
                found_preview = true;
            }
        }
        assert!(found_preview);
        runtime.notify("panel:event:agent-conversation", serde_json::json!({"action":"activate_block","text":format!("activity-details:{first_id}")})).await.unwrap();
        let mut opened = false;
        let mut inspected = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::OpenWorkspace { id, .. } => opened |= id == "agent-activity",
                PluginRequest::UpdateWorkspace { id, model } if id == "agent-activity" => {
                    assert_eq!(model.rows.len(), 8);
                    let detail = model
                        .detail
                        .iter()
                        .flatten()
                        .map(|s| s.text.as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    inspected |= detail.contains("/workspace/src/file0.rs")
                        && detail.contains("full diagnostic");
                }
                _ => {}
            }
        }
        assert!(opened && inspected);
        assert!(!state(&runtime)["transcript"]
            .as_str()
            .unwrap()
            .contains("full diagnostic"));
        drain_requests();
    }

    #[tokio::test]
    async fn bundled_agent_completion_places_late_activity_before_answer_and_preserves_terminal_errors(
    ) {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({"session_id":"late"}),
            )
            .await
            .unwrap();
        let state = |runtime: &Runtime| {
            let inner = runtime.inner.lock().unwrap();
            value_to_json(inner.host.policy().typed_states.get("agent").unwrap())
        };
        drain_requests();
        submit_agent_prompt(&mut runtime, "explain it").await;
        runtime
            .notify(
                "agent:message_completed",
                serde_json::json!({"session_id":"late","text":"The useful answer."}),
            )
            .await
            .unwrap();
        runtime
            .notify(
                "agent:activity",
                serde_json::json!({"session_id":"late","update":{
                    "session_update":"tool_call_update","tool_call_id":"late-read","kind":"read",
                    "title":"Reading optional file","status":"failed","detail":"outside workspace"
                }}),
            )
            .await
            .unwrap();
        runtime.notify("agent:completed", serde_json::json!({"session_id":"late","stop_reason":"completed","elapsed_ms":19000})).await.unwrap();
        let current = state(&runtime);
        assert_eq!(
            current["transcript_blocks"][1]["text"],
            "▸ Activity · 1 action · 1 issue"
        );
        assert_eq!(
            current["transcript_blocks"][2]["text"],
            "The useful answer."
        );
        assert_eq!(current["transcript_blocks"][3]["text"], "Worked for 19s");
        assert!(!current["transcript"]
            .as_str()
            .unwrap()
            .contains("outside workspace"));

        drain_requests();
        submit_agent_prompt(&mut runtime, "try another request").await;
        runtime
            .notify(
                "agent:activity",
                serde_json::json!({"session_id":"late","update":{
                    "session_update":"tool_call","tool_call_id":"pending-read","kind":"read",
                    "title":"Reading source","status":"in_progress"
                }}),
            )
            .await
            .unwrap();
        runtime
            .notify(
                "agent:error",
                serde_json::json!({"session_id":"late","message":"Connection lost"}),
            )
            .await
            .unwrap();
        let current = state(&runtime);
        let last = current["transcript_blocks"]
            .as_array()
            .unwrap()
            .last()
            .unwrap();
        assert_eq!(last["kind"], "error");
        assert_eq!(last["text"], "Connection lost");
        assert!(current["transcript"]
            .as_str()
            .unwrap()
            .ends_with("Error: Connection lost\n"));
        drain_requests();
    }

    #[tokio::test]
    async fn bundled_agent_completion_preserves_context_hidden_by_clear() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({"session_id":"clear-progress"}),
            )
            .await
            .unwrap();
        drain_requests();
        submit_agent_prompt(&mut runtime, "earlier question").await;
        runtime
            .notify(
                "agent:message_completed",
                serde_json::json!({"session_id":"clear-progress","text":"Earlier answer."}),
            )
            .await
            .unwrap();
        runtime
            .notify(
                "agent:completed",
                serde_json::json!({"session_id":"clear-progress","stop_reason":"completed"}),
            )
            .await
            .unwrap();
        runtime.execute_command("AgentClear").await.unwrap();
        drain_requests();
        submit_agent_prompt(&mut runtime, "new question").await;
        runtime
            .notify(
                "agent:message_completed",
                serde_json::json!({"session_id":"clear-progress","text":"New answer."}),
            )
            .await
            .unwrap();
        runtime.notify("agent:completed", serde_json::json!({"session_id":"clear-progress","stop_reason":"completed","elapsed_ms":2000})).await.unwrap();
        let current = {
            let inner = runtime.inner.lock().unwrap();
            value_to_json(inner.host.policy().typed_states.get("agent").unwrap())
        };
        assert_eq!(current["transcript"], "You: earlier question\nAgent: Earlier answer.\nYou: new question\nAgent: New answer.\nActivity: Worked for 2s\n");
        assert_eq!(current["transcript_blocks"].as_array().unwrap().len(), 3);
        drain_requests();
    }

    #[tokio::test]
    async fn bundled_agent_activity_decodes_typed_updates_and_ignores_future_variants() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-typed" }),
            )
            .await
            .unwrap();
        drain_requests();

        let state = |runtime: &Runtime| {
            let inner = runtime.inner.lock().unwrap();
            value_to_json(inner.host.policy().typed_states.get("agent").unwrap())
        };

        runtime
            .notify(
                "agent:activity",
                serde_json::json!({
                    "session_id": "session-typed",
                    "update": {
                        "session_update": "agent_thought_chunk",
                        "content": { "text": "inspect imports" },
                    },
                }),
            )
            .await
            .unwrap();
        assert_eq!(state(&runtime)["thought"], "inspect imports");
        assert_eq!(state(&runtime)["ui_phase"], "thinking");
        drain_requests();

        runtime
            .notify(
                "agent:activity",
                serde_json::json!({
                    "session_id": "session-typed",
                    "update": { "session_update": "agent_thought_chunk" },
                }),
            )
            .await
            .unwrap();
        assert_eq!(state(&runtime)["thought"], "inspect imports");
        drain_requests();

        runtime
            .notify(
                "agent:activity",
                serde_json::json!({
                    "session_id": "session-typed",
                    "update": {
                        "session_update": "tool_call",
                        "tool_call_id": "tool-1",
                        "kind": "Read file",
                    },
                }),
            )
            .await
            .unwrap();
        let current = state(&runtime);
        assert_eq!(current["activity_rows"][0]["title"], "Read file");
        assert_eq!(current["activity_rows"][0]["status"], "in_progress");
        assert_eq!(current["ui_phase"], "tool");
        drain_requests();

        runtime
            .notify(
                "agent:activity",
                serde_json::json!({
                    "session_id": "session-typed",
                    "update": {
                        "session_update": "tool_call_update",
                        "tool_call_id": "tool-1",
                        "status": "completed",
                    },
                }),
            )
            .await
            .unwrap();
        let current = state(&runtime);
        assert_eq!(current["activity_rows"][0]["title"], "Read file");
        assert_eq!(current["activity_rows"][0]["status"], "completed");
        assert_eq!(current["ui_phase"], "working");
        drain_requests();

        let previous = state(&runtime);
        runtime
            .notify(
                "agent:activity",
                serde_json::json!({
                    "session_id": "session-typed",
                    "update": { "session_update": "future_host_event", "new_field": 7 },
                }),
            )
            .await
            .unwrap();
        assert_eq!(state(&runtime), previous);
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        runtime
            .notify(
                "agent:activity",
                serde_json::json!({
                    "session_id": "different-session",
                    "update": { "session_update": "plan" },
                }),
            )
            .await
            .unwrap();
        assert_eq!(state(&runtime), previous);
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn bundled_agent_completion_splits_tool_summary_from_elapsed_footer_and_persists_both() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-summary" }),
            )
            .await
            .unwrap();
        drain_requests();
        submit_agent_prompt(&mut runtime, "explain it").await;
        drain_requests();
        runtime
            .notify(
                "agent:activity",
                serde_json::json!({
                    "session_id": "session-summary",
                    "update": {
                        "session_update": "tool_call",
                        "tool_call_id": "tool-1",
                        "title": "Read source",
                        "status": "completed",
                    },
                }),
            )
            .await
            .unwrap();
        drain_requests();
        runtime
            .notify(
                "agent:update",
                serde_json::json!({
                    "session_id": "session-summary",
                    "text": "### Result\n\n**Done.**",
                }),
            )
            .await
            .unwrap();
        drain_requests();
        runtime
            .notify(
                "agent:completed",
                serde_json::json!({
                    "session_id": "session-summary",
                    "stop_reason": "completed",
                    "elapsed_ms": 13_000,
                }),
            )
            .await
            .unwrap();

        let mut saw_final_blocks = false;
        let mut saw_persisted_summary = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::UpdateTextPanel { id, blocks } => {
                    saw_final_blocks |= id == "agent-conversation"
                        && blocks.len() == 4
                        && blocks[1].kind == crate::plugin::TextPanelBlockKind::Action
                        && blocks[1].text == "▸ Activity · 1 action"
                        && blocks[2].kind == crate::plugin::TextPanelBlockKind::Agent
                        && blocks[2].format == crate::plugin::TextPanelBlockFormat::Markdown
                        && blocks[2].text == "### Result\n\n**Done.**"
                        && blocks[3].kind == crate::plugin::TextPanelBlockKind::Activity
                        && blocks[3].text == "Worked for 13s";
                }
                PluginRequest::SetPluginStorage { plugin, key, value } => {
                    saw_persisted_summary |= plugin == "agent"
                        && key == "transcript"
                        && value.as_str().is_some_and(|text| {
                            text.ends_with("Activity: ▸ Activity · 1 action\nAgent: ### Result\n\n**Done.**\nActivity: Worked for 13s\n")
                        });
                }
                _ => {}
            }
        }
        assert!(
            saw_final_blocks,
            "tool summary must precede the response and elapsed time must follow it"
        );
        assert!(
            saw_persisted_summary,
            "tool summary and elapsed footer must survive transcript restoration"
        );
    }

    #[tokio::test]
    async fn bundled_agent_preserves_conversations_larger_than_the_legacy_limit() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-long" }),
            )
            .await
            .unwrap();
        drain_requests();
        submit_agent_prompt(&mut runtime, "keep the complete answer").await;
        drain_requests();

        let response = format!("# Complete response\n\n{}\n\nEND", "a".repeat(21_000));
        runtime
            .notify(
                "agent:update",
                serde_json::json!({
                    "session_id": "session-long",
                    "text": response,
                }),
            )
            .await
            .unwrap();
        drain_requests();
        runtime
            .notify(
                "agent:completed",
                serde_json::json!({
                    "session_id": "session-long",
                    "stop_reason": "completed",
                    "elapsed_ms": 1_000,
                }),
            )
            .await
            .unwrap();

        let mut saw_complete_blocks = false;
        let mut saw_complete_storage = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::UpdateTextPanel { id, blocks } => {
                    saw_complete_blocks |= id == "agent-conversation"
                        && blocks.iter().any(|block| {
                            block.kind == crate::plugin::TextPanelBlockKind::Agent
                                && block.text.starts_with("# Complete response")
                                && block.text.ends_with("END")
                                && block.text.len() > 20_000
                        });
                }
                PluginRequest::SetPluginStorage { plugin, key, value } => {
                    saw_complete_storage |= plugin == "agent"
                        && key == "transcript"
                        && value.as_str().is_some_and(|text| {
                            text.starts_with(
                                "You: keep the complete answer\nAgent: # Complete response",
                            ) && text.contains("\n\nEND\n")
                                && text.len() > 20_000
                        });
                }
                _ => {}
            }
        }
        assert!(
            saw_complete_blocks,
            "the panel must retain the full response"
        );
        assert!(
            saw_complete_storage,
            "persistent history must retain the full response"
        );
    }

    #[tokio::test]
    async fn bundled_agent_plugin_creates_prompts_streams_and_cancels() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        runtime.execute_command("AgentStart").await.unwrap();
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("cwd"));
                request_id
            }
            _ => panic!("expected current-directory request"),
        };
        runtime
            .resolve_request(request_id, serde_json::json!({ "value": "/workspace" }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));

        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::CreateTextPanel { id, config }
                if id == "agent-conversation"
                    && config.side == crate::plugin::PanelSide::Right
                    && config.width == 62
                    && config.title.as_deref() == Some("Agent")
                    && config.header_actions.iter().map(|action| action.id.as_str()).eq(["activity", "clear", "new", "close"])
        ));
        resolve_prompt_history(&mut runtime, serde_json::json!([])).await;
        expect_agent_model_header();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, blocks }
                if id == "agent-conversation"
                    && blocks.len() == 1
                    && blocks[0].id == "empty"
                    && blocks[0].kind == crate::plugin::TextPanelBlockKind::Activity
                    && blocks[0].format == crate::plugin::TextPanelBlockFormat::Plain
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "Agent session started"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelComposerHistory { id, history }
                if id == "agent-conversation" && history.is_empty()
        ));

        runtime.execute_command("Agent").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::FocusTextPanelComposer { id } if id == "agent-conversation"
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        runtime
            .notify(
                "panel:event:agent-conversation",
                serde_json::json!({
                    "action": "submit",
                    "text": "  inspect the workspace\ninclude all unsaved changes  ",
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelComposerHistory { id, history }
                if id == "agent-conversation"
                    && history == ["  inspect the workspace\ninclude all unsaved changes  "]
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, blocks }
                if id == "agent-conversation"
                    && blocks.len() == 1
                    && blocks[0].id == "user:1"
                    && blocks[0].kind == crate::plugin::TextPanelBlockKind::User
                    && blocks[0].format == crate::plugin::TextPanelBlockFormat::Plain
                    && blocks[0].text == "  inspect the workspace\ninclude all unsaved changes  "
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::FocusTextPanelComposer { id } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelStatus { id, status: Some(status) }
                if id == "agent-conversation"
                    && status.busy
                    && status.label == "Waiting for agent…"
                    && !status.stream
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Refresh)
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, .. }
                if plugin == "agent" && key == "transcript"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentPrompt { session_id, text }
                if session_id == "session-1"
                    && text == "  inspect the workspace\ninclude all unsaved changes  "
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, value }
                if plugin == "agent"
                    && key == "prompt_history"
                    && value == serde_json::json!(["  inspect the workspace\ninclude all unsaved changes  "])
        ));
        runtime
            .notify(
                "agent:update",
                serde_json::json!({
                    "session_id": "session-1",
                    "text": "streamed output",
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, blocks }
                if id == "agent-conversation"
                    && blocks.len() == 2
                    && blocks[1].id == "agent:2"
                    && blocks[1].kind == crate::plugin::TextPanelBlockKind::Agent
                    && blocks[1].format == crate::plugin::TextPanelBlockFormat::Markdown
                    && blocks[1].text.is_empty()
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelStatus { id, status: Some(status) }
                if id == "agent-conversation"
                    && status.busy
                    && status.label == "Writing…"
                    && status.stream
        ));
        runtime
            .notify(
                "agent:update",
                serde_json::json!({
                    "session_id": "session-1",
                    "text": " 👋\nnext line",
                }),
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(70)).await;
        for callback in runtime.poll_timer_callbacks() {
            if let PluginRequest::TimeoutCallback { timer_id } = callback {
                runtime
                    .notify(
                        "timeout:callback",
                        serde_json::json!({ "timer_id": timer_id }),
                    )
                    .await
                    .unwrap();
            }
        }
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, value }
                if plugin == "agent"
                    && key == "transcript"
                    && value
                        == serde_json::json!("You:   inspect the workspace\ninclude all unsaved changes  \nAgent: streamed output 👋\nnext line")
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AppendTextPanel { id, block_id, delta }
                if id == "agent-conversation"
                    && block_id == "agent:2"
                    && delta == "streamed output 👋\nnext line"
        ));

        runtime
            .notify(
                "agent:update",
                serde_json::json!({
                    "session_id": "session-1",
                    "text": "\n\ncontinued",
                }),
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(70)).await;
        for callback in runtime.poll_timer_callbacks() {
            if let PluginRequest::TimeoutCallback { timer_id } = callback {
                runtime
                    .notify(
                        "timeout:callback",
                        serde_json::json!({ "timer_id": timer_id }),
                    )
                    .await
                    .unwrap();
            }
        }
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, .. }
                if plugin == "agent" && key == "transcript"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AppendTextPanel { id, block_id, delta }
                if id == "agent-conversation"
                    && block_id == "agent:2"
                    && delta == "\n\ncontinued"
        ));

        let large_delta = "z".repeat(20_001);
        runtime
            .notify(
                "agent:update",
                serde_json::json!({
                    "session_id": "session-1",
                    "text": large_delta,
                }),
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(70)).await;
        for callback in runtime.poll_timer_callbacks() {
            if let PluginRequest::TimeoutCallback { timer_id } = callback {
                runtime
                    .notify(
                        "timeout:callback",
                        serde_json::json!({ "timer_id": timer_id }),
                    )
                    .await
                    .unwrap();
            }
        }
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, value }
                if plugin == "agent"
                    && key == "transcript"
                    && value.as_str().is_some_and(|text| {
                        text.len() > 20_000 && text.ends_with(large_delta.as_str())
                    })
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AppendTextPanel { id, block_id, delta }
                if id == "agent-conversation"
                    && block_id == "agent:2"
                    && delta == large_delta
        ));

        runtime
            .notify(
                "agent:completed",
                serde_json::json!({
                    "session_id": "session-1",
                    "stop_reason": "completed",
                    "elapsed_ms": 3_723_000,
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, value }
                if plugin == "agent"
                    && key == "transcript"
                    && value.as_str().is_some_and(|text| text.ends_with('\n'))
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, blocks }
                if id == "agent-conversation"
                    && blocks.last().is_some_and(|block| {
                        block.kind == crate::plugin::TextPanelBlockKind::Activity
                            && block.text == "Worked for 1h 2m 3s"
                    })
                    && blocks.get(1).is_some_and(|block| block.kind == crate::plugin::TextPanelBlockKind::Agent)
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, value }
                if plugin == "agent"
                    && key == "transcript"
                    && value.as_str().is_some_and(|text| {
                        text.ends_with(&format!("{large_delta}\nActivity: Worked for 1h 2m 3s\n"))
                    })
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelStatus { id, status: None }
                if id == "agent-conversation"
        ));

        runtime.execute_command("AgentCancel").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentCancel { session_id } if session_id == "session-1"
        ));

        runtime
            .notify(
                "agent:permission_requested",
                serde_json::json!({
                    "request_id": "permission-1",
                    "session_id": "session-1",
                    "tool_call": { "tool_call_id": "tool-1" },
                    "options": [{
                        "option_id": "allow-once-exact",
                        "name": "Allow once",
                        "kind": "allow_once",
                    }],
                }),
            )
            .await
            .unwrap();
        let (permission_picker, permission_items) = recv_agent_picker("Agent permission");
        runtime
            .notify_picker(
                permission_picker,
                PickerCallback::Selected(permission_items[0].clone()),
            )
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentPermissionResponse {
                request_id,
                option_id: Some(option_id),
            } if request_id == "permission-1" && option_id == "allow-once-exact"
        ));

        runtime.execute_command("AgentHistory").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::OpenWorkspace { id, .. } if id == "agent-history"
        ));
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::EditHistory { request_id } => request_id,
            _ => panic!("expected attributed history request"),
        };
        runtime
            .resolve_request(request_id, serde_json::json!({ "entries": [] }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateWorkspace { id, .. } if id == "agent-history"
        ));
        runtime
            .notify(
                "workspace:event:agent-history",
                serde_json::json!({
                    "action": "escape",
                    "row": {
                        "data": {
                            "transaction_id": "transaction-1",
                            "edits": []
                        }
                    }
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::CloseWorkspace { id } if id == "agent-history"
        ));
    }

    #[tokio::test]
    async fn bundled_agent_rejects_a_concurrent_prompt_without_closing_the_active_stream() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        drain_requests();

        submit_agent_prompt(&mut runtime, "first prompt").await;
        let mut first_prompt = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            first_prompt |= matches!(
                request,
                PluginRequest::AgentPrompt { session_id, text }
                    if session_id == "session-1" && text == "first prompt"
            );
        }
        assert!(first_prompt);
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        drain_requests();
        runtime
            .notify(
                "agent:update",
                serde_json::json!({ "session_id": "session-1", "text": "original output" }),
            )
            .await
            .unwrap();
        drain_requests();
        runtime
            .notify(
                "agent:cancelled",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        let mut cancellation_notice = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::Action(Action::Print(message)) => {
                    cancellation_notice |= message == "Agent cancellation requested";
                }
                PluginRequest::AgentCloseSession { .. } => {
                    panic!("cancellation must not close an active stream before completion")
                }
                _ => {}
            }
        }
        assert!(cancellation_notice);
        runtime
            .notify(
                "agent:error",
                serde_json::json!({ "message": "replacement session could not be created" }),
            )
            .await
            .unwrap();
        let mut setup_status = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::Action(Action::Print(message)) => {
                    setup_status |= message.contains("setup failed while a turn is active");
                }
                PluginRequest::SetPluginStorage { plugin, key, .. }
                    if plugin == "agent" && key == "transcript" =>
                {
                    panic!("unscoped setup failure closed the active transcript")
                }
                PluginRequest::UpdateTextPanel { .. } | PluginRequest::AppendTextPanel { .. } => {
                    panic!("unscoped setup failure changed the active conversation")
                }
                _ => {}
            }
        }
        assert!(setup_status);

        submit_agent_prompt(&mut runtime, "concurrent prompt").await;
        let mut history_saved = false;
        let mut status = false;
        let mut queued_visible = false;
        let mut refreshed = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::SetPluginStorage { plugin, key, value }
                    if plugin == "agent" && key == "prompt_history" =>
                {
                    history_saved = value.as_array().is_some_and(|history| {
                        history.first().and_then(serde_json::Value::as_str)
                            == Some("concurrent prompt")
                    });
                }
                PluginRequest::Action(Action::Print(message)) => {
                    status |= message.contains("turn is still running");
                }
                PluginRequest::UpdateTextPanel { blocks, .. } => {
                    queued_visible |= blocks.iter().any(|block| {
                        block.kind == crate::plugin::TextPanelBlockKind::User
                            && block.text == "concurrent prompt"
                    });
                }
                PluginRequest::Action(Action::Refresh) => {
                    refreshed = true;
                }
                PluginRequest::AgentPrompt { .. } | PluginRequest::AppendTextPanel { .. } => {
                    panic!("concurrent prompt started before the active turn completed")
                }
                _ => {}
            }
        }
        assert!(history_saved);
        assert!(status);
        assert!(queued_visible);
        assert!(refreshed);
        runtime
            .notify(
                "agent:update",
                serde_json::json!({ "session_id": "session-1", "text": " still original" }),
            )
            .await
            .unwrap();
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        runtime
            .notify(
                "agent:completed",
                serde_json::json!({ "session_id": "session-1", "stop_reason": "end_turn" }),
            )
            .await
            .unwrap();
        let mut closed = false;
        let mut replacement_request_id = None;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::AgentCloseSession { session_id } => {
                    closed |= session_id == "session-1";
                }
                PluginRequest::GetConfig { request_id, key } => {
                    assert_eq!(key.as_deref(), Some("cwd"));
                    replacement_request_id = Some(request_id);
                }
                _ => {}
            }
        }
        assert!(closed, "completed cancelled stream must rotate its session");
        runtime
            .resolve_request(
                replacement_request_id.expect("queued prompt must request a replacement session"),
                serde_json::json!({ "value": "/workspace" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-2" }),
            )
            .await
            .unwrap();
        let mut replacement_dispatched = false;
        let mut dispatched_prompts = Vec::new();
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::UpdateTextPanel { blocks, .. } => {
                    assert!(
                        blocks
                            .iter()
                            .filter(|block| block.text == "concurrent prompt")
                            .count()
                            <= 1,
                        "a queued prompt must not duplicate during session rotation"
                    );
                }
                PluginRequest::AgentPrompt { session_id, text } => {
                    assert_ne!(session_id, "session-1");
                    dispatched_prompts.push((session_id.clone(), text.clone()));
                    replacement_dispatched = session_id == "session-2"
                        && text.ends_with("Follow-up:\nconcurrent prompt");
                }
                _ => {}
            }
        }
        assert!(
            replacement_dispatched,
            "expected queued prompt on replacement session, got {dispatched_prompts:?}"
        );
    }

    #[tokio::test]
    async fn bundled_agent_panel_submits_and_drains_followups_in_fifo_order() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        drain_requests();

        runtime
            .notify(
                "panel:event:agent-conversation",
                serde_json::json!({ "action": "submit", "text": "first prompt" }),
            )
            .await
            .unwrap();
        let mut first = false;
        let mut focused = false;
        let mut rendered = false;
        let mut busy = false;
        let mut refreshed = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::UpdateTextPanel { id, blocks } => {
                    rendered |= id == "agent-conversation"
                        && blocks.iter().any(|block| block.text == "first prompt");
                }
                PluginRequest::FocusTextPanelComposer { id } => {
                    focused |= id == "agent-conversation";
                }
                PluginRequest::SetTextPanelStatus {
                    id,
                    status: Some(status),
                } => {
                    busy |= id == "agent-conversation"
                        && status.busy
                        && status.label == "Waiting for agent…";
                }
                PluginRequest::Action(Action::Refresh) => {
                    assert!(rendered, "the submitted text must be ready before refresh");
                    assert!(busy, "the busy status must be ready before refresh");
                    refreshed = true;
                }
                PluginRequest::AgentPrompt { session_id, text } => {
                    assert!(
                        refreshed,
                        "the conversation must render before agent dispatch"
                    );
                    first |= session_id == "session-1" && text == "first prompt";
                }
                _ => {}
            }
        }
        assert!(first);
        assert!(focused);
        assert!(rendered);
        assert!(busy);
        assert!(refreshed);

        runtime
            .notify(
                "agent:update",
                serde_json::json!({ "session_id": "session-1", "text": "first answer" }),
            )
            .await
            .unwrap();
        drain_requests();

        for text in ["second prompt", "third prompt"] {
            runtime
                .notify(
                    "panel:event:agent-conversation",
                    serde_json::json!({ "action": "submit", "text": text }),
                )
                .await
                .unwrap();
        }
        let mut queued = 0;
        let mut refreshes = 0;
        let mut second_visible = false;
        let mut third_visible = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::UpdateTextPanel { id, blocks } => {
                    assert_eq!(id, "agent-conversation");
                    second_visible |= blocks.iter().any(|block| {
                        block.kind == crate::plugin::TextPanelBlockKind::User
                            && block.text == "second prompt"
                    });
                    third_visible |= blocks.iter().any(|block| {
                        block.kind == crate::plugin::TextPanelBlockKind::User
                            && block.text == "third prompt"
                    });
                }
                PluginRequest::Action(Action::Refresh) => {
                    refreshes += 1;
                }
                PluginRequest::Action(Action::Print(message)) => {
                    queued += usize::from(message.contains("follow-up queued"));
                }
                PluginRequest::AgentPrompt { .. } => {
                    panic!("follow-ups must not start while the first turn is active")
                }
                _ => {}
            }
        }
        assert_eq!(queued, 2);
        assert_eq!(refreshes, 2);
        assert!(second_visible);
        assert!(third_visible);

        runtime
            .notify(
                "agent:update",
                serde_json::json!({ "session_id": "session-1", "text": " continues" }),
            )
            .await
            .unwrap();
        assert!(
            ACTION_DISPATCHER.try_recv_request().is_none(),
            "queueing must not end the active stream"
        );

        runtime
            .notify(
                "agent:completed",
                serde_json::json!({ "session_id": "session-1", "stop_reason": "end_turn" }),
            )
            .await
            .unwrap();
        let mut delivered_second = false;
        let mut refreshed_second = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            assert!(
                !matches!(&request, PluginRequest::FocusPanel { .. }),
                "queued follow-ups must not steal panel focus"
            );
            match request {
                PluginRequest::UpdateTextPanel { blocks, .. } => {
                    assert_eq!(
                        blocks
                            .iter()
                            .filter(|block| block.text == "second prompt")
                            .count(),
                        1,
                        "promoting a queued prompt must not duplicate its block"
                    );
                }
                PluginRequest::Action(Action::Refresh) => {
                    refreshed_second = true;
                }
                PluginRequest::AgentPrompt { session_id, text } => {
                    assert!(refreshed_second);
                    delivered_second = session_id == "session-1" && text == "second prompt";
                }
                _ => {}
            }
        }
        assert!(delivered_second);

        runtime
            .notify(
                "agent:update",
                serde_json::json!({ "session_id": "session-1", "text": "second answer" }),
            )
            .await
            .unwrap();
        let mut ordered_before_pending = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::UpdateTextPanel { blocks, .. } = request {
                let second_user = blocks
                    .iter()
                    .position(|block| block.text == "second prompt")
                    .unwrap();
                let second_agent = blocks
                    .iter()
                    .position(|block| {
                        block.kind == crate::plugin::TextPanelBlockKind::Agent
                            && block.id != "agent:2"
                    })
                    .unwrap();
                let third_user = blocks
                    .iter()
                    .position(|block| block.text == "third prompt")
                    .unwrap();
                ordered_before_pending = second_user < second_agent && second_agent < third_user;
            }
        }
        assert!(
            ordered_before_pending,
            "the active answer must render before later queued prompts"
        );

        runtime
            .notify(
                "agent:completed",
                serde_json::json!({ "session_id": "session-1", "stop_reason": "end_turn" }),
            )
            .await
            .unwrap();
        let mut delivered_third = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            assert!(
                !matches!(&request, PluginRequest::FocusPanel { .. }),
                "queued follow-ups must not steal panel focus"
            );
            if let PluginRequest::AgentPrompt { session_id, text } = request {
                delivered_third = session_id == "session-1" && text == "third prompt";
            }
        }
        assert!(delivered_third);
    }

    #[tokio::test]
    async fn bundled_agent_clear_only_resets_the_visible_view_and_stream_timer() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        drain_requests();
        runtime
            .notify(
                "panel:event:agent-conversation",
                serde_json::json!({ "action": "submit", "text": "keep the context" }),
            )
            .await
            .unwrap();
        drain_requests();
        runtime
            .notify(
                "agent:update",
                serde_json::json!({ "session_id": "session-1", "text": "first chunk" }),
            )
            .await
            .unwrap();
        drain_requests();

        runtime.execute_command("AgentClear").await.unwrap();

        let mut cleared = false;
        let mut status = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::UpdateTextPanel { id, blocks } => {
                    cleared |= id == "agent-conversation" && blocks.is_empty();
                }
                PluginRequest::SetTextPanelComposerState {
                    id,
                    enabled,
                    status: value,
                } => {
                    status |= id == "agent-conversation"
                        && enabled
                        && value
                            .as_deref()
                            .is_some_and(|value| value.contains("context preserved"));
                }
                PluginRequest::SetPluginStorage { plugin, key, value }
                    if plugin == "agent"
                        && key == "transcript"
                        && value == serde_json::json!("") =>
                {
                    panic!("clear must preserve the durable transcript")
                }
                PluginRequest::ClearTextPanelComposer { .. } => {
                    panic!("clear must preserve the current draft")
                }
                PluginRequest::AgentCloseSession { .. } => {
                    panic!("clear must preserve the active session")
                }
                _ => {}
            }
        }
        assert!(cleared);
        assert!(status);
        tokio::time::sleep(Duration::from_millis(70)).await;
        assert!(runtime.poll_timer_callbacks().is_empty());

        runtime
            .notify(
                "agent:update",
                serde_json::json!({ "session_id": "session-1", "text": "after clear" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, blocks }
                if id == "agent-conversation"
                    && blocks.len() == 1
                    && blocks[0].kind == crate::plugin::TextPanelBlockKind::Agent
        ));
        drain_requests();
    }

    #[tokio::test]
    async fn bundled_agent_open_creates_and_focuses_composer_without_starting_a_session() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        runtime.execute_command("AgentOpen").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::CreateTextPanel { id, .. } if id == "agent-conversation"
        ));
        let history_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage {
                plugin,
                key,
                request_id,
            } => {
                assert_eq!(plugin, "agent");
                assert_eq!(key, "prompt_history");
                request_id
            }
            _ => panic!("expected restored prompt-history request"),
        };
        runtime
            .resolve_request(
                history_request_id,
                serde_json::json!({ "value": ["persisted prompt"] }),
            )
            .await
            .unwrap();
        expect_agent_model_header();
        let _ = expect_default_model_request();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, blocks }
                if id == "agent-conversation"
                    && blocks.len() == 1
                    && blocks[0].text.starts_with("No messages yet.")
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPanelVisible { id, visible: true } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::FocusTextPanelComposer { id } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelComposerHistory { id, history }
                if id == "agent-conversation" && history == ["persisted prompt"]
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        runtime.execute_command("AgentOpen").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPanelVisible { id, visible: true } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::FocusTextPanelComposer { id } if id == "agent-conversation"
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn bundled_agent_toggle_focuses_new_composer_and_restores_reopened_panel() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        runtime.execute_command("AgentToggle").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::CreateTextPanel { id, .. } if id == "agent-conversation"
        ));
        resolve_prompt_history(&mut runtime, serde_json::json!([])).await;
        expect_agent_model_header();
        let _ = expect_default_model_request();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, .. } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::FocusTextPanelComposer { id } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelComposerHistory { id, history }
                if id == "agent-conversation" && history.is_empty()
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        runtime.execute_command("AgentToggle").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPanelVisible { id, visible: false } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::FocusEditor
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        runtime.execute_command("AgentToggle").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPanelVisible { id, visible: true } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::RestorePanelFocus { id } if id == "agent-conversation"
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn bundled_agent_close_reopens_without_recreating_and_new_resets_the_session() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        drain_requests();

        runtime.execute_command("AgentClose").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPanelVisible { id, visible: false } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::FocusEditor
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        runtime.execute_command("AgentOpen").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPanelVisible { id, visible: true } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::FocusTextPanelComposer { id } if id == "agent-conversation"
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        runtime.execute_command("AgentClose").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPanelVisible { id, visible: false } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::FocusEditor
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        runtime.execute_command("AgentPrompt").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPanelVisible { id, visible: true } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::FocusTextPanelComposer { id } if id == "agent-conversation"
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        runtime.execute_command("AgentNew").await.unwrap();
        let mut closed = false;
        let mut cleared = false;
        let mut reset_storage = false;
        let mut reset_draft = false;
        let mut requested_cwd = None;
        let mut focused_composer = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::AgentCloseSession { session_id } => {
                    closed |= session_id == "session-1";
                }
                PluginRequest::UpdateTextPanel { id, blocks } => {
                    cleared |= id == "agent-conversation" && blocks.is_empty();
                }
                PluginRequest::SetPluginStorage { plugin, key, value } => {
                    reset_storage |=
                        plugin == "agent" && key == "transcript" && value == serde_json::json!("");
                }
                PluginRequest::ClearTextPanelComposer { id } => {
                    reset_draft |= id == "agent-conversation";
                }
                PluginRequest::GetConfig {
                    key, request_id, ..
                } if key.as_deref() == Some("cwd") => {
                    requested_cwd = Some(request_id);
                }
                PluginRequest::FocusTextPanelComposer { id } => {
                    focused_composer |= id == "agent-conversation";
                }
                PluginRequest::GetPluginStorage { key, .. } if key == "prompt_history" => {
                    panic!("New must not open the floating ask popup");
                }
                PluginRequest::CreateTextPanel { .. } => {
                    panic!("new must reuse the existing conversation panel")
                }
                _ => {}
            }
        }
        assert!(closed);
        assert!(cleared);
        assert!(reset_storage);
        assert!(reset_draft);
        assert!(focused_composer);
        let request_id = requested_cwd.expect("New starts a session immediately");
        runtime
            .resolve_request(request_id, serde_json::json!({"value":"/workspace"}))
            .await
            .unwrap();
        assert!(
            matches!(ACTION_DISPATCHER.recv_request(), PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace"))
        );
        drain_requests();

        // A prompt submitted before thread/start finishes must use that same start.
        runtime
            .notify(
                "panel:event:agent-conversation",
                serde_json::json!({"action":"submit","text":"hello"}),
            )
            .await
            .unwrap();
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            assert!(!matches!(
                request,
                PluginRequest::AgentNewSession { .. } | PluginRequest::GetConfig { .. }
            ));
        }
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({"session_id":"session-2"}),
            )
            .await
            .unwrap();
        let mut submitted = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::AgentPrompt {
                session_id, text, ..
            } = request
            {
                submitted |= session_id == "session-2" && text == "hello";
            }
        }
        assert!(submitted);

        runtime
            .notify(
                "agent:update",
                serde_json::json!({ "session_id": "session-1", "text": "late output" }),
            )
            .await
            .unwrap();
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn host_accepts_explicit_agent_context_and_exposes_context_requests() {
        drain_requests();
        let source = r#"
            pub fn activate() {
                red::add_command("Ask", ask);
                red::add_command("Context", context);
            }
            fn ask() {
                red::execute("AgentPromptWithContext", "session-1", "explain", Json {
                    uri: "file:///workspace/main.rs",
                    text: "fn main() {}",
                });
            }
            fn context() { red::request("GetAgentContext", loaded); }
            fn loaded(result: Json) {}
        "#;
        let mut runtime = Runtime::new();
        runtime.load_plugin("test", source).await.unwrap();

        runtime.execute_command("Ask").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentPromptWithContext { session_id, text, uri, context }
                if session_id == "session-1"
                    && text == "explain"
                    && uri == "file:///workspace/main.rs"
                    && context == "fn main() {}"
        ));
        runtime.execute_command("Context").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::GetAgentContext { .. }
        ));
    }

    #[tokio::test]
    async fn bundled_agent_rotates_a_cancelled_session_before_the_next_prompt() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        drain_requests();

        submit_agent_prompt(&mut runtime, "first prompt").await;
        drain_requests();
        runtime
            .notify(
                "agent:completed",
                serde_json::json!({ "session_id": "session-1", "stop_reason": "cancelled" }),
            )
            .await
            .unwrap();
        let mut closed = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            closed |= matches!(
                request,
                PluginRequest::AgentCloseSession { session_id } if session_id == "session-1"
            );
        }
        assert!(closed, "cancelled session must be closed cleanly");

        submit_agent_prompt(&mut runtime, "next prompt").await;
        let mut config_request = None;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::GetConfig { request_id, key } = request {
                assert_eq!(key.as_deref(), Some("cwd"));
                config_request = Some(request_id);
            }
        }
        runtime
            .resolve_request(
                config_request.expect("next prompt must request a replacement session"),
                serde_json::json!({ "value": "/workspace" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-2" }),
            )
            .await
            .unwrap();
        let mut replacement_prompt = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            replacement_prompt |= matches!(
                request,
                PluginRequest::AgentPrompt { session_id, text }
                    if session_id == "session-2"
                        && text.contains("Previous conversation (the last turn was interrupted):")
                        && text.ends_with("Follow-up:\nnext prompt")
            );
        }
        assert!(replacement_prompt);
    }

    #[tokio::test]
    async fn bundled_agent_rotates_when_completion_wins_the_cancellation_race() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        drain_requests();

        submit_agent_prompt(&mut runtime, "first prompt").await;
        drain_requests();
        runtime
            .notify(
                "agent:completed",
                serde_json::json!({ "session_id": "session-1", "stop_reason": "end_turn" }),
            )
            .await
            .unwrap();
        drain_requests();
        runtime
            .notify(
                "agent:cancelled",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();

        let mut closed = false;
        let mut idle = false;
        let mut stopping = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::AgentCloseSession { session_id } => {
                    closed |= session_id == "session-1";
                }
                PluginRequest::SetTextPanelStatus { id, status: None } => {
                    idle |= id == "agent-conversation";
                }
                PluginRequest::SetTextPanelStatus {
                    id,
                    status: Some(status),
                } => {
                    stopping |= id == "agent-conversation" && status.label == "Stopping…";
                }
                _ => {}
            }
        }
        assert!(closed, "late cancellation must close the unusable session");
        assert!(idle, "late cancellation must leave the completed turn idle");
        assert!(!stopping, "late cancellation must not restart busy status");

        submit_agent_prompt(&mut runtime, "next prompt").await;
        let mut config_request = None;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::GetConfig { request_id, key } = request {
                assert_eq!(key.as_deref(), Some("cwd"));
                config_request = Some(request_id);
            }
        }
        runtime
            .resolve_request(
                config_request.expect("next prompt must request a replacement session"),
                serde_json::json!({ "value": "/workspace" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-2" }),
            )
            .await
            .unwrap();
        let mut replacement_prompt = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            replacement_prompt |= matches!(
                request,
                PluginRequest::AgentPrompt { session_id, text }
                    if session_id == "session-2"
                        && text.contains("Previous conversation (the last turn was interrupted):")
                        && text.ends_with("Follow-up:\nnext prompt")
            );
        }
        assert!(replacement_prompt);
    }

    #[tokio::test]
    async fn bundled_agent_rotates_when_cancellation_wins_the_completion_race() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        drain_requests();

        submit_agent_prompt(&mut runtime, "first prompt").await;
        drain_requests();
        runtime
            .notify(
                "agent:update",
                serde_json::json!({ "session_id": "session-1", "text": "streamed output" }),
            )
            .await
            .unwrap();
        drain_requests();
        runtime
            .notify(
                "agent:cancelled",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        let mut reset_status = false;
        let mut stopping_status = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::SetTextPanelStatus { id, status: None } => {
                    reset_status |= id == "agent-conversation";
                }
                PluginRequest::SetTextPanelStatus {
                    id,
                    status: Some(status),
                } => {
                    if id == "agent-conversation" && status.label == "Stopping…" {
                        assert!(reset_status, "stopping must restart the elapsed timer");
                        stopping_status = status.busy;
                    }
                }
                PluginRequest::AgentCloseSession { .. } => {
                    panic!("cancellation must not close an active stream before completion")
                }
                _ => {}
            }
        }
        assert!(reset_status);
        assert!(stopping_status);
        runtime
            .notify(
                "agent:completed",
                serde_json::json!({ "session_id": "session-1", "stop_reason": "end_turn" }),
            )
            .await
            .unwrap();

        let mut closed = false;
        let mut transcript_saved = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::AgentCloseSession { session_id } => {
                    closed |= session_id == "session-1";
                }
                PluginRequest::SetPluginStorage { plugin, key, value } => {
                    transcript_saved |= plugin == "agent"
                        && key == "transcript"
                        && value
                            == serde_json::json!("You: first prompt\nAgent: streamed output\n");
                }
                _ => {}
            }
        }
        assert!(closed, "completed turn must close the cancelled session");
        assert!(transcript_saved, "completed stream must remain in history");

        submit_agent_prompt(&mut runtime, "next prompt").await;
        let mut config_request = None;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::GetConfig { request_id, key } = request {
                assert_eq!(key.as_deref(), Some("cwd"));
                config_request = Some(request_id);
            }
        }
        runtime
            .resolve_request(
                config_request.expect("next prompt must request a replacement session"),
                serde_json::json!({ "value": "/workspace" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-2" }),
            )
            .await
            .unwrap();
        let mut replacement_prompt = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            replacement_prompt |= matches!(
                request,
                PluginRequest::AgentPrompt { session_id, text }
                    if session_id == "session-2"
                        && text.contains("Previous conversation (the last turn was interrupted):")
                        && text.ends_with("Follow-up:\nnext prompt")
            );
        }
        assert!(replacement_prompt);
    }

    #[tokio::test]
    async fn bundled_agent_rotates_a_cancelled_session_after_other_terminal_events() {
        for (event, payload, transcript_suffix) in [
            (
                "agent:completed",
                serde_json::json!({ "session_id": "session-1", "stop_reason": "max_tokens" }),
                "System: Agent stopped: max_tokens\n",
            ),
            (
                "agent:error",
                serde_json::json!({ "session_id": "session-1", "message": "turn failed" }),
                "Error: turn failed\n",
            ),
        ] {
            drain_requests();
            let mut runtime = Runtime::new();
            runtime
                .load_plugin("agent", include_str!("../../plugins/agent.hk"))
                .await
                .unwrap();
            runtime
                .notify(
                    "agent:session_created",
                    serde_json::json!({ "session_id": "session-1" }),
                )
                .await
                .unwrap();
            drain_requests();
            submit_agent_prompt(&mut runtime, "first prompt").await;
            drain_requests();
            runtime
                .notify(
                    "agent:update",
                    serde_json::json!({ "session_id": "session-1", "text": "streamed output" }),
                )
                .await
                .unwrap();
            drain_requests();
            runtime
                .notify(
                    "agent:cancelled",
                    serde_json::json!({ "session_id": "session-1" }),
                )
                .await
                .unwrap();
            drain_requests();
            runtime.notify(event, payload).await.unwrap();

            let mut closed = false;
            let mut transcript_saved = false;
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                match request {
                    PluginRequest::AgentCloseSession { session_id } => {
                        closed |= session_id == "session-1";
                    }
                    PluginRequest::SetPluginStorage { plugin, key, value } => {
                        transcript_saved |= plugin == "agent"
                            && key == "transcript"
                            && value.as_str().is_some_and(|text| {
                                text.starts_with("You: first prompt\nAgent: streamed output\n")
                                    && text.ends_with(transcript_suffix)
                            });
                    }
                    _ => {}
                }
            }
            assert!(closed, "{event} must close the cancelled session");
            assert!(transcript_saved, "{event} must preserve streamed output");
        }
    }

    #[tokio::test]
    async fn bundled_agent_start_keeps_the_previous_session_until_replacement_is_created() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        drain_requests();

        runtime.execute_command("AgentStart").await.unwrap();
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("cwd"));
                request_id
            }
            _ => panic!("expected current-directory request"),
        };
        runtime
            .resolve_request(request_id, serde_json::json!({ "value": "/workspace" }))
            .await
            .unwrap();

        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        runtime.execute_command("AgentCancel").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentCancel { session_id } if session_id == "session-1"
        ));
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-2" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentCloseSession { session_id } if session_id == "session-1"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, .. } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "Agent session started"
        ));
    }

    #[tokio::test]
    async fn bundled_agent_retries_an_unsent_prompt_after_the_live_session_is_lost() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        drain_requests();

        submit_agent_prompt(&mut runtime, "retry this exact prompt").await;
        let mut saw_prompt = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::AgentPrompt { session_id, text } = request {
                assert_eq!(session_id, "session-1");
                assert_eq!(text, "retry this exact prompt");
                saw_prompt = true;
            }
        }
        assert!(saw_prompt);

        runtime
            .notify(
                "agent:session_lost",
                serde_json::json!({
                    "session_id": "session-1",
                    "prompt": "retry this exact prompt",
                    "message": "no Codex session is running"
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelStatus { id, status: None }
                if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentArchiveSession { session_id } if session_id == "session-1"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message == "no Codex session is running; retrying the saved prompt"
        ));
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("cwd"));
                request_id
            }
            _ => panic!("expected a current-directory request for the saved prompt"),
        };
        runtime
            .resolve_request(request_id, serde_json::json!({ "value": "/workspace" }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));

        runtime
            .notify(
                "agent:error",
                serde_json::json!({ "message": "Codex app-server stopped" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelStatus { id, status: None }
                if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, .. } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, .. }
                if plugin == "agent" && key == "transcript"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message.contains("prompt is preserved")
        ));
        recv_agent_picker("Retry Codex");

        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-2" }),
            )
            .await
            .unwrap();
        let blocks = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdateTextPanel { id, blocks } => {
                assert_eq!(id, "agent-conversation");
                blocks
            }
            _ => panic!("expected the restored conversation panel"),
        };
        assert_eq!(
            blocks
                .iter()
                .filter(|block| block.text == "retry this exact prompt")
                .count(),
            1
        );
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "Agent session started"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentPrompt { session_id, text }
                if session_id == "session-2" && text == "retry this exact prompt"
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn bundled_agent_opens_setup_when_the_adapter_exits_during_lazy_start() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        runtime.execute_command("Agent").await.unwrap();
        let history_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage { request_id, .. } => request_id,
            _ => panic!("expected the agent prompt-history request"),
        };
        runtime
            .resolve_request(history_request_id, serde_json::json!({ "value": [] }))
            .await
            .unwrap();
        let composer = recv_agent_composer().0;
        runtime
            .notify_composer(
                composer,
                ComposerCallback::Submitted("keep this prompt".to_string()),
            )
            .unwrap();
        let cwd_request_id = recv_optimistic_agent_start(
            "keep this prompt",
            serde_json::json!(["keep this prompt"]),
            true,
        );
        runtime
            .resolve_request(cwd_request_id, serde_json::json!({ "value": "/workspace" }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));

        runtime
            .notify(
                "agent:session_lost",
                serde_json::json!({ "message": "Codex app-server stopped" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelStatus { id, status: None }
                if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelStatus { id, status: None }
                if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, blocks }
                if id == "agent-conversation"
                    && blocks.len() == 1
                    && blocks[0].text == "keep this prompt"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, .. }
                if plugin == "agent" && key == "transcript"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, blocks }
                if id == "agent-conversation"
                    && blocks.len() == 2
                    && blocks[0].text == "keep this prompt"
                    && blocks[1].text == "Codex app-server stopped"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, .. }
                if plugin == "agent" && key == "transcript"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message.contains("prompt is preserved")
        ));
        recv_agent_picker("Retry Codex");

        runtime.execute_command("Agent").await.unwrap();
        let history_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage { request_id, .. } => request_id,
            _ => panic!("expected the saved-prompt history request"),
        };
        runtime
            .resolve_request(
                history_request_id,
                serde_json::json!({ "value": ["keep this prompt"] }),
            )
            .await
            .unwrap();
        let (_, _, query, _) = recv_agent_composer();
        assert_eq!(query, "keep this prompt");
    }

    #[tokio::test]
    async fn bundled_agent_ignores_late_events_from_a_replaced_session() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        drain_requests();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-2" }),
            )
            .await
            .unwrap();
        drain_requests();

        for (event, payload) in [
            (
                "agent:update",
                serde_json::json!({ "session_id": "session-1", "text": "stale output" }),
            ),
            (
                "agent:completed",
                serde_json::json!({ "session_id": "session-1", "stop_reason": "end_turn" }),
            ),
            (
                "agent:cancelled",
                serde_json::json!({ "session_id": "session-1" }),
            ),
            (
                "agent:error",
                serde_json::json!({ "session_id": "session-1", "message": "stale error" }),
            ),
            (
                "agent:permission_requested",
                serde_json::json!({
                    "session_id": "session-1",
                    "request_id": "stale-permission",
                    "options": [{"option_id": "allow", "name": "Allow", "kind": "allow_once"}]
                }),
            ),
        ] {
            runtime.notify(event, payload).await.unwrap();
        }

        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn composer_submission_is_delivered_only_to_the_plugin_that_opened_it() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "owner",
                r#"
                    pub fn activate() {
                        red::add_command("OpenComposer", open);
                        red::on("composer:submitted:919", submitted);
                    }
                    fn open() {
                        red::execute("OpenAgentComposer", "Private prompt", 919, "draft", ["recent"]);
                    }
                    fn submitted(prompt: Json) {
                        red::execute("Print", "owner:" + red::string(prompt, ""));
                    }
                "#,
            )
            .await
            .unwrap();
        runtime
            .load_plugin(
                "observer",
                r#"
                    pub fn activate() { red::on("composer:submitted:919", submitted); }
                    fn submitted(prompt: Json) {
                        red::execute("Print", "observer:" + red::string(prompt, ""));
                    }
                "#,
            )
            .await
            .unwrap();

        runtime.execute_command("OpenComposer").await.unwrap();
        let owner = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenAgentComposer {
                owner,
                title,
                id,
                query,
                history,
            } => {
                assert_eq!(title.as_deref(), Some("Private prompt"));
                assert_eq!(id, 919);
                assert_eq!(query, "draft");
                assert_eq!(history, ["recent"]);
                owner
            }
            _ => panic!("expected agent composer request"),
        };
        assert_eq!(owner, "owner");

        let failures = runtime.notify_plugin_isolated(
            &owner,
            "composer:submitted:919",
            serde_json::json!("private prompt\n  with whitespace  "),
        );

        assert!(failures.is_empty());
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message == "owner:private prompt\n  with whitespace  "
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn bundled_agent_plugin_bounds_history_preserves_text_and_ignores_picker_events() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        runtime.execute_command("Agent").await.unwrap();
        let history_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage {
                plugin,
                key,
                request_id,
            } => {
                assert_eq!(plugin, "agent");
                assert_eq!(key, "prompt_history");
                request_id
            }
            _ => panic!("expected agent prompt-history request"),
        };
        let expected_history = (0..50)
            .map(|index| format!("  prompt {index}\n    detail {index}  "))
            .collect::<Vec<_>>();
        let mut stored_history = (0..54)
            .map(|index| format!("  prompt {index}\n    detail {index}  "))
            .collect::<Vec<_>>();
        let duplicate = stored_history[0].clone();
        stored_history.insert(1, duplicate);
        stored_history.insert(2, " \n \t ".to_string());
        runtime
            .resolve_request(
                history_request_id,
                serde_json::json!({ "value": stored_history }),
            )
            .await
            .unwrap();
        let (composer, title, query, history) = recv_agent_composer();
        assert_eq!(title.as_deref(), Some("Agent prompt"));
        assert!(query.is_empty());
        assert_eq!(history, expected_history);

        for (event, payload) in [
            ("picker:query:802", serde_json::json!("do not round-trip")),
            (
                "picker:action:802",
                serde_json::json!({ "action": "history_back" }),
            ),
            ("picker:selected:802", serde_json::json!({ "id": "submit" })),
            ("composer:cancelled:802", serde_json::json!({})),
        ] {
            runtime.notify(event, payload).await.unwrap();
            assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        }

        let submitted = expected_history[10].clone();
        runtime
            .notify_composer(composer, ComposerCallback::Submitted(submitted.clone()))
            .unwrap();
        let mut expected_saved = vec![submitted.clone()];
        expected_saved.extend(
            expected_history
                .iter()
                .filter(|entry| entry.as_str() != submitted)
                .take(49)
                .cloned(),
        );
        let _ = recv_optimistic_agent_start(&submitted, serde_json::json!(expected_saved), true);
    }

    #[tokio::test]
    async fn bundled_agent_plugin_lazily_starts_and_preserves_prompt() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        runtime.execute_command("Agent").await.unwrap();
        let history_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage {
                plugin,
                key,
                request_id,
            } => {
                assert_eq!(plugin, "agent");
                assert_eq!(key, "prompt_history");
                request_id
            }
            _ => panic!("expected agent prompt-history request"),
        };
        runtime
            .resolve_request(history_request_id, serde_json::json!({ "value": [] }))
            .await
            .unwrap();
        let (composer, title, query, history) = recv_agent_composer();
        assert_eq!(title.as_deref(), Some("Agent prompt"));
        assert!(query.is_empty());
        assert!(history.is_empty());

        runtime
            .notify_composer(
                composer,
                ComposerCallback::Submitted("inspect unsaved changes".to_string()),
            )
            .unwrap();
        let cwd_request_id = recv_optimistic_agent_start(
            "inspect unsaved changes",
            serde_json::json!(["inspect unsaved changes"]),
            true,
        );
        runtime
            .resolve_request(cwd_request_id, serde_json::json!({ "value": "/workspace" }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));

        runtime
            .notify(
                "agent:error",
                serde_json::json!({ "message": "Codex login required" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelStatus { id, status: None }
                if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, blocks }
                if id == "agent-conversation"
                    && blocks.len() == 1
                    && blocks[0].text == "inspect unsaved changes"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, .. }
                if plugin == "agent" && key == "transcript"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, blocks }
                if id == "agent-conversation"
                    && blocks.len() == 2
                    && blocks[0].text == "inspect unsaved changes"
                    && blocks[1].text == "Codex login required"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, .. }
                if plugin == "agent" && key == "transcript"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message.contains("prompt is preserved")
        ));
        let (setup_picker, items) = recv_agent_picker("Retry Codex");
        assert_eq!(
            items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["retry", "logs"]
        );
        assert_eq!(
            items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            ["Retry the saved prompt", "Open Red logs"]
        );

        runtime
            .notify_picker(setup_picker, PickerCallback::Cancelled)
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message == "Agent prompt saved. Press Space A when ready to retry"
        ));
        runtime.execute_command("Agent").await.unwrap();
        let history_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage {
                plugin,
                key,
                request_id,
            } => {
                assert_eq!(plugin, "agent");
                assert_eq!(key, "prompt_history");
                request_id
            }
            _ => panic!("expected saved-prompt history request"),
        };
        runtime
            .resolve_request(
                history_request_id,
                serde_json::json!({ "value": ["inspect unsaved changes"] }),
            )
            .await
            .unwrap();
        let (composer, title, query, history) = recv_agent_composer();
        assert_eq!(title.as_deref(), Some("Agent prompt"));
        assert_eq!(query, "inspect unsaved changes");
        assert_eq!(history, ["inspect unsaved changes"]);

        runtime
            .notify_composer(
                composer,
                ComposerCallback::Submitted("inspect unsaved changes".to_string()),
            )
            .unwrap();
        let cwd_request_id = recv_optimistic_agent_start(
            "inspect unsaved changes",
            serde_json::json!(["inspect unsaved changes"]),
            false,
        );
        runtime
            .resolve_request(cwd_request_id, serde_json::json!({ "value": "/workspace" }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));

        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-lazy" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, .. } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "Agent session started"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentPrompt { session_id, text }
                if session_id == "session-lazy" && text == "inspect unsaved changes"
        ));

        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn bundled_agent_plugin_setup_actions_dispatch_and_cancel_keeps_prompt() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        let (setup_picker, items) = open_agent_setup_picker(&mut runtime).await;
        runtime
            .notify_picker(setup_picker, PickerCallback::Selected(items[0].clone()))
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::GetConfig { key, .. } if key.as_deref() == Some("cwd")
        ));

        drain_requests();
        let (setup_picker, items) = open_agent_setup_picker(&mut runtime).await;
        runtime
            .notify_picker(setup_picker, PickerCallback::Selected(items[1].clone()))
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::ViewLogs)
        ));

        drain_requests();
        let (setup_picker, _) = open_agent_setup_picker(&mut runtime).await;
        runtime
            .notify_picker(setup_picker, PickerCallback::Cancelled)
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message == "Agent prompt saved. Press Space A when ready to retry"
        ));
    }

    #[tokio::test]
    async fn bundled_agent_plugin_legacy_start_failure_opens_setup() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        runtime.execute_command("AgentStart").await.unwrap();
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("cwd"));
                request_id
            }
            _ => panic!("expected agent current-directory request"),
        };
        runtime
            .resolve_request(request_id, serde_json::json!({ "value": "/workspace" }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));
        runtime
            .notify(
                "agent:session_lost",
                serde_json::json!({
                    "session_id": "",
                    "prompt": "",
                    "message": "Codex live authentication was rejected by workplace policy"
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelStatus { id, status: None }
                if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::CreateTextPanel { id, .. } if id == "agent-conversation"
        ));
        resolve_prompt_history(&mut runtime, serde_json::json!([])).await;
        expect_agent_model_header();
        let _ = expect_default_model_request();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelStatus { id, status: None }
                if id == "agent-conversation"
        ));
        let blocks = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdateTextPanel { id, blocks } => {
                assert_eq!(id, "agent-conversation");
                blocks
            }
            _ => panic!("expected persistent agent startup diagnostic"),
        };
        assert!(blocks.iter().any(|block| {
            block.kind == crate::plugin::TextPanelBlockKind::Error
                && block
                    .text
                    .contains("live authentication was rejected by workplace policy")
        }));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, .. }
                if plugin == "agent" && key == "transcript"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message.contains("live authentication was rejected by workplace policy")
                    && !message.contains("prompt is preserved")
        ));
        let (_, items) = recv_agent_picker("Retry Codex");
        assert_eq!(items[0].label, "Retry Codex startup");
        assert_eq!(items[1].label, "Open Red logs");
    }

    #[tokio::test]
    async fn bundled_agent_plugin_restores_markdown_tables_and_blank_lines() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        let markdown = "# Accepted arguments\n\n| Argument | Meaning |\n|---|---|\n| `--root` | Set the root |\n\nTrailing paragraph.";
        runtime
            .notify(
                "agent:transcript_restored",
                serde_json::json!({
                    "transcript": format!(
                        "You: list the arguments\nAgent: {markdown}\nActivity: Worked for 13s\nSystem: Agent stopped: end_turn\n"
                    )
                }),
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdateTextPanel { id, blocks } => {
                assert_eq!(id, "agent-conversation");
                assert_eq!(blocks.len(), 3);
                assert_eq!(blocks[0].kind, crate::plugin::TextPanelBlockKind::User);
                assert_eq!(blocks[0].text, "list the arguments");
                assert_eq!(blocks[1].kind, crate::plugin::TextPanelBlockKind::Agent);
                assert_eq!(
                    blocks[1].format,
                    crate::plugin::TextPanelBlockFormat::Markdown
                );
                assert_eq!(blocks[1].text, markdown);
                assert_eq!(blocks[2].kind, crate::plugin::TextPanelBlockKind::Activity);
                assert_eq!(blocks[2].text, "Worked for 13s");
            }
            _ => panic!("expected restored text panel update"),
        }
    }

    #[tokio::test]
    async fn bundled_agent_plugin_recreates_only_a_visible_restored_pane() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        runtime
            .notify(
                "editor:panes_restore",
                serde_json::json!({
                    "panels": [{ "id": "agent-conversation", "visible": false }]
                }),
            )
            .await
            .unwrap();
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        runtime
            .notify(
                "editor:panes_restore",
                serde_json::json!({
                    "panels": [{ "id": "agent-conversation", "visible": true }]
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::CreateTextPanel { id, .. } if id == "agent-conversation"
        ));
        resolve_prompt_history(&mut runtime, serde_json::json!([])).await;
        expect_agent_model_header();
        let _ = expect_default_model_request();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, .. } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelComposerHistory { id, history }
                if id == "agent-conversation" && history.is_empty()
        ));
    }

    #[tokio::test]
    async fn bundled_agent_plugin_restores_a_truncated_leading_response_as_markdown() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        let truncated_response = concat!(
            "nd raylib. This keeps the program small.\n\n",
            "### 2. Application entry point\n\n",
            "All behavior is implemented inside `main()`."
        );
        runtime
            .notify(
                "agent:transcript_restored",
                serde_json::json!({
                    "transcript": format!(
                        "{truncated_response}\nYou: review it again\nAgent: # Updated review\n"
                    )
                }),
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdateTextPanel { id, blocks } => {
                assert_eq!(id, "agent-conversation");
                assert_eq!(blocks.len(), 3);
                assert_eq!(blocks[0].kind, crate::plugin::TextPanelBlockKind::Agent);
                assert_eq!(
                    blocks[0].format,
                    crate::plugin::TextPanelBlockFormat::Markdown
                );
                assert_eq!(blocks[0].text, truncated_response);
                assert_eq!(blocks[1].kind, crate::plugin::TextPanelBlockKind::User);
                assert_eq!(blocks[1].text, "review it again");
                assert_eq!(blocks[2].kind, crate::plugin::TextPanelBlockKind::Agent);
                assert_eq!(
                    blocks[2].format,
                    crate::plugin::TextPanelBlockFormat::Markdown
                );
            }
            _ => panic!("expected restored text panel update"),
        }
    }

    #[tokio::test]
    async fn bundled_agent_plugin_restores_every_turn_in_a_multi_turn_transcript() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        runtime
            .notify(
                "agent:transcript_restored",
                serde_json::json!({
                    "transcript": concat!(
                        "You: first question\n",
                        "Agent: first answer\n",
                        "You: second question\n",
                        "Agent: second answer\n",
                        "You: third question\n",
                        "Agent: third answer\n",
                        "You: fourth question\n",
                        "Agent: fourth answer\n",
                    )
                }),
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdateTextPanel { id, blocks } => {
                assert_eq!(id, "agent-conversation");
                assert_eq!(blocks.len(), 8);
                assert_eq!(blocks[0].text, "first question");
                assert_eq!(blocks[1].text, "first answer");
                assert_eq!(blocks[6].text, "fourth question");
                assert_eq!(blocks[7].text, "fourth answer");
            }
            _ => panic!("expected restored text panel update"),
        }

        runtime.execute_command("Agent").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::CreateTextPanel { id, .. } if id == "agent-conversation"
        ));
        resolve_prompt_history(&mut runtime, serde_json::json!([])).await;
        expect_agent_model_header();
        let _ = expect_default_model_request();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, blocks }
                if id == "agent-conversation"
                    && blocks.len() == 8
                    && blocks[7].text == "fourth answer"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelComposerState {
                id,
                enabled: true,
                status: Some(status),
            } if id == "agent-conversation" && status.contains("Archived conversation")
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::FocusTextPanelComposer { id } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelComposerHistory { id, history }
                if id == "agent-conversation" && history.is_empty()
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn bundled_agent_plugin_restores_the_bound_codex_thread_before_enabling_input() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        runtime
            .notify(
                "agent:conversation_restore_pending",
                serde_json::json!({
                    "thread_id": "thread-restored",
                    "cwd": "/workspace",
                    "items": [
                        {"id": "user-1", "turn_id": "turn-1", "role": "user", "text": "Earlier question"},
                        {"id": "agent-1", "turn_id": "turn-1", "role": "agent", "text": "Earlier answer"}
                    ]
                }),
            )
            .await
            .unwrap();

        let mut saw_disabled_composer = false;
        let mut saw_resume = false;
        let mut saw_cached_transcript = false;
        let mut history_request_id = None;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::UpdateTextPanel { blocks, .. } => {
                    saw_cached_transcript = blocks.len() == 2
                        && blocks[0].text == "Earlier question"
                        && blocks[1].text == "Earlier answer";
                }
                PluginRequest::SetTextPanelComposerState { enabled: false, .. } => {
                    saw_disabled_composer = true;
                }
                PluginRequest::AgentResumeSession { cwd, session_id } => {
                    saw_resume = cwd == Path::new("/workspace") && session_id == "thread-restored";
                }
                PluginRequest::GetPluginStorage {
                    plugin,
                    key,
                    request_id,
                } => {
                    assert_eq!(plugin, "agent");
                    assert_eq!(key, "prompt_history");
                    history_request_id = Some(request_id);
                }
                _ => {}
            }
        }
        assert!(saw_cached_transcript);
        assert!(saw_disabled_composer);
        assert!(saw_resume);
        runtime
            .resolve_request(
                history_request_id.expect("prompt-history request"),
                serde_json::json!({ "value": ["restored prompt"] }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelComposerHistory { id, history }
                if id == "agent-conversation" && history == ["restored prompt"]
        ));

        runtime
            .notify(
                "agent:session_restored",
                serde_json::json!({
                    "thread_id": "thread-restored",
                    "cwd": "/workspace",
                    "items": [
                        {"id": "native-user", "turn_id": "native-turn", "role": "user", "text": "Earlier question"},
                        {"id": "native-agent", "turn_id": "native-turn", "role": "agent", "text": "Earlier answer"}
                    ]
                }),
            )
            .await
            .unwrap();

        let mut saw_enabled_composer = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if matches!(
                request,
                PluginRequest::SetTextPanelComposerState { enabled: true, .. }
            ) {
                saw_enabled_composer = true;
            }
        }
        assert!(saw_enabled_composer);
    }

    fn expect_default_model_request() -> RequestId {
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::AgentModelRequest {
                request_id,
                request: crate::codex::ModelRequest::ReadDefault { cwd },
            } => {
                assert_eq!(cwd, crate::utils::get_workspace_path());
                request_id
            }
            _ => panic!("expected read-only default model request"),
        }
    }

    async fn open_for_default_model(runtime: &mut Runtime) -> RequestId {
        runtime.execute_command("AgentOpen").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::CreateTextPanel { .. }
        ));
        let history_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage {
                plugin,
                key,
                request_id,
            } => {
                assert_eq!(plugin, "agent");
                assert_eq!(key, "prompt_history");
                request_id
            }
            _ => panic!("expected prompt-history request"),
        };
        runtime
            .resolve_request(history_request_id, serde_json::json!({ "value": [] }))
            .await
            .unwrap();
        let catalog_request = expect_agent_model_header();
        let request_id = expect_default_model_request();
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            assert!(matches!(
                request,
                PluginRequest::UpdateTextPanel { .. }
                    | PluginRequest::SetPanelVisible { .. }
                    | PluginRequest::FocusTextPanelComposer { .. }
                    | PluginRequest::SetTextPanelComposerHistory { .. }
            ));
        }
        runtime
            .resolve_request(catalog_request, test_model_catalog())
            .await
            .unwrap();
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        request_id
    }

    #[tokio::test]
    async fn bundled_agent_default_model_preview_is_read_only_and_thread_settings_win() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        let request_id = open_for_default_model(&mut runtime).await;
        runtime
            .resolve_request(
                request_id,
                serde_json::json!({"error":"","model_info":{"model":"configured","effort":"high"}}),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::SetTextPanelHeaderDetail {
                detail: Some(detail),
                ..
            } => {
                assert_eq!(
                    (detail.text.as_str(), detail.secondary.as_str()),
                    ("configured", "high")
                );
            }
            _ => panic!("preview must only update the header"),
        }
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        runtime.execute_command("AgentClose").await.unwrap();
        drain_requests();
        runtime.execute_command("AgentOpen").await.unwrap();
        let late = expect_default_model_request();
        drain_requests();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({"session_id":"thread"}),
            )
            .await
            .unwrap();
        runtime.notify("agent:model_changed", serde_json::json!({"session_id":"thread","model_info":{"model":"confirmed","effort":"low"}})).await.unwrap();
        assert_eq!(take_model_header().unwrap().text, "confirmed");
        runtime.resolve_request(late, serde_json::json!({"error":"","model_info":{"model":"stale-default","effort":"high"}})).await.unwrap();
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn bundled_agent_default_model_preview_does_not_replace_an_explicit_choice() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        let default_request = open_for_default_model(&mut runtime).await;
        let (picker, items, _) = open_test_model_picker(&mut runtime).await;
        runtime
            .notify_picker(picker, PickerCallback::Selected(items[1].clone()))
            .unwrap();
        let (effort_picker, efforts, _) = recv_model_picker();
        runtime
            .notify_picker(effort_picker, PickerCallback::Selected(efforts[0].clone()))
            .unwrap();
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::AgentModelRequest {
                request_id,
                request: crate::codex::ModelRequest::Set { session_id, .. },
            } => {
                assert!(session_id.is_empty());
                request_id
            }
            _ => panic!("expected pre-session selection"),
        };
        runtime
            .resolve_request(request_id, serde_json::json!({"accepted":true,"error":""}))
            .await
            .unwrap();
        let header = take_model_header().unwrap();
        assert_eq!(
            (header.text.as_str(), header.secondary.as_str()),
            ("second", "low")
        );
        runtime
            .resolve_request(
                default_request,
                serde_json::json!({"error":"","model_info":{"model":"first","effort":"high"}}),
            )
            .await
            .unwrap();
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn bundled_agent_default_model_lookup_failure_is_quiet_and_retries_on_reopen() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        let request_id = open_for_default_model(&mut runtime).await;
        runtime
            .resolve_request(request_id, serde_json::json!({"error":"unavailable"}))
            .await
            .unwrap();
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        runtime.execute_command("AgentClose").await.unwrap();
        drain_requests();
        runtime.execute_command("AgentOpen").await.unwrap();
        let retry = expect_default_model_request();
        drain_requests();
        runtime
            .resolve_request(
                retry,
                serde_json::json!({"error":"","model_info":{"model":"recovered"}}),
            )
            .await
            .unwrap();
        assert_eq!(take_model_header().unwrap().text, "recovered");
    }

    fn recv_model_picker() -> (PickerHandle, Vec<PickerItem>, crate::ui::PickerOptions) {
        loop {
            match ACTION_DISPATCHER.recv_request() {
                PluginRequest::OpenCallbackPicker {
                    handle,
                    items,
                    options,
                    ..
                } => return (handle, items, options),
                PluginRequest::Action(Action::Print(_)) => {}
                _ => panic!("expected model picker"),
            }
        }
    }

    fn take_model_header() -> Option<crate::plugin::TextPanelHeaderDetail> {
        let mut latest = None;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::SetTextPanelHeaderDetail { detail, .. } = request {
                latest = detail;
            }
        }
        latest
    }

    fn expect_model_catalog_request() -> RequestId {
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::AgentModelRequest {
                request_id,
                request: crate::codex::ModelRequest::List,
            } => request_id,
            _ => panic!("expected model catalog request"),
        }
    }

    fn test_model_catalog() -> serde_json::Value {
        serde_json::json!({"error":"", "models":[
            {"model":"first","displayName":"First","isDefault":true,"defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"high","description":"More reasoning"}]},
            {"model":"second","displayName":"Second","defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low","description":"Less reasoning"}]}
        ]})
    }

    fn recv_loaded_model_picker(handle: PickerHandle) -> (Vec<PickerItem>, String) {
        assert!(matches!(ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerBusy { id, busy: false } if id == handle.get()));
        let items = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePickerItems { id, items } if id == handle.get() => items,
            _ => panic!("expected in-place model catalog update"),
        };
        let selection = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePickerSelection { id, selection } if id == handle.get() => {
                selection
            }
            _ => panic!("expected current model selection"),
        };
        assert!(matches!(ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerStatus { id, status: Some(status) }
                if id == handle.get() && status == "This conversation only"));
        (items, selection)
    }

    async fn open_test_model_picker(
        runtime: &mut Runtime,
    ) -> (PickerHandle, Vec<PickerItem>, crate::ui::PickerOptions) {
        runtime.execute_command("AgentModel").await.unwrap();
        let (handle, items, mut options) = recv_model_picker();
        if !options.busy {
            return (handle, items, options);
        }
        assert!(items.is_empty());
        let request_id = expect_model_catalog_request();
        runtime
            .resolve_request(request_id, test_model_catalog())
            .await
            .unwrap();
        let (items, selection) = recv_loaded_model_picker(handle);
        options.busy = false;
        options.initial_selection = Some(selection);
        (handle, items, options)
    }

    async fn open_unprimed_agent(runtime: &mut Runtime) -> RequestId {
        runtime.execute_command("AgentOpen").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::CreateTextPanel { .. }
        ));
        resolve_prompt_history(runtime, serde_json::json!([])).await;
        let catalog = expect_agent_model_header();
        let _ = expect_default_model_request();
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            assert!(matches!(
                request,
                PluginRequest::UpdateTextPanel { .. }
                    | PluginRequest::SetPanelVisible { .. }
                    | PluginRequest::FocusTextPanelComposer { .. }
                    | PluginRequest::SetTextPanelComposerHistory { .. }
            ));
        }
        catalog
    }

    #[tokio::test]
    async fn bundled_agent_model_catalog_prefetch_populates_the_open_spinner_once() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        let catalog = open_unprimed_agent(&mut runtime).await;
        runtime.execute_command("AgentModel").await.unwrap();
        let (handle, items, options) = recv_model_picker();
        assert!(items.is_empty());
        assert!(options.busy);
        assert_eq!(options.status.as_deref(), Some("Loading models…"));
        assert!(
            ACTION_DISPATCHER.try_recv_request().is_none(),
            "reuse the in-flight preload"
        );
        runtime
            .resolve_request(catalog, test_model_catalog())
            .await
            .unwrap();
        let (items, selection) = recv_loaded_model_picker(handle);
        assert_eq!(selection, "first");
        assert_eq!(items.len(), 2);
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        runtime
            .notify_picker(handle, PickerCallback::Cancelled)
            .unwrap();
        let (_, cached, options) = open_test_model_picker(&mut runtime).await;
        assert!(!options.busy);
        assert_eq!(cached, items);
        assert!(
            ACTION_DISPATCHER.try_recv_request().is_none(),
            "cached opens must not fetch again"
        );
    }

    #[tokio::test]
    async fn bundled_agent_model_catalog_failure_retries_and_cancelled_load_stays_closed() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        let catalog = open_unprimed_agent(&mut runtime).await;
        runtime
            .resolve_request(catalog, serde_json::json!({"error":"offline"}))
            .await
            .unwrap();
        assert!(
            ACTION_DISPATCHER.try_recv_request().is_none(),
            "preload errors are quiet"
        );
        runtime.execute_command("AgentModel").await.unwrap();
        let (handle, _, options) = recv_model_picker();
        assert!(options.busy);
        let retry = expect_model_catalog_request();
        runtime
            .resolve_request(retry, serde_json::json!({"error":"still offline"}))
            .await
            .unwrap();
        assert!(
            matches!(ACTION_DISPATCHER.recv_request(), PluginRequest::UpdatePickerBusy { id, busy: false } if id == handle.get())
        );
        assert!(
            matches!(ACTION_DISPATCHER.recv_request(), PluginRequest::UpdatePickerStatus { id, status: Some(status) } if id == handle.get() && status.contains("still offline"))
        );
        runtime
            .notify_picker(handle, PickerCallback::Cancelled)
            .unwrap();
        runtime.execute_command("AgentModel").await.unwrap();
        let (cancelled, _, options) = recv_model_picker();
        assert!(options.busy);
        let retry = expect_model_catalog_request();
        runtime
            .notify_picker(cancelled, PickerCallback::Cancelled)
            .unwrap();
        runtime
            .resolve_request(retry, test_model_catalog())
            .await
            .unwrap();
        assert!(
            ACTION_DISPATCHER.try_recv_request().is_none(),
            "late results must not reopen a cancelled picker"
        );
        let (_, items, options) = open_test_model_picker(&mut runtime).await;
        assert_eq!(items.len(), 2);
        assert!(!options.busy);
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn bundled_agent_model_catalog_ignores_invalidated_and_foreign_session_results() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        let old = open_unprimed_agent(&mut runtime).await;
        runtime
            .notify("agent:backend_ready", serde_json::json!({}))
            .await
            .unwrap();
        let fresh = expect_model_catalog_request();
        runtime
            .resolve_request(old, test_model_catalog())
            .await
            .unwrap();
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        runtime.execute_command("AgentModel").await.unwrap();
        let (handle, items, options) = recv_model_picker();
        assert!(items.is_empty() && options.busy);
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({"session_id":"new-thread"}),
            )
            .await
            .unwrap();
        drain_requests();
        runtime
            .resolve_request(fresh, test_model_catalog())
            .await
            .unwrap();
        assert!(
            matches!(ACTION_DISPATCHER.recv_request(), PluginRequest::ClosePicker { id } if id == handle.get())
        );
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        let (_, items, options) = open_test_model_picker(&mut runtime).await;
        assert_eq!(items.len(), 2);
        assert!(!options.busy);
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn bundled_agent_model_picker_preserves_running_model_and_waits_for_acceptance() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        let _ = open_for_default_model(&mut runtime).await;
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({"session_id":"thread-1"}),
            )
            .await
            .unwrap();
        drain_requests();
        runtime.notify("agent:model_changed", serde_json::json!({"session_id":"thread-1","model_info":{"model":"first","effort":"high"}})).await.unwrap();
        assert_eq!(take_model_header().unwrap().text, "first");
        runtime
            .notify(
                "agent:turn_started",
                serde_json::json!({"session_id":"thread-1"}),
            )
            .await
            .unwrap();
        drain_requests();

        let (picker, items, options) = open_test_model_picker(&mut runtime).await;
        assert_eq!(options.initial_selection.as_deref(), Some("first"));
        assert_eq!(options.item_layout, crate::ui::PickerItemLayout::LabelFirst);
        assert_eq!(items[0].icon, Some(crate::ui::PickerIcon::Text("✓".into())));
        assert_eq!(
            items[1].icon,
            Some(crate::ui::PickerIcon::Text(String::new()))
        );
        assert!(items.iter().all(|item| item.annotation.is_none()));
        runtime
            .notify_picker(picker, PickerCallback::Selected(items[1].clone()))
            .unwrap();
        let (effort_picker, efforts, options) = recv_model_picker();
        assert_eq!(options.initial_selection.as_deref(), Some("low"));
        assert_eq!(options.item_layout, crate::ui::PickerItemLayout::LabelFirst);
        runtime
            .notify_picker(effort_picker, PickerCallback::Cancelled)
            .unwrap();
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        let (picker, items, _) = open_test_model_picker(&mut runtime).await;
        runtime
            .notify_picker(picker, PickerCallback::Selected(items[1].clone()))
            .unwrap();
        let (effort_picker, _, _) = recv_model_picker();
        runtime
            .notify_picker(effort_picker, PickerCallback::Selected(efforts[0].clone()))
            .unwrap();
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::AgentModelRequest {
                request_id,
                request:
                    crate::codex::ModelRequest::Set {
                        session_id,
                        selection,
                    },
            } => {
                assert_eq!(session_id, "thread-1");
                assert_eq!(selection.model, "second");
                assert_eq!(selection.effort.as_deref(), Some("low"));
                request_id
            }
            _ => panic!("expected conversation-scoped model selection"),
        };
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        runtime
            .resolve_request(request_id, serde_json::json!({"accepted":true,"error":""}))
            .await
            .unwrap();
        let pending = take_model_header().unwrap();
        assert_eq!(pending.text, "first");
        assert_eq!(pending.secondary, "Next: second · low");
        runtime
            .notify(
                "agent:model_changed",
                serde_json::json!({"session_id":"foreign","model_info":{"model":"wrong"}}),
            )
            .await
            .unwrap();
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        runtime.notify("agent:model_changed", serde_json::json!({"session_id":"thread-1","model_info":{"model":"second","effort":"low"}})).await.unwrap();
        assert!(
            take_model_header().is_none(),
            "running header already describes the same next model"
        );
        runtime
            .notify(
                "agent:completed",
                serde_json::json!({"session_id":"thread-1","stop_reason":"completed"}),
            )
            .await
            .unwrap();
        let finished = take_model_header().unwrap();
        assert_eq!(
            (finished.text.as_str(), finished.secondary.as_str()),
            ("second", "low")
        );
    }

    #[tokio::test]
    async fn bundled_agent_model_picker_rejects_stale_results_and_keeps_failed_selection() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        let _ = open_for_default_model(&mut runtime).await;
        let (picker, items, _) = open_test_model_picker(&mut runtime).await;
        runtime
            .notify_picker(picker, PickerCallback::Selected(items[0].clone()))
            .unwrap();
        let (effort_picker, efforts, _) = recv_model_picker();
        runtime
            .notify_picker(effort_picker, PickerCallback::Selected(efforts[0].clone()))
            .unwrap();
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::AgentModelRequest {
                request_id,
                request: crate::codex::ModelRequest::Set { session_id, .. },
            } => {
                assert!(session_id.is_empty());
                request_id
            }
            _ => panic!("expected pre-session selection"),
        };
        runtime
            .resolve_request(request_id, serde_json::json!({"error":"not allowed"}))
            .await
            .unwrap();
        assert!(take_model_header().is_none());
        let (picker, items, _) = open_test_model_picker(&mut runtime).await;
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({"session_id":"new-thread"}),
            )
            .await
            .unwrap();
        drain_requests();
        runtime
            .notify_picker(picker, PickerCallback::Selected(items[0].clone()))
            .unwrap();
        assert!(
            matches!(ACTION_DISPATCHER.recv_request(), PluginRequest::Action(Action::Print(message)) if message.contains("conversation changed"))
        );
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn pinned_example_plugin_typechecks_and_activates() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin_at(
                "example",
                "examples/example-plugin/index.hk",
                include_str!("../../examples/example-plugin/index.hk"),
            )
            .await
            .unwrap();
        runtime.execute_command("ExampleCommand").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message == "Hello from the example Husk plugin!"
        ));
    }

    #[test]
    fn plugin_source_validation_keeps_host_api_and_semantic_diagnostics() {
        let host_error = validate_plugin_source(
            "invalid-api",
            "plugins/invalid-api.hk",
            r#"pub fn activate() { red::execute("RemovedAction"); }"#,
        )
        .unwrap_err()
        .to_string();
        assert!(host_error.contains("HUSK-A0001"));
        assert!(host_error.contains("RemovedAction"));

        let semantic_error = validate_plugin_source(
            "invalid-type",
            "plugins/invalid-type.hk",
            r#"pub fn activate() { missing_name(); }"#,
        )
        .unwrap_err()
        .to_string();
        assert!(semantic_error.contains("HUSK-T0001"));
        assert!(semantic_error.contains("invalid-type"));

        let parse_error = validate_plugin_source(
            "invalid-parse",
            "plugins/invalid-parse.hk",
            "fn activate( {",
        )
        .unwrap_err()
        .to_string();
        assert!(parse_error.contains("HUSK-P0001"));
        assert!(parse_error.contains("plugins/invalid-parse.hk:1:"));
    }

    #[tokio::test]
    async fn transactional_reload_uses_explicit_state_migration_hooks() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "stateful",
                r#"
                    pub fn activate() {
                        red::state_set("value", "preserved");
                    }
                    fn state_export() -> Json { return red::state("value"); }
                "#,
            )
            .await
            .unwrap();
        runtime
            .load_plugin(
                "stateful",
                r#"
                    pub fn activate() { red::add_command("Migrated", show); }
                    fn state_import(saved: Json) { red::state_set("value", saved); }
                    fn show() { red::execute("Print", red::string(red::state("value"), "missing")); }
                "#,
            )
            .await
            .unwrap();

        runtime.execute_command("Migrated").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "preserved"
        ));
    }

    #[tokio::test]
    async fn successful_reload_commits_old_teardown_before_replacement_activation_and_import() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "reload-order",
                r#"
                    pub fn activate() { red::state_set("value", "preserved"); }
                    fn state_export() -> Json { return red::state("value"); }
                    fn deactivate() { red::execute("ClosePanel", "shared-panel"); }
                "#,
            )
            .await
            .unwrap();

        runtime
            .load_plugin(
                "reload-order",
                r#"
                    pub fn activate() {
                        red::execute("CreatePanel", "shared-panel", PanelConfig {
                            side: "right",
                            width: 32,
                            title: "Replacement",
                        });
                    }
                    fn state_import(saved: Json) {
                        red::execute("Print", "import:" + red::string(saved, "missing"));
                    }
                "#,
            )
            .await
            .unwrap();

        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::ClosePanel { id } if id == "shared-panel"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::CreatePanel { id, config }
                if id == "shared-panel" && config.title.as_deref() == Some("Replacement")
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "import:preserved"
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn failed_teardown_discards_replacement_effects_and_keeps_the_previous_plugin() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "reload-teardown-error",
                r#"
                    pub fn activate() {
                        red::state_set("value", "stable");
                        red::add_command("Stable", run);
                    }
                    fn run() { red::execute("Print", red::string(red::state("value"), "missing")); }
                    fn deactivate() {
                        red::state_set("value", "teardown-mutated");
                        red::execute("ClosePanel", "shared-panel");
                        red::execute("Print", 1 / 0);
                    }
                "#,
            )
            .await
            .unwrap();

        let error = runtime
            .load_plugin(
                "reload-teardown-error",
                r#"
                    pub fn activate() {
                        red::execute("CreatePanel", "shared-panel", PanelConfig {
                            side: "right",
                            width: 32,
                        });
                    }
                "#,
            )
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("integer division by zero"));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        runtime.execute_command("Stable").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "stable"
        ));
    }

    #[tokio::test]
    async fn failed_export_discards_staged_effects_and_keeps_live_plugin_state() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "reload-export-error",
                r#"
                    pub fn activate() {
                        red::state_set("value", "stable");
                        red::add_command("Stable", run);
                    }
                    fn run() { red::execute("Print", red::string(red::state("value"), "missing")); }
                    fn state_export() -> Json {
                        red::state_set("value", "export-mutated");
                        red::execute("ClosePanel", "shared-panel");
                        red::execute("Print", 1 / 0);
                        return red::state("value");
                    }
                "#,
            )
            .await
            .unwrap();

        let error = runtime
            .load_plugin("reload-export-error", "pub fn activate() {}")
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("integer division by zero"));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        runtime.execute_command("Stable").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "stable"
        ));
    }

    #[tokio::test]
    async fn failed_initial_activation_discards_all_staged_host_effects() {
        drain_requests();
        let mut runtime = Runtime::new();
        let timeout_count = runtime.pending_timeout_count();

        let error = runtime
            .load_plugin(
                "initial-activation-error",
                r#"
                    pub fn activate() {
                        red::add_command("Leaked", run);
                        red::execute("Print", "must not leak");
                        red::request("GetConfig", loaded, "cwd");
                        red::execute("SetTimeout", 0);
                        red::execute("Print", 1 / 0);
                    }
                    fn run() {}
                    fn loaded(event: Json) {}
                "#,
            )
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("integer division by zero"));
        assert_eq!(runtime.command_plugin("Leaked"), None);
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        assert_eq!(runtime.pending_timeout_count(), timeout_count);
    }

    #[tokio::test]
    async fn failed_reload_discards_staged_host_effects_and_keeps_previous_command() {
        drain_requests();
        let mut runtime = Runtime::new();
        let timeout_count = runtime.pending_timeout_count();
        runtime
            .load_plugin(
                "transactional",
                r#"
                    pub fn activate() { red::add_command("Stable", run); }
                    fn run() { red::execute("Print", "stable"); }
                "#,
            )
            .await
            .unwrap();

        let error = runtime
            .load_plugin(
                "transactional",
                r#"
                    pub fn activate() {
                        red::execute("Print", "must not leak");
                        red::request("GetConfig", loaded, "cwd");
                        red::execute("SetTimeout", 0);
                        red::execute("Print", 1 / 0);
                    }
                    fn loaded(event: Json) {}
                "#,
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("integer division by zero"));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        assert_eq!(runtime.pending_timeout_count(), timeout_count);

        runtime.execute_command("Stable").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "stable"
        ));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn failed_reload_cannot_kill_the_live_plugins_process() {
        drain_requests();
        let mut runtime = Runtime::new_with_permissions(HashMap::from([(
            "transactional-process".to_string(),
            PluginPermissions {
                process: vec!["/bin/sleep".to_string()],
            },
        )]));
        runtime
            .load_plugin(
                "transactional-process",
                r#"
                    pub fn activate() { red::add_command("Start", start); }
                    fn start() {
                        let id = red::execute("SpawnProcess", Process {
                            command: "/bin/sleep",
                            args: ["30"],
                        });
                        red::state_set("process_id", id);
                    }
                    fn deactivate() {
                        red::execute("KillProcess", red::state("process_id"));
                    }
                "#,
            )
            .await
            .unwrap();
        runtime.execute_command("Start").await.unwrap();
        assert_eq!(
            runtime
                .inner
                .lock()
                .unwrap()
                .host
                .process_manager
                .active_process_count("transactional-process"),
            1
        );

        let error = runtime
            .load_plugin("transactional-process", "pub fn activate() {}")
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("KillProcess is not allowed"));
        assert_eq!(
            runtime
                .inner
                .lock()
                .unwrap()
                .host
                .process_manager
                .active_process_count("transactional-process"),
            1
        );
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn unloading_a_failing_plugin_teardown_closes_its_session_and_kills_its_process() {
        drain_requests();
        let mut runtime = Runtime::new_with_permissions(HashMap::from([(
            "quarantined-process".to_string(),
            PluginPermissions {
                process: vec!["/bin/sleep".to_string()],
            },
        )]));
        runtime
            .load_plugin(
                "quarantined-process",
                r#"
                    pub fn activate() { red::add_command("Start", start); }
                    fn start() {
                        red::execute("SpawnProcess", Process {
                            command: "/bin/sleep",
                            args: ["30"],
                        });
                    }
                    fn deactivate() {
                        red::execute("AgentCloseSession", "session-1");
                        red::execute("Print", 1 / 0);
                    }
                "#,
            )
            .await
            .unwrap();
        runtime.execute_command("Start").await.unwrap();
        assert_eq!(
            runtime
                .inner
                .lock()
                .unwrap()
                .host
                .process_manager
                .active_process_count("quarantined-process"),
            1
        );

        let error = runtime
            .unload_plugin("quarantined-process")
            .unwrap_err()
            .to_string();

        assert!(error.contains("integer division by zero"));
        assert_eq!(runtime.command_plugin("Start"), None);
        assert_eq!(
            runtime
                .inner
                .lock()
                .unwrap()
                .host
                .process_manager
                .active_process_count("quarantined-process"),
            0
        );
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentCloseSession { session_id } if session_id == "session-1"
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn husk_can_request_correlated_buffer_text() {
        drain_requests();

        let source = r#"
            pub fn activate() {
                red::add_command("Read", read);
            }

            fn loaded(event: Json) {}

            fn read() {
                red::request("GetBufferText", loaded, 2, 7);
            }
        "#;
        let mut runtime = Runtime::new();

        runtime.load_plugin("test", source).await.unwrap();
        runtime.execute_command("Read").await.unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetBufferText {
                request_id,
                start_line,
                end_line,
            } => {
                assert!(request_id.get() > 0);
                assert_eq!(start_line, Some(2));
                assert_eq!(end_line, Some(7));
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn buffer_picker_lists_and_opens_existing_buffers() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "buffer_picker",
                include_str!("../../plugins/buffer_picker.hk"),
            )
            .await
            .unwrap();

        runtime.execute_command("BufferPicker").await.unwrap();

        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::EditorInfo(request_id) => request_id,
            _ => panic!("unexpected plugin request"),
        };

        runtime
            .resolve_request(
                request_id,
                serde_json::json!({
                    "buffers": [
                        {
                            "id": 41,
                            "name": "/workspace/src/main.rs",
                            "path": "/workspace/src/main.rs",
                            "display_path": "src/main.rs",
                            "dirty": false,
                            "active": true,
                            "alternate": false,
                            "line": 4,
                            "column": 8
                        },
                        {
                            "id": 42,
                            "name": "[No Name]",
                            "path": null,
                            "display_path": null,
                            "dirty": true,
                            "active": false,
                            "alternate": true,
                            "line": 0,
                            "column": 0
                        },
                        {
                            "id": 43,
                            "name": "[No Name]",
                            "path": null,
                            "display_path": null,
                            "dirty": false,
                            "active": false,
                            "alternate": false,
                            "line": 0,
                            "column": 0
                        }
                    ],
                }),
            )
            .await
            .unwrap();

        let (handle, items) = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker {
                owner,
                handle,
                title,
                items,
                options,
            } => {
                assert_eq!(owner, "buffer_picker");
                assert_eq!(title.as_deref(), Some("Buffers"));
                assert_eq!(items[0].id, "buffer:41");
                assert_eq!(items[0].label, "main.rs");
                assert_eq!(items[0].kind.as_deref(), Some("FilePath"));
                assert_eq!(items[0].annotation.as_deref(), Some("src"));
                assert_eq!(items[0].detail.as_deref(), Some("current"));
                assert_eq!(
                    items[0].preview,
                    Some(crate::ui::PickerPreview::Location {
                        path: "/workspace/src/main.rs".to_string(),
                        line: Some(4),
                        column: Some(8),
                        matches: Vec::new(),
                    })
                );
                assert_eq!(items[1].id, "buffer:42");
                assert_eq!(items[1].label, "[No Name] #42");
                assert_eq!(items[1].kind.as_deref(), Some("Buffer"));
                assert_eq!(items[1].detail.as_deref(), Some("● modified"));
                assert!(items[1].preview.is_none());
                assert_eq!(items[2].id, "buffer:43");
                assert_eq!(items[2].label, "[No Name] #43");
                assert!(items[2].preview.is_none());
                assert_ne!(items[1].id, items[2].id);
                assert_eq!(options.initial_selection.as_deref(), Some("buffer:42"));
                assert_eq!(options.status.as_deref(), Some("3 buffers · 1 modified"));
                assert_eq!(options.item_layout, crate::ui::PickerItemLayout::LabelFirst);
                assert_eq!(
                    options
                        .actions
                        .iter()
                        .map(|action| action.action.as_str())
                        .collect::<Vec<_>>(),
                    ["open_horizontal", "open_vertical", "toggle_preview"]
                );
                (handle, items)
            }
            _ => panic!("unexpected plugin request"),
        };

        runtime
            .notify_picker(
                handle,
                PickerCallback::Action {
                    action: "toggle_preview".to_string(),
                    item: Some(items[0].clone()),
                    query: "main".to_string(),
                },
            )
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePickerItems { id, items } => {
                assert_eq!(id, handle.get());
                assert!(items.iter().all(|item| item.preview.is_none()));
            }
            _ => panic!("preview toggle should update the existing picker"),
        }

        runtime
            .notify_picker(
                handle,
                PickerCallback::Action {
                    action: "toggle_preview".to_string(),
                    item: Some(items[0].clone()),
                    query: "main".to_string(),
                },
            )
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePickerItems { id, items } => {
                assert_eq!(id, handle.get());
                assert!(items[0].preview.is_some());
                assert!(items[1].preview.is_none());
                assert!(items[2].preview.is_none());
            }
            _ => panic!("enabling previews should restore file-backed buffer previews"),
        }

        runtime
            .notify_picker(
                handle,
                PickerCallback::Action {
                    action: "open_vertical".to_string(),
                    item: Some(items[1].clone()),
                    query: String::new(),
                },
            )
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::Action(Action::Print(message)) => {
                assert!(message.contains("unnamed buffer"));
            }
            _ => panic!("unnamed buffers must remain in the existing picker"),
        }

        runtime
            .notify_picker(handle, PickerCallback::Selected(items[2].clone()))
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::Action(Action::OpenBufferById(id)) => assert_eq!(id, 43),
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn buffer_picker_opens_file_splits_at_the_saved_utf8_cursor() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "buffer_picker",
                include_str!("../../plugins/buffer_picker.hk"),
            )
            .await
            .unwrap();
        runtime.execute_command("BufferPicker").await.unwrap();
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::EditorInfo(request_id) => request_id,
            _ => panic!("expected the open-buffer snapshot"),
        };
        runtime
            .resolve_request(
                request_id,
                serde_json::json!({
                    "buffers": [{
                        "id": 12,
                        "name": "/workspace/src/other.rs",
                        "path": "/workspace/src/other.rs",
                        "display_path": "src/other.rs",
                        "dirty": true,
                        "active": true,
                        "alternate": false,
                        "line": 7,
                        "column": 11
                    }]
                }),
            )
            .await
            .unwrap();
        let (handle, item) = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker { handle, items, .. } => (handle, items[0].clone()),
            _ => panic!("expected the modern buffer picker"),
        };

        runtime
            .notify_picker(
                handle,
                PickerCallback::Action {
                    action: "open_horizontal".to_string(),
                    item: Some(item),
                    query: String::new(),
                },
            )
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ClosePicker { id } => assert_eq!(id, handle.get()),
            PluginRequest::Action(Action::Print(message)) => {
                panic!("split selection should close its picker, printed {message:?}")
            }
            _ => panic!("split selection should close its picker"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenLocation { location, target } => {
                assert_eq!(location.path, "/workspace/src/other.rs");
                assert_eq!(location.line, 7);
                assert_eq!(location.column, 11);
                assert_eq!(
                    location.column_encoding,
                    crate::plugin::LocationColumnEncoding::Utf8Byte
                );
                assert_eq!(target, crate::plugin::OpenLocationTarget::Horizontal);
            }
            PluginRequest::Action(Action::Print(message)) => {
                panic!("split selection should preserve the saved cursor, printed {message:?}")
            }
            _ => panic!("split selection should preserve the saved cursor"),
        }
    }

    #[tokio::test]
    async fn cool_search_clears_search_highlight_on_non_search_movement() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("cool_search", include_str!("../../plugins/cool_search.hk"))
            .await
            .unwrap();

        runtime
            .notify("search:highlighted", serde_json::json!({}))
            .await
            .unwrap();
        runtime
            .notify(
                "cursor:moved",
                serde_json::json!({
                    "mode": "Normal",
                    "cause": "FindNext",
                }),
            )
            .await
            .unwrap();

        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        runtime
            .notify(
                "cursor:moved",
                serde_json::json!({
                    "mode": "Normal",
                    "cause": "MoveRight",
                }),
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::Action(Action::ClearSearchHighlight) => {}
            _ => panic!("unexpected plugin request"),
        }

        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn cool_search_clears_search_highlight_on_insert_mode() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("cool_search", include_str!("../../plugins/cool_search.hk"))
            .await
            .unwrap();

        runtime
            .notify("search:highlighted", serde_json::json!({}))
            .await
            .unwrap();
        runtime
            .notify(
                "mode:changed",
                serde_json::json!({
                    "from": "Normal",
                    "to": "Insert",
                }),
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::Action(Action::ClearSearchHighlight) => {}
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .notify(
                "cursor:moved",
                serde_json::json!({
                    "mode": "Normal",
                    "cause": "MoveRight",
                }),
            )
            .await
            .unwrap();

        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn indent_guides_reads_the_latest_viewport_snapshot() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime.set_snapshot(
            "editor_info",
            sample_indent_editor_info(
                Color::Rgb {
                    r: 40,
                    g: 41,
                    b: 42,
                },
                Color::Rgb {
                    r: 80,
                    g: 81,
                    b: 82,
                },
            ),
        );
        runtime.set_snapshot("viewport_layout", sample_indent_layout());
        runtime
            .load_plugin(
                "indent_guides",
                include_str!("../../plugins/indent_guides.hk"),
            )
            .await
            .unwrap();

        assert!(matches!(
            ACTION_DISPATCHER.try_recv_request(),
            Some(PluginRequest::SetDecorations { .. })
        ));

        let mut next_layout = sample_indent_layout();
        next_layout["cursor"]["y"] = serde_json::json!(3);
        runtime.set_snapshot("viewport_layout", next_layout);
        runtime
            .notify("buffer:changed", serde_json::json!({}))
            .await
            .unwrap();

        assert!(matches!(
            ACTION_DISPATCHER.try_recv_request(),
            Some(PluginRequest::SetDecorations { .. })
        ));
    }

    #[tokio::test]
    async fn indent_guides_skip_same_line_edits_without_indentation_changes() {
        drain_requests();

        let mut layout = sample_indent_layout();
        layout["indentation_key"] = serde_json::json!("0:0:0;1:4:0;2:8:0;3:4:0;4:0:0;");
        let mut runtime = Runtime::new();
        runtime.set_snapshot(
            "editor_info",
            sample_indent_editor_info(
                Color::Rgb {
                    r: 40,
                    g: 41,
                    b: 42,
                },
                Color::Rgb {
                    r: 80,
                    g: 81,
                    b: 82,
                },
            ),
        );
        runtime.set_snapshot("viewport_layout", layout.clone());
        runtime
            .load_plugin(
                "indent_guides",
                include_str!("../../plugins/indent_guides.hk"),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.try_recv_request(),
            Some(PluginRequest::SetDecorations { .. })
        ));

        layout["revision"] = serde_json::json!(2);
        layout["cursor"]["x"] = serde_json::json!(9);
        layout["rows"][2]["text"] = serde_json::json!("        updated();");
        runtime.set_snapshot("viewport_layout", layout.clone());
        runtime
            .notify("cursor:moved", serde_json::json!({}))
            .await
            .unwrap();
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        layout["revision"] = serde_json::json!(3);
        layout["rows"][2]["text"] = serde_json::json!("            updated();");
        layout["indentation_key"] = serde_json::json!("0:0:0;1:4:0;2:12:0;3:4:0;4:0:0;");
        runtime.set_snapshot("viewport_layout", layout.clone());
        runtime
            .notify("buffer:changed", serde_json::json!({}))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.try_recv_request(),
            Some(PluginRequest::SetDecorations { .. })
        ));

        layout["cursor"]["y"] = serde_json::json!(3);
        runtime.set_snapshot("viewport_layout", layout);
        runtime
            .notify("cursor:moved", serde_json::json!({}))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.try_recv_request(),
            Some(PluginRequest::SetDecorations { .. })
        ));
    }

    #[tokio::test]
    async fn indent_guides_renders_decorations_from_viewport_layout_response() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime.set_snapshot(
            "editor_info",
            sample_indent_editor_info(
                Color::Rgb {
                    r: 40,
                    g: 41,
                    b: 42,
                },
                Color::Rgb {
                    r: 80,
                    g: 81,
                    b: 82,
                },
            ),
        );
        runtime.set_snapshot("viewport_layout", sample_indent_layout());
        runtime
            .load_plugin(
                "indent_guides",
                include_str!("../../plugins/indent_guides.hk"),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::SetDecorations {
                namespace,
                decorations,
            } => {
                assert_eq!(namespace, "indent-guides");
                assert_eq!(decorations[0].buffer_index, Some(3));
                assert_eq!(decorations[0].line, 1);
                assert_eq!(decorations[0].text, "\u{2502}   ");
                assert!(decorations
                    .iter()
                    .any(|decoration| decoration.line == 2 && decoration.priority == 1024));
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn indent_guides_handles_non_tabstop_indentation() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime.set_snapshot(
            "editor_info",
            sample_indent_editor_info(
                Color::Rgb {
                    r: 40,
                    g: 41,
                    b: 42,
                },
                Color::Rgb {
                    r: 80,
                    g: 81,
                    b: 82,
                },
            ),
        );
        runtime.set_snapshot("viewport_layout", non_tabstop_indent_layout());
        runtime
            .load_plugin(
                "indent_guides",
                include_str!("../../plugins/indent_guides.hk"),
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::SetDecorations { decorations, .. } => {
                let active = decorations
                    .iter()
                    .find(|decoration| decoration.priority == 1024)
                    .unwrap();
                assert_eq!(active.line, 1);
                assert_eq!(active.column, 32);
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn indent_guides_reuses_precomputed_widths_and_infers_blank_runs() {
        drain_requests();

        let mut layout = sample_indent_layout();
        layout["cursor"]["y"] = serde_json::json!(3);
        layout["rows"] = serde_json::json!([
            { "line": 0, "text": "root", "first_segment": true, "indent_width": 0 },
            { "line": 1, "text": "not visibly indented", "first_segment": true, "indent_width": 8 },
            { "line": 2, "text": "", "first_segment": true, "indent_width": 0 },
            { "line": 3, "text": "   ", "first_segment": true, "indent_width": 3 },
            { "line": 4, "text": "", "first_segment": true, "indent_width": 0 },
            { "line": 5, "text": "tail", "first_segment": true, "indent_width": 4 }
        ]);
        let mut runtime = Runtime::new();
        runtime.set_snapshot(
            "editor_info",
            sample_indent_editor_info(
                Color::Rgb {
                    r: 40,
                    g: 41,
                    b: 42,
                },
                Color::Rgb {
                    r: 80,
                    g: 81,
                    b: 82,
                },
            ),
        );
        runtime.set_snapshot("viewport_layout", layout);

        runtime
            .load_plugin(
                "indent_guides",
                include_str!("../../plugins/indent_guides.hk"),
            )
            .await
            .unwrap();

        let PluginRequest::SetDecorations { decorations, .. } = ACTION_DISPATCHER.recv_request()
        else {
            panic!("unexpected plugin request");
        };
        assert_eq!(
            decorations
                .iter()
                .find(|decoration| decoration.line == 1 && decoration.priority == 1)
                .unwrap()
                .text,
            "\u{2502}   \u{2502}   "
        );
        for line in 2..=4 {
            assert_eq!(
                decorations
                    .iter()
                    .find(|decoration| decoration.line == line && decoration.priority == 1)
                    .unwrap()
                    .text,
                "\u{2502}   "
            );
        }
        assert!(decorations
            .iter()
            .any(|decoration| decoration.line == 3 && decoration.priority == 1024));
    }

    #[tokio::test]
    async fn indent_guides_rebuild_theme_styles_without_layout_changes() {
        drain_requests();

        let original = Color::Rgb {
            r: 40,
            g: 41,
            b: 42,
        };
        let original_active = Color::Rgb {
            r: 80,
            g: 81,
            b: 82,
        };
        let updated = Color::Rgb {
            r: 90,
            g: 91,
            b: 92,
        };
        let updated_active = Color::Rgb {
            r: 120,
            g: 121,
            b: 122,
        };
        let mut runtime = Runtime::new();
        runtime.set_snapshot(
            "editor_info",
            sample_indent_editor_info(original, original_active),
        );
        runtime.set_snapshot("viewport_layout", sample_indent_layout());
        runtime
            .load_plugin(
                "indent_guides",
                include_str!("../../plugins/indent_guides.hk"),
            )
            .await
            .unwrap();

        let _ = ACTION_DISPATCHER.recv_request();
        runtime.set_snapshot(
            "editor_info",
            sample_indent_editor_info(updated, updated_active),
        );
        runtime
            .notify("theme:changed", serde_json::json!({ "name": "updated" }))
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::SetDecorations { decorations, .. } => {
                assert_eq!(decorations[0].style.fg, Some(updated));
                assert_eq!(
                    decorations
                        .iter()
                        .find(|decoration| decoration.priority == 1024)
                        .unwrap()
                        .style
                        .fg,
                    Some(updated_active)
                );
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn indent_guides_clears_decorations_on_deactivate() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime.set_snapshot(
            "editor_info",
            sample_indent_editor_info(
                Color::Rgb {
                    r: 40,
                    g: 41,
                    b: 42,
                },
                Color::Rgb {
                    r: 80,
                    g: 81,
                    b: 82,
                },
            ),
        );
        runtime.set_snapshot("viewport_layout", sample_indent_layout());
        runtime
            .load_plugin(
                "indent_guides",
                include_str!("../../plugins/indent_guides.hk"),
            )
            .await
            .unwrap();
        let _ = ACTION_DISPATCHER.recv_request();

        runtime.deactivate_all().await.unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ClearDecorations { namespace } => {
                assert_eq!(namespace, "indent-guides");
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn inlay_hints_requests_visible_range_and_sets_eol_decorations() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime.set_snapshot(
            "editor_info",
            serde_json::json!({
                "theme": {
                    "colors": {
                        "editorInlayHint.typeForeground": "#c8c8c8",
                        "editor.background": "#0a141e",
                    },
                    "gutter_style": { "fg": null },
                }
            }),
        );
        runtime.set_snapshot("viewport_layout", sample_indent_layout());
        runtime
            .load_plugin("inlay_hints", include_str!("../../plugins/inlay_hints.hk"))
            .await
            .unwrap();

        let _config_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key, None);
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        let hints_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::InlayHints { request_id, range } => {
                let range = range.unwrap();
                assert_eq!(range.start.line, 0);
                assert_eq!(range.end.line, 5);
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        runtime
            .resolve_request(
                hints_request_id,
                serde_json::json!({
                    "ok": true,
                    "hints": [{
                        "kind": 1,
                        "position": { "line": 1, "character": 8 },
                        "label": [{ "value": ": String" }],
                    }],
                }),
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::SetDecorations {
                namespace,
                decorations,
            } => {
                assert_eq!(namespace, "inlay-hints");
                assert_eq!(decorations.len(), 1);
                assert_eq!(decorations[0].line, 1);
                assert_eq!(decorations[0].anchor, crate::plugin::DecorationAnchor::Eol);
                assert_eq!(decorations[0].text, " => String");
                assert_eq!(decorations[0].priority, 1001);
                assert_eq!(
                    decorations[0].style.fg,
                    Some(crate::color::Color::Rgb {
                        r: 90,
                        g: 96,
                        b: 101,
                    })
                );
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn inlay_hints_retry_after_a_recoverable_lsp_error() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime.set_snapshot(
            "editor_info",
            serde_json::json!({
                "theme": {
                    "colors": {
                        "editorInlayHint.typeForeground": "#c8c8c8",
                        "editor.background": "#0a141e",
                    },
                    "gutter_style": { "fg": null },
                }
            }),
        );
        runtime.set_snapshot("viewport_layout", sample_indent_layout());
        runtime
            .load_plugin("inlay_hints", include_str!("../../plugins/inlay_hints.hk"))
            .await
            .unwrap();

        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::GetConfig { .. }
        ));
        let hints_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::InlayHints { request_id, .. } => request_id,
            _ => panic!("expected initial inlay-hint request"),
        };
        runtime
            .resolve_request(
                hints_request_id,
                serde_json::json!({
                    "ok": false,
                    "hints": [],
                    "error": "content modified",
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::ClearDecorations { .. }
        ));

        let timer_id = {
            let inner = runtime.inner.lock().unwrap();
            inner
                .host
                .policy()
                .typed_states
                .get("inlay_hints")
                .and_then(|state| match state {
                    Value::Struct { fields, .. } => fields.get("timer"),
                    _ => None,
                })
                .and_then(Value::as_str)
                .expect("expected retry timer")
                .to_string()
        };
        runtime.cancel_test_timeout(&timer_id);
        runtime
            .notify(
                "timeout:callback",
                serde_json::json!({ "timer_id": timer_id }),
            )
            .await
            .unwrap();

        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::InlayHints { .. }
        ));
    }

    #[tokio::test]
    async fn inlay_hints_bound_pathological_same_line_results() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime.set_snapshot(
            "editor_info",
            serde_json::json!({
                "theme": {
                    "colors": {
                        "editorInlayHint.typeForeground": "#c8c8c8",
                        "editor.background": "#0a141e",
                    },
                    "gutter_style": { "fg": null },
                }
            }),
        );
        runtime.set_snapshot("viewport_layout", sample_indent_layout());
        runtime
            .load_plugin("inlay_hints", include_str!("../../plugins/inlay_hints.hk"))
            .await
            .unwrap();

        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::GetConfig { .. }
        ));
        let hints_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::InlayHints { request_id, .. } => request_id,
            _ => panic!("expected inlay-hint request"),
        };
        let hints = (0..1_000)
            .map(|index| {
                serde_json::json!({
                    "kind": 1,
                    "position": { "line": 1, "character": index },
                    "label": ": Type"
                })
            })
            .collect::<Vec<_>>();

        runtime
            .resolve_request(
                hints_request_id,
                serde_json::json!({ "ok": true, "hints": hints }),
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::SetDecorations { decorations, .. } => {
                assert_eq!(decorations.len(), 1);
                assert_eq!(decorations[0].line, 1);
                assert_eq!(decorations[0].text.matches("Type").count(), 24);
            }
            _ => panic!("expected bounded inlay-hint decorations"),
        }
    }

    #[tokio::test]
    async fn inlay_hints_ignore_stale_layout_and_render_configured_parameter_hints() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime.set_snapshot(
            "editor_info",
            serde_json::json!({
                "theme": {
                    "colors": {
                        "editorInlayHint.typeForeground": "#c8c8c8",
                        "editor.background": "#0a141e"
                    },
                    "gutter_style": { "fg": null }
                }
            }),
        );
        runtime.set_snapshot("viewport_layout", sample_indent_layout());
        runtime
            .load_plugin("inlay_hints", include_str!("../../plugins/inlay_hints.hk"))
            .await
            .unwrap();
        let config_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, .. } => request_id,
            _ => panic!("unexpected plugin request"),
        };
        let _initial_hints_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::InlayHints { request_id, .. } => request_id,
            _ => panic!("unexpected plugin request"),
        };

        runtime
            .resolve_request(
                config_request_id,
                serde_json::json!({
                    "value": {
                        "plugin_config": {
                            "inlay_hints": { "parameter_hints": true }
                        }
                    }
                }),
            )
            .await
            .unwrap();
        let hints_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::InlayHints { request_id, .. } => request_id,
            _ => panic!("unexpected plugin request"),
        };

        runtime
            .resolve_request(
                hints_request_id,
                serde_json::json!({
                    "ok": true,
                    "hints": [
                        {
                            "kind": 1,
                            "position": { "line": 1, "character": 8 },
                            "label": ": String"
                        },
                        {
                            "kind": 2,
                            "position": { "line": 1, "character": 1 },
                            "label": "arg:"
                        },
                        {
                            "kind": 1,
                            "position": { "line": 1, "character": 3 },
                            "label": ": Number"
                        }
                    ]
                }),
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::SetDecorations { decorations, .. } => {
                assert_eq!(decorations.len(), 1);
                assert_eq!(decorations[0].text, " <- (arg) => Number,String");
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn fidget_renders_lsp_progress_in_overlay() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("fidget", include_str!("../../plugins/fidget.hk"))
            .await
            .unwrap();
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::EditorInfo(request_id) => request_id,
            _ => panic!("unexpected plugin request"),
        };
        runtime
            .resolve_request(
                request_id,
                serde_json::json!({
                    "size": [80, 24],
                    "theme": {
                        "ui_style": {
                            "muted": {
                                "fg": { "Rgb": { "r": 153, "g": 153, "b": 153 } },
                                "bg": { "Rgb": { "r": 17, "g": 17, "b": 17 } },
                                "bold": false,
                                "italic": false
                            },
                            "popup_title": {
                                "fg": { "Rgb": { "r": 238, "g": 238, "b": 238 } },
                                "bg": { "Rgb": { "r": 34, "g": 34, "b": 34 } },
                                "bold": true,
                                "italic": false
                            }
                        }
                    }
                }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::CreateOverlay { id, config } => {
                assert_eq!(id, "fidget-progress");
                assert_eq!(config.max_width, 60);
                assert!(matches!(
                    config.overflow,
                    crate::plugin::OverlayOverflow::TruncateLeft
                ));
                assert_eq!(config.truncate_marker, "…");
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdateOverlay { id, lines } => {
                assert_eq!(id, "fidget-progress");
                assert!(lines.is_empty());
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .notify(
                "lsp:progress",
                serde_json::json!({
                    "token": "index",
                    "value": {
                        "kind": "begin",
                        "title": "Indexing",
                        "message": "Loading",
                        "percentage": 25,
                    },
                    "lsp_client": { "name": "rust_analyzer" },
                }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdateOverlay { id, lines } => {
                assert_eq!(id, "fidget-progress");
                assert_eq!(lines.len(), 2);
                assert_eq!(lines[0].0, "Loading (25%) Indexing");
                assert_eq!(lines[1].0, "rust-analyzer ⠋");
                assert!(lines.iter().all(|(_, style)| style.bg.is_none()));
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn fidget_handles_numeric_tokens_typed_overrides_and_future_progress_variants() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("fidget", include_str!("../../plugins/fidget.hk"))
            .await
            .unwrap();
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::EditorInfo(request_id) => request_id,
            _ => panic!("expected Fidget editor information request"),
        };
        runtime
            .resolve_request(
                request_id,
                serde_json::json!({ "size": [80, 24], "theme": { "ui_style": {} } }),
            )
            .await
            .unwrap();
        drain_requests();

        runtime
            .notify(
                "lsp:progress",
                serde_json::json!({
                    "token": 42,
                    "kind": "begin",
                    "message": "Override",
                    "percentage": 65,
                    "title": "Explicit title",
                    "value": {
                        "kind": "begin",
                        "title": "Nested title",
                        "message": "Nested message",
                        "percentage": 25,
                    },
                    "lsp_client": { "name": "rust_analyzer" },
                }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdateOverlay { id, lines } => {
                assert_eq!(id, "fidget-progress");
                assert_eq!(lines.len(), 2);
                assert_eq!(lines[0].0, "Override (65%) Explicit title");
                assert_eq!(lines[1].0, "rust-analyzer ⠋");
            }
            _ => panic!("expected typed Fidget overlay update"),
        }

        runtime
            .notify(
                "lsp:progress",
                serde_json::json!({
                    "token": 42,
                    "value": { "kind": "future_progress", "message": "Ignore me" },
                    "lsp_client": { "name": "rust_analyzer" },
                }),
            )
            .await
            .unwrap();
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn fidget_cancels_animation_and_completion_timers() {
        drain_requests();
        let mut runtime = Runtime::new();
        let timeout_count = runtime.pending_timeout_count();
        runtime
            .load_plugin("fidget", include_str!("../../plugins/fidget.hk"))
            .await
            .unwrap();
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::EditorInfo(request_id) => request_id,
            _ => panic!("unexpected plugin request"),
        };
        runtime
            .resolve_request(
                request_id,
                serde_json::json!({ "size": [80, 24], "theme": { "ui_style": {} } }),
            )
            .await
            .unwrap();
        drain_requests();

        runtime
            .notify(
                "lsp:progress",
                serde_json::json!({
                    "token": "index",
                    "value": { "kind": "begin", "title": "Indexing" }
                }),
            )
            .await
            .unwrap();
        assert_eq!(runtime.pending_timeout_count(), timeout_count + 1);

        runtime
            .notify(
                "lsp:progress",
                serde_json::json!({
                    "token": "index",
                    "value": { "kind": "end", "message": "Done" }
                }),
            )
            .await
            .unwrap();
        assert_eq!(runtime.pending_timeout_count(), timeout_count + 1);

        runtime.deactivate_all().await.unwrap();

        assert_eq!(runtime.pending_timeout_count(), timeout_count);
    }

    #[tokio::test]
    async fn bundled_plugin_deactivation_cancels_pending_refresh_timers() {
        drain_requests();

        for (name, source, event, payload) in [
            (
                "inlay_hints",
                include_str!("../../plugins/inlay_hints.hk"),
                "buffer:changed",
                serde_json::json!({}),
            ),
            (
                "barbecue",
                include_str!("../../plugins/barbecue.hk"),
                "buffer:changed",
                serde_json::json!({}),
            ),
        ] {
            let mut runtime = Runtime::new();
            let timeout_count = runtime.pending_timeout_count();
            runtime.set_snapshot("viewport_layout", sample_indent_layout());
            runtime.set_snapshot("windows", serde_json::json!({ "windows": [] }));
            runtime.set_snapshot(
                "editor_info",
                serde_json::json!({
                    "size": [80, 24],
                    "theme": { "ui_style": {}, "colors": {}, "gutter_style": {} }
                }),
            );
            runtime.load_plugin(name, source).await.unwrap();
            drain_requests();

            runtime.notify(event, payload).await.unwrap();
            assert_eq!(runtime.pending_timeout_count(), timeout_count + 1);

            runtime.deactivate_all().await.unwrap();
            assert_eq!(runtime.pending_timeout_count(), timeout_count);
            drain_requests();
        }
    }

    #[tokio::test]
    async fn project_search_cancels_pending_debounce_when_picker_closes() {
        drain_requests();
        let mut runtime = Runtime::new();
        let timeout_count = runtime.pending_timeout_count();
        runtime
            .load_plugin(
                "project_search",
                include_str!("../../plugins/project_search.hk"),
            )
            .await
            .unwrap();

        let handle = open_project_search_picker(&mut runtime).await;

        runtime
            .notify_picker(handle, PickerCallback::Query("needle".to_string()))
            .unwrap();
        assert_eq!(runtime.pending_timeout_count(), timeout_count + 1);

        runtime
            .notify_picker(handle, PickerCallback::Cancelled)
            .unwrap();

        assert_eq!(runtime.pending_timeout_count(), timeout_count);
        assert!(runtime.picker_plugin(handle).is_none());
        assert!(!runtime
            .notify_picker(handle, PickerCallback::Query("stale".to_string()))
            .unwrap());
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn project_search_recreates_a_restored_export_without_stealing_focus() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "project_search",
                include_str!("../../plugins/project_search.hk"),
            )
            .await
            .unwrap();

        runtime
            .notify(
                "editor:panes_restore",
                serde_json::json!({
                    "panels": [{ "id": "project-search-results", "visible": true }]
                }),
            )
            .await
            .unwrap();
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage {
                plugin,
                key,
                request_id,
            } => {
                assert_eq!(plugin, "project_search");
                assert_eq!(key, "exported_panel");
                request_id
            }
            _ => panic!("expected exported-panel storage request"),
        };
        runtime
            .resolve_request(
                request_id,
                serde_json::json!({
                    "value": {
                        "items": [],
                        "query": "needle",
                        "hidden": false,
                        "ignored": false,
                        "follow": false,
                        "regex": true,
                        "preview": true,
                        "truncated": false
                    }
                }),
            )
            .await
            .unwrap();

        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::CreatePanel { id, .. } if id == "project-search-results"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePanel { id, .. } if id == "project-search-results"
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn project_search_deactivation_cancels_debounce_and_releases_picker() {
        drain_requests();
        let mut runtime = Runtime::new();
        let timeout_count = runtime.pending_timeout_count();
        runtime
            .load_plugin(
                "project_search",
                include_str!("../../plugins/project_search.hk"),
            )
            .await
            .unwrap();

        let handle = open_project_search_picker(&mut runtime).await;
        runtime
            .notify_picker(handle, PickerCallback::Query("needle".to_string()))
            .unwrap();
        assert_eq!(runtime.pending_timeout_count(), timeout_count + 1);

        runtime.deactivate_all().await.unwrap();

        assert_eq!(runtime.pending_timeout_count(), timeout_count);
        assert!(runtime.picker_plugin(handle).is_none());
        drain_requests();
    }

    #[tokio::test]
    async fn barbecue_handles_large_symbol_lists_and_opens_symbol_action() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime.set_snapshot(
            "windows",
            serde_json::json!({
                "windows": [
                    {
                        "window_id": 7,
                        "buffer_index": 2,
                        "document_id": 42,
                        "buffer_path": "/repo/plugins/example.rs",
                        "breadcrumb_components": ["plugins", "example.rs"],
                        "revision": 4,
                        "cursor": { "x": 1, "y": 906 },
                        "lsp_position": { "line": 906, "character": 1 },
                    },
                    {
                        "window_id": 8,
                        "buffer_index": 2,
                        "document_id": 42,
                        "buffer_path": "/repo/plugins/example.rs",
                        "breadcrumb_components": ["plugins", "example.rs"],
                        "revision": 4,
                        "cursor": { "x": 1, "y": 905 },
                        "lsp_position": { "line": 905, "character": 1 },
                    }
                ]
            }),
        );
        runtime.set_snapshot(
            "editor_info",
            serde_json::json!({
                "theme": {
                    "style": {
                        "fg": null,
                        "bg": "#111111",
                        "bold": false,
                        "italic": false
                    }
                }
            }),
        );
        runtime
            .load_plugin("barbecue", include_str!("../../plugins/barbecue.hk"))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::CreateWindowBar { .. }
        ));
        let config_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, .. } => request_id,
            _ => panic!("unexpected plugin request"),
        };
        let windows_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetWindows { request_id } => request_id,
            _ => panic!("expected Barbecue windows request"),
        };

        runtime
            .resolve_request(
                config_request_id,
                serde_json::json!({
                    "value": {
                        "cwd": "/repo",
                        "plugin_config": {
                            "barbecue": { "separator": "›" }
                        }
                    }
                }),
            )
            .await
            .unwrap();
        runtime
            .resolve_request(
                windows_request_id,
                serde_json::json!({
                    "windows": [
                        {
                            "window_id": 7,
                            "buffer_index": 2,
                            "document_id": 42,
                            "buffer_path": "/repo/plugins/example.rs",
                            "breadcrumb_components": ["plugins", "example.rs"],
                            "revision": 4,
                            "cursor": { "x": 1, "y": 906 },
                            "lsp_position": { "line": 906, "character": 1 },
                        },
                        {
                            "window_id": 8,
                            "buffer_index": 2,
                            "document_id": 42,
                            "buffer_path": "/repo/plugins/example.rs",
                            "breadcrumb_components": ["plugins", "example.rs"],
                            "revision": 4,
                            "cursor": { "x": 1, "y": 905 },
                            "lsp_position": { "line": 905, "character": 1 },
                        }
                    ]
                }),
            )
            .await
            .unwrap();
        let mut symbol_request_id = None;
        let mut symbol_request_count = 0;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::DocumentSymbols {
                request_id,
                buffer_index,
            } = request
            {
                assert_eq!(buffer_index, Some(2));
                symbol_request_id = Some(request_id);
                symbol_request_count += 1;
            }
        }
        let symbol_request_id = symbol_request_id.expect("expected symbol request");
        assert_eq!(symbol_request_count, 1);

        let symbols = (0..1_000)
            .map(|index| {
                let (id, name, parent_id, depth, start_line, end_line) = if index == 905 {
                    (
                        "root:905:outer".to_string(),
                        "outer".to_string(),
                        serde_json::Value::Null,
                        0,
                        905,
                        908,
                    )
                } else if index == 906 {
                    (
                        "root:905:outer:0:inner".to_string(),
                        "inner".to_string(),
                        serde_json::json!("root:905:outer"),
                        1,
                        906,
                        907,
                    )
                } else {
                    (
                        format!("symbol-{index}"),
                        format!("symbol_{index}"),
                        serde_json::Value::Null,
                        0,
                        index,
                        index + 1,
                    )
                };
                serde_json::json!({
                    "id": id,
                    "parent_id": parent_id,
                    "name": name,
                    "kind_name": "Function",
                    "file": "plugins/example.rs",
                    "depth": depth,
                    "range": {
                        "start": { "line": start_line, "character": 0 },
                        "end": { "line": end_line, "character": 0 }
                    },
                    "selection_range": {
                        "start": { "line": start_line, "character": 0 },
                        "end": { "line": start_line, "character": 5 }
                    }
                })
            })
            .collect::<Vec<_>>();
        runtime
            .resolve_request(
                symbol_request_id,
                serde_json::json!({
                    "ok": true,
                    "file": "plugins/example.rs",
                    "buffer_index": 2,
                    "document_id": 42,
                    "revision": 4,
                    "symbols": symbols,
                }),
            )
            .await
            .unwrap();

        let mut saw_outer = false;
        let mut saw_inner = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::UpdateWindowBar {
                window_id,
                segments,
                ..
            } = request
            {
                let outer = segments.iter().any(|segment| segment.text == "󰊕 outer");
                let inner = segments.iter().any(|segment| segment.text == "󰊕 inner");
                if window_id == 7 {
                    assert!(outer && inner);
                    saw_inner = true;
                } else if window_id == 8 {
                    assert!(outer && !inner);
                    saw_outer = true;
                }
            }
        }
        assert!(saw_outer && saw_inner);

        runtime
            .notify(
                "window_bar:action:barbecue",
                serde_json::json!({ "action": "jump:42:root:905:outer:0:inner" }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenLocation { location, .. } => {
                assert_eq!(location.path, "plugins/example.rs");
                assert_eq!(location.line, 906);
                assert_eq!(location.column, 0);
                assert_eq!(
                    location.column_encoding,
                    crate::plugin::LocationColumnEncoding::Utf16
                );
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn barbecue_debounced_refresh_uses_live_window_cursor() {
        drain_requests();

        let stale_windows = serde_json::json!({
            "windows": [{
                "window_id": 7,
                "buffer_index": 2,
                "document_id": 42,
                "buffer_path": "/repo/src/main.rs",
                "breadcrumb_components": ["src", "main.rs"],
                "revision": 4,
                "cursor": { "x": 1, "y": 48 },
                "lsp_position": { "line": 48, "character": 1 },
            }]
        });
        let symbols = serde_json::json!([
            {
                "id": "detached-paste-bytes",
                "parent_id": null,
                "name": "DETACHED_PASTE_CHUNK_BYTES",
                "kind_name": "Constant",
                "file": "/repo/src/main.rs",
                "depth": 0,
                "range": {
                    "start": { "line": 47, "character": 0 },
                    "end": { "line": 49, "character": 0 }
                },
                "selection_range": {
                    "start": { "line": 47, "character": 6 },
                    "end": { "line": 47, "character": 32 }
                }
            },
            {
                "id": "run",
                "parent_id": null,
                "name": "run",
                "kind_name": "Function",
                "file": "/repo/src/main.rs",
                "depth": 0,
                "range": {
                    "start": { "line": 81, "character": 0 },
                    "end": { "line": 126, "character": 0 }
                },
                "selection_range": {
                    "start": { "line": 81, "character": 9 },
                    "end": { "line": 81, "character": 12 }
                }
            }
        ]);

        let mut runtime = Runtime::new();
        runtime.set_snapshot("windows", stale_windows.clone());
        runtime.set_snapshot(
            "editor_info",
            serde_json::json!({
                "theme": {
                    "style": {
                        "fg": null,
                        "bg": "#111111",
                        "bold": false,
                        "italic": false
                    }
                }
            }),
        );
        runtime
            .load_plugin("barbecue", include_str!("../../plugins/barbecue.hk"))
            .await
            .unwrap();

        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::CreateWindowBar { .. }
        ));
        let config_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, .. } => request_id,
            _ => panic!("expected Barbecue config request"),
        };
        runtime
            .resolve_request(
                config_request_id,
                serde_json::json!({
                    "value": {
                        "cwd": "/repo",
                        "plugin_config": { "barbecue": {} }
                    }
                }),
            )
            .await
            .unwrap();

        let initial_symbols_request_id = loop {
            match ACTION_DISPATCHER.recv_request() {
                PluginRequest::DocumentSymbols { request_id, .. } => break request_id,
                PluginRequest::GetWindows { request_id } => {
                    runtime
                        .resolve_request(request_id, stale_windows.clone())
                        .await
                        .unwrap();
                }
                PluginRequest::CreateWindowBar { .. } | PluginRequest::UpdateWindowBar { .. } => {}
                _ => panic!("unexpected Barbecue startup request"),
            }
        };
        runtime
            .resolve_request(
                initial_symbols_request_id,
                serde_json::json!({
                    "ok": true,
                    "file": "/repo/src/main.rs",
                    "buffer_index": 2,
                    "document_id": 42,
                    "revision": 4,
                    "symbols": symbols.clone(),
                }),
            )
            .await
            .unwrap();
        drain_requests();

        runtime
            .notify(
                "cursor:moved",
                serde_json::json!({
                    "window_id": 7,
                    "x": 8,
                    "y": 97,
                    "lsp_character": 8,
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateWindowBar { segments, .. }
                if segments.iter().any(|segment| segment.text.contains("run"))
                    && !segments.iter().any(|segment| segment.text.contains("DETACHED_PASTE_CHUNK_BYTES"))
        ));

        runtime
            .notify(
                "buffer:changed",
                serde_json::json!({
                    "buffer_id": 2,
                    "revision": 5,
                    "cursor": { "line": 97, "column": 8 },
                }),
            )
            .await
            .unwrap();
        let timer_id = {
            let inner = runtime.inner.lock().unwrap();
            inner
                .host
                .policy()
                .typed_states
                .get("barbecue")
                .and_then(|state| match state {
                    Value::Struct { fields, .. } => fields.get("timer"),
                    _ => None,
                })
                .and_then(Value::as_str)
                .expect("expected Barbecue refresh timer")
                .to_string()
        };
        runtime.cancel_test_timeout(&timer_id);
        runtime
            .notify(
                "timeout:callback",
                serde_json::json!({ "timer_id": timer_id }),
            )
            .await
            .unwrap();

        let refresh_windows_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetWindows { request_id } => request_id,
            _ => panic!("expected live Barbecue windows refresh"),
        };
        runtime
            .resolve_request(
                refresh_windows_request_id,
                serde_json::json!({
                    "windows": [{
                        "window_id": 7,
                        "buffer_index": 2,
                        "document_id": 42,
                        "buffer_path": "/repo/src/main.rs",
                        "breadcrumb_components": ["src", "main.rs"],
                        "revision": 5,
                        "cursor": { "x": 8, "y": 97 },
                        "lsp_position": { "line": 97, "character": 8 },
                    }]
                }),
            )
            .await
            .unwrap();

        let refreshed_symbols_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::DocumentSymbols { request_id, .. } => request_id,
            PluginRequest::UpdateWindowBar { .. } => {
                panic!("Barbecue redrew before refreshed symbols were available");
            }
            _ => panic!("unexpected Barbecue refresh request"),
        };
        runtime
            .notify(
                "cursor:moved",
                serde_json::json!({
                    "window_id": 7,
                    "x": 8,
                    "y": 98,
                    "lsp_character": 8,
                }),
            )
            .await
            .unwrap();
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        runtime
            .resolve_request(
                refreshed_symbols_request_id,
                serde_json::json!({
                    "ok": true,
                    "file": "/repo/src/main.rs",
                    "buffer_index": 2,
                    "document_id": 42,
                    "revision": 5,
                    "symbols": [],
                }),
            )
            .await
            .unwrap();
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        runtime
            .notify(
                "file:saved",
                serde_json::json!({
                    "file": "/repo/src/main.rs",
                    "buffer_index": 2,
                    "document_id": 42,
                }),
            )
            .await
            .unwrap();
        let saved_windows_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetWindows { request_id } => request_id,
            _ => panic!("expected Barbecue windows request after save"),
        };
        runtime
            .resolve_request(
                saved_windows_request_id,
                serde_json::json!({
                    "windows": [{
                        "window_id": 7,
                        "buffer_index": 2,
                        "document_id": 42,
                        "buffer_path": "/repo/src/main.rs",
                        "breadcrumb_components": ["src", "main.rs"],
                        "revision": 5,
                        "cursor": { "x": 8, "y": 98 },
                        "lsp_position": { "line": 98, "character": 8 },
                    }]
                }),
            )
            .await
            .unwrap();
        let saved_symbols_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::DocumentSymbols { request_id, .. } => request_id,
            _ => panic!("expected Barbecue symbols request after save"),
        };
        runtime
            .resolve_request(
                saved_symbols_request_id,
                serde_json::json!({
                    "ok": true,
                    "file": "/repo/src/main.rs",
                    "buffer_index": 2,
                    "document_id": 42,
                    "revision": 5,
                    "symbols": [],
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateWindowBar { segments, .. }
                if !segments.iter().any(|segment| segment.text.contains("run"))
        ));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn git_dashboard_bounds_pathological_porcelain_status() {
        drain_requests();
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        for index in 0..600 {
            fs::write(root.join(format!("untracked_{index}.txt")), "pending\n").unwrap();
        }

        let mut runtime = Runtime::new_with_permissions(HashMap::from([(
            "git".to_string(),
            PluginPermissions {
                process: vec!["git".to_string()],
            },
        )]));
        runtime
            .load_plugin("git", include_str!("../../plugins/git.hk"))
            .await
            .unwrap();
        let mut saw_cwd = false;
        let mut saw_config = false;
        let mut saw_info = false;
        let mut cwd_request_id = None;
        let mut config_request_id = None;
        let mut info_request_id = None;
        for _ in 0..3 {
            match ACTION_DISPATCHER.recv_request() {
                PluginRequest::GetConfig { request_id, key } => {
                    if key.as_deref() == Some("cwd") {
                        cwd_request_id = Some(request_id);
                        saw_cwd = true;
                    } else {
                        assert_eq!(key, None);
                        config_request_id = Some(request_id);
                        saw_config = true;
                    }
                }
                PluginRequest::EditorInfo(request_id) => {
                    info_request_id = Some(request_id);
                    saw_info = true;
                }
                _ => panic!("unexpected plugin request"),
            }
        }
        assert!(saw_cwd && saw_config && saw_info);
        runtime
            .resolve_request(
                cwd_request_id.expect("expected cwd request"),
                serde_json::json!({ "value": root.display().to_string() }),
            )
            .await
            .unwrap();
        runtime
            .resolve_request(
                config_request_id.expect("expected config request"),
                serde_json::json!({ "value": { "executable": "red", "plugin_config": {} } }),
            )
            .await
            .unwrap();
        runtime
            .resolve_request(
                info_request_id.expect("expected editor info request"),
                serde_json::json!({
                    "theme": {
                        "style": { "fg": null, "bg": null, "bold": false, "italic": false },
                        "ui_style": {
                            "muted": { "fg": null, "bg": null, "bold": false, "italic": false },
                            "popup_title": { "fg": null, "bg": null, "bold": false, "italic": false }
                        },
                        "colors": {}
                    }
                }),
            )
            .await
            .unwrap();
        runtime.execute_command("GitDashboard").await.unwrap();

        loop {
            if let PluginRequest::OpenWorkspace { id, config } = ACTION_DISPATCHER.recv_request() {
                assert_eq!(id, "git-dashboard");
                assert_eq!(config.title, "Git");
                break;
            }
        }

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            pump_process_events(&mut runtime).await.unwrap();
            let mut found = false;
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                if let PluginRequest::UpdateWorkspace { id, model } = request {
                    assert_eq!(id, "git-dashboard");
                    assert!(!model.header.is_empty());
                    assert!(!model.rows.is_empty());
                    if model.rows.iter().any(|row| row.id == "status-truncated") {
                        assert_eq!(model.rows.len(), 502);
                        found = true;
                    }
                }
            }
            if found {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "git dashboard did not render the bounded status"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn git_dashboard_renders_numstat_for_a_large_tracked_file_list() {
        drain_requests();
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path();
        let git = |args: &[&str]| {
            assert!(Command::new("git")
                .args(args)
                .current_dir(root)
                .output()
                .unwrap()
                .status
                .success());
        };
        git(&["init", "-q"]);
        for index in 0..500 {
            fs::write(root.join(format!("tracked_{index}.txt")), "before\n").unwrap();
        }
        git(&["add", "."]);
        git(&[
            "-c",
            "user.name=Red Test",
            "-c",
            "user.email=red@example.test",
            "commit",
            "-qm",
            "baseline",
        ]);
        for index in 0..500 {
            fs::write(root.join(format!("tracked_{index}.txt")), "after\n").unwrap();
        }
        let mut runtime = load_git_runtime(root).await;
        runtime.execute_command("GitDashboard").await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            pump_process_events(&mut runtime).await.unwrap();
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                if let PluginRequest::UpdateWorkspace { model, .. } = request {
                    let counted = model
                        .rows
                        .iter()
                        .filter(|row| {
                            row.right_segments
                                .iter()
                                .any(|segment| segment.text.contains("+1 −1"))
                        })
                        .count();
                    if counted == 500 {
                        return;
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "large file list did not receive numstat counts"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn git_menus_and_prompts_use_scoped_picker_callbacks() {
        drain_requests();

        // Submitting the branch prompt runs real Git. Resolve the plugin's cwd
        // before invoking it so the test cannot switch the checkout under test.
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec![
                "-c",
                "user.name=Red Tests",
                "-c",
                "user.email=red@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-qm",
                "test: initial",
            ],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .unwrap()
                .success());
        }
        let mut runtime = load_git_runtime(root).await;
        drain_requests();

        runtime
            .notify(
                "workspace:event:git-dashboard",
                serde_json::json!({ "action": "b", "row": null }),
            )
            .await
            .unwrap();
        let (menu_handle, create_item) = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker {
                owner,
                handle,
                title,
                items,
                ..
            } => {
                assert_eq!(owner, "git");
                assert_eq!(title.as_deref(), Some("Branch"));
                let create_item = items
                    .into_iter()
                    .find(|item| item.id == "Create")
                    .expect("branch picker should contain Create");
                (handle, create_item)
            }
            _ => panic!("expected callback-backed branch picker"),
        };

        runtime
            .notify_picker(menu_handle, PickerCallback::Selected(create_item))
            .unwrap();
        let prompt_handle = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker {
                owner,
                handle,
                title,
                items,
                options,
            } => {
                assert_eq!(owner, "git");
                assert_eq!(title.as_deref(), Some("New branch name"));
                assert!(options.external_filter);
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].id, "submit");
                handle
            }
            _ => panic!("expected callback-backed branch prompt"),
        };

        runtime
            .notify_picker(
                prompt_handle,
                PickerCallback::Query("feature/readable-pickers".to_string()),
            )
            .unwrap();
        let submit_item = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePickerItems { id, items } => {
                assert_eq!(id, prompt_handle.get());
                assert_eq!(items.len(), 1);
                assert_eq!(
                    items[0]
                        .data
                        .get("query")
                        .and_then(serde_json::Value::as_str),
                    Some("feature/readable-pickers")
                );
                assert_eq!(
                    items[0]
                        .data
                        .get("prompt_kind")
                        .and_then(serde_json::Value::as_str),
                    Some("branch-create")
                );
                items[0].clone()
            }
            _ => panic!("expected prompt item update"),
        };

        runtime
            .notify_picker(prompt_handle, PickerCallback::Selected(submit_item))
            .unwrap();
        runtime
            .notify(
                "workspace:event:git-dashboard",
                serde_json::json!({ "action": "$", "row": null }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker {
                owner,
                title,
                items,
                ..
            } => {
                assert_eq!(owner, "git");
                assert_eq!(title.as_deref(), Some("Git command log"));
                assert!(items
                    .iter()
                    .any(|item| item.label == "git switch -c feature/readable-pickers"));
            }
            _ => panic!("expected callback-backed command log"),
        }

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            pump_process_events(&mut runtime).await.unwrap();
            drain_requests();
            if runtime
                .inner
                .lock()
                .unwrap()
                .host
                .process_manager
                .active_process_count("git")
                == 0
            {
                break;
            }
            assert!(Instant::now() < deadline, "branch creation did not finish");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let branch = Command::new("git")
            .args(["symbolic-ref", "--short", "HEAD"])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(branch.status.success());
        assert_eq!(
            String::from_utf8(branch.stdout).unwrap().trim(),
            "feature/readable-pickers"
        );
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn git_push_previews_outgoing_commits_cancels_safely_and_reports_success() {
        drain_requests();
        let remote = tempfile::tempdir().unwrap();
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path();
        assert!(Command::new("git")
            .args(["init", "--bare", "-q"])
            .current_dir(remote.path())
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        for args in [
            vec!["config", "user.name", "Red Tests"],
            vec!["config", "user.email", "red@example.com"],
            vec!["remote", "add", "origin", remote.path().to_str().unwrap()],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .unwrap()
                .success());
        }
        fs::write(root.join("tracked.txt"), "one\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-qm", "feat: initial"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["push", "-qu", "origin", "main"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        let remote_before = Command::new("git")
            .args(["rev-parse", "refs/heads/main"])
            .current_dir(remote.path())
            .output()
            .unwrap()
            .stdout;
        fs::write(root.join("tracked.txt"), "one\ntwo\n").unwrap();
        assert!(Command::new("git")
            .args(["commit", "-qam", "feat: add second line"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());

        let mut runtime = load_git_runtime(root).await;
        runtime.execute_command("GitDashboard").await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            pump_process_events(&mut runtime).await.unwrap();
            let mut ahead = false;
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                if let PluginRequest::UpdateWorkspace { model, .. } = request {
                    ahead = model
                        .header
                        .iter()
                        .any(|segment| segment.text.contains("↑1"));
                }
            }
            if ahead {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Git status did not report one outgoing commit"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let open_push_confirmation = async |runtime: &mut Runtime, action: &str| {
            runtime
                .notify(
                    "workspace:event:git-dashboard",
                    serde_json::json!({ "action": action, "row": null }),
                )
                .await
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                pump_process_events(runtime).await.unwrap();
                while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                    if let PluginRequest::OpenCallbackConfirmation {
                        handle,
                        title,
                        options,
                        ..
                    } = request
                    {
                        assert_eq!(title, "Push changes");
                        assert_eq!(options.accept_label.as_deref(), Some("Push"));
                        assert_eq!(options.cancel_label.as_deref(), Some("Cancel"));
                        let text = options
                            .rows
                            .iter()
                            .flatten()
                            .map(|segment| segment.text.as_str())
                            .collect::<String>();
                        assert!(text.contains("main  →  origin/main"));
                        assert!(text.contains("1 outgoing commit"));
                        assert!(text.contains("feat: add second line"));
                        return handle;
                    }
                }
                assert!(Instant::now() < deadline, "push confirmation did not open");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };

        let cancel_handle = open_push_confirmation(&mut runtime, "y").await;
        runtime
            .notify_picker(cancel_handle, PickerCallback::Cancelled)
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let remote_after_cancel = Command::new("git")
            .args(["rev-parse", "refs/heads/main"])
            .current_dir(remote.path())
            .output()
            .unwrap()
            .stdout;
        assert_eq!(remote_after_cancel, remote_before);

        let accept_handle = open_push_confirmation(&mut runtime, "p").await;
        let accept = serde_json::from_value(serde_json::json!({
            "id": "accept",
            "label": "Push",
        }))
        .unwrap();
        runtime
            .notify_picker(accept_handle, PickerCallback::Selected(accept))
            .unwrap();
        let mut created = false;
        let mut busy = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::CreateOverlay { id, .. } if id == "git-push-progress" => {
                    created = true;
                }
                PluginRequest::UpdateOverlayBusy { id, busy: value }
                    if id == "git-push-progress" && value =>
                {
                    busy = true;
                }
                _ => {}
            }
        }
        assert!(created && busy, "push should start with a busy overlay");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut succeeded = false;
        while !succeeded {
            pump_process_events(&mut runtime).await.unwrap();
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                match request {
                    PluginRequest::UpdateOverlay { id, lines }
                        if id == "git-push-progress"
                            && lines
                                .iter()
                                .any(|(line, _)| line.contains("✓ Pushed 1 commit")) =>
                    {
                        succeeded = true;
                    }
                    _ => {}
                }
            }
            assert!(Instant::now() < deadline, "push did not report success");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let local_head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(root)
            .output()
            .unwrap()
            .stdout;
        let remote_head = Command::new("git")
            .args(["rev-parse", "refs/heads/main"])
            .current_dir(remote.path())
            .output()
            .unwrap()
            .stdout;
        assert_eq!(remote_head, local_head);

        assert!(Command::new("git")
            .args(["branch", "--unset-upstream"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        fs::write(root.join("tracked.txt"), "one\ntwo\nthree\n").unwrap();
        assert!(Command::new("git")
            .args(["commit", "-qam", "feat: add third line"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        runtime
            .notify(
                "workspace:event:git-dashboard",
                serde_json::json!({ "action": "r", "row": null }),
            )
            .await
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            pump_process_events(&mut runtime).await.unwrap();
            let mut without_upstream = false;
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                if let PluginRequest::UpdateWorkspace { model, .. } = request {
                    without_upstream = !model
                        .header
                        .iter()
                        .any(|segment| segment.text.contains("origin/main"));
                }
            }
            if without_upstream {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Git status retained a removed upstream"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        runtime
            .notify(
                "workspace:event:git-dashboard",
                serde_json::json!({ "action": "p", "row": null }),
            )
            .await
            .unwrap();
        let (remote_handle, origin_item) = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker {
                handle,
                title,
                items,
                ..
            } => {
                assert_eq!(title.as_deref(), Some("Set upstream"));
                let origin = items
                    .into_iter()
                    .find(|item| item.id == "origin")
                    .expect("origin remote choice");
                (handle, origin)
            }
            _ => panic!("expected set-upstream picker"),
        };
        runtime
            .notify_picker(remote_handle, PickerCallback::Selected(origin_item))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let set_upstream_handle = loop {
            pump_process_events(&mut runtime).await.unwrap();
            let mut found = None;
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                if let PluginRequest::OpenCallbackConfirmation {
                    handle, options, ..
                } = request
                {
                    let text = options
                        .rows
                        .iter()
                        .flatten()
                        .map(|segment| segment.text.as_str())
                        .collect::<String>();
                    assert!(text.contains("main  →  origin/main"));
                    assert!(text.contains("feat: add third line"));
                    found = Some(handle);
                }
            }
            if let Some(handle) = found {
                break handle;
            }
            assert!(
                Instant::now() < deadline,
                "set-upstream confirmation did not open"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let accept = serde_json::from_value(serde_json::json!({
            "id": "accept",
            "label": "Push",
        }))
        .unwrap();
        runtime
            .notify_picker(set_upstream_handle, PickerCallback::Selected(accept))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            pump_process_events(&mut runtime).await.unwrap();
            let mut done = false;
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                if let PluginRequest::UpdateOverlay { id, lines } = request {
                    done = id == "git-push-progress"
                        && lines
                            .iter()
                            .any(|(line, _)| line.contains("✓ Pushed 1 commit"));
                }
            }
            if done {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "set-upstream push did not complete"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let upstream = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(upstream.status.success());
        assert_eq!(
            String::from_utf8_lossy(&upstream.stdout).trim(),
            "origin/main"
        );
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn git_generated_commit_reports_failure_retry_and_success() {
        drain_requests();
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        fs::write(root.join("base.txt"), "base\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "base.txt"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args([
                "-c",
                "user.name=Red Test",
                "-c",
                "user.email=red@example.test",
                "commit",
                "-qm",
                "feat(core): establish repository style",
            ])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["config", "user.name", "Red Test"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["config", "user.email", "red@example.test"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["config", "commit.gpgsign", "false"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        fs::write(root.join("staged.txt"), "hello\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "staged.txt"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());

        let mut runtime = load_git_runtime(root).await;
        runtime.execute_command("GitDashboard").await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            pump_process_events(&mut runtime).await.unwrap();
            let mut staged_visible = false;
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                if let PluginRequest::UpdateWorkspace { model, .. } = request {
                    staged_visible = model.rows.iter().any(|row| row.id == "staged:staged.txt");
                }
            }
            if staged_visible {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "staged Git status did not render"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        runtime
            .notify(
                "workspace:event:git-dashboard",
                serde_json::json!({ "action": "c", "row": null }),
            )
            .await
            .unwrap();
        let (picker, generate) = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker {
                handle,
                title,
                items,
                ..
            } => {
                assert_eq!(title.as_deref(), Some("Commit"));
                assert_eq!(items.first().map(|item| item.id.as_str()), Some("generate"));
                assert_eq!(
                    items.first().map(|item| item.label.as_str()),
                    Some("Generate message")
                );
                assert!(items.iter().any(|item| item.id == "create"));
                let generate = items
                    .into_iter()
                    .find(|item| item.id == "generate")
                    .expect("commit picker should contain generated message");
                (handle, generate)
            }
            _ => panic!("expected the commit picker"),
        };
        runtime
            .notify_picker(picker, PickerCallback::Selected(generate))
            .unwrap();
        let mut generation_progress = false;
        let request_id = loop {
            match ACTION_DISPATCHER.recv_request() {
                PluginRequest::UpdateOverlayBusy { id, busy }
                    if id == "git-operation-progress" && busy =>
                {
                    generation_progress = true;
                }
                PluginRequest::GetEditorState { request_id } => break request_id,
                _ => {}
            }
        };
        assert!(
            generation_progress,
            "commit generation should show progress"
        );
        let editor_snapshot = serde_json::json!({
            "version": 1,
            "cwd": root.display().to_string(),
            "saved_at": 1,
            "buffers": [{
                "index": 0,
                "path": root.join("base.txt").display().to_string(),
                "dirty": false,
                "cursor": { "x": 0, "y": 0 },
                "viewport_top": 0
            }],
            "current_buffer_index": 0,
            "window_layout": {
                "active_window_id": 0,
                "root": {
                    "kind": "window",
                    "buffer_index": 0,
                    "vtop": 0,
                    "vleft": 0,
                    "cx": 0,
                    "cy": 0,
                    "vx": 0
                }
            }
        });
        runtime
            .resolve_request(request_id, editor_snapshot.clone())
            .await
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let generation_request = loop {
            pump_process_events(&mut runtime).await.unwrap();
            let mut generation_request = None;
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                if let PluginRequest::GenerateCommitMessage {
                    request_id,
                    cwd,
                    branch,
                    staged_diff,
                    recent_commits,
                } = request
                {
                    assert_eq!(cwd.canonicalize().unwrap(), root.canonicalize().unwrap());
                    assert!(branch == "master" || branch == "main");
                    assert!(staged_diff.contains("staged.txt"));
                    assert!(staged_diff.contains("+hello"));
                    assert!(recent_commits.contains("feat(core): establish repository style"));
                    generation_request = Some(request_id);
                }
            }
            if let Some(request_id) = generation_request {
                break request_id;
            }
            assert!(
                Instant::now() < deadline,
                "commit message generation was not requested"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        runtime
            .resolve_request(
                generation_request,
                serde_json::json!({
                    "message": "feat(git): describe staged files",
                    "error": ""
                }),
            )
            .await
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let scratch_request = loop {
            pump_process_events(&mut runtime).await.unwrap();
            let mut scratch = None;
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                if let PluginRequest::OpenScratchBuffer {
                    request_id,
                    name,
                    text,
                    syntax,
                    submit_command,
                    cancel_command,
                } = request
                {
                    scratch = Some((
                        request_id,
                        name,
                        text,
                        syntax,
                        submit_command,
                        cancel_command,
                    ));
                }
            }
            if let Some((request_id, name, text, syntax, submit, cancel)) = scratch {
                assert_eq!(name, "[Git Commit].gitcommit");
                assert_eq!(syntax.as_deref(), Some("gitcommit"));
                assert_eq!(submit.as_deref(), Some("GitSubmitMessage"));
                assert_eq!(cancel.as_deref(), Some("GitCancelMessage"));
                assert!(text.starts_with("feat(git): describe staged files\n\n#"));
                assert!(text.contains("# --- Red commit context"));
                assert!(text.contains("# Changes to be committed:"));
                assert!(text.contains("staged.txt"));
                assert!(text.contains("# Staged diff:"));
                assert!(text.contains("# +hello"));
                break request_id;
            }
            assert!(
                Instant::now() < deadline,
                "commit scratch buffer did not open"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        runtime
            .resolve_request(scratch_request, serde_json::json!({ "buffer_index": 7 }))
            .await
            .unwrap();

        let hook = root.join(".git/hooks/pre-commit");
        fs::write(
            &hook,
            "#!/bin/sh\necho blocked by commit feedback test >&2\nexit 1\n",
        )
        .unwrap();
        assert!(Command::new("chmod")
            .args(["+x", hook.to_str().unwrap()])
            .status()
            .unwrap()
            .success());

        // Force the stdin writer to observe the hook's early rejection instead
        // of relying on whether a short message fits in the pipe before Git exits.
        let rejected_message = format!(
            "feat(git): describe staged files\n\n{}\n",
            "commit feedback regression ".repeat(8192)
        );
        runtime.execute_command("GitSubmitMessage").await.unwrap();
        let buffer_text_request = loop {
            if let PluginRequest::GetBufferText { request_id, .. } =
                ACTION_DISPATCHER.recv_request()
            {
                break request_id;
            }
        };
        runtime
            .resolve_request(
                buffer_text_request,
                serde_json::json!({ "text": rejected_message }),
            )
            .await
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut failure_reported = false;
        let mut failure_overlay = false;
        let mut commit_started = false;
        let mut commit_busy = false;
        let mut retry_requested = false;
        loop {
            pump_process_events(&mut runtime).await.unwrap();
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                match request {
                    PluginRequest::CloseScratchBuffer { buffer_index } => {
                        assert_eq!(buffer_index, 7);
                    }
                    PluginRequest::RestoreEditorState { request_id, .. } => {
                        runtime
                            .resolve_request(
                                request_id,
                                serde_json::json!({ "restored": true, "warnings": [] }),
                            )
                            .await
                            .unwrap();
                    }
                    PluginRequest::Action(Action::Print(message))
                        if message.starts_with("Git commit failed:") =>
                    {
                        assert_eq!(
                            message,
                            "Git commit failed: blocked by commit feedback test"
                        );
                        failure_reported = true;
                    }
                    PluginRequest::CreateOverlay { id, .. } if id == "git-operation-progress" => {
                        commit_started = true;
                    }
                    PluginRequest::UpdateOverlayBusy { id, busy }
                        if id == "git-operation-progress" && busy =>
                    {
                        commit_busy = true;
                    }
                    PluginRequest::UpdateOverlay { id, lines }
                        if id == "git-operation-progress"
                            && lines.iter().any(|(line, _)| {
                                line == "Git commit failed: blocked by commit feedback test"
                            }) =>
                    {
                        failure_overlay = true;
                    }
                    PluginRequest::GetEditorState { request_id } => {
                        runtime
                            .resolve_request(request_id, editor_snapshot.clone())
                            .await
                            .unwrap();
                        retry_requested = true;
                    }
                    _ => {}
                }
            }
            if failure_reported
                && failure_overlay
                && commit_started
                && commit_busy
                && retry_requested
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "commit failure was not reported: printed={failure_reported} overlay={failure_overlay} started={commit_started} busy={commit_busy} retry={retry_requested}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let deadline = Instant::now() + Duration::from_secs(5);
        let retry_scratch = loop {
            pump_process_events(&mut runtime).await.unwrap();
            let mut scratch = None;
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                if let PluginRequest::OpenScratchBuffer {
                    request_id,
                    text,
                    submit_command,
                    ..
                } = request
                {
                    scratch = Some((request_id, text, submit_command));
                }
            }
            if let Some((request_id, text, submit)) = scratch {
                assert_eq!(submit.as_deref(), Some("GitSubmitMessage"));
                assert!(text.starts_with(rejected_message.trim_end()));
                break request_id;
            }
            assert!(
                Instant::now() < deadline,
                "failed commit message was not reopened for retry"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        runtime
            .resolve_request(retry_scratch, serde_json::json!({ "buffer_index": 8 }))
            .await
            .unwrap();
        fs::remove_file(hook).unwrap();

        runtime.execute_command("GitSubmitMessage").await.unwrap();
        let buffer_text_request = loop {
            if let PluginRequest::GetBufferText { request_id, .. } =
                ACTION_DISPATCHER.recv_request()
            {
                break request_id;
            }
        };
        runtime
            .resolve_request(
                buffer_text_request,
                serde_json::json!({ "text": "feat(git): describe staged files\n" }),
            )
            .await
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut restored = false;
        let mut success_overlay = false;
        let success = loop {
            pump_process_events(&mut runtime).await.unwrap();
            let mut success = false;
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                match request {
                    PluginRequest::CloseScratchBuffer { buffer_index } => {
                        assert_eq!(buffer_index, 8);
                    }
                    PluginRequest::RestoreEditorState { request_id, .. } => {
                        runtime
                            .resolve_request(
                                request_id,
                                serde_json::json!({ "restored": true, "warnings": [] }),
                            )
                            .await
                            .unwrap();
                        restored = true;
                    }
                    PluginRequest::Action(Action::Print(message))
                        if message == "✓ Commit created successfully" =>
                    {
                        success = true;
                    }
                    PluginRequest::UpdateOverlay { id, lines }
                        if id == "git-operation-progress"
                            && lines
                                .iter()
                                .any(|(line, _)| line == "✓ Commit created successfully") =>
                    {
                        success_overlay = true;
                    }
                    _ => {}
                }
            }
            if success {
                break true;
            }
            assert!(Instant::now() < deadline, "commit success was not reported");
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert!(success);
        assert!(success_overlay);
        assert!(restored);
        assert_eq!(
            String::from_utf8(
                Command::new("git")
                    .args(["log", "-1", "--format=%s"])
                    .current_dir(root)
                    .output()
                    .unwrap()
                    .stdout
            )
            .unwrap()
            .trim(),
            "feat(git): describe staged files"
        );

        runtime.execute_command("GitDashboard").await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            pump_process_events(&mut runtime).await.unwrap();
            let mut rendered = false;
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                if matches!(request, PluginRequest::UpdateWorkspace { .. }) {
                    rendered = true;
                }
            }
            if rendered {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Git dashboard did not reopen for amend"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        runtime
            .notify(
                "workspace:event:git-dashboard",
                serde_json::json!({ "action": "c", "row": null }),
            )
            .await
            .unwrap();
        let (commit_picker, no_edit) = loop {
            if let PluginRequest::OpenCallbackPicker {
                handle,
                title,
                items,
                ..
            } = ACTION_DISPATCHER.recv_request()
            {
                if title.as_deref() == Some("Commit") {
                    let no_edit = items
                        .into_iter()
                        .find(|item| item.id == "no-edit")
                        .expect("commit picker should contain amend without editing");
                    break (handle, no_edit);
                }
            }
        };
        runtime
            .notify_picker(commit_picker, PickerCallback::Selected(no_edit))
            .unwrap();
        let (confirmation, proceed) = loop {
            if let PluginRequest::OpenCallbackPicker {
                handle,
                title,
                items,
                ..
            } = ACTION_DISPATCHER.recv_request()
            {
                if title.as_deref() == Some("Amend commit") {
                    let proceed = items
                        .into_iter()
                        .find(|item| item.id == "proceed")
                        .expect("amend confirmation should contain proceed");
                    break (handle, proceed);
                }
            }
        };
        runtime
            .notify_picker(confirmation, PickerCallback::Selected(proceed))
            .unwrap();

        let mut amend_started = false;
        let mut amend_busy = false;
        let mut amend_succeeded = false;
        let mut amend_messages = Vec::new();
        let amend_result = tokio::time::timeout(Duration::from_secs(30), async {
            while !amend_started || !amend_busy || !amend_succeeded {
                pump_process_events(&mut runtime).await.unwrap();
                while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                    match request {
                        PluginRequest::UpdateOverlayBusy { id, busy }
                            if id == "git-operation-progress" && busy =>
                        {
                            amend_busy = true;
                        }
                        PluginRequest::UpdateOverlay { id, lines }
                            if id == "git-operation-progress" =>
                        {
                            for (line, _) in lines {
                                amend_started |= line == "Amending commit…";
                                amend_succeeded |= line == "✓ Commit amended successfully";
                                assert!(
                                    !line.starts_with("Git commit failed:"),
                                    "amend failed: {line}"
                                );
                                amend_messages.push(line);
                            }
                        }
                        PluginRequest::Action(Action::Print(message)) => {
                            amend_messages.push(message);
                        }
                        _ => {}
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(
            amend_result.is_ok(),
            "amend progress was not reported: started={amend_started} busy={amend_busy} succeeded={amend_succeeded} messages={amend_messages:?}"
        );
        assert_eq!(
            String::from_utf8(
                Command::new("git")
                    .args(["rev-list", "--count", "HEAD"])
                    .current_dir(root)
                    .output()
                    .unwrap()
                    .stdout
            )
            .unwrap()
            .trim(),
            "2"
        );
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn git_user_operations_report_success_failure_and_cleanup() {
        drain_requests();
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path();
        assert!(Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        fs::write(root.join("tracked.txt"), "one\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args([
                "-c",
                "user.name=Red Test",
                "-c",
                "user.email=red@example.test",
                "commit",
                "-qm",
                "feat: initial",
            ])
            .current_dir(root)
            .status()
            .unwrap()
            .success());

        let mut runtime = load_git_runtime(root).await;
        runtime.execute_command("GitDashboard").await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            pump_process_events(&mut runtime).await.unwrap();
            let mut rendered = false;
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                if matches!(request, PluginRequest::UpdateWorkspace { .. }) {
                    rendered = true;
                }
            }
            if rendered {
                break;
            }
            assert!(Instant::now() < deadline, "Git dashboard did not render");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        runtime
            .notify(
                "workspace:event:git-dashboard",
                serde_json::json!({ "action": "f", "row": null }),
            )
            .await
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut fetch_succeeded = false;
        let mut fetch_stopped_busy = false;
        while !fetch_succeeded || !fetch_stopped_busy {
            pump_process_events(&mut runtime).await.unwrap();
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                match request {
                    PluginRequest::UpdateOverlay { id, lines }
                        if id == "git-operation-progress"
                            && lines.iter().any(|(line, _)| line == "✓ Fetch completed") =>
                    {
                        fetch_succeeded = true;
                    }
                    PluginRequest::UpdateOverlayBusy { id, busy }
                        if id == "git-operation-progress" && !busy =>
                    {
                        fetch_stopped_busy = true;
                    }
                    _ => {}
                }
            }
            assert!(Instant::now() < deadline, "fetch success was not reported");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        runtime
            .notify(
                "workspace:event:git-dashboard",
                serde_json::json!({ "action": "P", "row": null }),
            )
            .await
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut pull_failed = false;
        while !pull_failed {
            pump_process_events(&mut runtime).await.unwrap();
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                if let PluginRequest::UpdateOverlay { id, lines } = request {
                    if id == "git-operation-progress"
                        && lines
                            .iter()
                            .any(|(line, _)| line.starts_with("Git operation failed:"))
                    {
                        pull_failed = true;
                    }
                }
            }
            assert!(Instant::now() < deadline, "pull failure was not reported");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        runtime.deactivate_all().await.unwrap();
        let mut removed = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::RemoveOverlay { id } = request {
                if id == "git-operation-progress" {
                    removed = true;
                }
            }
        }
        assert!(
            removed,
            "Git operation overlay was not removed on deactivation"
        );
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn git_dashboard_renders_untracked_file_contents() {
        drain_requests();
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        fs::write(
            root.join("new-file.txt"),
            "first new line\nsecond new line\n",
        )
        .unwrap();

        let mut runtime = load_git_runtime(root).await;
        runtime.execute_command("GitDashboard").await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let document = loop {
            pump_process_events(&mut runtime).await.unwrap();
            let mut document = None;
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                if let PluginRequest::UpdateWorkspace { model, .. } = request {
                    if let Some(detail) = model.detail_document {
                        document = Some(detail);
                    }
                }
            }
            if let Some(document) = document {
                break document;
            }
            assert!(
                Instant::now() < deadline,
                "untracked file contents did not render"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(document.path, "new-file.txt");
        let added = document
            .lines
            .iter()
            .filter(|line| line.kind == "added")
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(added, ["first new line", "second new line"]);
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn git_dashboard_reopens_with_unchanged_status() {
        drain_requests();
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        fs::write(root.join("new-file.txt"), "new contents\n").unwrap();

        let mut runtime = load_git_runtime(root).await;
        runtime.execute_command("GitDashboard").await.unwrap();
        let wait_for_row = async |runtime: &mut Runtime, failure: &str| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                pump_process_events(runtime).await.unwrap();
                let mut found = false;
                while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                    if let PluginRequest::UpdateWorkspace { model, .. } = request {
                        found = model
                            .rows
                            .iter()
                            .any(|row| row.id == "untracked:new-file.txt");
                    }
                }
                if found {
                    break;
                }
                assert!(Instant::now() < deadline, "{failure}");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        wait_for_row(&mut runtime, "first Git dashboard did not render").await;

        runtime
            .notify(
                "workspace:event:git-dashboard",
                serde_json::json!({ "action": "q", "row": null }),
            )
            .await
            .unwrap();
        drain_requests();
        runtime.execute_command("GitDashboard").await.unwrap();
        wait_for_row(
            &mut runtime,
            "reopened Git dashboard did not restore unchanged status",
        )
        .await;
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn git_dashboard_reports_failed_row_actions() {
        drain_requests();
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        fs::write(root.join("new-file.txt"), "new contents\n").unwrap();

        let mut runtime = load_git_runtime(root).await;
        fs::write(root.join(".git/index.lock"), "").unwrap();
        runtime
            .notify(
                "workspace:event:git-dashboard",
                serde_json::json!({
                    "action": "s",
                    "focus": "rows",
                    "row": {
                        "id": "untracked:new-file.txt",
                        "selectable": true,
                        "depth": 1,
                        "path": "new-file.txt",
                        "segments": [],
                        "right_segments": [],
                        "data": {
                            "section": "untracked",
                            "path": "new-file.txt",
                            "entry": null
                        }
                    }
                }),
            )
            .await
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let error = loop {
            pump_process_events(&mut runtime).await.unwrap();
            let mut error = None;
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                if let PluginRequest::Action(Action::Print(message)) = request {
                    error = Some(message);
                }
            }
            if let Some(error) = error {
                break error;
            }
            assert!(
                Instant::now() < deadline,
                "failed Git row action did not report its error"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert!(
            error.contains("index.lock"),
            "unexpected Git error: {error}"
        );
        assert!(Command::new("git")
            .args(["status", "--short"])
            .current_dir(root)
            .output()
            .unwrap()
            .stdout
            .starts_with(b"?? new-file.txt"));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn git_dashboard_renders_structured_diff_and_stages_one_selected_line() {
        drain_requests();
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path();
        let file = root.join("tracked.rs");
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        fs::write(&file, "one\ntwo\nthree\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "tracked.rs"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args([
                "-c",
                "user.name=Red Test",
                "-c",
                "user.email=red@example.test",
                "commit",
                "-qm",
                "initial",
            ])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        fs::write(&file, "ONE\ntwo\nTHREE\n").unwrap();

        let mut runtime = load_git_runtime(root).await;
        runtime.execute_command("GitDashboard").await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let selected_row = loop {
            pump_process_events(&mut runtime).await.unwrap();
            let mut selected = None;
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                if let PluginRequest::UpdateWorkspace { model, .. } = request {
                    selected = model
                        .rows
                        .into_iter()
                        .find(|row| row.id == "unstaged:tracked.rs");
                }
            }
            if let Some(row) = selected {
                break row;
            }
            assert!(Instant::now() < deadline, "Git status row did not appear");
            tokio::time::sleep(Duration::from_millis(10)).await;
        };

        runtime
            .notify(
                "workspace:event:git-dashboard",
                serde_json::json!({
                    "action": "down",
                    "focus": "rows",
                    "row": selected_row,
                }),
            )
            .await
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let selected_line = loop {
            pump_process_events(&mut runtime).await.unwrap();
            let mut selected = None;
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                if let PluginRequest::UpdateWorkspace { model, .. } = request {
                    selected = model.detail_document.and_then(|document| {
                        document
                            .lines
                            .into_iter()
                            .find(|line| line.kind == "added" && line.text == "ONE")
                    });
                }
            }
            if let Some(line) = selected {
                break line;
            }
            assert!(
                Instant::now() < deadline,
                "structured Git diff did not appear"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };

        runtime
            .notify(
                "workspace:event:git-dashboard",
                serde_json::json!({
                    "action": "s",
                    "focus": "detail",
                    "detail_index": 0,
                    "detail_line": selected_line,
                    "detail_selection": null,
                    "row": selected_row,
                }),
            )
            .await
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let staged = loop {
            pump_process_events(&mut runtime).await.unwrap();
            while ACTION_DISPATCHER.try_recv_request().is_some() {}
            let staged = Command::new("git")
                .args(["show", ":tracked.rs"])
                .current_dir(root)
                .output()
                .unwrap();
            let contents = String::from_utf8(staged.stdout).unwrap();
            if contents.contains("ONE") {
                break contents;
            }
            assert!(Instant::now() < deadline, "selected line was not staged");
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert!(staged.contains("ONE"));
        assert!(!staged.contains("THREE"));
        assert_eq!(fs::read_to_string(file).unwrap(), "ONE\ntwo\nTHREE\n");
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn git_dashboard_debounces_rapid_detail_selection_to_one_process() {
        drain_requests();
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        for name in ["first.rs", "second.rs"] {
            fs::write(root.join(name), "fn before() {}\n").unwrap();
        }
        assert!(Command::new("git")
            .args(["add", "."])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args([
                "-c",
                "user.name=Red Test",
                "-c",
                "user.email=red@example.test",
                "commit",
                "-qm",
                "initial",
            ])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        fs::write(root.join("first.rs"), "fn first() {}\n").unwrap();
        fs::write(root.join("second.rs"), "fn second() {}\n").unwrap();

        let mut runtime = load_git_runtime(root).await;
        runtime.execute_command("GitDashboard").await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut rows = None;
        let (first, second) = loop {
            pump_process_events(&mut runtime).await.unwrap();
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                match request {
                    PluginRequest::GetWindows { request_id } => {
                        runtime
                            .resolve_request(request_id, serde_json::json!({ "windows": [] }))
                            .await
                            .unwrap();
                    }
                    PluginRequest::UpdateWorkspace { model, .. }
                        if model.detail_document.is_some() =>
                    {
                        let first = model
                            .rows
                            .iter()
                            .find(|row| row.id == "unstaged:first.rs")
                            .cloned();
                        let second = model
                            .rows
                            .iter()
                            .find(|row| row.id == "unstaged:second.rs")
                            .cloned();
                        if let (Some(first), Some(second)) = (first, second) {
                            rows = Some((first, second));
                        }
                    }
                    _ => {}
                }
            }
            if let Some(rows) = rows.as_ref() {
                if runtime
                    .inner
                    .lock()
                    .unwrap()
                    .host
                    .process_manager
                    .active_process_count("git")
                    == 0
                {
                    break rows.clone();
                }
            }
            assert!(
                Instant::now() < deadline,
                "initial Git detail did not settle"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };

        for index in 0..40 {
            let row = if index % 2 == 0 { &first } else { &second };
            runtime
                .notify(
                    "workspace:event:git-dashboard",
                    serde_json::json!({ "action": "down", "focus": "rows", "row": row }),
                )
                .await
                .unwrap();
        }
        assert_eq!(
            runtime
                .inner
                .lock()
                .unwrap()
                .host
                .process_manager
                .active_process_count("git"),
            0,
            "selection changes should wait for the debounce window"
        );

        tokio::time::sleep(Duration::from_millis(70)).await;
        let callbacks = runtime.poll_timer_callbacks();
        assert!(
            !callbacks.is_empty(),
            "the final detail timer should remain"
        );
        for callback in callbacks {
            if let PluginRequest::TimeoutCallback { timer_id } = callback {
                runtime
                    .notify(
                        "timeout:callback",
                        serde_json::json!({ "timer_id": timer_id }),
                    )
                    .await
                    .unwrap();
            }
        }
        assert_eq!(
            runtime
                .inner
                .lock()
                .unwrap()
                .host
                .process_manager
                .active_process_count("git"),
            1,
            "the debounce callback should spawn one detail process"
        );

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            pump_process_events(&mut runtime).await.unwrap();
            let mut selected_path = None;
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                if let PluginRequest::UpdateWorkspace { model, .. } = request {
                    selected_path = model.detail_document.map(|document| document.path);
                }
            }
            if selected_path.as_deref() == Some("second.rs") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "debounced Git detail did not render"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn git_signs_deduplicate_split_windows_and_apply_staged_configuration() {
        drain_requests();
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path();
        let file = root.join("tracked.txt");
        let second_file = root.join("second.txt");
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        let original = (0..600)
            .map(|line| format!("before {line}\n"))
            .collect::<String>();
        fs::write(&file, original).unwrap();
        fs::write(&second_file, "before\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "."])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args([
                "-c",
                "user.name=Red Test",
                "-c",
                "user.email=red@example.test",
                "commit",
                "-qm",
                "initial",
            ])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        let modified = (0..600)
            .map(|line| format!("after {line}\n"))
            .collect::<String>();
        fs::write(&file, modified).unwrap();
        fs::write(&second_file, "after\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "."])
            .current_dir(root)
            .status()
            .unwrap()
            .success());

        let mut runtime = Runtime::new_with_permissions(HashMap::from([(
            "git".to_string(),
            PluginPermissions {
                process: vec!["git".to_string()],
            },
        )]));
        runtime
            .load_plugin("git", include_str!("../../plugins/git.hk"))
            .await
            .unwrap();
        let mut cwd_request_id = None;
        let mut config_request_id = None;
        let mut info_request_id = None;
        for _ in 0..3 {
            match ACTION_DISPATCHER.recv_request() {
                PluginRequest::GetConfig { request_id, key } if key.as_deref() == Some("cwd") => {
                    cwd_request_id = Some(request_id);
                }
                PluginRequest::GetConfig {
                    request_id,
                    key: None,
                } => {
                    config_request_id = Some(request_id);
                }
                PluginRequest::EditorInfo(request_id) => info_request_id = Some(request_id),
                _ => panic!("unexpected plugin request"),
            }
        }
        runtime
            .resolve_request(
                cwd_request_id.unwrap(),
                serde_json::json!({ "value": root.display().to_string() }),
            )
            .await
            .unwrap();
        runtime
            .resolve_request(
                config_request_id.unwrap(),
                serde_json::json!({
                    "value": {
                        "executable": "red",
                        "plugin_config": {
                            "git": {
                                "staged_signs": { "change": "old" },
                                "signs_staged": { "change": "!" }
                            }
                        }
                    }
                }),
            )
            .await
            .unwrap();
        runtime
            .resolve_request(
                info_request_id.unwrap(),
                serde_json::json!({
                    "theme": {
                        "style": { "fg": null, "bg": null, "bold": false, "italic": false },
                        "ui_style": {
                            "muted": { "fg": null, "bg": null, "bold": false, "italic": false },
                            "popup_title": { "fg": null, "bg": null, "bold": false, "italic": false }
                        },
                        "colors": {}
                    }
                }),
            )
            .await
            .unwrap();
        runtime.execute_command("GitRefresh").await.unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut expected_sign_count = 0;
        let mut saw_second_sign = false;
        loop {
            pump_process_events(&mut runtime).await.unwrap();
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                match request {
                    PluginRequest::GetWindows { request_id } => {
                        runtime
                            .resolve_request(
                                request_id,
                                serde_json::json!({
                                    "windows": [
                                        {
                                            "buffer_path": file.display().to_string(),
                                            "buffer_index": 7,
                                            "active": true
                                        },
                                        {
                                            "buffer_path": file.display().to_string(),
                                            "buffer_index": 7,
                                            "active": false
                                        },
                                        {
                                            "buffer_path": second_file.display().to_string(),
                                            "buffer_index": 8,
                                            "active": false
                                        }
                                    ]
                                }),
                            )
                            .await
                            .unwrap();
                    }
                    PluginRequest::SetGutterSigns { signs, .. } => {
                        expected_sign_count = signs
                            .iter()
                            .filter(|sign| {
                                sign.buffer_index == 7 && sign.text == "!" && sign.priority == 5
                            })
                            .count();
                        saw_second_sign = signs.iter().any(|sign| {
                            sign.buffer_index == 8 && sign.text == "!" && sign.priority == 5
                        });
                    }
                    _ => {}
                }
            }
            let active_process_count = runtime
                .inner
                .lock()
                .unwrap()
                .host
                .process_manager
                .active_process_count("git");
            if expected_sign_count > 0 && saw_second_sign && active_process_count == 0 {
                assert_eq!(expected_sign_count, 200);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "configured staged gutter sign was not emitted"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn git_hunk_navigation_targets_changed_lines_and_reports_boundaries() {
        drain_requests();
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path();
        let file = root.join("tracked.txt");
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        let original = (1..=30)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        fs::write(&file, &original).unwrap();
        assert!(Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args([
                "-c",
                "user.name=Red Test",
                "-c",
                "user.email=red@example.test",
                "commit",
                "-qm",
                "initial",
            ])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        let modified = original
            .replace("line 14\n", "changed 14\n")
            .replace("line 26\n", "changed 26\n");
        fs::write(&file, &modified).unwrap();

        let mut runtime = Runtime::new_with_permissions(HashMap::from([(
            "git".to_string(),
            PluginPermissions {
                process: vec!["git".to_string()],
            },
        )]));
        runtime
            .load_plugin("git", include_str!("../../plugins/git.hk"))
            .await
            .unwrap();
        let mut cwd_request_id = None;
        let mut config_request_id = None;
        let mut info_request_id = None;
        for _ in 0..3 {
            match ACTION_DISPATCHER.recv_request() {
                PluginRequest::GetConfig { request_id, key } if key.as_deref() == Some("cwd") => {
                    cwd_request_id = Some(request_id);
                }
                PluginRequest::GetConfig {
                    request_id,
                    key: None,
                } => config_request_id = Some(request_id),
                PluginRequest::EditorInfo(request_id) => info_request_id = Some(request_id),
                _ => panic!("unexpected plugin request"),
            }
        }
        runtime
            .resolve_request(
                cwd_request_id.unwrap(),
                serde_json::json!({ "value": root.display().to_string() }),
            )
            .await
            .unwrap();
        runtime
            .resolve_request(
                config_request_id.unwrap(),
                serde_json::json!({ "value": { "executable": "red", "plugin_config": {} } }),
            )
            .await
            .unwrap();
        runtime
            .resolve_request(
                info_request_id.unwrap(),
                serde_json::json!({
                    "theme": {
                        "style": { "fg": null, "bg": null, "bold": false, "italic": false },
                        "ui_style": {
                            "muted": { "fg": null, "bg": null, "bold": false, "italic": false },
                            "popup_title": { "fg": null, "bg": null, "bold": false, "italic": false }
                        },
                        "colors": {}
                    }
                }),
            )
            .await
            .unwrap();

        for (command, cursor_line, expected) in [
            ("GitHunkNext", 0, Ok((0, 13))),
            ("GitHunkPrevious", 29, Ok((0, 25))),
            (
                "GitHunkNext",
                -1,
                Err("warning:No more Git hunks to move to".to_string()),
            ),
            (
                "GitHunkNext",
                25,
                Err("warning:No more Git hunks to move to".to_string()),
            ),
            (
                "GitHunkPrevious",
                13,
                Err("warning:No more Git hunks to move to".to_string()),
            ),
            (
                "GitHunkStage",
                0,
                Err("warning:No Git hunk under cursor".to_string()),
            ),
            (
                "GitHunkUnstage",
                0,
                Err("warning:No Git hunk under cursor".to_string()),
            ),
            (
                "GitHunkReset",
                0,
                Err("warning:No Git hunk under cursor".to_string()),
            ),
        ] {
            runtime.execute_command(command).await.unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let result = loop {
                pump_process_events(&mut runtime).await.unwrap();
                let mut result = None;
                while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                    match request {
                        PluginRequest::GetWindows { request_id } => {
                            let buffer_path = if cursor_line < 0 {
                                String::new()
                            } else {
                                file.display().to_string()
                            };
                            runtime
                                .resolve_request(
                                    request_id,
                                    serde_json::json!({
                                        "windows": [{
                                            "buffer_path": buffer_path,
                                            "buffer_index": 7,
                                            "active": true
                                        }]
                                    }),
                                )
                                .await
                                .unwrap();
                        }
                        PluginRequest::GetSelection { request_id } => {
                            runtime
                                .resolve_request(request_id, serde_json::Value::Null)
                                .await
                                .unwrap();
                        }
                        PluginRequest::GetBufferText { request_id, .. } => {
                            runtime
                                .resolve_request(
                                    request_id,
                                    serde_json::json!({ "text": modified.clone() }),
                                )
                                .await
                                .unwrap();
                        }
                        PluginRequest::GetCursorPosition { request_id } => {
                            runtime
                                .resolve_request(
                                    request_id,
                                    serde_json::json!({ "x": 0, "y": cursor_line }),
                                )
                                .await
                                .unwrap();
                        }
                        PluginRequest::SetCursorPosition { x, y, jump } => {
                            assert!(jump);
                            result = Some(Ok((x, y)));
                        }
                        PluginRequest::Action(Action::PrintWarning(message)) => {
                            result = Some(Err(format!("warning:{message}")));
                        }
                        PluginRequest::Action(Action::Print(message)) => {
                            result = Some(Err(message));
                        }
                        _ => {}
                    }
                }
                if let Some(result) = result {
                    break result;
                }
                assert!(Instant::now() < deadline, "hunk action did not complete");
                tokio::time::sleep(Duration::from_millis(10)).await;
            };
            assert_eq!(result, expected);
        }
    }

    #[tokio::test]
    async fn project_search_streams_rg_matches_into_picker() {
        drain_requests();

        let mut runtime = Runtime::new_with_permissions(HashMap::from([(
            "project_search".to_string(),
            PluginPermissions {
                process: vec!["rg".to_string()],
            },
        )]));
        runtime
            .load_plugin(
                "project_search",
                include_str!("../../plugins/project_search.hk"),
            )
            .await
            .unwrap();

        let handle = open_project_search_picker(&mut runtime).await;

        let query = ["ProjectSearch", "State"].concat();
        runtime
            .notify_picker(handle, PickerCallback::Query(query.clone()))
            .unwrap();

        tokio::time::sleep(Duration::from_millis(120)).await;
        for callback in runtime.poll_timer_callbacks() {
            if let PluginRequest::TimeoutCallback { timer_id } = callback {
                runtime
                    .notify(
                        "timeout:callback",
                        serde_json::json!({ "timer_id": timer_id }),
                    )
                    .await
                    .unwrap();
            }
        }

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePickerItems { id, items } => {
                assert_eq!(id, handle.get());
                assert!(items.is_empty());
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePickerStatus { id, status } => {
                assert_eq!(id, handle.get());
                assert!(status
                    .as_deref()
                    .is_some_and(|status| status.starts_with("Searching (0/500)")));
            }
            _ => panic!("unexpected plugin request"),
        }

        let deadline = Instant::now() + Duration::from_secs(5);
        let item = loop {
            pump_process_events(&mut runtime).await.unwrap();
            for callback in runtime.poll_timer_callbacks() {
                if let PluginRequest::TimeoutCallback { timer_id } = callback {
                    runtime
                        .notify(
                            "timeout:callback",
                            serde_json::json!({ "timer_id": timer_id }),
                        )
                        .await
                        .unwrap();
                }
            }
            let mut found = None;
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                if let PluginRequest::UpdatePickerItems { id, items } = request {
                    assert_eq!(id, handle.get());
                    if let Some(item) = items.first() {
                        found = Some(item.clone());
                        break;
                    }
                }
            }
            if let Some(item) = found {
                break item;
            }
            assert!(
                Instant::now() < deadline,
                "project search did not produce a picker item"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };

        assert_eq!(item.label, "project_search.hk");
        assert!(item
            .annotation
            .as_deref()
            .is_some_and(|annotation| annotation.starts_with("plugins/:")));
        assert_eq!(item.kind.as_deref(), Some("FileMatch"));
        assert!(item
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains(&["ProjectSearch", "State"].concat())));

        drain_requests();
        runtime
            .notify_picker(
                handle,
                PickerCallback::Action {
                    action: "toggle_preview".to_string(),
                    item: Some(item.clone()),
                    query: query.clone(),
                },
            )
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePickerItems { id, items } => {
                assert_eq!(id, handle.get());
                assert!(items.iter().all(|item| item.preview.is_none()));
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePickerStatus { id, .. } => assert_eq!(id, handle.get()),
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .notify_picker(handle, PickerCallback::Selected(item))
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::SetPluginStorage {
                plugin, key, value, ..
            } => {
                assert_eq!(plugin, "project_search");
                assert_eq!(key, "history_by_cwd");
                assert_eq!(value, serde_json::json!({ ".": [query] }));
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ClosePicker { id } => assert_eq!(id, handle.get()),
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenLocation { location, target } => {
                assert_eq!(
                    PathBuf::from(location.path),
                    Path::new("plugins").join("project_search.hk")
                );
                assert_eq!(target, crate::plugin::OpenLocationTarget::Current);
            }
            _ => panic!("unexpected plugin request"),
        }
        assert!(runtime.picker_plugin(handle).is_none());
        assert!(!runtime
            .notify_picker(handle, PickerCallback::Query("stale".to_string()))
            .unwrap());
    }

    #[tokio::test]
    async fn session_restore_loads_matching_snapshot_and_saves_only_clean_buffers() {
        drain_requests();

        let snapshot = serde_json::json!({
            "version": 2,
            "cwd": "/tmp/project",
            "saved_at": 1,
            "buffers": [
                {
                    "index": 0,
                    "path": "src/main.rs",
                    "dirty": false,
                    "cursor": { "x": 0, "y": 0 },
                    "viewport_top": 0,
                },
                {
                    "index": 1,
                    "path": "scratch.rs",
                    "dirty": true,
                    "cursor": { "x": 0, "y": 0 },
                    "viewport_top": 0,
                }
            ],
            "current_buffer_index": 0,
            "window_layout": {
                "active_window_id": 0,
                "root": {
                    "kind": "window",
                    "buffer_index": 0,
                    "vtop": 0,
                    "vleft": 0,
                    "cx": 0,
                    "cy": 0,
                    "vx": 0,
                }
            },
            "panels": {
                "panels": [{
                    "id": "agent-conversation",
                    "kind": "text",
                    "visible": true,
                    "z_index": 0,
                    "side": "right"
                }],
                "focused": "agent-conversation"
            }
        });
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "session_restore",
                include_str!("../../plugins/session_restore.hk"),
            )
            .await
            .unwrap();

        runtime
            .notify("editor:ready", serde_json::json!({}))
            .await
            .unwrap();
        let resumed_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("startup_session_resumed"));
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        runtime
            .resolve_request(resumed_request_id, serde_json::json!({ "value": false }))
            .await
            .unwrap();
        let startup_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("startup_file_count"));
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        runtime
            .resolve_request(startup_request_id, serde_json::json!({ "value": 0 }))
            .await
            .unwrap();
        let storage_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage {
                plugin,
                key,
                request_id,
            } => {
                assert_eq!(plugin, "session_restore");
                assert_eq!(key, "latest");
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        runtime
            .resolve_request(
                storage_request_id,
                serde_json::json!({ "value": snapshot.clone() }),
            )
            .await
            .unwrap();
        let cwd_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("cwd"));
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        runtime
            .resolve_request(
                cwd_request_id,
                serde_json::json!({ "value": "/tmp/project" }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::RestoreEditorState {
                request_id,
                snapshot,
            } => {
                assert!(request_id.get() > 0);
                assert_eq!(snapshot.buffers.len(), 2);
                assert_eq!(snapshot.panels.panels.len(), 1);
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime.before_exit(snapshot).await.unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::SetPluginStorage {
                plugin, key, value, ..
            } => {
                assert_eq!(plugin, "session_restore");
                assert_eq!(key, "latest");
                assert_eq!(value["buffers"].as_array().unwrap().len(), 1);
                assert_eq!(value["buffers"][0]["path"], "src/main.rs");
                assert_eq!(value["panels"]["panels"][0]["id"], "agent-conversation");
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn session_restore_does_not_override_core_recovery() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "session_restore",
                include_str!("../../plugins/session_restore.hk"),
            )
            .await
            .unwrap();

        runtime
            .notify("editor:ready", serde_json::json!({}))
            .await
            .unwrap();
        let resumed_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("startup_session_resumed"));
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        runtime
            .resolve_request(resumed_request_id, serde_json::json!({ "value": true }))
            .await
            .unwrap();

        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn neotree_renders_a_panel_expands_directories_and_opens_files() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("neotree", include_str!("../../plugins/neotree.hk"))
            .await
            .unwrap();

        runtime.execute_command("NeoTree").await.unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::CreatePanel { id, config } => {
                assert_eq!(id, "neotree");
                assert_eq!(config.side, crate::plugin::PanelSide::Left);
                assert_eq!(config.width, 30);
                assert_eq!(
                    config.surface.as_ref().unwrap().background,
                    ["sideBar.background", "editor.background"]
                );
                assert_eq!(
                    config.border.as_ref().unwrap().foreground,
                    [
                        "sideBar.border",
                        "panel.border",
                        "editorLineNumber.foreground"
                    ]
                );
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePanel { id, rows } => {
                assert_eq!(id, "neotree");
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].id, "loading");
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::FocusPanel { id } => assert_eq!(id, "neotree"),
            _ => panic!("unexpected plugin request"),
        }
        let _cwd_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("cwd"));
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        let _windows_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetWindows { request_id } => request_id,
            _ => panic!("unexpected plugin request"),
        };
        let root_directory_request_id = loop {
            if let PluginRequest::ListDirectory { path, request_id } =
                ACTION_DISPATCHER.recv_request()
            {
                assert_eq!(path, ".");
                break request_id;
            }
        };
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetGitStatus { path, request_id } => {
                assert_eq!(path, ".");
                assert!(request_id.get() > 0);
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .resolve_request(
                root_directory_request_id,
                serde_json::json!({
                    "path": ".",
                    "entries": [
                        { "name": "src", "path": "./src", "kind": "directory" },
                        { "name": "Cargo.toml", "path": "./Cargo.toml", "kind": "file" }
                    ],
                    "error": null
                }),
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::WatchDirectory {
                path,
                watch_id,
                recursive,
                ..
            } => {
                assert_eq!(path, ".");
                assert_eq!(watch_id, 700);
                assert!(!recursive);
            }
            _ => panic!("unexpected plugin request"),
        }
        let root_rows = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePanel { id, rows } => {
                assert_eq!(id, "neotree");
                assert_eq!(rows.len(), 3);
                assert_eq!(rows[0].id, ".");
                assert_eq!(rows[1].id, "./src");
                assert_eq!(rows[2].id, "./Cargo.toml");
                rows
            }
            _ => panic!("unexpected plugin request"),
        };

        let directory_row = serde_json::to_value(&root_rows[1]).unwrap();
        runtime
            .notify(
                "panel:event:neotree",
                serde_json::json!({
                    "action": "activate",
                    "row": directory_row,
                }),
            )
            .await
            .unwrap();
        let src_directory_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ListDirectory { path, request_id } => {
                assert_eq!(path, "./src");
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePanel { id, rows } => {
                assert_eq!(id, "neotree");
                assert_eq!(rows.len(), 3);
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .resolve_request(
                src_directory_request_id,
                serde_json::json!({
                    "path": "./src",
                    "entries": [
                        { "name": "main.rs", "path": "./src/main.rs", "kind": "file" }
                    ],
                    "error": null
                }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::WatchDirectory {
                path,
                watch_id,
                recursive,
                ..
            } => {
                assert_eq!(path, "./src");
                assert_eq!(watch_id, 701);
                assert!(!recursive);
            }
            _ => panic!("unexpected plugin request"),
        }
        let expanded_rows = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePanel { id, rows } => {
                assert_eq!(id, "neotree");
                assert_eq!(rows.len(), 4);
                assert_eq!(rows[2].id, "./src/main.rs");
                rows
            }
            _ => panic!("unexpected plugin request"),
        };

        let file_row = serde_json::to_value(&expanded_rows[2]).unwrap();
        runtime
            .notify(
                "panel:event:neotree",
                serde_json::json!({
                    "action": "activate",
                    "row": file_row,
                }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenLocation { location, target } => {
                assert_eq!(location.path, "./src/main.rs");
                assert_eq!(target, crate::plugin::OpenLocationTarget::Current);
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UnwatchDirectory { watch_id } => assert_eq!(watch_id, 700),
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UnwatchDirectory { watch_id } => assert_eq!(watch_id, 701),
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ClosePanel { id } => assert_eq!(id, "neotree"),
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::FocusEditor => {}
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn neotree_recreates_a_restored_pane_without_stealing_focus() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("neotree", include_str!("../../plugins/neotree.hk"))
            .await
            .unwrap();

        runtime
            .notify(
                "editor:panes_restore",
                serde_json::json!({
                    "panels": [{ "id": "neotree", "visible": true }]
                }),
            )
            .await
            .unwrap();
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage {
                plugin,
                key,
                request_id,
            } => {
                assert_eq!(plugin, "neotree");
                assert_eq!(key, "pane_session");
                request_id
            }
            _ => panic!("expected Neo-tree storage request"),
        };
        runtime
            .resolve_request(
                request_id,
                serde_json::json!({
                    "value": { "expanded": [".", "src"], "selected": [] }
                }),
            )
            .await
            .unwrap();

        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::CreatePanel { id, .. } if id == "neotree"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePanel { id, .. } if id == "neotree"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::GetConfig { .. }
        ));
    }

    #[tokio::test]
    async fn neotree_emits_create_and_multi_item_delete_file_operations() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("neotree", include_str!("../../plugins/neotree.hk"))
            .await
            .unwrap();

        runtime
            .notify(
                "panel:event:neotree",
                serde_json::json!({
                    "action": "a",
                    "row": { "path": "./src", "kind": "directory" },
                }),
            )
            .await
            .unwrap();
        let create_handle = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackInput {
                owner,
                handle,
                title,
                initial,
            } => {
                assert_eq!(owner, "neotree");
                assert_eq!(title, "New file or directory (trailing /)");
                assert_eq!(initial, "");
                handle
            }
            _ => panic!("expected Neo-tree create prompt"),
        };
        runtime
            .notify_composer(
                create_handle,
                ComposerCallback::Submitted("generated/".to_string()),
            )
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::FileOperation {
                operation,
                request_id,
            } => {
                assert_eq!(
                    operation,
                    serde_json::json!({
                        "kind": "create_directory",
                        "path": "./src/generated",
                    })
                );
                assert!(request_id.get() > 0);
            }
            _ => panic!("expected Neo-tree create file operation"),
        }

        for path in ["./src/one.rs", "./src/two.rs"] {
            runtime
                .notify(
                    "panel:event:neotree",
                    serde_json::json!({
                        "action": "Tab",
                        "row": { "path": path, "kind": "file" },
                    }),
                )
                .await
                .unwrap();
        }
        runtime
            .notify(
                "panel:event:neotree",
                serde_json::json!({
                    "action": "d",
                    "row": { "path": "./src/two.rs", "kind": "file" },
                }),
            )
            .await
            .unwrap();
        let delete_handle = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackConfirmation {
                handle,
                title,
                message,
                owner,
                options,
            } => {
                assert_eq!(owner, "neotree");
                assert_eq!(title, "Confirm delete");
                assert_eq!(
                    message,
                    "Permanently delete selected items. This cannot be undone."
                );
                assert!(options.rows.is_empty());
                assert!(options.accept_label.is_none());
                handle
            }
            _ => panic!("expected Neo-tree delete confirmation"),
        };
        let accept = serde_json::from_value(serde_json::json!({
            "id": "accept",
            "label": "Accept",
        }))
        .unwrap();
        runtime
            .notify_picker(delete_handle, PickerCallback::Selected(accept))
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::FileOperation { operation, .. } => {
                assert_eq!(operation["kind"], "delete");
                assert_eq!(
                    operation["paths"],
                    serde_json::json!(["./src/one.rs", "./src/two.rs"])
                );
            }
            _ => panic!("expected Neo-tree delete file operation"),
        }
    }

    #[tokio::test]
    async fn neotree_selects_a_new_file_after_refreshing_its_parent() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("neotree", include_str!("../../plugins/neotree.hk"))
            .await
            .unwrap();

        runtime
            .notify(
                "panel:event:neotree",
                serde_json::json!({
                    "action": "a",
                    "row": { "path": "./src", "kind": "directory" },
                }),
            )
            .await
            .unwrap();
        let create_handle = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackInput { handle, .. } => handle,
            _ => panic!("expected Neo-tree create prompt"),
        };
        runtime
            .notify_composer(
                create_handle,
                ComposerCallback::Submitted("generated.rs".to_string()),
            )
            .unwrap();
        let operation_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::FileOperation {
                operation,
                request_id,
            } => {
                assert_eq!(
                    operation,
                    serde_json::json!({
                        "kind": "create",
                        "path": "./src/generated.rs",
                    })
                );
                request_id
            }
            _ => panic!("expected Neo-tree create file operation"),
        };

        runtime
            .resolve_request(
                operation_request_id,
                serde_json::json!({
                    "ok": true,
                    "error": null,
                    "undo_supported": false,
                    "created": ["src/generated.rs"],
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message == "Neo-tree create complete"
        ));
        let root_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ListDirectory { path, request_id } => {
                assert_eq!(path, ".");
                request_id
            }
            _ => panic!("expected Neo-tree root refresh"),
        };
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::GetGitStatus { path, .. } if path == "."
        ));

        runtime
            .resolve_request(
                root_request_id,
                serde_json::json!({
                    "path": ".",
                    "entries": [
                        { "name": "src", "path": "./src", "kind": "directory" },
                    ],
                    "error": null,
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::WatchDirectory { path, .. } if path == "."
        ));
        let src_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ListDirectory { path, request_id } => {
                assert_eq!(path, "./src");
                request_id
            }
            _ => panic!("expected created file parent refresh"),
        };

        runtime
            .resolve_request(
                src_request_id,
                serde_json::json!({
                    "path": "./src",
                    "entries": [
                        { "name": "generated.rs", "path": "./src/generated.rs", "kind": "file" },
                    ],
                    "error": null,
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::WatchDirectory { path, .. } if path == "./src"
        ));
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::SelectPanelRow { id, row_id } => {
                assert_eq!(id, "neotree");
                assert_eq!(row_id, "./src/generated.rs");
            }
            _ => panic!("expected Neo-tree to select the created file"),
        }
    }

    #[tokio::test]
    async fn neotree_handles_optional_selection_and_nullable_file_operation_errors() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("neotree", include_str!("../../plugins/neotree.hk"))
            .await
            .unwrap();

        runtime
            .notify(
                "panel:event:neotree",
                serde_json::json!({ "action": "a", "row": null }),
            )
            .await
            .unwrap();
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        for (error, expected) in [
            (
                serde_json::Value::Null,
                "Neo-tree create failed: unknown error",
            ),
            (
                serde_json::json!("permission denied"),
                "Neo-tree create failed: permission denied",
            ),
        ] {
            runtime
                .notify(
                    "panel:event:neotree",
                    serde_json::json!({
                        "action": "a",
                        "row": { "path": ".", "kind": "directory" },
                    }),
                )
                .await
                .unwrap();
            let handle = match ACTION_DISPATCHER.recv_request() {
                PluginRequest::OpenCallbackInput { handle, .. } => handle,
                _ => panic!("expected Neo-tree create input"),
            };
            runtime
                .notify_composer(handle, ComposerCallback::Submitted("new.rs".to_string()))
                .unwrap();
            let request_id = match ACTION_DISPATCHER.recv_request() {
                PluginRequest::FileOperation {
                    operation,
                    request_id,
                } => {
                    assert_eq!(operation["kind"], "create");
                    request_id
                }
                _ => panic!("expected typed Neo-tree create operation"),
            };
            runtime
                .resolve_request(
                    request_id,
                    serde_json::json!({ "ok": false, "error": error }),
                )
                .await
                .unwrap();
            assert!(matches!(
                ACTION_DISPATCHER.recv_request(),
                PluginRequest::Action(Action::Print(message)) if message == expected
            ));
        }
    }

    #[tokio::test]
    async fn neotree_reveals_the_active_file_and_renders_git_status() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("neotree", include_str!("../../plugins/neotree.hk"))
            .await
            .unwrap();

        runtime.execute_command("NeoTree").await.unwrap();
        let mut cwd_request_id = None;
        let mut windows_request_id = None;
        let mut git_status_request_id = None;
        for _ in 0..7 {
            match ACTION_DISPATCHER.recv_request() {
                PluginRequest::GetConfig { request_id, .. } => cwd_request_id = Some(request_id),
                PluginRequest::GetWindows { request_id } => windows_request_id = Some(request_id),
                PluginRequest::GetGitStatus { request_id, .. } => {
                    git_status_request_id = Some(request_id)
                }
                _ => {}
            }
        }

        runtime
            .resolve_request(
                cwd_request_id.expect("expected cwd request"),
                serde_json::json!({ "value": "/repo" }),
            )
            .await
            .unwrap();

        runtime
            .resolve_request(
                windows_request_id.expect("expected windows request"),
                serde_json::json!({
                    "windows": [{
                        "active": true,
                        "buffer_path": "/repo/src/main.rs",
                    }],
                }),
            )
            .await
            .unwrap();
        let root_directory_request_id = loop {
            if let PluginRequest::ListDirectory { path, request_id } =
                ACTION_DISPATCHER.recv_request()
            {
                assert_eq!(path, ".");
                break request_id;
            }
        };

        runtime
            .resolve_request(
                root_directory_request_id,
                serde_json::json!({
                    "path": ".",
                    "entries": [
                        { "name": "src", "path": "./src", "kind": "directory" },
                    ],
                    "error": null,
                }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::WatchDirectory { path, watch_id, .. } => {
                assert_eq!(path, ".");
                assert_eq!(watch_id, 700);
            }
            _ => panic!("unexpected plugin request"),
        }
        let src_directory_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ListDirectory { path, request_id } => {
                assert_eq!(path, "./src");
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePanel { id, rows } => {
                assert_eq!(id, "neotree");
                assert_eq!(rows.len(), 2);
                assert!(rows[1].expanded.unwrap_or(false));
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .resolve_request(
                src_directory_request_id,
                serde_json::json!({
                    "path": "./src",
                    "entries": [
                        { "name": "main.rs", "path": "./src/main.rs", "kind": "file" },
                    ],
                    "error": null,
                }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::WatchDirectory { path, watch_id, .. } => {
                assert_eq!(path, "./src");
                assert_eq!(watch_id, 701);
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePanel { id, rows } => {
                assert_eq!(id, "neotree");
                assert_eq!(rows[2].id, "./src/main.rs");
                assert_eq!(rows[1].segments[0].text, "  ");
                assert_eq!(rows[1].segments[1].text, "  ");
                assert_eq!(rows[2].segments[0].text, "  ");
                assert_eq!(rows[2].segments[1].text, "  ");
                assert_eq!(rows[2].segments[2].text, "└ ");
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::SelectPanelRow { id, row_id } => {
                assert_eq!(id, "neotree");
                assert_eq!(row_id, "./src/main.rs");
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .resolve_request(
                git_status_request_id.expect("expected git status request"),
                serde_json::json!({
                    "root": "/repo",
                    "statuses": [{
                        "path": "src/main.rs",
                        "absolute_path": "/repo/src/main.rs",
                        "status": "modified",
                    }],
                    "status_index": {
                        "/repo": "modified",
                        "/repo/src": "modified",
                        "/repo/src/main.rs": "modified",
                    },
                    "error": null,
                }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePanel { id, rows } => {
                assert_eq!(id, "neotree");
                assert_eq!(rows[2].right_segments[0].text, "");
                assert!(rows[2].right_segments[0].semantic.is_some());
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn neotree_renders_a_large_git_status_listing_within_the_instruction_budget() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("neotree", include_str!("../../plugins/neotree.hk"))
            .await
            .unwrap();
        runtime.execute_command("NeoTree").await.unwrap();

        let mut directory_request = None;
        let mut status_request = None;
        for _ in 0..7 {
            match ACTION_DISPATCHER.recv_request() {
                PluginRequest::ListDirectory { path, request_id } => {
                    assert_eq!(path, ".");
                    directory_request = Some(request_id);
                }
                PluginRequest::GetGitStatus { path, request_id } => {
                    assert_eq!(path, ".");
                    status_request = Some(request_id);
                }
                _ => {}
            }
        }

        let mut entries = (0..120)
            .map(|index| {
                serde_json::json!({
                    "name": format!("dir-{index:03}"),
                    "path": format!("./dir-{index:03}"),
                    "kind": "directory",
                })
            })
            .collect::<Vec<_>>();
        entries.push(serde_json::json!({
            "name": "tracked.rs",
            "path": "./tracked.rs",
            "kind": "file",
        }));
        runtime
            .resolve_request(
                directory_request.expect("expected root directory request"),
                serde_json::json!({ "path": ".", "entries": entries, "error": null }),
            )
            .await
            .unwrap();
        drain_requests();

        let mut statuses = Vec::new();
        for index in 0..120 {
            for (offset, status) in [
                "ignored",
                "untracked",
                "modified",
                "added",
                "deleted",
                "renamed",
                "conflict",
                "staged",
            ]
            .into_iter()
            .enumerate()
            {
                statuses.push(serde_json::json!({
                    "path": format!("dir-{index:03}/nested/file-{offset}.rs"),
                    "absolute_path": format!("/repo/dir-{index:03}/nested/file-{offset}.rs"),
                    "status": status,
                }));
            }
        }
        statuses.push(serde_json::json!({
            "path": "tracked.rs",
            "absolute_path": "/repo/tracked.rs",
            "status": "modified",
        }));
        let status_index = crate::editor::git_status_index(&statuses, "/repo");

        runtime
            .resolve_request(
                status_request.expect("expected git status request"),
                serde_json::json!({
                    "root": "/repo",
                    "statuses": statuses,
                    "status_index": status_index,
                    "error": null,
                }),
            )
            .await
            .unwrap();

        let rows = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePanel { id, rows } => {
                assert_eq!(id, "neotree");
                rows
            }
            _ => panic!("expected neotree panel update"),
        };
        assert_eq!(rows.len(), 122);
        assert!(rows[0].right_segments.is_empty());
        assert!(rows[1..121]
            .iter()
            .all(|row| row.right_segments[0].text == ""));
        assert_eq!(rows[121].right_segments[0].text, "");
    }

    #[tokio::test]
    async fn neotree_virtualizes_large_visible_listings_within_the_instruction_budget() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("neotree", include_str!("../../plugins/neotree.hk"))
            .await
            .unwrap();
        runtime.execute_command("NeoTree").await.unwrap();

        let mut directory_request = None;
        for _ in 0..7 {
            if let PluginRequest::ListDirectory { path, request_id } =
                ACTION_DISPATCHER.recv_request()
            {
                assert_eq!(path, ".");
                directory_request = Some(request_id);
            }
        }

        let entries = (0..1_000)
            .map(|index| {
                serde_json::json!({
                    "name": format!("file-{index:04}.rlib"),
                    "path": format!("./file-{index:04}.rlib"),
                    "kind": "file",
                })
            })
            .collect::<Vec<_>>();
        runtime
            .resolve_request(
                directory_request.expect("expected root directory request"),
                serde_json::json!({
                    "path": ".",
                    "entries": entries,
                    "truncated": false,
                    "error": null,
                }),
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::WatchDirectory { path, .. } => assert_eq!(path, "."),
            _ => panic!("expected neotree directory watch"),
        }
        let model = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdateTreePanel { id, model } => {
                assert_eq!(id, "neotree");
                model
            }
            _ => panic!("expected virtualized Neo-tree panel update"),
        };
        assert_eq!(model.len(), 1_001);
        assert_eq!(model.row(1_000).unwrap().id, "./file-0999.rlib");
    }

    #[tokio::test]
    async fn neotree_renders_git_status_for_a_filesystem_root_repository() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("neotree", include_str!("../../plugins/neotree.hk"))
            .await
            .unwrap();
        runtime.execute_command("NeoTree").await.unwrap();

        let mut directory_request = None;
        let mut status_request = None;
        for _ in 0..7 {
            match ACTION_DISPATCHER.recv_request() {
                PluginRequest::ListDirectory { path, request_id } => {
                    assert_eq!(path, ".");
                    directory_request = Some(request_id);
                }
                PluginRequest::GetGitStatus { path, request_id } => {
                    assert_eq!(path, ".");
                    status_request = Some(request_id);
                }
                _ => {}
            }
        }

        runtime
            .resolve_request(
                directory_request.expect("expected root directory request"),
                serde_json::json!({
                    "path": ".",
                    "entries": [{ "name": "src", "path": "./src", "kind": "directory" }],
                    "error": null,
                }),
            )
            .await
            .unwrap();
        drain_requests();

        let statuses = [serde_json::json!({
            "path": "src/main.rs",
            "absolute_path": "/src/main.rs",
            "status": "modified",
        })];
        let status_index = crate::editor::git_status_index(&statuses, "/");

        runtime
            .resolve_request(
                status_request.expect("expected git status request"),
                serde_json::json!({
                    "root": "/",
                    "statuses": statuses,
                    "status_index": status_index,
                    "error": null,
                }),
            )
            .await
            .unwrap();

        let rows = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePanel { id, rows } => {
                assert_eq!(id, "neotree");
                rows
            }
            _ => panic!("expected neotree panel update"),
        };
        assert_eq!(rows.len(), 2);
        assert!(rows[0].right_segments.is_empty());
        assert_eq!(rows[1].right_segments[0].text, "");
    }

    #[tokio::test]
    async fn theme_browser_previews_restores_and_sets_selected_theme() {
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "theme_browser",
                include_str!("../../plugins/theme_browser.hk"),
            )
            .await
            .unwrap();

        runtime.execute_command("ThemeBrowser").await.unwrap();

        let config_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("theme"));
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        let assets_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ListRuntimeAssets { kind, request_id } => {
                assert_eq!(kind, RuntimeAssetKind::Theme);
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };

        runtime
            .resolve_request(
                config_request_id,
                serde_json::json!({ "value": "custom.json" }),
            )
            .await
            .unwrap();
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        let listing = serde_json::json!({
            "kind": "themes",
            "entries": [
                {
                    "file": "mocha.json",
                    "name": "Mocha",
                    "source": "embedded",
                    "shadows": [],
                },
                {
                    "file": "custom.json",
                    "name": "Custom",
                    "source": "user",
                    "shadows": ["embedded"],
                },
                {
                    "file": "custom-dark.json",
                    "name": "Custom",
                    "source": "embedded",
                    "shadows": [],
                }
            ],
            "error": null,
        });
        runtime
            .resolve_request(assets_request_id, listing.clone())
            .await
            .unwrap();

        let (handle, items) = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker {
                owner,
                handle,
                title,
                items,
                options,
            } => {
                assert_eq!(owner, "theme_browser");
                assert_eq!(title.as_deref(), Some("Themes"));
                assert_eq!(options.initial_selection.as_deref(), Some("custom.json"));
                assert_eq!(options.presentation, PickerPresentation::Compact);
                assert_eq!(items[0].label, "Mocha");
                assert_eq!(items[0].kind.as_deref(), Some("Theme"));
                assert_eq!(items[1].label, "Custom");
                assert_eq!(items[2].label, "Custom");
                assert_eq!(items[1].annotation.as_deref(), Some("custom.json"));
                (handle, items)
            }
            _ => panic!("unexpected plugin request"),
        };

        runtime
            .notify_picker(handle, PickerCallback::Changed(items[0].clone()))
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::Action(Action::PreviewTheme(theme)) => {
                assert_eq!(theme, "mocha.json");
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .notify_picker(handle, PickerCallback::Cancelled)
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::Action(Action::PreviewTheme(theme)) => {
                assert_eq!(theme, "custom.json");
            }
            _ => panic!("unexpected plugin request"),
        }

        assert!(!runtime
            .notify_picker(handle, PickerCallback::Selected(items[1].clone()))
            .unwrap());
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        runtime.execute_command("ThemeBrowser").await.unwrap();
        let config_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, .. } => request_id,
            _ => panic!("unexpected plugin request"),
        };
        let assets_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ListRuntimeAssets { request_id, .. } => request_id,
            _ => panic!("unexpected plugin request"),
        };
        runtime
            .resolve_request(
                config_request_id,
                serde_json::json!({ "value": "custom.json" }),
            )
            .await
            .unwrap();
        runtime
            .resolve_request(assets_request_id, listing)
            .await
            .unwrap();
        let (handle, item) = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker {
                handle, mut items, ..
            } => (handle, items.remove(1)),
            _ => panic!("unexpected plugin request"),
        };
        runtime
            .notify_picker(handle, PickerCallback::Selected(item))
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::Action(Action::SetTheme(theme)) => {
                assert_eq!(theme, "custom.json");
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn callback_pickers_are_owner_isolated_and_cleaned_up() {
        drain_requests();

        let source = |command: &str, prefix: &str| {
            format!(
                r#"
                    pub fn activate() {{ red::add_command("{command}", open); }}
                    fn open() {{
                        red::execute("OpenPicker", "Items", [
                            PickerItem {{ id: "one", label: "One", data: Json {{}} }},
                        ], PickerOptions {{}}, PickerHandlers {{
                            selected: selected,
                        }});
                    }}
                    fn selected(item: PickerItem) {{
                        red::execute("Print", "{prefix}:" + item.id);
                    }}
                "#
            )
        };

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("first", &source("FirstPicker", "first"))
            .await
            .unwrap();
        runtime
            .load_plugin("second", &source("SecondPicker", "second"))
            .await
            .unwrap();

        runtime.execute_command("FirstPicker").await.unwrap();
        let (first_handle, first_item) = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker { handle, items, .. } => (handle, items[0].clone()),
            _ => panic!("expected first callback picker"),
        };
        runtime.execute_command("SecondPicker").await.unwrap();
        let (second_handle, second_item) = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker { handle, items, .. } => (handle, items[0].clone()),
            _ => panic!("expected second callback picker"),
        };
        assert_ne!(first_handle, second_handle);

        runtime
            .notify_picker(second_handle, PickerCallback::Selected(second_item))
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "second:one"
        ));
        runtime
            .notify_picker(first_handle, PickerCallback::Selected(first_item))
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "first:one"
        ));

        runtime.execute_command("FirstPicker").await.unwrap();
        let stale_handle = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker { handle, .. } => handle,
            _ => panic!("expected callback picker before unload"),
        };
        runtime.unload_plugin("first").unwrap();
        assert!(!runtime
            .notify_picker(stale_handle, PickerCallback::Cancelled)
            .unwrap());
    }

    #[tokio::test]
    async fn callback_composers_are_typed_one_shot_and_cleaned_up() {
        drain_requests();
        let source = r#"
            pub fn activate() { red::add_command("ScopedComposer", open); }
            fn open() {
                red::execute("OpenComposer", "Prompt", "draft", ["recent"], ComposerHandlers {
                    submitted: submitted,
                    cancelled: cancelled,
                });
            }
            fn submitted(prompt: String) { red::execute("Print", "submitted:" + prompt); }
            fn cancelled(event: ComposerCancelled) { red::execute("Print", "cancelled"); }
        "#;
        let mut runtime = Runtime::new();
        runtime.load_plugin("owner", source).await.unwrap();

        runtime.execute_command("ScopedComposer").await.unwrap();
        let submitted_handle = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackComposer {
                owner,
                handle,
                title,
                query,
                history,
            } => {
                assert_eq!(owner, "owner");
                assert_eq!(title.as_deref(), Some("Prompt"));
                assert_eq!(query, "draft");
                assert_eq!(history, ["recent"]);
                handle
            }
            _ => panic!("expected callback composer"),
        };
        assert!(runtime
            .notify_composer(
                submitted_handle,
                ComposerCallback::Submitted("exact".to_string()),
            )
            .unwrap());
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "submitted:exact"
        ));
        assert!(!runtime
            .notify_composer(submitted_handle, ComposerCallback::Cancelled)
            .unwrap());

        runtime.execute_command("ScopedComposer").await.unwrap();
        let cancelled_handle = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackComposer { handle, .. } => handle,
            _ => panic!("expected callback composer"),
        };
        assert!(runtime
            .notify_composer(cancelled_handle, ComposerCallback::Cancelled)
            .unwrap());
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "cancelled"
        ));

        runtime.execute_command("ScopedComposer").await.unwrap();
        let stale_handle = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackComposer { handle, .. } => handle,
            _ => panic!("expected callback composer"),
        };
        runtime.unload_plugin("owner").unwrap();
        assert!(!runtime
            .notify_composer(stale_handle, ComposerCallback::Cancelled)
            .unwrap());
    }

    #[tokio::test]
    async fn callback_picker_handles_from_an_old_plugin_generation_are_stale() {
        drain_requests();
        let source = r#"
            pub fn activate() { red::add_command("OpenGenerationPicker", open); }
            fn open() {
                red::execute("OpenPicker", "Items", [
                    PickerItem { id: "one", label: "One", data: Json {} },
                ], PickerOptions {}, PickerHandlers { cancelled: cancelled });
            }
            fn cancelled(event: PickerCancelled) {
                red::execute("Print", "cancelled");
            }
        "#;
        let mut runtime = Runtime::new();
        runtime.load_plugin("owner", source).await.unwrap();
        runtime
            .execute_command("OpenGenerationPicker")
            .await
            .unwrap();
        let stale_handle = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker { handle, .. } => handle,
            _ => panic!("expected callback picker"),
        };

        runtime.load_plugin("owner", source).await.unwrap();
        assert!(!runtime
            .notify_picker(stale_handle, PickerCallback::Cancelled)
            .unwrap());
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn callback_picker_rejects_non_function_handlers_before_publishing_dialog() {
        drain_requests();
        let mut runtime = Runtime::new();
        let error = runtime
            .load_plugin(
                "invalid-picker",
                r#"
                    pub fn activate() {
                        red::execute("OpenPicker", "Items", [
                            PickerItem { id: "one", label: "One", data: Json {} },
                        ], PickerOptions {}, PickerHandlers { selected: "not a callback" });
                    }
                "#,
            )
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("handler `selected` must be a function callback"));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn terminal_picker_handler_is_consumed_before_callback_failure() {
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "failing-picker",
                r#"
                    pub fn activate() { red::add_command("FailingPicker", open); }
                    fn open() {
                        red::execute("OpenPicker", "Items", [
                            PickerItem { id: "one", label: "One", data: Json {} },
                        ], PickerOptions {}, PickerHandlers { selected: selected });
                    }
                    fn selected(item: PickerItem) { let value = 1 / 0; }
                "#,
            )
            .await
            .unwrap();
        runtime.execute_command("FailingPicker").await.unwrap();
        let (handle, item) = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker { handle, items, .. } => (handle, items[0].clone()),
            _ => panic!("expected callback picker"),
        };

        assert!(runtime
            .notify_picker(handle, PickerCallback::Selected(item.clone()))
            .is_err());
        assert!(!runtime
            .notify_picker(handle, PickerCallback::Selected(item))
            .unwrap());
    }

    #[tokio::test]
    async fn lsp_symbols_annotations_preserve_all_command_discovery_metadata() {
        drain_requests();
        let mut runtime = Runtime::new();
        load_lsp_symbols(&mut runtime).await;

        let commands = runtime.registered_commands();
        assert_eq!(
            commands
                .iter()
                .map(|command| command.name.as_str())
                .collect::<Vec<_>>(),
            ["LspDocumentSymbols", "LspReferences", "LspWorkspaceSymbols"]
        );
        let document = &commands[0];
        assert_eq!(document.plugin, "lsp_symbols");
        assert_eq!(
            document.metadata.title.as_deref(),
            Some("Show document symbols")
        );
        assert_eq!(document.metadata.category.as_deref(), Some("LSP"));
        assert_eq!(
            document.metadata.description.as_deref(),
            Some("Find symbols in the current document")
        );
        assert_eq!(document.metadata.aliases, ["outline", "symbols"]);
        assert_eq!(commands[1].metadata.aliases, ["usages", "references"]);
        assert_eq!(commands[2].metadata.aliases, ["symbols"]);
        assert_eq!(document.metadata.scope, CommandScope::Global);
        assert_eq!(commands[1].metadata.scope, CommandScope::Editor);
        assert_eq!(commands[2].metadata.scope, CommandScope::Global);
    }

    #[tokio::test]
    async fn lsp_symbols_requests_document_symbols_and_opens_picker() {
        drain_requests();

        let mut runtime = Runtime::new();
        load_lsp_symbols(&mut runtime).await;

        runtime.execute_command("LspDocumentSymbols").await.unwrap();

        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::DocumentSymbols {
                request_id,
                buffer_index,
            } => {
                assert_eq!(buffer_index, None);
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };

        runtime
            .resolve_request(request_id, sample_symbol_payload())
            .await
            .unwrap();

        let handle = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker {
                owner,
                handle,
                title,
                items,
                ..
            } => {
                assert_eq!(owner, "lsp_symbols");
                assert_eq!(title.as_deref(), Some("Document Symbols"));
                assert_eq!(items[0].label, "main");
                assert_eq!(items[0].kind.as_deref(), Some("Function"));
                handle
            }
            _ => panic!("unexpected plugin request"),
        };
        assert_eq!(
            runtime.picker_plugin(handle).as_deref(),
            Some("lsp_symbols")
        );
    }

    #[tokio::test]
    async fn lsp_document_symbols_warns_when_no_symbols_are_available() {
        drain_requests();
        let mut runtime = Runtime::new();
        load_lsp_symbols(&mut runtime).await;

        runtime.execute_command("LspDocumentSymbols").await.unwrap();
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::DocumentSymbols { request_id, .. } => request_id,
            _ => panic!("expected document-symbol request"),
        };
        runtime
            .resolve_request(request_id, sample_symbol_payload_with_count(0))
            .await
            .unwrap();

        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::PrintWarning(message))
                if message == "No document symbols found"
        ));
    }

    #[tokio::test]
    async fn lsp_symbols_batches_pathological_document_symbol_results() {
        drain_requests();

        let mut runtime = Runtime::new();
        load_lsp_symbols(&mut runtime).await;

        runtime.execute_command("LspDocumentSymbols").await.unwrap();
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::DocumentSymbols { request_id, .. } => request_id,
            _ => panic!("expected document-symbol request"),
        };
        runtime
            .resolve_request(request_id, sample_symbol_payload_with_count(4_097))
            .await
            .unwrap();

        let first_handle = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker {
                handle,
                items,
                options,
                ..
            } => {
                assert!(items.is_empty());
                assert_eq!(options.status.as_deref(), Some("Loading 0/4097 symbols"));
                handle
            }
            _ => panic!("expected empty document-symbol picker"),
        };

        let mut final_items = Vec::new();
        let mut final_status = None;
        for _ in 0..80 {
            let callbacks = runtime.poll_timer_callbacks();
            assert!(!callbacks.is_empty(), "expected a pending symbol batch");
            for callback in callbacks {
                if let PluginRequest::TimeoutCallback { timer_id } = callback {
                    runtime
                        .notify(
                            "timeout:callback",
                            serde_json::json!({ "timer_id": timer_id }),
                        )
                        .await
                        .unwrap();
                }
            }
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                match request {
                    PluginRequest::UpdatePickerItems { id, items } => {
                        assert_eq!(id, first_handle.get());
                        final_items = items;
                    }
                    PluginRequest::UpdatePickerStatus { id, status } => {
                        assert_eq!(id, first_handle.get());
                        final_status = status;
                    }
                    _ => panic!("unexpected request while batching document symbols"),
                }
            }
            if final_items.len() == 4_096 {
                break;
            }
        }

        assert_eq!(final_items.len(), 4_096);
        assert_eq!(final_items[4_095].label, "symbol_4095");
        assert_eq!(
            final_status.as_deref(),
            Some("4096 symbols (results truncated)")
        );

        let timeout_count = runtime.pending_timeout_count();
        runtime.execute_command("LspDocumentSymbols").await.unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ClosePicker { id } => assert_eq!(id, first_handle.get()),
            _ => panic!("expected the previous document-symbol picker to close"),
        }
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::DocumentSymbols { request_id, .. } => request_id,
            _ => panic!("expected another document-symbol request"),
        };
        runtime
            .resolve_request(request_id, sample_symbol_payload_with_count(65))
            .await
            .unwrap();
        let second_handle = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker { handle, .. } => handle,
            _ => panic!("expected another document-symbol picker"),
        };
        assert_eq!(runtime.pending_timeout_count(), timeout_count + 1);

        runtime
            .notify_picker(second_handle, PickerCallback::Cancelled)
            .unwrap();
        assert_eq!(runtime.pending_timeout_count(), timeout_count);
        assert!(runtime.picker_plugin(second_handle).is_none());
        assert!(!runtime
            .notify_picker(second_handle, PickerCallback::Cancelled)
            .unwrap());
    }

    #[tokio::test]
    async fn lsp_references_waits_for_active_server_progress_before_requesting() {
        drain_requests();

        let mut runtime = Runtime::new();
        load_lsp_symbols(&mut runtime).await;
        notify_lsp_symbols_progress(&mut runtime, "begin").await;

        runtime.execute_command("LspReferences").await.unwrap();
        let handle = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker {
                handle,
                items,
                options,
                ..
            } => {
                assert!(items.is_empty());
                assert_eq!(
                    options.status.as_deref(),
                    Some("Waiting for language server...")
                );
                assert!(options.busy);
                handle
            }
            _ => panic!("expected waiting references picker"),
        };
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        notify_lsp_symbols_progress(&mut runtime, "end").await;
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerStatus { id, status: Some(status) }
                if id == handle.get() && status == "Fetching references..."
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerBusy { id, busy: true } if id == handle.get()
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::References {
                include_declaration: true,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn lsp_references_warns_after_a_final_empty_result() {
        drain_requests();
        let mut runtime = Runtime::new();
        load_lsp_symbols(&mut runtime).await;

        runtime.execute_command("LspReferences").await.unwrap();
        let handle = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker { handle, .. } => handle,
            _ => panic!("expected references loading picker"),
        };
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::References { request_id, .. } => request_id,
            _ => panic!("expected references request"),
        };
        runtime
            .resolve_request(request_id, sample_reference_payload_with_count(0))
            .await
            .unwrap();

        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::ClosePicker { id } if id == handle.get()
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::PrintWarning(message))
                if message == "No references found"
        ));
    }

    #[tokio::test]
    async fn lsp_references_retries_empty_and_timed_out_results_after_progress() {
        drain_requests();

        let mut runtime = Runtime::new();
        load_lsp_symbols(&mut runtime).await;

        runtime.execute_command("LspReferences").await.unwrap();
        let handle = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker { handle, .. } => handle,
            _ => panic!("expected references loading picker"),
        };
        let first_request = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::References { request_id, .. } => request_id,
            _ => panic!("expected references request"),
        };
        notify_lsp_symbols_progress(&mut runtime, "begin").await;
        runtime
            .resolve_request(first_request, sample_reference_payload_with_count(0))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerStatus { id, status: Some(status) }
                if id == handle.get() && status == "Waiting for language server..."
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerBusy { id, busy: true } if id == handle.get()
        ));

        notify_lsp_symbols_progress(&mut runtime, "end").await;
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerStatus { id, .. } if id == handle.get()
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerBusy { id, busy: true } if id == handle.get()
        ));
        let second_request = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::References { request_id, .. } => request_id,
            _ => panic!("expected retried references request"),
        };

        notify_lsp_symbols_progress(&mut runtime, "begin").await;
        runtime
            .resolve_request(
                second_request,
                serde_json::json!({
                    "ok": false,
                    "error": "LSP request timed out after 30s",
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerStatus { id, status: Some(status) }
                if id == handle.get() && status == "Waiting for language server..."
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerBusy { id, busy: true } if id == handle.get()
        ));

        notify_lsp_symbols_progress(&mut runtime, "end").await;
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerStatus { id, .. } if id == handle.get()
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerBusy { id, busy: true } if id == handle.get()
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::References { .. }
        ));
        assert!(runtime
            .notify_picker(handle, PickerCallback::Cancelled)
            .unwrap());
    }

    #[tokio::test]
    async fn lsp_symbols_batches_pathological_reference_results() {
        drain_requests();

        let mut runtime = Runtime::new();
        load_lsp_symbols(&mut runtime).await;

        runtime.execute_command("LspReferences").await.unwrap();
        let reference_handle = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker {
                handle,
                title,
                items,
                options,
                ..
            } => {
                assert_eq!(title.as_deref(), Some("References"));
                assert!(items.is_empty());
                assert_eq!(options.status.as_deref(), Some("Fetching references..."));
                assert!(options.busy);
                handle
            }
            _ => panic!("expected references loading picker"),
        };
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::References {
                request_id,
                include_declaration,
            } => {
                assert!(include_declaration);
                request_id
            }
            _ => panic!("expected references request"),
        };
        runtime
            .resolve_request(request_id, sample_reference_payload_with_count(4_097))
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePickerStatus { id, status } => {
                assert_eq!(id, reference_handle.get());
                assert_eq!(status.as_deref(), Some("Loading 0/4097 references"));
            }
            _ => panic!("expected references loading status"),
        }

        let mut final_items = Vec::new();
        let mut final_status = None;
        let mut busy = true;
        for _ in 0..80 {
            let callbacks = runtime.poll_timer_callbacks();
            assert!(!callbacks.is_empty(), "expected a pending reference batch");
            for callback in callbacks {
                if let PluginRequest::TimeoutCallback { timer_id } = callback {
                    runtime
                        .notify(
                            "timeout:callback",
                            serde_json::json!({ "timer_id": timer_id }),
                        )
                        .await
                        .unwrap();
                }
            }
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                match request {
                    PluginRequest::UpdatePickerItems { id, items } => {
                        assert_eq!(id, reference_handle.get());
                        final_items = items;
                    }
                    PluginRequest::UpdatePickerStatus { id, status } => {
                        assert_eq!(id, reference_handle.get());
                        final_status = status;
                    }
                    PluginRequest::UpdatePickerBusy {
                        id,
                        busy: next_busy,
                    } => {
                        assert_eq!(id, reference_handle.get());
                        busy = next_busy;
                    }
                    _ => panic!("unexpected request while batching references"),
                }
            }
            if final_items.len() == 4_096 {
                break;
            }
        }

        assert_eq!(final_items.len(), 4_096);
        assert_eq!(final_items[4_095].label, "src/reference_4095.rs");
        assert_eq!(
            final_status.as_deref(),
            Some("4096 references (results truncated)")
        );
        assert!(!busy);
        assert!(runtime.poll_timer_callbacks().is_empty());
        assert!(runtime
            .notify_picker(reference_handle, PickerCallback::Cancelled)
            .unwrap());
    }

    #[tokio::test]
    async fn lsp_symbols_workspace_query_updates_picker() {
        drain_requests();

        let mut runtime = Runtime::new();
        load_lsp_symbols(&mut runtime).await;
        let timeout_count = runtime.pending_timeout_count();

        runtime
            .execute_command("LspWorkspaceSymbols")
            .await
            .unwrap();

        let handle = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker {
                handle,
                title,
                options,
                ..
            } => {
                assert_eq!(title.as_deref(), Some("Workspace Symbols"));
                assert!(!options.external_filter);
                assert!(options.busy);
                assert_eq!(options.item_layout, crate::ui::PickerItemLayout::LabelFirst);
                handle
            }
            _ => panic!("unexpected plugin request"),
        };
        let initial_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::WorkspaceSymbols { request_id, query } => {
                assert_eq!(query, "");
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };

        runtime
            .notify_picker(handle, PickerCallback::Query("mai".to_string()))
            .unwrap();
        runtime
            .notify_picker(handle, PickerCallback::Query("main".to_string()))
            .unwrap();
        assert_eq!(runtime.pending_timeout_count(), timeout_count + 1);
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        runtime
            .resolve_request(initial_request_id, sample_symbol_payload_with_count(2))
            .await
            .unwrap();
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        tokio::time::sleep(Duration::from_millis(120)).await;
        let callbacks = runtime.poll_timer_callbacks();
        assert_eq!(callbacks.len(), 1);
        let PluginRequest::TimeoutCallback { timer_id } = &callbacks[0] else {
            panic!("expected workspace-symbol debounce timeout");
        };
        runtime
            .notify(
                "timeout:callback",
                serde_json::json!({ "timer_id": timer_id }),
            )
            .await
            .unwrap();

        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerBusy { id, busy: true } if id == handle.get()
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerStatus { id, status }
                if id == handle.get() && status.as_deref() == Some("Searching workspace symbols")
        ));

        let query_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::WorkspaceSymbols { request_id, query } => {
                assert_eq!(query, "main");
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };

        runtime
            .resolve_request(query_request_id, sample_symbol_payload())
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePickerItems { id, items } => {
                assert_eq!(id, handle.get());
                assert_eq!(items[0].label, "main");
                assert_eq!(items[0].kind.as_deref(), Some("Function"));
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePickerStatus { id, status } => {
                assert_eq!(id, handle.get());
                assert_eq!(status.as_deref(), Some("1 symbols"));
            }
            _ => panic!("unexpected plugin request"),
        }
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerBusy { id, busy: false } if id == handle.get()
        ));

        runtime
            .notify_picker(handle, PickerCallback::Query("later".to_string()))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(120)).await;
        let callbacks = runtime.poll_timer_callbacks();
        assert_eq!(callbacks.len(), 1);
        let PluginRequest::TimeoutCallback { timer_id } = &callbacks[0] else {
            panic!("expected workspace-symbol debounce timeout");
        };
        runtime
            .notify(
                "timeout:callback",
                serde_json::json!({ "timer_id": timer_id }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerBusy { id, busy: true } if id == handle.get()
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerStatus { id, status }
                if id == handle.get() && status.as_deref() == Some("Searching workspace symbols")
        ));
        let late_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::WorkspaceSymbols { request_id, query } => {
                assert_eq!(query, "later");
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        runtime
            .notify_picker(handle, PickerCallback::Cancelled)
            .unwrap();
        runtime
            .resolve_request(late_request_id, sample_symbol_payload())
            .await
            .unwrap();
        assert!(runtime.picker_plugin(handle).is_none());
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn lsp_symbols_workspace_batch_keeps_previous_items_until_replacement_arrives() {
        drain_requests();

        let mut runtime = Runtime::new();
        load_lsp_symbols(&mut runtime).await;
        runtime
            .execute_command("LspWorkspaceSymbols")
            .await
            .unwrap();

        let handle = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker { handle, .. } => handle,
            _ => panic!("expected workspace-symbol picker"),
        };
        let initial_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::WorkspaceSymbols { request_id, .. } => request_id,
            _ => panic!("expected initial workspace-symbol request"),
        };
        runtime
            .resolve_request(initial_request_id, sample_symbol_payload())
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerItems { id, ref items }
                if id == handle.get() && items.len() == 1
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerStatus { id, .. } if id == handle.get()
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerBusy { id, busy: false } if id == handle.get()
        ));

        runtime
            .notify_picker(handle, PickerCallback::Query("symbol".to_string()))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(120)).await;
        let callbacks = runtime.poll_timer_callbacks();
        assert_eq!(callbacks.len(), 1);
        let PluginRequest::TimeoutCallback { timer_id } = &callbacks[0] else {
            panic!("expected workspace-symbol debounce timeout");
        };
        runtime
            .notify(
                "timeout:callback",
                serde_json::json!({ "timer_id": timer_id }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerBusy { id, busy: true } if id == handle.get()
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerStatus { id, .. } if id == handle.get()
        ));
        let query_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::WorkspaceSymbols { request_id, query } => {
                assert_eq!(query, "symbol");
                request_id
            }
            _ => panic!("expected debounced workspace-symbol request"),
        };

        runtime
            .resolve_request(query_request_id, sample_symbol_payload_with_count(65))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerStatus { id, ref status }
                if id == handle.get() && status.as_deref() == Some("Loading 0/65 symbols")
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        for expected_count in [64, 65] {
            let callbacks = runtime.poll_timer_callbacks();
            assert_eq!(callbacks.len(), 1);
            let PluginRequest::TimeoutCallback { timer_id } = &callbacks[0] else {
                panic!("expected workspace-symbol batch timeout");
            };
            runtime
                .notify(
                    "timeout:callback",
                    serde_json::json!({ "timer_id": timer_id }),
                )
                .await
                .unwrap();
            assert!(matches!(
                ACTION_DISPATCHER.recv_request(),
                PluginRequest::UpdatePickerItems { id, ref items }
                    if id == handle.get() && items.len() == expected_count
            ));
            assert!(matches!(
                ACTION_DISPATCHER.recv_request(),
                PluginRequest::UpdatePickerStatus { id, .. } if id == handle.get()
            ));
        }
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerBusy { id, busy: false } if id == handle.get()
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn lsp_symbols_picker_selection_opens_symbol_location() {
        drain_requests();

        let mut runtime = Runtime::new();
        load_lsp_symbols(&mut runtime).await;
        runtime.execute_command("LspDocumentSymbols").await.unwrap();
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::DocumentSymbols { request_id, .. } => request_id,
            _ => panic!("unexpected plugin request"),
        };
        runtime
            .resolve_request(request_id, sample_symbol_payload())
            .await
            .unwrap();
        let (handle, item) = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker { handle, items, .. } => (handle, items[0].clone()),
            _ => panic!("unexpected plugin request"),
        };

        runtime
            .notify_picker(handle, PickerCallback::Selected(item))
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenLocation { location, target } => {
                assert_eq!(location.path, "src/main.rs");
                assert_eq!(location.line, 4);
                assert_eq!(location.column, 3);
                assert_eq!(
                    location.column_encoding,
                    crate::plugin::LocationColumnEncoding::Utf16
                );
                assert_eq!(target, crate::plugin::OpenLocationTarget::Current);
            }
            _ => panic!("unexpected plugin request"),
        }
        assert!(runtime.picker_plugin(handle).is_none());
    }

    #[tokio::test]
    async fn lsp_symbols_reference_picker_ignores_replaced_request_and_opens_selection() {
        drain_requests();

        let mut runtime = Runtime::new();
        load_lsp_symbols(&mut runtime).await;

        runtime.execute_command("LspReferences").await.unwrap();
        let stale_handle = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker { handle, .. } => handle,
            _ => panic!("expected references loading picker"),
        };
        let stale_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::References {
                request_id,
                include_declaration,
            } => {
                assert!(include_declaration);
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        runtime.execute_command("LspReferences").await.unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ClosePicker { id } => assert_eq!(id, stale_handle.get()),
            _ => panic!("expected stale references picker to close"),
        }
        let handle = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenCallbackPicker { handle, .. } => handle,
            _ => panic!("expected replacement references loading picker"),
        };
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::References { request_id, .. } => request_id,
            _ => panic!("unexpected plugin request"),
        };

        let payload = serde_json::json!({
            "ok": true,
            "file": "src/main.rs",
            "position": { "line": 4, "character": 3 },
            "references": [
                {
                    "file": "src/main.rs",
                    "range": {
                        "start": { "line": 4, "character": 3 },
                        "end": { "line": 4, "character": 7 }
                    }
                },
                {
                    "file": "src/lib.rs",
                    "range": {
                        "start": { "line": 8, "character": 2 },
                        "end": { "line": 8, "character": 6 }
                    }
                },
                {
                    "file": "tests/example.rs",
                    "range": {
                        "start": { "line": 12, "character": 1 },
                        "end": { "line": 12, "character": 5 }
                    }
                }
            ]
        });
        runtime
            .resolve_request(stale_request_id, payload.clone())
            .await
            .unwrap();
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        runtime.resolve_request(request_id, payload).await.unwrap();

        let item = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePickerItems { id, items } => {
                assert_eq!(id, handle.get());
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].label, "src/lib.rs");
                items[0].clone()
            }
            _ => panic!("expected reference picker items"),
        };
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePickerStatus { id, status } => {
                assert_eq!(id, handle.get());
                assert_eq!(status.as_deref(), Some("2 references"));
            }
            _ => panic!("expected reference count"),
        }
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdatePickerBusy { id, busy: false } if id == handle.get()
        ));
        runtime
            .notify_picker(handle, PickerCallback::Selected(item))
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenLocation { location, target } => {
                assert_eq!(location.path, "src/lib.rs");
                assert_eq!(location.line, 8);
                assert_eq!(location.column, 2);
                assert_eq!(
                    location.column_encoding,
                    crate::plugin::LocationColumnEncoding::Utf16
                );
                assert_eq!(target, crate::plugin::OpenLocationTarget::Current);
            }
            _ => panic!("unexpected plugin request"),
        }
        assert!(runtime.picker_plugin(handle).is_none());
    }
}
