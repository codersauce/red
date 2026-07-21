//! Core type system representations for Husk.
//!
//! This crate defines the internal type language used by the type checker.

use std::{collections::HashSet, fmt};

pub use semver::Version;

/// Primitive types built into Husk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrimitiveType {
    I32,
    I64,
    F64,
    Bool,
    String,
    Unit,
}

/// Identifier for a type variable used during type checking / inference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeVarId(pub u32);

/// A type in Husk's core type language.
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    /// A primitive type such as `i32`, `bool`, `String`, or `()`.
    Primitive(PrimitiveType),
    /// A named, possibly generic type such as `Option<T>` or `Result<T, E>`.
    Named {
        /// The name of the type constructor (e.g., "Option", "Result", "MyType").
        name: String,
        /// Type arguments applied to the constructor.
        args: Vec<Type>,
    },
    /// A function type: `(T1, T2, ...) -> R`.
    Function { params: Vec<Type>, ret: Box<Type> },
    /// A type variable introduced during type checking.
    Var(TypeVarId),
    /// An array type: `[T]`.
    Array(Box<Type>),
    /// A tuple type: `(T1, T2, ...)`.
    Tuple(Vec<Type>),
    /// An impl Trait type: `impl Iterator<T>` - used for return types
    /// Stores the underlying trait type for resolution
    ImplTrait {
        /// The trait type (e.g., `Iterator<i32>`)
        trait_ty: Box<Type>,
    },
}

impl Type {
    pub fn i32() -> Self {
        Type::Primitive(PrimitiveType::I32)
    }

    pub fn i64() -> Self {
        Type::Primitive(PrimitiveType::I64)
    }

    pub fn f64() -> Self {
        Type::Primitive(PrimitiveType::F64)
    }

    pub fn bool() -> Self {
        Type::Primitive(PrimitiveType::Bool)
    }

    pub fn string() -> Self {
        Type::Primitive(PrimitiveType::String)
    }

    pub fn unit() -> Self {
        Type::Primitive(PrimitiveType::Unit)
    }

    pub fn named(name: impl Into<String>, args: Vec<Type>) -> Self {
        Type::Named {
            name: name.into(),
            args,
        }
    }

    pub fn function(params: Vec<Type>, ret: Type) -> Self {
        Type::Function {
            params,
            ret: Box::new(ret),
        }
    }

    pub fn tuple(elements: Vec<Type>) -> Self {
        Type::Tuple(elements)
    }

    pub fn impl_trait(trait_ty: Type) -> Self {
        Type::ImplTrait {
            trait_ty: Box::new(trait_ty),
        }
    }
}

/// A validated root module name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleName(String);

