//! Core AST definitions for the language.

use std::ops::Range;
use std::sync::Arc;

// ============================================================================
// Attributes
// ============================================================================

/// An attribute like `#[getter]` or `#[js_name = "innerHTML"]` or `#[cfg(test)]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    /// The attribute name (e.g., "getter", "setter", "js_name", "cfg", "test")
    pub name: Ident,
    /// Optional value for key-value attributes (e.g., "innerHTML" for `#[js_name = "innerHTML"]`)
    pub value: Option<String>,
    /// Optional cfg predicate for `#[cfg(...)]` attributes
    pub cfg_predicate: Option<CfgPredicate>,
    pub span: Span,
}

// ============================================================================
// Conditional Compilation (cfg)
// ============================================================================

/// A predicate for conditional compilation, used in `#[cfg(...)]` attributes.
/// Supports simple flags, key-value pairs, and boolean combinators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CfgPredicate {
    /// Simple flag: `#[cfg(test)]`, `#[cfg(debug)]`
    Flag(String),
    /// Key-value pair: `#[cfg(target = "esm")]`
    KeyValue { key: String, value: String },
    /// All predicates must be true: `#[cfg(all(test, debug))]`
    All(Vec<CfgPredicate>),
    /// Any predicate must be true: `#[cfg(any(node, bun))]`
    Any(Vec<CfgPredicate>),
    /// Negation: `#[cfg(not(test))]`
    Not(Box<CfgPredicate>),
}

/// A span in the source file, represented as a byte range.
/// Optionally includes the file path for multi-file error reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    /// The file path this span belongs to. None means the "current" or "main" file.
    pub file: Option<Arc<str>>,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self {
            range: start..end,
            file: None,
        }
    }

    /// Create a span with an associated file path.
    pub fn with_file(start: usize, end: usize, file: Arc<str>) -> Self {
        Self {
            range: start..end,
            file: Some(file),
        }
    }

    /// Set the file path on this span, returning a new span.
    pub fn in_file(self, file: Arc<str>) -> Self {
        Self {
            range: self.range,
            file: Some(file),
        }
    }

    /// Get the file path, if any.
    pub fn file_path(&self) -> Option<&str> {
        self.file.as_deref()
    }
}

/// An identifier with its source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

/// Literal values.
#[derive(Debug, Clone, PartialEq)]
pub enum LiteralKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Literal {
    pub kind: LiteralKind,
    pub span: Span,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Not, // !
    Neg, // -
}

/// Binary operators (MVP subset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod, // % modulo/remainder
    Eq,
    NotEq,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
}

/// Assignment operators for compound assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Assign,    // =
    AddAssign, // +=
    SubAssign, // -=
    ModAssign, // %=
}

/// Expressions.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Literal(Literal),
    Ident(Ident),
    /// Path-like expression, e.g. `Enum::Variant`.
    /// For the MVP this is primarily used for enum constructors.
    Path {
        segments: Vec<Ident>,
    },
    Call {
        callee: Box<Expr>,
        /// Turbofish type arguments: `foo::<i32, String>(x)`.
        /// Empty if no explicit type arguments provided.
        type_args: Vec<TypeExpr>,
        args: Vec<Expr>,
    },
    Field {
        base: Box<Expr>,
        member: Ident,
    },
    MethodCall {
        receiver: Box<Expr>,
        method: Ident,
        /// Turbofish type arguments: `x.parse::<i32>()`.
        /// Empty if no explicit type arguments provided.
        type_args: Vec<TypeExpr>,
        args: Vec<Expr>,
    },
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// If expression: `if cond { ... } else { ... }`
    /// Always requires an `else` branch so the expression has a value.
    If {
        cond: Box<Expr>,
        then_branch: Box<Expr>,
        else_branch: Box<Expr>,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    Block(Block),
    /// Struct instantiation expression, e.g. `Point { x: 1, y: 2 }`.
    Struct {
        /// The struct type name (possibly a path like `module::Type`).
        name: Vec<Ident>,
        /// Field initializers.
        fields: Vec<FieldInit>,
    },
    /// println/print-style formatted output: `println("Value: {}", x)` or `print("Value: {}", x)`
    FormatPrint {
        /// The parsed format string with placeholders
        format: FormatString,
        /// Arguments to substitute into placeholders
        args: Vec<Expr>,
        /// Whether to append a newline (true for println, false for print)
        newline: bool,
    },
    /// format!-style string formatting: `format("Value: {}", x)` -> String
    Format {
        /// The parsed format string with placeholders
        format: FormatString,
        /// Arguments to substitute into placeholders
        args: Vec<Expr>,
    },
    /// Closure expression: `|x, y| x + y` or `|x: i32| -> i32 { x + 1 }`
    Closure {
        params: Vec<ClosureParam>,
        ret_type: Option<TypeExpr>,
        body: Box<Expr>,
    },
    /// Array literal expression: `[1, 2, 3]` or `[]`
    Array {
        elements: Vec<Expr>,
    },
    /// Array index expression: `array[index]`
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    /// Range expression: `start..end`, `start..=end`, `start..`, `..end`, or `..`
    /// When used as slice syntax in index expressions, start/end can be omitted.
    Range {
        start: Option<Box<Expr>>,
        end: Option<Box<Expr>>,
        inclusive: bool,
    },
    /// Assignment expression: `target = value` or `target += value` etc.
    /// Returns the assigned value.
    Assign {
        target: Box<Expr>,
        op: AssignOp,
        value: Box<Expr>,
    },
    /// Embedded JavaScript literal: `js { console.log("hi"); }`
    /// The code field contains the raw JavaScript between the braces,
    /// with leading/trailing whitespace trimmed but internal structure preserved.
    JsLiteral {
        /// The raw JavaScript code (content between braces, trimmed)
        code: String,
    },
    /// Type cast expression: `expr as Type`
    Cast {
        expr: Box<Expr>,
        target_ty: TypeExpr,
    },
    /// Tuple literal expression: `(1, "hello", true)` or `(a, b)`
    Tuple {
        elements: Vec<Expr>,
    },
    /// Tuple field access: `tuple.0`, `tuple.1`, etc.
    TupleField {
        base: Box<Expr>,
        index: usize,
    },
    /// Try expression: `expr?`
    /// For Result<T, E>: returns early with Err(e) if Err, otherwise unwraps Ok(t) to t.
    /// For Option<T>: returns early with None if None, otherwise unwraps Some(t) to t.
    Try {
        expr: Box<Expr>,
    },
}

