//! Runtime-neutral executable representation for Husk.
//!
//! The parser AST remains available on compiled artifacts for tools and
//! diagnostics. Execution consumes this smaller representation, whose node and
//! local identifiers are stable for a deterministic source traversal.

use std::collections::{BTreeSet, HashMap};

use husk_ast::{self as ast, FormatString, Span, TypeExpr, TypeExprKind};

pub use husk_ast::{AssignOp, BinaryOp, UnaryOp};

/// Stable identity of one executable node within a function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u32);

impl NodeId {
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// Stable identity of one local binding within a function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocalId(u32);

impl LocalId {
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// Stable identity of one script function in a compiled program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FunctionId(u64);

impl FunctionId {
    #[must_use]
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Stable identity of one registered native or Wasm module function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModuleFunctionId(u64);

impl ModuleFunctionId {
    #[must_use]
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Stable identity of one language-provided method implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IntrinsicMethodId(u64);

impl IntrinsicMethodId {
    #[must_use]
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Language-provided free functions that do not dispatch through a host module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntrinsicFunction {
    Println,
    Assert,
    AssertMessage,
}

/// Resolved target for a function-call expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallTarget {
    /// Filled by the compiled-program finalization pass.
    Unresolved,
    /// A local function value or closure; the callee expression supplies the handle.
    Indirect,
    Script(FunctionId),
    Module(ModuleFunctionId),
    Intrinsic(IntrinsicFunction),
    Constructor,
    /// Explicitly retained only for the legacy compatibility profile.
    LegacyDynamic,
}

/// Resolved target for a method-call expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MethodTarget {
    /// Filled by the compiled-program finalization pass.
    Unresolved,
    Script(FunctionId),
    Module(ModuleFunctionId),
    Intrinsic(IntrinsicMethodId),
    /// Explicitly retained only for the legacy compatibility profile.
    LegacyDynamic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Local {
    pub id: LocalId,
    pub name: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub parameters: Vec<Local>,
    pub body: Vec<Stmt>,
    pub node_count: u32,
    pub local_count: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LiteralKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub id: NodeId,
    pub kind: ExprKind,
    /// Resolved semantic type when type checking was enabled.
    pub ty: Option<String>,
    /// Resolved enum constructor for imported calls such as `Ok(value)`.
    pub constructor: Option<VariantRef>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantRef {
    pub type_name: String,
    pub case: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Literal(LiteralKind),
    Ident {
        name: String,
        local: Option<LocalId>,
        function: Option<FunctionId>,
    },
    Path {
        segments: Vec<String>,
        function: Option<FunctionId>,
    },
    Call {
        callee: Box<Expr>,
        target: CallTarget,
        type_args: Vec<String>,
        args: Vec<Expr>,
    },
    Field {
        base: Box<Expr>,
        member: String,
        member_span: Span,
    },
    MethodCall {
        receiver: Box<Expr>,
        method: String,
        method_span: Span,
        target: MethodTarget,
        type_args: Vec<String>,
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
    Struct {
        name: Vec<String>,
        fields: Vec<FieldInit>,
    },
    FormatPrint {
        format: FormatString,
        args: Vec<Expr>,
        newline: bool,
    },
    Format {
        format: FormatString,
        args: Vec<Expr>,
    },
    Closure {
        params: Vec<ClosureParameter>,
        /// Outer locals captured by this closure, ordered by stable local ID.
        captures: Vec<LocalId>,
        ret_type: Option<String>,
        body: Box<Expr>,
    },
    Array {
        elements: Vec<Expr>,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    Range {
        start: Option<Box<Expr>>,
        end: Option<Box<Expr>>,
        inclusive: bool,
    },
    Assign {
        target: Box<Expr>,
        op: AssignOp,
        value: Box<Expr>,
    },
    JsLiteral {
        code: String,
    },
    Cast {
        expr: Box<Expr>,
        target_ty: String,
    },
    Tuple {
        elements: Vec<Expr>,
    },
    TupleField {
        base: Box<Expr>,
        index: usize,
    },
    Try {
        expr: Box<Expr>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldInit {
    pub name: String,
    pub span: Span,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClosureParameter {
    pub local: Local,
    pub ty: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub expr: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    pub id: NodeId,
    pub kind: PatternKind,
    /// Resolved enum case for imported patterns such as `Some(value)`.
    pub variant: Option<VariantRef>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PatternKind {
    Wildcard,
    Binding(Local),
    EnumUnit {
        path: Vec<String>,
    },
    EnumTuple {
        path: Vec<String>,
        fields: Vec<Pattern>,
    },
    EnumStruct {
        path: Vec<String>,
        fields: Vec<(String, Pattern)>,
    },
    Tuple {
        fields: Vec<Pattern>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub id: NodeId,
    pub statements: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub id: NodeId,
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    Let {
        mutable: bool,
        pattern: Pattern,
        ty: Option<String>,
        value: Option<Expr>,
        else_block: Option<Block>,
    },
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
        else_branch: Option<Box<Stmt>>,
    },
    While {
        cond: Expr,
        body: Block,
    },
    Loop {
        body: Block,
    },
    ForIn {
        binding: Local,
        iterable: Expr,
        body: Block,
    },
    Break,
    Continue,
    Block(Block),
    IfLet {
        pattern: Pattern,
        scrutinee: Expr,
        then_branch: Block,
        else_branch: Option<Box<Stmt>>,
    },
}

/// Lower one source function into deterministic executable HIR.
///
/// `name_resolution` is the semantic analyzer's alpha-renaming map. Passing an
/// empty map is supported for parse-only compatibility mode.
#[must_use]
pub fn lower_function(
    parameters: &[ast::Param],
    body: &[ast::Stmt],
    name_resolution: &HashMap<(usize, usize), String>,
    type_resolution: &HashMap<(usize, usize), String>,
    variant_calls: &HashMap<(usize, usize), (String, String)>,
    variant_patterns: &HashMap<(usize, usize), (String, String)>,
) -> Function {
    let mut lowerer = Lowerer::new(
        name_resolution,
        type_resolution,
        variant_calls,
        variant_patterns,
    );
    for parameter in parameters {
        lowerer.collect_binding(&parameter.name);
    }
    for statement in body {
        lowerer.collect_statement_bindings(statement);
    }
    let parameters = parameters
        .iter()
        .map(|parameter| lowerer.lower_local(&parameter.name))
        .collect();
    let body = body
        .iter()
        .map(|statement| lowerer.lower_statement(statement))
        .collect();
    Function {
        parameters,
        body,
        node_count: lowerer.next_node,
        local_count: lowerer.next_local,
    }
}

struct Lowerer<'resolution> {
    name_resolution: &'resolution HashMap<(usize, usize), String>,
    type_resolution: &'resolution HashMap<(usize, usize), String>,
    variant_calls: &'resolution HashMap<(usize, usize), (String, String)>,
    variant_patterns: &'resolution HashMap<(usize, usize), (String, String)>,
    locals: HashMap<String, LocalId>,
    next_node: u32,
    next_local: u32,
}

impl<'resolution> Lowerer<'resolution> {
    fn new(
        name_resolution: &'resolution HashMap<(usize, usize), String>,
        type_resolution: &'resolution HashMap<(usize, usize), String>,
        variant_calls: &'resolution HashMap<(usize, usize), (String, String)>,
        variant_patterns: &'resolution HashMap<(usize, usize), (String, String)>,
    ) -> Self {
        Self {
            name_resolution,
            type_resolution,
            variant_calls,
            variant_patterns,
            locals: HashMap::new(),
            next_node: 0,
            next_local: 0,
        }
    }

    fn node(&mut self) -> NodeId {
        let id = NodeId(self.next_node);
        self.next_node = self
            .next_node
            .checked_add(1)
            .expect("a function cannot contain more than u32::MAX HIR nodes");
        id
    }

    fn resolved_name(&self, ident: &ast::Ident) -> String {
        self.name_resolution
            .get(&(ident.span.range.start, ident.span.range.end))
            .cloned()
            .unwrap_or_else(|| ident.name.clone())
    }

    fn collect_binding(&mut self, ident: &ast::Ident) {
        let name = self.resolved_name(ident);
        self.locals.entry(name).or_insert_with(|| {
            let id = LocalId(self.next_local);
            self.next_local = self
                .next_local
                .checked_add(1)
                .expect("a function cannot contain more than u32::MAX locals");
            id
        });
    }

    fn collect_pattern_bindings(&mut self, pattern: &ast::Pattern) {
        match &pattern.kind {
            ast::PatternKind::Binding(ident) => self.collect_binding(ident),
            ast::PatternKind::EnumTuple { fields, .. } | ast::PatternKind::Tuple { fields } => {
                for field in fields {
                    self.collect_pattern_bindings(field);
                }
            }
            ast::PatternKind::EnumStruct { fields, .. } => {
                for (_, field) in fields {
                    self.collect_pattern_bindings(field);
                }
            }
            ast::PatternKind::Wildcard | ast::PatternKind::EnumUnit { .. } => {}
        }
    }

    fn collect_statement_bindings(&mut self, statement: &ast::Stmt) {
        match &statement.kind {
            ast::StmtKind::Let {
                pattern,
                value,
                else_block,
                ..
            } => {
                self.collect_pattern_bindings(pattern);
                if let Some(value) = value {
                    self.collect_expression_bindings(value);
                }
                if let Some(block) = else_block {
                    self.collect_block_bindings(block);
                }
            }
            ast::StmtKind::Assign { target, value, .. } => {
                if let ast::ExprKind::Ident(ident) = &target.kind {
                    self.collect_binding(ident);
                }
                self.collect_expression_bindings(target);
                self.collect_expression_bindings(value);
            }
            ast::StmtKind::Expr(expr) | ast::StmtKind::Semi(expr) => {
                self.collect_expression_bindings(expr);
            }
            ast::StmtKind::Return { value } => {
                if let Some(value) = value {
                    self.collect_expression_bindings(value);
                }
            }
            ast::StmtKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.collect_expression_bindings(cond);
                self.collect_block_bindings(then_branch);
                if let Some(branch) = else_branch {
                    self.collect_statement_bindings(branch);
                }
            }
            ast::StmtKind::While { cond, body } => {
                self.collect_expression_bindings(cond);
                self.collect_block_bindings(body);
            }
            ast::StmtKind::Loop { body } | ast::StmtKind::Block(body) => {
                self.collect_block_bindings(body);
            }
            ast::StmtKind::ForIn {
                binding,
                iterable,
                body,
            } => {
                self.collect_binding(binding);
                self.collect_expression_bindings(iterable);
                self.collect_block_bindings(body);
            }
            ast::StmtKind::IfLet {
                pattern,
                scrutinee,
                then_branch,
                else_branch,
            } => {
                self.collect_pattern_bindings(pattern);
                self.collect_expression_bindings(scrutinee);
                self.collect_block_bindings(then_branch);
                if let Some(branch) = else_branch {
                    self.collect_statement_bindings(branch);
                }
            }
            ast::StmtKind::Break | ast::StmtKind::Continue => {}
        }
    }

    fn collect_block_bindings(&mut self, block: &ast::Block) {
        for statement in &block.stmts {
            self.collect_statement_bindings(statement);
        }
    }

    fn collect_expression_bindings(&mut self, expression: &ast::Expr) {
        match &expression.kind {
            ast::ExprKind::Call { callee, args, .. } => {
                self.collect_expression_bindings(callee);
                for argument in args {
                    self.collect_expression_bindings(argument);
                }
            }
            ast::ExprKind::Field { base, .. }
            | ast::ExprKind::Unary { expr: base, .. }
            | ast::ExprKind::TupleField { base, .. }
            | ast::ExprKind::Try { expr: base }
            | ast::ExprKind::Cast { expr: base, .. } => {
                self.collect_expression_bindings(base);
            }
            ast::ExprKind::MethodCall { receiver, args, .. } => {
                self.collect_expression_bindings(receiver);
                for argument in args {
                    self.collect_expression_bindings(argument);
                }
            }
            ast::ExprKind::Binary { left, right, .. }
            | ast::ExprKind::Index {
                base: left,
                index: right,
            }
            | ast::ExprKind::Assign {
                target: left,
                value: right,
                ..
            } => {
                self.collect_expression_bindings(left);
                self.collect_expression_bindings(right);
            }
            ast::ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.collect_expression_bindings(cond);
                self.collect_expression_bindings(then_branch);
                self.collect_expression_bindings(else_branch);
            }
            ast::ExprKind::Match { scrutinee, arms } => {
                self.collect_expression_bindings(scrutinee);
                for arm in arms {
                    self.collect_pattern_bindings(&arm.pattern);
                    self.collect_expression_bindings(&arm.expr);
                }
            }
            ast::ExprKind::Block(block) => self.collect_block_bindings(block),
            ast::ExprKind::Struct { fields, .. } => {
                for field in fields {
                    self.collect_expression_bindings(&field.value);
                }
            }
            ast::ExprKind::FormatPrint { args, .. }
            | ast::ExprKind::Format { args, .. }
            | ast::ExprKind::Array { elements: args }
            | ast::ExprKind::Tuple { elements: args } => {
                for argument in args {
                    self.collect_expression_bindings(argument);
                }
            }
            ast::ExprKind::Closure { params, body, .. } => {
                for parameter in params {
                    self.collect_binding(&parameter.name);
                }
                self.collect_expression_bindings(body);
            }
            ast::ExprKind::Range { start, end, .. } => {
                if let Some(start) = start {
                    self.collect_expression_bindings(start);
                }
                if let Some(end) = end {
                    self.collect_expression_bindings(end);
                }
            }
            ast::ExprKind::Literal(_)
            | ast::ExprKind::Ident(_)
            | ast::ExprKind::Path { .. }
            | ast::ExprKind::JsLiteral { .. } => {}
        }
    }

    fn lower_local(&self, ident: &ast::Ident) -> Local {
        let resolved = self.resolved_name(ident);
        Local {
            id: *self
                .locals
                .get(&resolved)
                .expect("binding collection runs before HIR lowering"),
            name: ident.name.clone(),
            span: ident.span.clone(),
        }
    }

    fn lower_pattern(&mut self, pattern: &ast::Pattern) -> Pattern {
        let kind = match &pattern.kind {
            ast::PatternKind::Wildcard => PatternKind::Wildcard,
            ast::PatternKind::Binding(ident) => PatternKind::Binding(self.lower_local(ident)),
            ast::PatternKind::EnumUnit { path } => PatternKind::EnumUnit {
                path: path_names(path),
            },
            ast::PatternKind::EnumTuple { path, fields } => PatternKind::EnumTuple {
                path: path_names(path),
                fields: fields
                    .iter()
                    .map(|field| self.lower_pattern(field))
                    .collect(),
            },
            ast::PatternKind::EnumStruct { path, fields } => PatternKind::EnumStruct {
                path: path_names(path),
                fields: fields
                    .iter()
                    .map(|(name, pattern)| (name.name.clone(), self.lower_pattern(pattern)))
                    .collect(),
            },
            ast::PatternKind::Tuple { fields } => PatternKind::Tuple {
                fields: fields
                    .iter()
                    .map(|field| self.lower_pattern(field))
                    .collect(),
            },
        };
        Pattern {
            id: self.node(),
            kind,
            variant: self
                .variant_patterns
                .get(&(pattern.span.range.start, pattern.span.range.end))
                .map(|(type_name, case)| VariantRef {
                    type_name: type_name.clone(),
                    case: case.clone(),
                }),
            span: pattern.span.clone(),
        }
    }

    fn lower_block(&mut self, block: &ast::Block) -> Block {
        let statements = block
            .stmts
            .iter()
            .map(|statement| self.lower_statement(statement))
            .collect();
        Block {
            id: self.node(),
            statements,
            span: block.span.clone(),
        }
    }

    fn lower_statement(&mut self, statement: &ast::Stmt) -> Stmt {
        let kind = match &statement.kind {
            ast::StmtKind::Let {
                mutable,
                pattern,
                ty,
                value,
                else_block,
            } => StmtKind::Let {
                mutable: *mutable,
                pattern: self.lower_pattern(pattern),
                ty: ty.as_ref().map(format_type),
                value: value.as_ref().map(|value| self.lower_expression(value)),
                else_block: else_block.as_ref().map(|block| self.lower_block(block)),
            },
            ast::StmtKind::Assign { target, op, value } => StmtKind::Assign {
                target: self.lower_expression(target),
                op: *op,
                value: self.lower_expression(value),
            },
            ast::StmtKind::Expr(expr) => StmtKind::Expr(self.lower_expression(expr)),
            ast::StmtKind::Semi(expr) => StmtKind::Semi(self.lower_expression(expr)),
            ast::StmtKind::Return { value } => StmtKind::Return {
                value: value.as_ref().map(|value| self.lower_expression(value)),
            },
            ast::StmtKind::If {
                cond,
                then_branch,
                else_branch,
            } => StmtKind::If {
                cond: self.lower_expression(cond),
                then_branch: self.lower_block(then_branch),
                else_branch: else_branch
                    .as_ref()
                    .map(|branch| Box::new(self.lower_statement(branch))),
            },
            ast::StmtKind::While { cond, body } => StmtKind::While {
                cond: self.lower_expression(cond),
                body: self.lower_block(body),
            },
            ast::StmtKind::Loop { body } => StmtKind::Loop {
                body: self.lower_block(body),
            },
            ast::StmtKind::ForIn {
                binding,
                iterable,
                body,
            } => StmtKind::ForIn {
                binding: self.lower_local(binding),
                iterable: self.lower_expression(iterable),
                body: self.lower_block(body),
            },
            ast::StmtKind::Break => StmtKind::Break,
            ast::StmtKind::Continue => StmtKind::Continue,
            ast::StmtKind::Block(block) => StmtKind::Block(self.lower_block(block)),
            ast::StmtKind::IfLet {
                pattern,
                scrutinee,
                then_branch,
                else_branch,
            } => StmtKind::IfLet {
                pattern: self.lower_pattern(pattern),
                scrutinee: self.lower_expression(scrutinee),
                then_branch: self.lower_block(then_branch),
                else_branch: else_branch
                    .as_ref()
                    .map(|branch| Box::new(self.lower_statement(branch))),
            },
        };
        Stmt {
            id: self.node(),
            kind,
            span: statement.span.clone(),
        }
    }

    fn lower_expression(&mut self, expression: &ast::Expr) -> Expr {
        let kind = match &expression.kind {
            ast::ExprKind::Literal(literal) => ExprKind::Literal(match &literal.kind {
                ast::LiteralKind::Int(value) => LiteralKind::Int(*value),
                ast::LiteralKind::Float(value) => LiteralKind::Float(*value),
                ast::LiteralKind::Bool(value) => LiteralKind::Bool(*value),
                ast::LiteralKind::String(value) => LiteralKind::String(value.clone()),
            }),
            ast::ExprKind::Ident(ident) => {
                let resolved = self.resolved_name(ident);
                ExprKind::Ident {
                    name: ident.name.clone(),
                    local: self.locals.get(&resolved).copied(),
                    function: None,
                }
            }
            ast::ExprKind::Path { segments } => ExprKind::Path {
                segments: path_names(segments),
                function: None,
            },
            ast::ExprKind::Call {
                callee,
                type_args,
                args,
            } => ExprKind::Call {
                callee: Box::new(self.lower_expression(callee)),
                target: CallTarget::Unresolved,
                type_args: type_args.iter().map(format_type).collect(),
                args: args
                    .iter()
                    .map(|argument| self.lower_expression(argument))
                    .collect(),
            },
            ast::ExprKind::Field { base, member } => ExprKind::Field {
                base: Box::new(self.lower_expression(base)),
                member: member.name.clone(),
                member_span: member.span.clone(),
            },
            ast::ExprKind::MethodCall {
                receiver,
                method,
                type_args,
                args,
            } => ExprKind::MethodCall {
                receiver: Box::new(self.lower_expression(receiver)),
                method: method.name.clone(),
                method_span: method.span.clone(),
                target: MethodTarget::Unresolved,
                type_args: type_args.iter().map(format_type).collect(),
                args: args
                    .iter()
                    .map(|argument| self.lower_expression(argument))
                    .collect(),
            },
            ast::ExprKind::Unary { op, expr } => ExprKind::Unary {
                op: *op,
                expr: Box::new(self.lower_expression(expr)),
            },
            ast::ExprKind::Binary { op, left, right } => ExprKind::Binary {
                op: *op,
                left: Box::new(self.lower_expression(left)),
                right: Box::new(self.lower_expression(right)),
            },
            ast::ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => ExprKind::If {
                cond: Box::new(self.lower_expression(cond)),
                then_branch: Box::new(self.lower_expression(then_branch)),
                else_branch: Box::new(self.lower_expression(else_branch)),
            },
            ast::ExprKind::Match { scrutinee, arms } => ExprKind::Match {
                scrutinee: Box::new(self.lower_expression(scrutinee)),
                arms: arms
                    .iter()
                    .map(|arm| MatchArm {
                        pattern: self.lower_pattern(&arm.pattern),
                        expr: self.lower_expression(&arm.expr),
                    })
                    .collect(),
            },
            ast::ExprKind::Block(block) => ExprKind::Block(self.lower_block(block)),
            ast::ExprKind::Struct { name, fields } => ExprKind::Struct {
                name: path_names(name),
                fields: fields
                    .iter()
                    .map(|field| FieldInit {
                        name: field.name.name.clone(),
                        span: field.name.span.clone(),
                        value: self.lower_expression(&field.value),
                    })
                    .collect(),
            },
            ast::ExprKind::FormatPrint {
                format,
                args,
                newline,
            } => ExprKind::FormatPrint {
                format: format.clone(),
                args: args
                    .iter()
                    .map(|argument| self.lower_expression(argument))
                    .collect(),
                newline: *newline,
            },
            ast::ExprKind::Format { format, args } => ExprKind::Format {
                format: format.clone(),
                args: args
                    .iter()
                    .map(|argument| self.lower_expression(argument))
                    .collect(),
            },
            ast::ExprKind::Closure {
                params,
                ret_type,
                body,
            } => {
                let params = params
                    .iter()
                    .map(|parameter| ClosureParameter {
                        local: self.lower_local(&parameter.name),
                        ty: parameter.ty.as_ref().map(format_type),
                    })
                    .collect::<Vec<_>>();
                let body = self.lower_expression(body);
                let captures = closure_captures(&params, &body);
                ExprKind::Closure {
                    params,
                    captures,
                    ret_type: ret_type.as_ref().map(format_type),
                    body: Box::new(body),
                }
            }
            ast::ExprKind::Array { elements } => ExprKind::Array {
                elements: elements
                    .iter()
                    .map(|element| self.lower_expression(element))
                    .collect(),
            },
            ast::ExprKind::Index { base, index } => ExprKind::Index {
                base: Box::new(self.lower_expression(base)),
                index: Box::new(self.lower_expression(index)),
            },
            ast::ExprKind::Range {
                start,
                end,
                inclusive,
            } => ExprKind::Range {
                start: start
                    .as_ref()
                    .map(|start| Box::new(self.lower_expression(start))),
                end: end.as_ref().map(|end| Box::new(self.lower_expression(end))),
                inclusive: *inclusive,
            },
            ast::ExprKind::Assign { target, op, value } => ExprKind::Assign {
                target: Box::new(self.lower_expression(target)),
                op: *op,
                value: Box::new(self.lower_expression(value)),
            },
            ast::ExprKind::JsLiteral { code } => ExprKind::JsLiteral { code: code.clone() },
            ast::ExprKind::Cast { expr, target_ty } => ExprKind::Cast {
                expr: Box::new(self.lower_expression(expr)),
                target_ty: format_type(target_ty),
            },
            ast::ExprKind::Tuple { elements } => ExprKind::Tuple {
                elements: elements
                    .iter()
                    .map(|element| self.lower_expression(element))
                    .collect(),
            },
            ast::ExprKind::TupleField { base, index } => ExprKind::TupleField {
                base: Box::new(self.lower_expression(base)),
                index: *index,
            },
            ast::ExprKind::Try { expr } => ExprKind::Try {
                expr: Box::new(self.lower_expression(expr)),
            },
        };
        Expr {
            id: self.node(),
            kind,
            ty: self
                .type_resolution
                .get(&(expression.span.range.start, expression.span.range.end))
                .cloned(),
            constructor: self
                .variant_calls
                .get(&(expression.span.range.start, expression.span.range.end))
                .map(|(type_name, case)| VariantRef {
                    type_name: type_name.clone(),
                    case: case.clone(),
                }),
            span: expression.span.clone(),
        }
    }
}

fn closure_captures(params: &[ClosureParameter], body: &Expr) -> Vec<LocalId> {
    let mut declared = params
        .iter()
        .map(|parameter| parameter.local.id)
        .collect::<BTreeSet<_>>();
    collect_declared_expr(body, &mut declared);

    let mut referenced = BTreeSet::new();
    collect_referenced_expr(body, &mut referenced);
    referenced
        .into_iter()
        .filter(|local| !declared.contains(local))
        .collect()
}

fn collect_declared_pattern(pattern: &Pattern, declared: &mut BTreeSet<LocalId>) {
    match &pattern.kind {
        PatternKind::Binding(local) => {
            declared.insert(local.id);
        }
        PatternKind::EnumTuple { fields, .. } | PatternKind::Tuple { fields } => {
            for field in fields {
                collect_declared_pattern(field, declared);
            }
        }
        PatternKind::EnumStruct { fields, .. } => {
            for (_, field) in fields {
                collect_declared_pattern(field, declared);
            }
        }
        PatternKind::Wildcard | PatternKind::EnumUnit { .. } => {}
    }
}

fn collect_declared_block(block: &Block, declared: &mut BTreeSet<LocalId>) {
    for statement in &block.statements {
        match &statement.kind {
            StmtKind::Let {
                pattern,
                value,
                else_block,
                ..
            } => {
                collect_declared_pattern(pattern, declared);
                if let Some(value) = value {
                    collect_declared_expr(value, declared);
                }
                if let Some(else_block) = else_block {
                    collect_declared_block(else_block, declared);
                }
            }
            StmtKind::ForIn {
                binding,
                iterable,
                body,
            } => {
                declared.insert(binding.id);
                collect_declared_expr(iterable, declared);
                collect_declared_block(body, declared);
            }
            StmtKind::IfLet {
                pattern,
                scrutinee,
                then_branch,
                else_branch,
            } => {
                collect_declared_pattern(pattern, declared);
                collect_declared_expr(scrutinee, declared);
                collect_declared_block(then_branch, declared);
                if let Some(else_branch) = else_branch {
                    collect_declared_statement(else_branch, declared);
                }
            }
            _ => collect_declared_statement(statement, declared),
        }
    }
}

fn collect_declared_statement(statement: &Stmt, declared: &mut BTreeSet<LocalId>) {
    match &statement.kind {
        StmtKind::Let { .. } | StmtKind::ForIn { .. } | StmtKind::IfLet { .. } => {
            // These are handled by `collect_declared_block`, which needs their
            // binding-specific fields.
        }
        StmtKind::Assign { target, value, .. } => {
            collect_declared_expr(target, declared);
            collect_declared_expr(value, declared);
        }
        StmtKind::Expr(expr) | StmtKind::Semi(expr) => collect_declared_expr(expr, declared),
        StmtKind::Return { value } => {
            if let Some(value) = value {
                collect_declared_expr(value, declared);
            }
        }
        StmtKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            collect_declared_expr(cond, declared);
            collect_declared_block(then_branch, declared);
            if let Some(else_branch) = else_branch {
                collect_declared_statement(else_branch, declared);
            }
        }
        StmtKind::While { cond, body } => {
            collect_declared_expr(cond, declared);
            collect_declared_block(body, declared);
        }
        StmtKind::Loop { body } | StmtKind::Block(body) => {
            collect_declared_block(body, declared);
        }
        StmtKind::Break | StmtKind::Continue => {}
    }
}

fn collect_declared_expr(expr: &Expr, declared: &mut BTreeSet<LocalId>) {
    match &expr.kind {
        ExprKind::Call { callee, args, .. } => {
            collect_declared_expr(callee, declared);
            for argument in args {
                collect_declared_expr(argument, declared);
            }
        }
        ExprKind::Field { base, .. }
        | ExprKind::Unary { expr: base, .. }
        | ExprKind::TupleField { base, .. }
        | ExprKind::Try { expr: base }
        | ExprKind::Cast { expr: base, .. } => collect_declared_expr(base, declared),
        ExprKind::MethodCall { receiver, args, .. } => {
            collect_declared_expr(receiver, declared);
            for argument in args {
                collect_declared_expr(argument, declared);
            }
        }
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
            collect_declared_expr(left, declared);
            collect_declared_expr(right, declared);
        }
        ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            collect_declared_expr(cond, declared);
            collect_declared_expr(then_branch, declared);
            collect_declared_expr(else_branch, declared);
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_declared_expr(scrutinee, declared);
            for arm in arms {
                collect_declared_pattern(&arm.pattern, declared);
                collect_declared_expr(&arm.expr, declared);
            }
        }
        ExprKind::Block(block) => collect_declared_block(block, declared),
        ExprKind::Struct { fields, .. } => {
            for field in fields {
                collect_declared_expr(&field.value, declared);
            }
        }
        ExprKind::FormatPrint { args, .. }
        | ExprKind::Format { args, .. }
        | ExprKind::Array { elements: args }
        | ExprKind::Tuple { elements: args } => {
            for argument in args {
                collect_declared_expr(argument, declared);
            }
        }
        ExprKind::Closure { .. } => {
            // A nested closure owns its declarations. Its capture list is
            // considered by `collect_referenced_expr` instead.
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(start) = start {
                collect_declared_expr(start, declared);
            }
            if let Some(end) = end {
                collect_declared_expr(end, declared);
            }
        }
        ExprKind::Literal(_)
        | ExprKind::Ident { .. }
        | ExprKind::Path { .. }
        | ExprKind::JsLiteral { .. } => {}
    }
}

fn collect_referenced_block(block: &Block, referenced: &mut BTreeSet<LocalId>) {
    for statement in &block.statements {
        match &statement.kind {
            StmtKind::Let {
                value, else_block, ..
            } => {
                if let Some(value) = value {
                    collect_referenced_expr(value, referenced);
                }
                if let Some(else_block) = else_block {
                    collect_referenced_block(else_block, referenced);
                }
            }
            StmtKind::Assign { target, value, .. } => {
                collect_referenced_expr(target, referenced);
                collect_referenced_expr(value, referenced);
            }
            StmtKind::Expr(expr) | StmtKind::Semi(expr) => {
                collect_referenced_expr(expr, referenced);
            }
            StmtKind::Return { value } => {
                if let Some(value) = value {
                    collect_referenced_expr(value, referenced);
                }
            }
            StmtKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                collect_referenced_expr(cond, referenced);
                collect_referenced_block(then_branch, referenced);
                if let Some(else_branch) = else_branch {
                    collect_referenced_statement(else_branch, referenced);
                }
            }
            StmtKind::While { cond, body } => {
                collect_referenced_expr(cond, referenced);
                collect_referenced_block(body, referenced);
            }
            StmtKind::Loop { body } | StmtKind::Block(body) => {
                collect_referenced_block(body, referenced);
            }
            StmtKind::ForIn { iterable, body, .. } => {
                collect_referenced_expr(iterable, referenced);
                collect_referenced_block(body, referenced);
            }
            StmtKind::IfLet {
                scrutinee,
                then_branch,
                else_branch,
                ..
            } => {
                collect_referenced_expr(scrutinee, referenced);
                collect_referenced_block(then_branch, referenced);
                if let Some(else_branch) = else_branch {
                    collect_referenced_statement(else_branch, referenced);
                }
            }
            StmtKind::Break | StmtKind::Continue => {}
        }
    }
}

fn collect_referenced_statement(statement: &Stmt, referenced: &mut BTreeSet<LocalId>) {
    let block = Block {
        id: statement.id,
        statements: vec![statement.clone()],
        span: statement.span.clone(),
    };
    collect_referenced_block(&block, referenced);
}

fn collect_referenced_expr(expr: &Expr, referenced: &mut BTreeSet<LocalId>) {
    match &expr.kind {
        ExprKind::Ident {
            local: Some(local), ..
        } => {
            referenced.insert(*local);
        }
        ExprKind::Call { callee, args, .. } => {
            collect_referenced_expr(callee, referenced);
            for argument in args {
                collect_referenced_expr(argument, referenced);
            }
        }
        ExprKind::Field { base, .. }
        | ExprKind::Unary { expr: base, .. }
        | ExprKind::TupleField { base, .. }
        | ExprKind::Try { expr: base }
        | ExprKind::Cast { expr: base, .. } => collect_referenced_expr(base, referenced),
        ExprKind::MethodCall { receiver, args, .. } => {
            collect_referenced_expr(receiver, referenced);
            for argument in args {
                collect_referenced_expr(argument, referenced);
            }
        }
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
            collect_referenced_expr(left, referenced);
            collect_referenced_expr(right, referenced);
        }
        ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            collect_referenced_expr(cond, referenced);
            collect_referenced_expr(then_branch, referenced);
            collect_referenced_expr(else_branch, referenced);
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_referenced_expr(scrutinee, referenced);
            for arm in arms {
                collect_referenced_expr(&arm.expr, referenced);
            }
        }
        ExprKind::Block(block) => collect_referenced_block(block, referenced),
        ExprKind::Struct { fields, .. } => {
            for field in fields {
                collect_referenced_expr(&field.value, referenced);
            }
        }
        ExprKind::FormatPrint { args, .. }
        | ExprKind::Format { args, .. }
        | ExprKind::Array { elements: args }
        | ExprKind::Tuple { elements: args } => {
            for argument in args {
                collect_referenced_expr(argument, referenced);
            }
        }
        ExprKind::Closure { captures, .. } => referenced.extend(captures),
        ExprKind::Range { start, end, .. } => {
            if let Some(start) = start {
                collect_referenced_expr(start, referenced);
            }
            if let Some(end) = end {
                collect_referenced_expr(end, referenced);
            }
        }
        ExprKind::Literal(_)
        | ExprKind::Ident { local: None, .. }
        | ExprKind::Path { .. }
        | ExprKind::JsLiteral { .. } => {}
    }
}

fn path_names(path: &[ast::Ident]) -> Vec<String> {
    path.iter().map(|segment| segment.name.clone()).collect()
}

fn format_type(ty: &TypeExpr) -> String {
    match &ty.kind {
        TypeExprKind::Named(name) => name.name.clone(),
        TypeExprKind::Generic { name, args } => format!(
            "{}<{}>",
            name.name,
            args.iter().map(format_type).collect::<Vec<_>>().join(", ")
        ),
        TypeExprKind::Function { params, ret } => format!(
            "fn({}) -> {}",
            params
                .iter()
                .map(format_type)
                .collect::<Vec<_>>()
                .join(", "),
            format_type(ret)
        ),
        TypeExprKind::Array(element) => format!("[{}]", format_type(element)),
        TypeExprKind::Tuple(elements) => format!(
            "({})",
            elements
                .iter()
                .map(format_type)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TypeExprKind::ImplTrait { trait_ty } => {
            format!("impl {}", format_type(trait_ty))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::{ExprKind, LocalId, StmtKind, lower_function};

    #[test]
    fn lowering_assigns_stable_unique_node_and_local_ids() {
        let parsed = husk_ast_file(
            "fn sample(value: i32) -> i32 {\n\
             let doubled = value * 2;\n\
             doubled\n\
             }",
        );
        let husk_ast::ItemKind::Fn { params, body, .. } = &parsed.items[0].kind else {
            panic!("expected function");
        };
        let lowered = lower_function(
            params,
            body,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(lowered.parameters[0].id, LocalId(0));
        assert_eq!(lowered.local_count, 2);

        let ids = lowered
            .body
            .iter()
            .flat_map(statement_node_ids)
            .collect::<Vec<_>>();
        assert_eq!(ids.len(), ids.iter().collect::<HashSet<_>>().len());
        assert_eq!(lowered.node_count as usize, ids.len());
    }

    #[test]
    fn closure_captures_are_precise_and_stably_ordered() {
        let parsed = husk_ast_file(
            "fn sample(outer: i32) -> i32 {\n\
             let offset = 2;\n\
             let factory = |parameter: i32| || outer + offset + parameter;\n\
             factory(3)()\n\
             }",
        );
        let husk_ast::ItemKind::Fn { params, body, .. } = &parsed.items[0].kind else {
            panic!("expected function");
        };
        let lowered = lower_function(
            params,
            body,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        let StmtKind::Let {
            value: Some(factory),
            ..
        } = &lowered.body[1].kind
        else {
            panic!("expected closure binding");
        };
        let ExprKind::Closure {
            params,
            captures,
            body,
            ..
        } = &factory.kind
        else {
            panic!("expected outer closure");
        };
        assert_eq!(captures, &[LocalId(0), LocalId(1)]);

        let ExprKind::Closure {
            captures: nested_captures,
            ..
        } = &body.kind
        else {
            panic!("expected nested closure");
        };
        assert_eq!(
            nested_captures,
            &[LocalId(0), LocalId(1), params[0].local.id]
        );
    }

    fn statement_node_ids(statement: &super::Stmt) -> Vec<u32> {
        let mut ids = vec![statement.id.index()];
        match &statement.kind {
            StmtKind::Let { pattern, value, .. } => {
                ids.push(pattern.id.index());
                if let Some(value) = value {
                    expression_node_ids(value, &mut ids);
                }
            }
            StmtKind::Expr(expr) | StmtKind::Semi(expr) => {
                expression_node_ids(expr, &mut ids);
            }
            _ => {}
        }
        ids
    }

    fn expression_node_ids(expression: &super::Expr, ids: &mut Vec<u32>) {
        ids.push(expression.id.index());
        if let ExprKind::Binary { left, right, .. } = &expression.kind {
            expression_node_ids(left, ids);
            expression_node_ids(right, ids);
        }
    }

    fn husk_ast_file(source: &str) -> husk_ast::File {
        let parsed = husk_parser::parse_str(source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        parsed.file.unwrap()
    }
}