impl ModuleName {
    /// Validate and construct a module name.
    ///
    /// Module names use Husk identifiers so they can appear as the first
    /// segment of a qualified call.
    pub fn new(name: impl Into<String>) -> Result<Self, DescriptorError> {
        let name = name.into();
        validate_identifier(&name, "module")?;
        Ok(Self(name))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ModuleName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Public type language used by module signatures.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeDescriptor {
    Unit,
    Bool,
    I32,
    I64,
    F64,
    String,
    Json,
    List(Box<TypeDescriptor>),
    Tuple(Vec<TypeDescriptor>),
    Option(Box<TypeDescriptor>),
    Result {
        ok: Box<TypeDescriptor>,
        error: Box<TypeDescriptor>,
    },
    /// An extension resource transferred into or out of a call.
    OwnResource(String),
    /// An extension resource borrowed for the duration of a call.
    BorrowResource(String),
    /// A nominal type declared by this module or a future module graph.
    Named(String),
}

impl TypeDescriptor {
    #[must_use]
    pub fn list(element: Self) -> Self {
        Self::List(Box::new(element))
    }

    #[must_use]
    pub fn option(element: Self) -> Self {
        Self::Option(Box::new(element))
    }

    #[must_use]
    pub fn result(ok: Self, error: Self) -> Self {
        Self::Result {
            ok: Box::new(ok),
            error: Box::new(error),
        }
    }

    fn validate(&self) -> Result<(), DescriptorError> {
        match self {
            Self::Named(name) | Self::OwnResource(name) | Self::BorrowResource(name) => {
                validate_identifier(name, "type")
            }
            Self::List(element) | Self::Option(element) => element.validate(),
            Self::Tuple(elements) => {
                for element in elements {
                    element.validate()?;
                }
                Ok(())
            }
            Self::Result { ok, error } => {
                ok.validate()?;
                error.validate()
            }
            Self::Unit
            | Self::Bool
            | Self::I32
            | Self::I64
            | Self::F64
            | Self::String
            | Self::Json => Ok(()),
        }
    }

    fn canonical(&self, output: &mut String) {
        match self {
            Self::Unit => output.push_str("unit"),
            Self::Bool => output.push_str("bool"),
            Self::I32 => output.push_str("i32"),
            Self::I64 => output.push_str("i64"),
            Self::F64 => output.push_str("f64"),
            Self::String => output.push_str("string"),
            Self::Json => output.push_str("json"),
            Self::List(element) => {
                output.push_str("list<");
                element.canonical(output);
                output.push('>');
            }
            Self::Tuple(elements) => {
                output.push_str("tuple<");
                for (index, element) in elements.iter().enumerate() {
                    if index > 0 {
                        output.push(',');
                    }
                    element.canonical(output);
                }
                output.push('>');
            }
            Self::Option(element) => {
                output.push_str("option<");
                element.canonical(output);
                output.push('>');
            }
            Self::Result { ok, error } => {
                output.push_str("result<");
                ok.canonical(output);
                output.push(',');
                error.canonical(output);
                output.push('>');
            }
            Self::OwnResource(name) => push_canonical_string(output, "own-resource", name),
            Self::BorrowResource(name) => push_canonical_string(output, "borrow-resource", name),
            Self::Named(name) => push_canonical_string(output, "named", name),
        }
    }
}

/// One field in an external record definition.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldDescriptor {
    pub name: String,
    pub ty: TypeDescriptor,
}

impl FieldDescriptor {
    pub fn new(name: impl Into<String>, ty: TypeDescriptor) -> Result<Self, DescriptorError> {
        let field = Self {
            name: name.into(),
            ty,
        };
        validate_identifier(&field.name, "field")?;
        field.ty.validate()?;
        Ok(field)
    }

    fn canonical(&self, output: &mut String) {
        push_canonical_string(output, "field", &self.name);
        self.ty.canonical(output);
    }
}

/// One case in an external variant definition.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VariantCaseDescriptor {
    pub name: String,
    pub payload: Option<TypeDescriptor>,
}

impl VariantCaseDescriptor {
    pub fn new(
        name: impl Into<String>,
        payload: Option<TypeDescriptor>,
    ) -> Result<Self, DescriptorError> {
        let case = Self {
            name: name.into(),
            payload,
        };
        validate_identifier(&case.name, "variant case")?;
        if let Some(payload) = &case.payload {
            payload.validate()?;
        }
        Ok(case)
    }

    fn canonical(&self, output: &mut String) {
        push_canonical_string(output, "case", &self.name);
        match &self.payload {
            Some(payload) => {
                output.push('1');
                payload.canonical(output);
            }
            None => output.push('0'),
        }
    }
}

/// The shape behind one named type exported by a host module.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeDefinitionKind {
    Alias(TypeDescriptor),
    Record(Vec<FieldDescriptor>),
    Enum(Vec<String>),
    Variant(Vec<VariantCaseDescriptor>),
    Resource,
}

/// A named external type made visible while checking module calls.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypeDefinitionDescriptor {
    pub name: String,
    pub kind: TypeDefinitionKind,
}