// ============================================================================
// Format String Types (for println! style formatting)
// ============================================================================

/// A parsed format string with placeholders.
#[derive(Debug, Clone, PartialEq)]
pub struct FormatString {
    /// The original string literal span
    pub span: Span,
    /// Parsed segments (alternating literal and placeholder)
    pub segments: Vec<FormatSegment>,
}

/// A segment of a format string - either literal text or a placeholder.
#[derive(Debug, Clone, PartialEq)]
pub enum FormatSegment {
    /// Literal text (between placeholders)
    Literal(String),
    /// A format placeholder like {} or {:x}
    Placeholder(FormatPlaceholder),
}

/// A format placeholder like `{}`, `{0}`, `{name}`, or `{:x}`.
#[derive(Debug, Clone, PartialEq)]
pub struct FormatPlaceholder {
    /// Position in argument list (None = next sequential, Some(n) = explicit position)
    pub position: Option<usize>,
    /// Named argument identifier (e.g., "name" for {name})
    pub name: Option<String>,
    /// Format specifier details
    pub spec: FormatSpec,
    /// Span of this placeholder in the source
    pub span: Span,
}

/// Format specifier details like width, precision, alignment, etc.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FormatSpec {
    /// Fill character for padding (default: space)
    pub fill: Option<char>,
    /// Alignment: '<' left, '>' right, '^' center
    pub align: Option<char>,
    /// Show sign for positive numbers (+)
    pub sign: bool,
    /// Alternate form (#) - adds 0x, 0b, 0o prefixes or pretty-prints debug
    pub alternate: bool,
    /// Zero-pad (0)
    pub zero_pad: bool,
    /// Minimum width
    pub width: Option<usize>,
    /// Precision for floats / max length for strings
    pub precision: Option<usize>,
    /// Type specifier: None (display), '?' (debug), 'x'/'X' (hex), 'b' (binary), 'o' (octal)
    pub ty: Option<char>,
}

/// A field initializer in a struct expression.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldInit {
    pub name: Ident,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

/// A block of statements delimited by `{}`.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

/// A pattern used in `match` expressions (MVP subset).
#[derive(Debug, Clone, PartialEq)]
pub enum PatternKind {
    Wildcard,       // _
    Binding(Ident), // x
    EnumUnit {
        path: Vec<Ident>, // e.g., Message::Quit -> [Message, Quit]
    },
    EnumTuple {
        path: Vec<Ident>,
        fields: Vec<Pattern>,
    },
    EnumStruct {
        path: Vec<Ident>,
        fields: Vec<(Ident, Pattern)>,
    },
    /// Tuple pattern: `(x, y, z)` for destructuring tuples
    Tuple {
        fields: Vec<Pattern>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
}

/// A single `match` arm.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub expr: Expr,
}