impl TypeDefinitionDescriptor {
    pub fn new(name: impl Into<String>, kind: TypeDefinitionKind) -> Result<Self, DescriptorError> {
        let definition = Self {
            name: name.into(),
            kind,
        };
        definition.validate()?;
        Ok(definition)
    }

    fn validate(&self) -> Result<(), DescriptorError> {
        validate_identifier(&self.name, "type")?;
        match &self.kind {
            TypeDefinitionKind::Alias(ty) => ty.validate(),
            TypeDefinitionKind::Record(fields) => {
                validate_named_set("field", fields.iter().map(|field| field.name.as_str()))?;
                for field in fields {
                    validate_identifier(&field.name, "field")?;
                    field.ty.validate()?;
                }
                Ok(())
            }
            TypeDefinitionKind::Enum(cases) => {
                validate_named_set("enum case", cases.iter().map(String::as_str))?;
                for case in cases {
                    validate_identifier(case, "enum case")?;
                }
                Ok(())
            }
            TypeDefinitionKind::Variant(cases) => {
                validate_named_set("variant case", cases.iter().map(|case| case.name.as_str()))?;
                for case in cases {
                    validate_identifier(&case.name, "variant case")?;
                    if let Some(payload) = &case.payload {
                        payload.validate()?;
                    }
                }
                Ok(())
            }
            TypeDefinitionKind::Resource => Ok(()),
        }
    }

    fn canonical(&self, output: &mut String) {
        push_canonical_string(output, "type", &self.name);
        match &self.kind {
            TypeDefinitionKind::Alias(ty) => {
                output.push_str("alias");
                ty.canonical(output);
            }
            TypeDefinitionKind::Record(fields) => {
                output.push_str("record");
                for field in fields {
                    field.canonical(output);
                }
            }
            TypeDefinitionKind::Enum(cases) => {
                output.push_str("enum");
                for case in cases {
                    push_canonical_string(output, "case", case);
                }
            }
            TypeDefinitionKind::Variant(cases) => {
                output.push_str("variant");
                for case in cases {
                    case.canonical(output);
                }
            }
            TypeDefinitionKind::Resource => output.push_str("resource"),
        }
    }
}

/// One named function parameter.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParameterDescriptor {
    pub name: String,
    pub ty: TypeDescriptor,
}

impl ParameterDescriptor {
    pub fn new(name: impl Into<String>, ty: TypeDescriptor) -> Result<Self, DescriptorError> {
        let name = name.into();
        validate_identifier(&name, "parameter")?;
        ty.validate()?;
        Ok(Self { name, ty })
    }
}

/// A callable exposed by a module or interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionDescriptor {
    pub name: String,
    pub parameters: Vec<ParameterDescriptor>,
    pub result: TypeDescriptor,
    pub documentation: Option<String>,
}

impl FunctionDescriptor {
    pub fn new(
        name: impl Into<String>,
        parameters: Vec<ParameterDescriptor>,
        result: TypeDescriptor,
    ) -> Result<Self, DescriptorError> {
        let descriptor = Self {
            name: name.into(),
            parameters,
            result,
            documentation: None,
        };
        descriptor.validate()?;
        Ok(descriptor)
    }

    fn validate(&self) -> Result<(), DescriptorError> {
        validate_identifier(&self.name, "function")?;
        self.result.validate()?;
        let mut names = HashSet::new();
        for parameter in &self.parameters {
            validate_identifier(&parameter.name, "parameter")?;
            parameter.ty.validate()?;
            if !names.insert(parameter.name.as_str()) {
                return Err(DescriptorError::DuplicateName {
                    kind: "parameter",
                    name: parameter.name.clone(),
                });
            }
        }
        Ok(())
    }

    fn canonical(&self, output: &mut String) {
        push_canonical_string(output, "function", &self.name);
        output.push('(');
        for parameter in &self.parameters {
            push_canonical_string(output, "parameter", &parameter.name);
            parameter.ty.canonical(output);
        }
        output.push(')');
        self.result.canonical(output);
        push_optional_canonical_string(output, "docs", self.documentation.as_deref());
    }
}

/// A nested namespace such as a WIT exported interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceDescriptor {
    pub name: String,
    pub types: Vec<TypeDefinitionDescriptor>,
    pub functions: Vec<FunctionDescriptor>,
    pub documentation: Option<String>,
}

impl InterfaceDescriptor {
    pub fn new(
        name: impl Into<String>,
        functions: Vec<FunctionDescriptor>,
    ) -> Result<Self, DescriptorError> {
        let descriptor = Self {
            name: name.into(),
            types: Vec::new(),
            functions,
            documentation: None,
        };
        descriptor.validate()?;
        Ok(descriptor)
    }

    pub fn with_types(
        mut self,
        types: Vec<TypeDefinitionDescriptor>,
    ) -> Result<Self, DescriptorError> {
        self.types = types;
        self.validate()?;
        Ok(self)
    }

    fn validate(&self) -> Result<(), DescriptorError> {
        validate_identifier(&self.name, "interface")?;
        validate_function_set(&self.functions)?;
        validate_type_set(&self.types)
    }

    fn canonical(&self, output: &mut String) {
        push_canonical_string(output, "interface", &self.name);
        let mut functions = self.functions.iter().collect::<Vec<_>>();
        functions.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        let mut types = self.types.iter().collect::<Vec<_>>();
        types.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        for ty in types {
            ty.canonical(output);
        }
        for function in functions {
            function.canonical(output);
        }
        push_optional_canonical_string(output, "docs", self.documentation.as_deref());
    }
}

/// One immutable module signature shared by semantic analysis and dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleDescriptor {
    pub name: ModuleName,
    pub version: Version,
    pub types: Vec<TypeDefinitionDescriptor>,
    pub functions: Vec<FunctionDescriptor>,
    pub interfaces: Vec<InterfaceDescriptor>,
    pub documentation: Option<String>,
}

impl ModuleDescriptor {
    pub fn new(
        name: impl Into<String>,
        version: Version,
        functions: Vec<FunctionDescriptor>,
        interfaces: Vec<InterfaceDescriptor>,
    ) -> Result<Self, DescriptorError> {
        let descriptor = Self {
            name: ModuleName::new(name)?,
            version,
            types: Vec::new(),
            functions,
            interfaces,
            documentation: None,
        };
        descriptor.validate()?;
        Ok(descriptor)
    }

    pub fn with_types(
        mut self,
        types: Vec<TypeDefinitionDescriptor>,
    ) -> Result<Self, DescriptorError> {
        self.types = types;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), DescriptorError> {
        validate_function_set(&self.functions)?;
        validate_type_set(&self.types)?;
        let mut type_names = self
            .types
            .iter()
            .map(|ty| ty.name.as_str())
            .collect::<HashSet<_>>();
        let root_functions = self
            .functions
            .iter()
            .map(|function| function.name.as_str())
            .collect::<HashSet<_>>();
        let mut interfaces = HashSet::new();
        for interface in &self.interfaces {
            interface.validate()?;
            for ty in &interface.types {
                if !type_names.insert(ty.name.as_str()) {
                    return Err(DescriptorError::DuplicateName {
                        kind: "module type",
                        name: ty.name.clone(),
                    });
                }
            }
            if root_functions.contains(interface.name.as_str())
                || !interfaces.insert(interface.name.as_str())
            {
                return Err(DescriptorError::DuplicateName {
                    kind: "module member",
                    name: interface.name.clone(),
                });
            }
        }
        Ok(())
    }