/// Statements.
#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    Let {
        mutable: bool,
        pattern: Pattern,
        ty: Option<TypeExpr>,
        value: Option<Expr>,
        else_block: Option<Block>, // for let...else syntax
    },
    /// Assignment statement: `target = value` or `target += value` etc.
    Assign {
        target: Expr,
        op: AssignOp,
        value: Expr,
    },
    Expr(Expr),
    Semi(Expr),
    Return {
        value: Option<Expr>,
    },
    If {
        cond: Expr,
        then_branch: Block,
        else_branch: Option<Box<Stmt>>, // usually another If or a Block stmt
    },
    While {
        cond: Expr,
        body: Block,
    },
    /// Infinite loop: `loop { body }`
    /// Can only be exited via `break` or `return`.
    Loop {
        body: Block,
    },
    /// For-in loop: `for item in collection { body }`
    ForIn {
        binding: Ident,
        iterable: Expr,
        body: Block,
    },
    Break,
    Continue,
    Block(Block),
    /// if let pattern = expr { then } [else { else }]
    IfLet {
        pattern: Pattern,
        scrutinee: Expr,
        then_branch: Block,
        else_branch: Option<Box<Stmt>>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

/// Simple type expressions for the MVP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeExprKind {
    Named(Ident),
    Generic {
        name: Ident,
        args: Vec<TypeExpr>,
    },
    /// Function type: `fn(T, U) -> V` or `fn() -> T`
    Function {
        params: Vec<TypeExpr>,
        ret: Box<TypeExpr>,
    },
    /// Array type: `[ElementType]`
    Array(Box<TypeExpr>),
    /// Tuple type: `(T1, T2, T3)`
    Tuple(Vec<TypeExpr>),
    /// Impl Trait type: `impl Iterator<T>` - used for return types
    ImplTrait {
        /// The trait type expression (e.g., `Iterator<i32>`)
        trait_ty: Box<TypeExpr>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeExpr {
    pub kind: TypeExprKind,
    pub span: Span,
}

/// Function parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// Attributes on the parameter, e.g., `#[this]` for explicit this binding.
    pub attributes: Vec<Attribute>,
    pub name: Ident,
    pub ty: TypeExpr,
}

/// Closure parameter (type annotation is optional for inference).
#[derive(Debug, Clone, PartialEq)]
pub struct ClosureParam {
    pub name: Ident,
    pub ty: Option<TypeExpr>,
}

/// What kind of use import this is.
#[derive(Debug, Clone, PartialEq)]
pub enum UseKind {
    /// Import the item at the path: `use crate::foo::Bar;`
    Item,
    /// Glob import all variants: `use Option::*;`
    Glob,
    /// Import specific variants: `use Result::{Ok, Err};` or `use Result::Ok;`
    Variants(Vec<Ident>),
}

/// Item-level definitions.
#[derive(Debug, Clone, PartialEq)]
pub enum ItemKind {
    Fn {
        name: Ident,
        type_params: Vec<TypeParam>,
        params: Vec<Param>,
        ret_type: Option<TypeExpr>,
        body: Vec<Stmt>,
    },
    Struct {
        name: Ident,
        type_params: Vec<Ident>,
        fields: Vec<StructField>,
    },
    Enum {
        name: Ident,
        type_params: Vec<Ident>,
        variants: Vec<EnumVariant>,
    },
    TypeAlias {
        name: Ident,
        ty: TypeExpr,
    },
    ExternBlock {
        abi: String,
        items: Vec<ExternItem>,
    },
    Use {
        /// Path like `crate::foo::bar` or `Option` (for variant imports)
        path: Vec<Ident>,
        /// What kind of import this is
        kind: UseKind,
    },
    /// Trait definition: `trait Name { fn method(&self); }`
    Trait(TraitDef),
    /// Implementation block: `impl Trait for Type { ... }` or `impl Type { ... }`
    Impl(ImplBlock),
}

// ============================================================================
// Trait and Impl AST Nodes
// ============================================================================

/// A trait definition: `trait Name: SuperTrait { fn method(&self); }`
#[derive(Debug, Clone, PartialEq)]
pub struct TraitDef {
    pub name: Ident,
    pub type_params: Vec<TypeParam>,
    /// Supertraits that this trait requires (e.g., `Eq: PartialEq` means PartialEq is a supertrait)
    pub supertraits: Vec<TypeExpr>,
    pub items: Vec<TraitItem>,
    pub span: Span,
}

/// A type parameter with optional trait bounds: `T` or `T: Foo + Bar`
#[derive(Debug, Clone, PartialEq)]
pub struct TypeParam {
    pub name: Ident,
    pub bounds: Vec<TypeExpr>, // Trait bounds
}

/// An item inside a trait definition.
#[derive(Debug, Clone, PartialEq)]
pub struct TraitItem {
    pub kind: TraitItemKind,
    pub span: Span,
}

/// Kinds of items that can appear in a trait.
#[derive(Debug, Clone, PartialEq)]
pub enum TraitItemKind {
    /// A method signature (possibly with default implementation)
    Method(TraitMethod),
}

/// A method in a trait definition.
#[derive(Debug, Clone, PartialEq)]
pub struct TraitMethod {
    /// Attributes on this method (e.g., #[js_name = "..."])
    pub attributes: Vec<Attribute>,
    pub name: Ident,
    pub receiver: Option<SelfReceiver>,
    pub params: Vec<Param>,
    pub ret_type: Option<TypeExpr>,
    /// Default implementation body (None = required method)
    pub default_body: Option<Vec<Stmt>>,
    /// If true, this is an `extern "js" fn` declaration (no body, direct JS call)
    pub is_extern: bool,
}

impl TraitMethod {
    /// Returns the JS name if specified via #[js_name = "..."], otherwise None.
    pub fn js_name(&self) -> Option<&str> {
        self.attributes
            .iter()
            .find(|a| a.name.name == "js_name")
            .and_then(|a| a.value.as_deref())
    }
}

/// The self receiver in a method: `self`, `&self`, or `&mut self`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfReceiver {
    /// `self` - by value
    Value,
    /// `&self` - immutable reference
    Ref,
    /// `&mut self` - mutable reference
    RefMut,
}

/// An impl block: `impl Trait for Type { ... }` or `impl Type { ... }`
#[derive(Debug, Clone, PartialEq)]
pub struct ImplBlock {
    pub type_params: Vec<TypeParam>,
    /// The trait being implemented (None for inherent impl)
    pub trait_ref: Option<TypeExpr>,
    /// The type this impl is for
    pub self_ty: TypeExpr,
    pub items: Vec<ImplItem>,
    pub span: Span,
}