    /// Stable descriptor identity independent of vector insertion order.
    ///
    /// This hash detects signature drift and cache mismatches. Artifact
    /// integrity uses a separate cryptographic digest at the package boundary.
    #[must_use]
    pub fn stable_hash(&self) -> DescriptorHash {
        let mut canonical = String::new();
        push_canonical_string(&mut canonical, "module", self.name.as_str());
        push_canonical_string(&mut canonical, "version", &self.version.to_string());
        let mut functions = self.functions.iter().collect::<Vec<_>>();
        functions.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        let mut types = self.types.iter().collect::<Vec<_>>();
        types.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        for ty in types {
            ty.canonical(&mut canonical);
        }
        for function in functions {
            function.canonical(&mut canonical);
        }
        let mut interfaces = self.interfaces.iter().collect::<Vec<_>>();
        interfaces.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        for interface in interfaces {
            interface.canonical(&mut canonical);
        }
        push_optional_canonical_string(&mut canonical, "docs", self.documentation.as_deref());

        let mut bytes = [0_u8; 32];
        for (index, seed) in [
            0xcbf2_9ce4_8422_2325,
            0x8422_2325_cbf2_9ce4,
            0x9e37_79b9_7f4a_7c15,
            0x517c_c1b7_2722_0a95,
        ]
        .into_iter()
        .enumerate()
        {
            let hash = stable_fnv64(canonical.as_bytes(), seed);
            bytes[index * 8..(index + 1) * 8].copy_from_slice(&hash.to_be_bytes());
        }
        DescriptorHash(bytes)
    }
}

fn validate_type_set(types: &[TypeDefinitionDescriptor]) -> Result<(), DescriptorError> {
    validate_named_set("type", types.iter().map(|ty| ty.name.as_str()))?;
    for ty in types {
        ty.validate()?;
    }
    Ok(())
}

fn validate_named_set<'a>(
    kind: &'static str,
    names: impl IntoIterator<Item = &'a str>,
) -> Result<(), DescriptorError> {
    let mut seen = HashSet::new();
    for name in names {
        if !seen.insert(name) {
            return Err(DescriptorError::DuplicateName {
                kind,
                name: name.to_string(),
            });
        }
    }
    Ok(())
}

fn validate_function_set(functions: &[FunctionDescriptor]) -> Result<(), DescriptorError> {
    let mut names = HashSet::new();
    for function in functions {
        function.validate()?;
        if !names.insert(function.name.as_str()) {
            return Err(DescriptorError::DuplicateName {
                kind: "function",
                name: function.name.clone(),
            });
        }
    }
    Ok(())
}

fn validate_identifier(name: &str, kind: &'static str) -> Result<(), DescriptorError> {
    let mut characters = name.chars();
    let valid_start = characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic());
    let valid_rest = valid_start
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric());
    if valid_rest {
        Ok(())
    } else {
        Err(DescriptorError::InvalidIdentifier {
            kind,
            name: name.to_string(),
        })
    }
}

fn push_canonical_string(output: &mut String, field: &str, value: &str) {
    output.push_str(field);
    output.push(':');
    output.push_str(&value.len().to_string());
    output.push(':');
    output.push_str(value);
    output.push(';');
}

fn push_optional_canonical_string(output: &mut String, field: &str, value: Option<&str>) {
    match value {
        Some(value) => push_canonical_string(output, field, value),
        None => {
            output.push_str(field);
            output.push_str(":none;");
        }
    }
}

fn stable_fnv64(bytes: &[u8], seed: u64) -> u64 {
    bytes.iter().fold(seed, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Deterministic module signature hash.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DescriptorHash([u8; 32]);

impl DescriptorHash {
    #[must_use]
    pub fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Debug for DescriptorHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for DescriptorHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Invalid or ambiguous module signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DescriptorError {
    InvalidIdentifier { kind: &'static str, name: String },
    DuplicateName { kind: &'static str, name: String },
}

impl fmt::Display for DescriptorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentifier { kind, name } => {
                write!(formatter, "invalid Husk {kind} identifier `{name}`")
            }
            Self::DuplicateName { kind, name } => {
                write!(formatter, "duplicate Husk {kind} name `{name}`")
            }
        }
    }
}

impl std::error::Error for DescriptorError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives_constructors_work() {
        assert_eq!(Type::i32(), Type::Primitive(PrimitiveType::I32));
        assert_eq!(Type::i64(), Type::Primitive(PrimitiveType::I64));
        assert_eq!(Type::bool(), Type::Primitive(PrimitiveType::Bool));
        assert_eq!(Type::string(), Type::Primitive(PrimitiveType::String));
        assert_eq!(Type::unit(), Type::Primitive(PrimitiveType::Unit));
    }

    #[test]
    fn named_type_with_args() {
        let t = Type::named("Option", vec![Type::string()]);
        match t {
            Type::Named { name, args } => {
                assert_eq!(name, "Option");
                assert_eq!(args.len(), 1);
                assert_eq!(args[0], Type::string());
            }
            _ => panic!("expected named type"),
        }
    }

    #[test]
    fn function_type() {
        let t = Type::function(vec![Type::i32(), Type::bool()], Type::string());
        match t {
            Type::Function { params, ret } => {
                assert_eq!(params.len(), 2);
                assert_eq!(params[0], Type::i32());
                assert_eq!(params[1], Type::bool());
                assert_eq!(*ret, Type::string());
            }
            _ => panic!("expected function type"),
        }
    }

    #[test]
    fn tuple_type() {
        let t = Type::tuple(vec![Type::i32(), Type::string(), Type::bool()]);
        match t {
            Type::Tuple(elements) => {
                assert_eq!(elements.len(), 3);
                assert_eq!(elements[0], Type::i32());
                assert_eq!(elements[1], Type::string());
                assert_eq!(elements[2], Type::bool());
            }
            _ => panic!("expected tuple type"),
        }
    }

    #[test]
    fn impl_trait_type() {
        let inner = Type::named("Iterator", vec![Type::i32()]);
        let t = Type::impl_trait(inner.clone());
        match t {
            Type::ImplTrait { trait_ty } => {
                assert_eq!(*trait_ty, inner);
            }
            _ => panic!("expected impl trait type"),
        }
    }

    fn add_function() -> FunctionDescriptor {
        FunctionDescriptor::new(
            "add",
            vec![
                ParameterDescriptor::new("left", TypeDescriptor::I64).unwrap(),
                ParameterDescriptor::new("right", TypeDescriptor::I64).unwrap(),
            ],
            TypeDescriptor::I64,
        )
        .unwrap()
    }

    #[test]
    fn module_descriptors_validate_identifiers_and_duplicates() {
        let duplicate = ModuleDescriptor::new(
            "sample",
            Version::new(1, 0, 0),
            vec![add_function(), add_function()],
            Vec::new(),
        )
        .unwrap_err();
        assert_eq!(
            duplicate,
            DescriptorError::DuplicateName {
                kind: "function",
                name: "add".to_string(),
            }
        );

        let invalid = ModuleName::new("not-valid").unwrap_err();
        assert_eq!(
            invalid,
            DescriptorError::InvalidIdentifier {
                kind: "module",
                name: "not-valid".to_string(),
            }
        );
    }

    #[test]
    fn descriptor_hash_is_stable_across_member_order() {
        let subtract = FunctionDescriptor::new(
            "subtract",
            vec![
                ParameterDescriptor::new("left", TypeDescriptor::I64).unwrap(),
                ParameterDescriptor::new("right", TypeDescriptor::I64).unwrap(),
            ],
            TypeDescriptor::I64,
        )
        .unwrap();
        let left = ModuleDescriptor::new(
            "sample",
            Version::new(1, 2, 3),
            vec![add_function(), subtract.clone()],
            Vec::new(),
        )
        .unwrap();
        let right = ModuleDescriptor::new(
            "sample",
            Version::new(1, 2, 3),
            vec![subtract, add_function()],
            Vec::new(),
        )
        .unwrap();

        assert_eq!(left.stable_hash(), right.stable_hash());
        assert_eq!(left.stable_hash().to_string().len(), 64);
    }
}