/// An item inside an impl block.
#[derive(Debug, Clone, PartialEq)]
pub struct ImplItem {
    pub kind: ImplItemKind,
    pub span: Span,
}

/// Kinds of items that can appear in an impl block.
#[derive(Debug, Clone, PartialEq)]
pub enum ImplItemKind {
    /// A method implementation
    Method(ImplMethod),
    /// An extern property declaration (e.g., `#[getter] extern "js" body: JsValue;`)
    Property(ExternProperty),
}

/// An extern property declaration in an impl block.
/// Example: `#[getter] #[setter] extern "js" text_content: String;`
#[derive(Debug, Clone, PartialEq)]
pub struct ExternProperty {
    /// Attributes on this property (e.g., #[getter], #[setter], #[js_name = "..."])
    pub attributes: Vec<Attribute>,
    /// The property name in Husk (e.g., "text_content")
    pub name: Ident,
    /// The property type
    pub ty: TypeExpr,
    pub span: Span,
}

impl ExternProperty {
    /// Returns true if the property has a #[getter] attribute.
    pub fn has_getter(&self) -> bool {
        self.attributes.iter().any(|a| a.name.name == "getter")
    }

    /// Returns true if the property has a #[setter] attribute.
    pub fn has_setter(&self) -> bool {
        self.attributes.iter().any(|a| a.name.name == "setter")
    }

    /// Returns the JS name if specified via #[js_name = "..."], otherwise None.
    pub fn js_name(&self) -> Option<&str> {
        self.attributes
            .iter()
            .find(|a| a.name.name == "js_name")
            .and_then(|a| a.value.as_deref())
    }
}

/// A method in an impl block.
#[derive(Debug, Clone, PartialEq)]
pub struct ImplMethod {
    /// Attributes on this method (e.g., #[js_name = "..."])
    pub attributes: Vec<Attribute>,
    pub name: Ident,
    pub receiver: Option<SelfReceiver>,
    pub params: Vec<Param>,
    pub ret_type: Option<TypeExpr>,
    pub body: Vec<Stmt>,
    /// If true, this is an `extern "js" fn` declaration (no body, direct JS call)
    pub is_extern: bool,
}

impl ImplMethod {
    /// Returns the JS name if specified via #[js_name = "..."], otherwise None.
    pub fn js_name(&self) -> Option<&str> {
        self.attributes
            .iter()
            .find(|a| a.name.name == "js_name")
            .and_then(|a| a.value.as_deref())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructField {
    pub name: Ident,
    pub ty: TypeExpr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumVariant {
    pub name: Ident,
    pub fields: EnumVariantFields,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EnumVariantFields {
    /// Tuple-like variant: Variant(Type1, Type2, ...)
    Tuple(Vec<TypeExpr>),
    /// Struct-like variant: Variant { field: Type, ... }
    Struct(Vec<StructField>),
    /// Unit variant: Variant
    Unit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// Attributes on this item (e.g., #[test], #[cfg(test)], #[ignore])
    pub attributes: Vec<Attribute>,
    pub visibility: Visibility,
    pub kind: ItemKind,
    pub span: Span,
}

impl Item {
    /// Returns the #[cfg(...)] predicate if this item has one.
    pub fn cfg_predicate(&self) -> Option<&CfgPredicate> {
        self.attributes
            .iter()
            .find(|a| a.name.name == "cfg")
            .and_then(|a| a.cfg_predicate.as_ref())
    }

    /// Returns true if this item has a #[test] attribute.
    pub fn is_test(&self) -> bool {
        self.attributes.iter().any(|a| a.name.name == "test")
    }

    /// Returns true if this item has a #[ignore] attribute.
    pub fn is_ignored(&self) -> bool {
        self.attributes.iter().any(|a| a.name.name == "ignore")
    }

    /// Returns true if this item has a #[should_panic] attribute.
    pub fn should_panic(&self) -> bool {
        self.attributes
            .iter()
            .any(|a| a.name.name == "should_panic")
    }

    /// Returns the expected panic message if #[should_panic(expected = "...")] is present.
    pub fn expected_panic_message(&self) -> Option<&str> {
        self.attributes
            .iter()
            .find(|a| a.name.name == "should_panic")
            .and_then(|a| a.value.as_deref())
    }

    /// Returns true if this item (usually an enum) has an #[untagged] attribute.
    /// Untagged enums serialize without a tag field, matching TypeScript's untagged unions.
    pub fn is_untagged(&self) -> bool {
        self.attributes.iter().any(|a| a.name.name == "untagged")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Private,
}

/// Items that may appear inside an `extern` block.
#[derive(Debug, Clone, PartialEq)]
pub enum ExternItemKind {
    /// Bare function declaration: `fn foo() -> i32;`
    /// Generates globalThis.foo() calls.
    Fn {
        name: Ident,
        params: Vec<Param>,
        ret_type: Option<TypeExpr>,
    },
    /// Module import declaration: `mod express;` or `mod "package-name" as alias;`
    /// This declares a dependency on a JS module by package name.
    /// When `items` is empty, imports the default export.
    /// When `items` has functions, generates named imports for those functions.
    /// Use `mod global Array { ... }` for JavaScript builtins that don't need require/import.
    Mod {
        /// The npm package name (e.g., "express", "@scope/pkg", "lodash-es")
        package: String,
        /// The identifier to use in Husk code (derived from package or explicit alias)
        binding: Ident,
        /// Optional nested function declarations to import from this module.
        /// If non-empty, generates `import { fn1, fn2 } from "package";`
        items: Vec<ModItem>,
        /// If true, this is a JavaScript global (e.g., Array, Math, JSON) that
        /// doesn't need to be imported/required.
        is_global: bool,
    },
    /// Extern struct declaration: `struct JsValue;`
    /// Declares an opaque JavaScript type. No constructor is generated.
    /// Methods on extern structs use direct JS method calls.
    Struct {
        name: Ident,
        /// Optional type parameters: `struct JsArray<T>;`
        type_params: Vec<Ident>,
    },
    /// Static variable declaration: `static __dirname: String;`
    /// Declares a global JavaScript variable accessible from Husk code.
    Static { name: Ident, ty: TypeExpr },
    /// Constant declaration: `const VERSION: String;`
    /// Declares an immutable JavaScript constant accessible from Husk code.
    /// Unlike `Static`, this represents a truly immutable binding from the JS side.
    Const { name: Ident, ty: TypeExpr },
    /// Impl block inside extern block: `impl Request { fn get(&self) -> String; }`
    /// All methods inside are treated as extern "js" methods.
    Impl {
        /// Type parameters on the impl block
        type_params: Vec<TypeParam>,
        /// The type being implemented
        self_ty: TypeExpr,
        /// The methods in the impl block
        items: Vec<ImplItem>,
    },
}

/// Items that may appear inside a `mod` block within an extern block.
#[derive(Debug, Clone, PartialEq)]
pub struct ModItem {
    pub attributes: Vec<Attribute>,
    pub kind: ModItemKind,
    pub span: Span,
}

impl ModItem {
    pub fn is_default(&self) -> bool {
        self.attributes.iter().any(|a| a.name.name == "default")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ModItemKind {
    Fn {
        name: Ident,
        params: Vec<Param>,
        ret_type: Option<TypeExpr>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExternItem {
    /// Attributes on this extern item (e.g., #[js_name = "..."])
    pub attributes: Vec<Attribute>,
    pub kind: ExternItemKind,
    pub span: Span,
}

impl ExternItem {
    /// Returns true if this extern item has a #[default] attribute.
    /// When on a `mod` declaration, indicates the module uses default import
    /// and all functions are methods on the default export.
    pub fn is_default(&self) -> bool {
        self.attributes.iter().any(|a| a.name.name == "default")
    }

    /// Returns the JS name if specified via #[js_name = "..."], otherwise None.
    pub fn js_name(&self) -> Option<&str> {
        self.attributes.iter().find_map(|attr| {
            if attr.name.name == "js_name" {
                attr.value.as_deref()
            } else {
                None
            }
        })
    }

    /// Returns the module binding to call directly if specified via #[module_call = "binding"].
    /// This is used for class module callables where the module itself is the constructor.
    pub fn module_call(&self) -> Option<&str> {
        self.attributes.iter().find_map(|attr| {
            if attr.name.name == "module_call" {
                attr.value.as_deref()
            } else {
                None
            }
        })
    }

    /// Returns the namespace binding to access this function through if specified via #[ns = "binding"].
    /// This is used for namespace member functions (e.g., express.json).
    pub fn namespace(&self) -> Option<&str> {
        self.attributes.iter().find_map(|attr| {
            if attr.name.name == "ns" {
                attr.value.as_deref()
            } else {
                None
            }
        })
    }
}

/// A source file (compilation unit).
#[derive(Debug, Clone, PartialEq)]
pub struct File {
    pub items: Vec<Item>,
}

// ============================================================================
// File Path Setting
// ============================================================================

/// A trait for AST nodes that can have their spans' file paths set recursively.
pub trait SetFilePath {
    /// Set the file path on all spans in this node and its children.
    fn set_file_path(&mut self, file: Arc<str>);
}

impl SetFilePath for Span {
    fn set_file_path(&mut self, file: Arc<str>) {
        self.file = Some(file);
    }
}

impl SetFilePath for Ident {
    fn set_file_path(&mut self, file: Arc<str>) {
        self.span.set_file_path(file);
    }
}

impl SetFilePath for TypeExpr {
    fn set_file_path(&mut self, file: Arc<str>) {
        self.span.set_file_path(file.clone());
        match &mut self.kind {
            TypeExprKind::Named(ident) => ident.set_file_path(file),
            TypeExprKind::Generic { name, args } => {
                name.set_file_path(file.clone());
                for arg in args {
                    arg.set_file_path(file.clone());
                }
            }
            TypeExprKind::Function { params, ret } => {
                for param in params {
                    param.set_file_path(file.clone());
                }
                ret.set_file_path(file);
            }
            TypeExprKind::Array(elem) => elem.set_file_path(file),
            TypeExprKind::Tuple(types) => {
                for ty in types {
                    ty.set_file_path(file.clone());
                }
            }
            TypeExprKind::ImplTrait { trait_ty } => {
                trait_ty.set_file_path(file);
            }
        }
    }
}

impl SetFilePath for Expr {
    fn set_file_path(&mut self, file: Arc<str>) {
        self.span.set_file_path(file.clone());
        match &mut self.kind {
            ExprKind::Literal(lit) => lit.span.set_file_path(file),
            ExprKind::Ident(ident) => ident.set_file_path(file),
            ExprKind::Path { segments } => {
                for seg in segments {
                    seg.set_file_path(file.clone());
                }
            }
            ExprKind::Call {
                callee,
                type_args,
                args,
            } => {
                callee.set_file_path(file.clone());
                for arg in type_args {
                    arg.set_file_path(file.clone());
                }
                for arg in args {
                    arg.set_file_path(file.clone());
                }
            }
            ExprKind::Field { base, member } => {
                base.set_file_path(file.clone());
                member.set_file_path(file);
            }
            ExprKind::MethodCall {
                receiver,
                method,
                type_args,
                args,
            } => {
                receiver.set_file_path(file.clone());
                method.set_file_path(file.clone());
                for arg in type_args {
                    arg.set_file_path(file.clone());
                }
                for arg in args {
                    arg.set_file_path(file.clone());
                }
            }
            ExprKind::Unary { expr, .. } => expr.set_file_path(file),
            ExprKind::Binary { left, right, .. } => {
                left.set_file_path(file.clone());
                right.set_file_path(file);
            }
            ExprKind::Match { scrutinee, arms } => {
                scrutinee.set_file_path(file.clone());
                for arm in arms {
                    arm.pattern.set_file_path(file.clone());
                    arm.expr.set_file_path(file.clone());
                }
            }
            ExprKind::Block(block) => block.set_file_path(file),
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                cond.set_file_path(file.clone());
                then_branch.set_file_path(file.clone());
                else_branch.set_file_path(file);
            }
            ExprKind::Struct { name, fields } => {
                for seg in name {
                    seg.set_file_path(file.clone());
                }
                for field in fields {
                    field.name.set_file_path(file.clone());
                    field.value.set_file_path(file.clone());
                }
            }
            ExprKind::FormatPrint { format, args, .. } | ExprKind::Format { format, args } => {
                format.span.set_file_path(file.clone());
                for arg in args {
                    arg.set_file_path(file.clone());
                }
            }
            ExprKind::Closure {
                params,
                ret_type,
                body,
            } => {
                for param in params {
                    param.name.set_file_path(file.clone());
                    if let Some(ty) = &mut param.ty {
                        ty.set_file_path(file.clone());
                    }
                }
                if let Some(ret) = ret_type {
                    ret.set_file_path(file.clone());
                }
                body.set_file_path(file);
            }
            ExprKind::Array { elements } => {
                for elem in elements {
                    elem.set_file_path(file.clone());
                }
            }
            ExprKind::Index { base, index } => {
                base.set_file_path(file.clone());
                index.set_file_path(file);
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    s.set_file_path(file.clone());
                }
                if let Some(e) = end {
                    e.set_file_path(file);
                }
            }
            ExprKind::Assign { target, value, .. } => {
                target.set_file_path(file.clone());
                value.set_file_path(file);
            }
            ExprKind::JsLiteral { .. } => {}
            ExprKind::Cast { expr, target_ty } => {
                expr.set_file_path(file.clone());
                target_ty.set_file_path(file);
            }
            ExprKind::Tuple { elements } => {
                for elem in elements {
                    elem.set_file_path(file.clone());
                }
            }
            ExprKind::TupleField { base, .. } => {
                base.set_file_path(file);
            }
            ExprKind::Try { expr } => {
                expr.set_file_path(file);
            }
        }
    }
}

impl SetFilePath for Pattern {
    fn set_file_path(&mut self, file: Arc<str>) {
        self.span.set_file_path(file.clone());
        match &mut self.kind {
            PatternKind::Binding(ident) => ident.set_file_path(file),
            PatternKind::Wildcard => {}
            PatternKind::EnumUnit { path } => {
                for seg in path {
                    seg.set_file_path(file.clone());
                }
            }
            PatternKind::EnumTuple { path, fields } => {
                for seg in path {
                    seg.set_file_path(file.clone());
                }
                for p in fields {
                    p.set_file_path(file.clone());
                }
            }
            PatternKind::EnumStruct { path, fields } => {
                for seg in path {
                    seg.set_file_path(file.clone());
                }
                for (field_name, field_pat) in fields {
                    field_name.set_file_path(file.clone());
                    field_pat.set_file_path(file.clone());
                }
            }
            PatternKind::Tuple { fields } => {
                for p in fields {
                    p.set_file_path(file.clone());
                }
            }
        }
    }
}

impl SetFilePath for Block {
    fn set_file_path(&mut self, file: Arc<str>) {
        self.span.set_file_path(file.clone());
        for stmt in &mut self.stmts {
            stmt.set_file_path(file.clone());
        }
    }
}

impl SetFilePath for Stmt {
    fn set_file_path(&mut self, file: Arc<str>) {
        self.span.set_file_path(file.clone());
        match &mut self.kind {
            StmtKind::Let {
                pattern,
                ty,
                value,
                else_block,
                ..
            } => {
                pattern.set_file_path(file.clone());
                if let Some(t) = ty {
                    t.set_file_path(file.clone());
                }
                if let Some(e) = value {
                    e.set_file_path(file.clone());
                }
                if let Some(b) = else_block {
                    b.set_file_path(file);
                }
            }
            StmtKind::Assign { target, value, .. } => {
                target.set_file_path(file.clone());
                value.set_file_path(file);
            }
            StmtKind::Expr(expr) | StmtKind::Semi(expr) => expr.set_file_path(file),
            StmtKind::Return { value } => {
                if let Some(e) = value {
                    e.set_file_path(file);
                }
            }
            StmtKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                cond.set_file_path(file.clone());
                then_branch.set_file_path(file.clone());
                if let Some(else_b) = else_branch {
                    else_b.set_file_path(file);
                }
            }
            StmtKind::While { cond, body } => {
                cond.set_file_path(file.clone());
                body.set_file_path(file);
            }
            StmtKind::Loop { body } => {
                body.set_file_path(file);
            }
            StmtKind::ForIn {
                binding,
                iterable,
                body,
            } => {
                binding.set_file_path(file.clone());
                iterable.set_file_path(file.clone());
                body.set_file_path(file);
            }
            StmtKind::Break | StmtKind::Continue => {}
            StmtKind::Block(block) => block.set_file_path(file),
            StmtKind::IfLet {
                pattern,
                scrutinee,
                then_branch,
                else_branch,
            } => {
                pattern.set_file_path(file.clone());
                scrutinee.set_file_path(file.clone());
                then_branch.set_file_path(file.clone());
                if let Some(else_b) = else_branch {
                    else_b.set_file_path(file);
                }
            }
        }
    }
}

impl SetFilePath for Item {
    fn set_file_path(&mut self, file: Arc<str>) {
        self.span.set_file_path(file.clone());
        for attr in &mut self.attributes {
            attr.span.set_file_path(file.clone());
            attr.name.set_file_path(file.clone());
        }
        match &mut self.kind {
            ItemKind::Fn {
                name,
                type_params,
                params,
                ret_type,
                body,
            } => {
                name.set_file_path(file.clone());
                for tp in type_params {
                    tp.name.set_file_path(file.clone());
                    for bound in &mut tp.bounds {
                        bound.set_file_path(file.clone());
                    }
                }
                for param in params {
                    for attr in &mut param.attributes {
                        attr.span.set_file_path(file.clone());
                        attr.name.set_file_path(file.clone());
                    }
                    param.name.set_file_path(file.clone());
                    param.ty.set_file_path(file.clone());
                }
                if let Some(ret) = ret_type {
                    ret.set_file_path(file.clone());
                }
                for stmt in body {
                    stmt.set_file_path(file.clone());
                }
            }
            ItemKind::Struct {
                name,
                type_params,
                fields,
            } => {
                name.set_file_path(file.clone());
                for tp in type_params {
                    tp.set_file_path(file.clone());
                }
                for field in fields {
                    field.name.set_file_path(file.clone());
                    field.ty.set_file_path(file.clone());
                }
            }
            ItemKind::Enum {
                name,
                type_params,
                variants,
            } => {
                name.set_file_path(file.clone());
                for tp in type_params {
                    tp.set_file_path(file.clone());
                }
                for variant in variants {
                    variant.name.set_file_path(file.clone());
                    match &mut variant.fields {
                        EnumVariantFields::Unit => {}
                        EnumVariantFields::Tuple(types) => {
                            for ty in types {
                                ty.set_file_path(file.clone());
                            }
                        }
                        EnumVariantFields::Struct(fields) => {
                            for field in fields {
                                field.name.set_file_path(file.clone());
                                field.ty.set_file_path(file.clone());
                            }
                        }
                    }
                }
            }
            ItemKind::TypeAlias { name, ty } => {
                name.set_file_path(file.clone());
                ty.set_file_path(file);
            }
            ItemKind::ExternBlock { items, .. } => {
                for item in items {
                    item.set_file_path(file.clone());
                }
            }
            ItemKind::Use { path, .. } => {
                for seg in path {
                    seg.set_file_path(file.clone());
                }
            }
            ItemKind::Trait(trait_def) => {
                trait_def.name.set_file_path(file.clone());
                for tp in &mut trait_def.type_params {
                    tp.name.set_file_path(file.clone());
                    for bound in &mut tp.bounds {
                        bound.set_file_path(file.clone());
                    }
                }
                for item in &mut trait_def.items {
                    item.span.set_file_path(file.clone());
                    let TraitItemKind::Method(method) = &mut item.kind;
                    method.name.set_file_path(file.clone());
                    for param in &mut method.params {
                        for attr in &mut param.attributes {
                            attr.span.set_file_path(file.clone());
                            attr.name.set_file_path(file.clone());
                        }
                        param.name.set_file_path(file.clone());
                        param.ty.set_file_path(file.clone());
                    }
                    if let Some(ret) = &mut method.ret_type {
                        ret.set_file_path(file.clone());
                    }
                    if let Some(body) = &mut method.default_body {
                        for stmt in body {
                            stmt.set_file_path(file.clone());
                        }
                    }
                }
            }
            ItemKind::Impl(impl_block) => {
                impl_block.set_file_path(file);
            }
        }
    }
}

impl SetFilePath for ImplBlock {
    fn set_file_path(&mut self, file: Arc<str>) {
        self.span.set_file_path(file.clone());
        for tp in &mut self.type_params {
            tp.name.set_file_path(file.clone());
            for bound in &mut tp.bounds {
                bound.set_file_path(file.clone());
            }
        }
        if let Some(trait_ref) = &mut self.trait_ref {
            trait_ref.set_file_path(file.clone());
        }
        self.self_ty.set_file_path(file.clone());
        for item in &mut self.items {
            item.span.set_file_path(file.clone());
            match &mut item.kind {
                ImplItemKind::Method(method) => {
                    for attr in &mut method.attributes {
                        attr.span.set_file_path(file.clone());
                        attr.name.set_file_path(file.clone());
                    }
                    method.name.set_file_path(file.clone());
                    for param in &mut method.params {
                        for attr in &mut param.attributes {
                            attr.span.set_file_path(file.clone());
                            attr.name.set_file_path(file.clone());
                        }
                        param.name.set_file_path(file.clone());
                        param.ty.set_file_path(file.clone());
                    }
                    if let Some(ret) = &mut method.ret_type {
                        ret.set_file_path(file.clone());
                    }
                    for stmt in &mut method.body {
                        stmt.set_file_path(file.clone());
                    }
                }
                ImplItemKind::Property(prop) => {
                    for attr in &mut prop.attributes {
                        attr.span.set_file_path(file.clone());
                        attr.name.set_file_path(file.clone());
                    }
                    prop.name.set_file_path(file.clone());
                    prop.ty.set_file_path(file.clone());
                }
            }
        }
    }
}

impl SetFilePath for ExternItem {
    fn set_file_path(&mut self, file: Arc<str>) {
        self.span.set_file_path(file.clone());
        for attr in &mut self.attributes {
            attr.span.set_file_path(file.clone());
            attr.name.set_file_path(file.clone());
        }
        match &mut self.kind {
            ExternItemKind::Fn {
                name,
                params,
                ret_type,
            } => {
                name.set_file_path(file.clone());
                for param in params {
                    for attr in &mut param.attributes {
                        attr.span.set_file_path(file.clone());
                        attr.name.set_file_path(file.clone());
                    }
                    param.name.set_file_path(file.clone());
                    param.ty.set_file_path(file.clone());
                }
                if let Some(ret) = ret_type {
                    ret.set_file_path(file);
                }
            }
            ExternItemKind::Mod { binding, items, .. } => {
                binding.set_file_path(file.clone());
                for item in items {
                    item.set_file_path(file.clone());
                }
            }
            ExternItemKind::Struct {
                name, type_params, ..
            } => {
                name.set_file_path(file.clone());
                for tp in type_params {
                    tp.set_file_path(file.clone());
                }
            }
            ExternItemKind::Impl {
                type_params,
                self_ty,
                items,
            } => {
                for tp in type_params {
                    tp.name.set_file_path(file.clone());
                    for bound in &mut tp.bounds {
                        bound.set_file_path(file.clone());
                    }
                }
                self_ty.set_file_path(file.clone());
                for item in items {
                    item.span.set_file_path(file.clone());
                    match &mut item.kind {
                        ImplItemKind::Method(method) => {
                            for attr in &mut method.attributes {
                                attr.span.set_file_path(file.clone());
                                attr.name.set_file_path(file.clone());
                            }
                            method.name.set_file_path(file.clone());
                            for param in &mut method.params {
                                for attr in &mut param.attributes {
                                    attr.span.set_file_path(file.clone());
                                    attr.name.set_file_path(file.clone());
                                }
                                param.name.set_file_path(file.clone());
                                param.ty.set_file_path(file.clone());
                            }
                            if let Some(ret) = &mut method.ret_type {
                                ret.set_file_path(file.clone());
                            }
                            for stmt in &mut method.body {
                                stmt.set_file_path(file.clone());
                            }
                        }
                        ImplItemKind::Property(prop) => {
                            for attr in &mut prop.attributes {
                                attr.span.set_file_path(file.clone());
                                attr.name.set_file_path(file.clone());
                            }
                            prop.name.set_file_path(file.clone());
                            prop.ty.set_file_path(file.clone());
                        }
                    }
                }
            }
            ExternItemKind::Static { name, ty } => {
                name.set_file_path(file.clone());
                ty.set_file_path(file);
            }
            ExternItemKind::Const { name, ty } => {
                name.set_file_path(file.clone());
                ty.set_file_path(file);
            }
        }
    }
}

impl SetFilePath for ModItem {
    fn set_file_path(&mut self, file: Arc<str>) {
        self.span.set_file_path(file.clone());
        match &mut self.kind {
            ModItemKind::Fn {
                name,
                params,
                ret_type,
            } => {
                name.set_file_path(file.clone());
                for param in params {
                    for attr in &mut param.attributes {
                        attr.span.set_file_path(file.clone());
                        attr.name.set_file_path(file.clone());
                    }
                    param.name.set_file_path(file.clone());
                    param.ty.set_file_path(file.clone());
                }
                if let Some(ret) = ret_type {
                    ret.set_file_path(file);
                }
            }
        }
    }
}
