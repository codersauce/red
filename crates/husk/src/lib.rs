//! Embedded Husk runtime used by Red's plugin system.
//!
//! This crate is intentionally Red-agnostic. The VM owns Husk programs,
//! callbacks, and the small interpreter surface; the host implements editor
//! operations through [`Host`].

use std::collections::HashMap;

use husk_ast::{
    AssignOp, BinaryOp, Block, Expr, ExprKind, ItemKind, LiteralKind, PatternKind, Stmt, StmtKind,
    UnaryOp,
};

/// A dynamically typed value crossing the Husk/host boundary.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Unit,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Json(serde_json::Value),
    Callback(Callback),
}

impl Value {
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }
}

/// A Husk function reference stored for commands and event listeners.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Callback {
    plugin: String,
    function: String,
}

impl Callback {
    #[must_use]
    pub fn new(plugin: impl Into<String>, function: impl Into<String>) -> Self {
        Self {
            plugin: plugin.into(),
            function: function.into(),
        }
    }
}

/// Rust host operations callable from Husk.
pub trait Host {
    fn log(&mut self, message: &str);
    fn execute(&mut self, plugin: &str, action: &str, args: &[Value]) -> anyhow::Result<Value>;
}

/// A parsed Husk plugin program.
#[derive(Debug, Clone)]
pub struct Program {
    functions: HashMap<String, Function>,
}

impl Program {
    /// Parse a Husk source file into a program.
    ///
    /// # Errors
    ///
    /// Returns all parser diagnostics as a single error until the diagnostic
    /// API grows a richer type.
    pub fn parse(name: impl Into<String>, source: &str) -> anyhow::Result<Self> {
        let name = name.into();
        let parsed = husk_parser::parse_str(source);
        if !parsed.errors.is_empty() {
            let errors = parsed
                .errors
                .iter()
                .map(|error| {
                    format!(
                        "{} at {}..{}",
                        error.message, error.span.range.start, error.span.range.end
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::bail!("failed to parse Husk plugin `{name}`:\n{errors}");
        }

        let file = parsed
            .file
            .ok_or_else(|| anyhow::anyhow!("parser did not return a file for `{name}`"))?;
        let mut functions = HashMap::new();
        for item in file.items {
            if let ItemKind::Fn {
                name, params, body, ..
            } = item.kind
            {
                let params = params.into_iter().map(|param| param.name.name).collect();
                functions.insert(name.name, Function { params, body });
            }
        }

        Ok(Self { functions })
    }
}

#[derive(Debug, Clone)]
struct Function {
    params: Vec<String>,
    body: Vec<Stmt>,
}

/// Embedded Husk VM.
#[derive(Debug, Default)]
pub struct Vm {
    programs: HashMap<String, Program>,
    commands: HashMap<String, Callback>,
    event_listeners: HashMap<String, Vec<Callback>>,
    plugin_states: HashMap<String, HashMap<String, Value>>,
    instruction_budget: usize,
}

impl Vm {
    #[must_use]
    pub fn new() -> Self {
        Self {
            instruction_budget: 10_000,
            ..Self::default()
        }
    }

    /// Set the maximum number of statements/expressions run by one callback.
    pub fn set_instruction_budget(&mut self, budget: usize) {
        self.instruction_budget = budget;
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
        let program = Program::parse(name.clone(), source)?;
        self.plugin_states.remove(&name);
        self.programs.insert(name.clone(), program);
        if self.has_function(&name, "activate") {
            self.call_function(&Callback::new(name, "activate"), Vec::new(), host)?;
        }
        Ok(())
    }

    /// Execute a command previously registered by `red::add_command`.
    ///
    /// # Errors
    ///
    /// Returns an error when the command is unknown or the callback fails.
    pub fn execute_command<H: Host>(&mut self, command: &str, host: &mut H) -> anyhow::Result<()> {
        let callback = self
            .commands
            .get(command)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown Husk plugin command `{command}`"))?;
        self.call_function(&callback, Vec::new(), host)?;
        Ok(())
    }

    /// Notify listeners registered by `red::on`.
    ///
    /// # Errors
    ///
    /// Returns the first listener error.
    pub fn notify<H: Host>(
        &mut self,
        event: &str,
        payload: serde_json::Value,
        host: &mut H,
    ) -> anyhow::Result<()> {
        let listeners = self.event_listeners.get(event).cloned().unwrap_or_default();
        for callback in listeners {
            self.call_function(&callback, vec![Value::Json(payload.clone())], host)?;
        }
        Ok(())
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
        self.commands.clear();
        self.event_listeners.clear();
        self.plugin_states.clear();
        Ok(())
    }

    #[must_use]
    pub fn commands(&self) -> &HashMap<String, Callback> {
        &self.commands
    }

    fn has_function(&self, plugin: &str, function: &str) -> bool {
        self.programs
            .get(plugin)
            .is_some_and(|program| program.functions.contains_key(function))
    }

    fn call_function<H: Host>(
        &mut self,
        callback: &Callback,
        args: Vec<Value>,
        host: &mut H,
    ) -> anyhow::Result<Value> {
        let mut frame = Frame {
            plugin: callback.plugin.clone(),
            locals: HashMap::new(),
            remaining: self.instruction_budget,
        };

        let function = self
            .programs
            .get(&callback.plugin)
            .and_then(|program| program.functions.get(&callback.function))
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown Husk function `{}::{}`",
                    callback.plugin,
                    callback.function
                )
            })?;

        for (name, value) in function.params.iter().zip(args) {
            frame.locals.insert(name.clone(), value);
        }

        self.eval_statements(&function.body, &mut frame, host)
    }

    fn eval_statements<H: Host>(
        &mut self,
        statements: &[Stmt],
        frame: &mut Frame,
        host: &mut H,
    ) -> anyhow::Result<Value> {
        let mut value = Value::Unit;
        for statement in statements {
            frame.consume()?;
            match self.eval_statement(statement, frame, host)? {
                Flow::Continue(next) => value = next,
                Flow::Return(result) => return Ok(result),
                Flow::Break => anyhow::bail!("Husk `break` escaped a loop"),
                Flow::LoopContinue => anyhow::bail!("Husk `continue` escaped a loop"),
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
            StmtKind::Let { pattern, value, .. } => {
                if let PatternKind::Binding(name) = &pattern.kind {
                    let value = if let Some(value) = value {
                        self.eval_expr(value, frame, host)?
                    } else {
                        Value::Unit
                    };
                    frame.locals.insert(name.name.clone(), value);
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
                    frame.consume()?;
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
                for item in iterable_values(iterable)? {
                    frame.locals.insert(binding.name.clone(), item);
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
            _ => anyhow::bail!("unsupported Husk statement in embedded runtime"),
        }
    }

    fn eval_block<H: Host>(
        &mut self,
        block: &Block,
        frame: &mut Frame,
        host: &mut H,
    ) -> anyhow::Result<Flow> {
        let mut value = Value::Unit;
        for statement in &block.stmts {
            frame.consume()?;
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
        frame.consume()?;
        match &expr.kind {
            ExprKind::Literal(literal) => match &literal.kind {
                LiteralKind::Bool(value) => Ok(Value::Bool(*value)),
                LiteralKind::Int(value) => Ok(Value::Int(*value)),
                LiteralKind::Float(value) => Ok(Value::Float(*value)),
                LiteralKind::String(value) => Ok(Value::String(value.clone())),
            },
            ExprKind::Ident(ident) => {
                if let Some(value) = frame.locals.get(&ident.name) {
                    return Ok(value.clone());
                }
                if self.has_function(&frame.plugin, &ident.name) {
                    return Ok(Value::Callback(Callback::new(
                        frame.plugin.clone(),
                        ident.name.clone(),
                    )));
                }
                Ok(Value::String(ident.name.clone()))
            }
            ExprKind::Path { segments } => Ok(Value::String(
                segments
                    .iter()
                    .map(|segment| segment.name.as_str())
                    .collect::<Vec<_>>()
                    .join("::"),
            )),
            ExprKind::Call { callee, args, .. } => {
                let callee = self.eval_expr(callee, frame, host)?;
                let args = args
                    .iter()
                    .map(|arg| self.eval_expr(arg, frame, host))
                    .collect::<anyhow::Result<Vec<_>>>()?;
                self.call_value(callee, args, frame, host)
            }
            ExprKind::Field { base, member } => {
                let base = self.eval_expr(base, frame, host)?;
                self.field_value(base, &member.name)
            }
            ExprKind::Array { elements } => {
                let elements = elements
                    .iter()
                    .map(|element| {
                        self.eval_expr(element, frame, host)
                            .map(|value| value_to_json(&value))
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;
                Ok(Value::Json(serde_json::Value::Array(elements)))
            }
            ExprKind::Struct { name: _, fields } => {
                let mut object = serde_json::Map::new();
                for field in fields {
                    let value = self.eval_expr(&field.value, frame, host)?;
                    object.insert(field.name.name.clone(), value_to_json(&value));
                }
                Ok(Value::Json(serde_json::Value::Object(object)))
            }
            ExprKind::Index { base, index } => {
                let base = self.eval_expr(base, frame, host)?;
                let index = self.eval_expr(index, frame, host)?;
                self.index_value(base, index)
            }
            ExprKind::Unary { op, expr } => {
                let value = self.eval_expr(expr, frame, host)?;
                self.eval_unary(*op, value)
            }
            ExprKind::Binary { op, left, right } => self.eval_binary(*op, left, right, frame, host),
            ExprKind::Block(block) => self.eval_statements(&block.stmts, frame, host),
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
            ExprKind::Assign { target, op, value } => {
                self.eval_assignment(target, *op, value, frame, host)
            }
            _ => anyhow::bail!("unsupported Husk expression in embedded runtime"),
        }
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
            ExprKind::Ident(ident) => {
                let left = frame
                    .locals
                    .get(&ident.name)
                    .cloned()
                    .unwrap_or(Value::Unit);
                let assigned = self.assignment_value(op, left, right)?;
                frame.locals.insert(ident.name.clone(), assigned.clone());
                Ok(assigned)
            }
            ExprKind::Field { base, member } => {
                let ExprKind::Ident(ident) = &base.kind else {
                    anyhow::bail!(
                        "embedded Husk runtime only supports assignment through a local binding"
                    );
                };
                let base = frame
                    .locals
                    .get(&ident.name)
                    .cloned()
                    .unwrap_or(Value::Unit);
                let Value::Json(serde_json::Value::Object(mut object)) = base else {
                    anyhow::bail!("cannot assign field on a non-object");
                };
                let left = object.get(&member.name).map_or(Value::Unit, json_to_value);
                let assigned = self.assignment_value(op, left, right)?;
                object.insert(member.name.clone(), value_to_json(&assigned));
                frame.locals.insert(
                    ident.name.clone(),
                    Value::Json(serde_json::Value::Object(object)),
                );
                Ok(assigned)
            }
            ExprKind::Index { base, index } => {
                let ExprKind::Ident(ident) = &base.kind else {
                    anyhow::bail!(
                        "embedded Husk runtime only supports assignment through a local binding"
                    );
                };
                let index = self.eval_expr(index, frame, host)?;
                let base = frame
                    .locals
                    .get(&ident.name)
                    .cloned()
                    .unwrap_or(Value::Unit);
                let (updated, assigned) = match (base, index) {
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
                frame.locals.insert(ident.name.clone(), updated);
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
            BinaryOp::Div => numeric_binary(left, right, |left, right| left / right),
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

    fn field_value(&self, base: Value, member: &str) -> anyhow::Result<Value> {
        match base {
            Value::Json(value) => {
                let Some(field) = value.get(member) else {
                    return Ok(Value::Unit);
                };
                Ok(json_to_value(field))
            }
            value => anyhow::bail!("cannot read field `{member}` from Husk value `{value:?}`"),
        }
    }

    fn index_value(&self, base: Value, index: Value) -> anyhow::Result<Value> {
        match (base, index) {
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

    fn call_value<H: Host>(
        &mut self,
        callee: Value,
        args: Vec<Value>,
        frame: &Frame,
        host: &mut H,
    ) -> anyhow::Result<Value> {
        match callee {
            Value::String(name) => self.call_named(&name, args, frame, host),
            Value::Callback(callback) => self.call_function(&callback, args, host),
            _ => anyhow::bail!("value is not callable: {callee:?}"),
        }
    }

    fn call_named<H: Host>(
        &mut self,
        name: &str,
        args: Vec<Value>,
        frame: &Frame,
        host: &mut H,
    ) -> anyhow::Result<Value> {
        match name {
            "red::add_command" => {
                let command = required_string(&args, 0, name)?;
                let callback = required_callback(&args, 1, name)?;
                self.commands.insert(command.to_string(), callback.clone());
                Ok(Value::Unit)
            }
            "red::on" => {
                let event = required_string(&args, 0, name)?;
                let callback = required_callback(&args, 1, name)?;
                self.event_listeners
                    .entry(event.to_string())
                    .or_default()
                    .push(callback.clone());
                Ok(Value::Unit)
            }
            "red::execute" => {
                let action = required_string(&args, 0, name)?;
                host.execute(&frame.plugin, action, &args[1..])
            }
            "red::log" => {
                let message = args
                    .iter()
                    .map(value_to_log_string)
                    .collect::<Vec<_>>()
                    .join(" ");
                host.log(&message);
                Ok(Value::Unit)
            }
            "red::state_bool" => {
                let key = required_string(&args, 0, name)?;
                Ok(Value::Bool(
                    self.plugin_state_value(&frame.plugin, key)
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                ))
            }
            "red::state_set" => {
                let key = required_string(&args, 0, name)?.to_string();
                let value = args.get(1).cloned().unwrap_or(Value::Unit);
                self.plugin_states
                    .entry(frame.plugin.clone())
                    .or_default()
                    .insert(key, value);
                Ok(Value::Unit)
            }
            "red::state" => {
                let key = required_string(&args, 0, name)?;
                Ok(self
                    .plugin_state_value(&frame.plugin, key)
                    .cloned()
                    .unwrap_or(Value::Unit))
            }
            "red::push" => {
                let mut values = match args.first() {
                    Some(Value::Json(serde_json::Value::Array(values))) => values.clone(),
                    Some(value) => {
                        anyhow::bail!("`{name}` argument 0 must be an array, got {value:?}")
                    }
                    None => anyhow::bail!("`{name}` requires an array argument"),
                };
                values.push(args.get(1).map_or(serde_json::Value::Null, value_to_json));
                Ok(Value::Json(serde_json::Value::Array(values)))
            }
            "red::contains" => {
                let values = required_array(&args, 0, name)?;
                let needle = args.get(1).map_or(serde_json::Value::Null, value_to_json);
                Ok(Value::Bool(values.contains(&needle)))
            }
            "red::remove" => {
                let values = required_array(&args, 0, name)?;
                let needle = args.get(1).map_or(serde_json::Value::Null, value_to_json);
                Ok(Value::Json(serde_json::Value::Array(
                    values
                        .iter()
                        .filter(|value| **value != needle)
                        .cloned()
                        .collect(),
                )))
            }
            "red::len" => {
                let length = match args.first() {
                    Some(Value::String(value)) => value.chars().count(),
                    Some(Value::Json(serde_json::Value::Array(values))) => values.len(),
                    Some(Value::Json(serde_json::Value::Object(values))) => values.len(),
                    Some(Value::Unit) | None => 0,
                    Some(value) => anyhow::bail!("`{name}` argument 0 has no length: {value:?}"),
                };
                Ok(Value::Int(i64::try_from(length).unwrap_or(i64::MAX)))
            }
            "red::int" => {
                let fallback = args.get(1).and_then(value_to_i64).unwrap_or(0);
                Ok(Value::Int(
                    args.first().and_then(value_to_i64).unwrap_or(fallback),
                ))
            }
            "red::bool" => {
                let fallback = args.get(1).and_then(Value::as_bool).unwrap_or(false);
                Ok(Value::Bool(
                    args.first().and_then(value_to_bool).unwrap_or(fallback),
                ))
            }
            "red::string" => {
                let fallback = args.get(1).map(value_to_log_string).unwrap_or_default();
                Ok(Value::String(
                    args.first()
                        .and_then(value_to_plain_string)
                        .unwrap_or(fallback),
                ))
            }
            "red::text_field" => {
                let text = args
                    .first()
                    .and_then(value_to_json_object)
                    .and_then(|field| {
                        field
                            .get("text")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                            .or_else(|| {
                                field
                                    .get("bytes")
                                    .and_then(serde_json::Value::as_str)
                                    .and_then(decode_base64)
                                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                            })
                    })
                    .unwrap_or_default();
                Ok(Value::String(text))
            }
            "red::utf8_byte_to_char_index" => {
                let text = required_string(&args, 0, name)?;
                let offset = args.get(1).and_then(value_to_i64).unwrap_or(0);
                let offset = usize::try_from(offset).unwrap_or(0);
                let index = text
                    .char_indices()
                    .take_while(|(byte_index, _)| *byte_index < offset)
                    .count();
                Ok(Value::Int(i64::try_from(index).unwrap_or(i64::MAX)))
            }
            "red::char_at" => {
                let value = required_string(&args, 0, name)?;
                let index = args.get(1).and_then(value_to_i64).unwrap_or(0);
                let character = usize::try_from(index)
                    .ok()
                    .and_then(|index| value.chars().nth(index))
                    .map_or_else(String::new, |character| character.to_string());
                Ok(Value::String(character))
            }
            "red::trim" => {
                let value = required_string(&args, 0, name)?;
                Ok(Value::String(value.trim().to_string()))
            }
            "red::lower" => {
                let value = required_string(&args, 0, name)?;
                Ok(Value::String(value.to_lowercase()))
            }
            "red::split" => {
                let value = required_string(&args, 0, name)?;
                let delimiter = required_string(&args, 1, name)?;
                Ok(Value::Json(serde_json::Value::Array(
                    value
                        .split(delimiter)
                        .map(|part| serde_json::Value::String(part.to_string()))
                        .collect(),
                )))
            }
            "red::starts_with" => {
                let value = required_string(&args, 0, name)?;
                let prefix = required_string(&args, 1, name)?;
                Ok(Value::Bool(value.starts_with(prefix)))
            }
            "red::ends_with" => {
                let value = required_string(&args, 0, name)?;
                let suffix = required_string(&args, 1, name)?;
                Ok(Value::Bool(value.ends_with(suffix)))
            }
            "red::replace_all" => {
                let value = required_string(&args, 0, name)?;
                let from = required_string(&args, 1, name)?;
                let to = required_string(&args, 2, name)?;
                Ok(Value::String(value.replace(from, to)))
            }
            "red::trim_line_end" => {
                let value = required_string(&args, 0, name)?;
                Ok(Value::String(
                    value
                        .strip_suffix("\r\n")
                        .or_else(|| value.strip_suffix('\n'))
                        .unwrap_or(value)
                        .to_string(),
                ))
            }
            "red::slice" => {
                let value = required_string(&args, 0, name)?;
                let len = i64::try_from(value.chars().count()).unwrap_or(i64::MAX);
                let start = args.get(1).and_then(value_to_i64).unwrap_or(0);
                let end = args.get(2).and_then(value_to_i64).unwrap_or(len);
                let start = normalize_string_index(start, len);
                let end = normalize_string_index(end, len);
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
                let value = required_string(&args, 0, name)?;
                Ok(Value::Bool(value.chars().all(char::is_whitespace)))
            }
            "red::char" => {
                let codepoint = args.first().and_then(value_to_i64).unwrap_or(0);
                let value = u32::try_from(codepoint)
                    .ok()
                    .and_then(char::from_u32)
                    .map_or_else(String::new, |character| character.to_string());
                Ok(Value::String(value))
            }
            "red::null" => Ok(Value::Unit),
            "red::parse_json" => {
                let value = required_string(&args, 0, name)?;
                Ok(serde_json::from_str(value)
                    .map(Value::Json)
                    .unwrap_or(Value::Unit))
            }
            function if self.has_function(&frame.plugin, function) => self.call_function(
                &Callback::new(frame.plugin.clone(), function.to_string()),
                args,
                host,
            ),
            _ => anyhow::bail!("unknown Husk function `{name}`"),
        }
    }

    fn plugin_state_value(&self, plugin: &str, key: &str) -> Option<&Value> {
        self.plugin_states.get(plugin)?.get(key)
    }
}

fn iterable_values(value: Value) -> anyhow::Result<Vec<Value>> {
    match value {
        Value::Json(serde_json::Value::Array(values)) => {
            Ok(values.iter().map(json_to_value).collect())
        }
        Value::String(value) => Ok(value
            .chars()
            .map(|value| Value::String(value.to_string()))
            .collect()),
        value => anyhow::bail!("Husk `for` requires an iterable value, got {value:?}"),
    }
}

fn json_to_value(value: &serde_json::Value) -> Value {
    match value {
        serde_json::Value::Null => Value::Unit,
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
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => Value::Json(value.clone()),
    }
}

fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Unit => serde_json::Value::Null,
        Value::Bool(value) => serde_json::Value::Bool(*value),
        Value::Int(value) => serde_json::Value::Number((*value).into()),
        Value::Float(value) => serde_json::Number::from_f64(*value)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::String(value) => serde_json::Value::String(value.clone()),
        Value::Json(value) => value.clone(),
        Value::Callback(callback) => {
            serde_json::Value::String(format!("{}::{}", callback.plugin, callback.function))
        }
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

fn value_to_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(value) => Some(*value),
        Value::Json(serde_json::Value::Bool(value)) => Some(*value),
        _ => None,
    }
}

fn value_to_plain_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Json(serde_json::Value::String(value)) => Some(value.clone()),
        _ => None,
    }
}

fn value_to_json_object(value: &Value) -> Option<&serde_json::Map<String, serde_json::Value>> {
    match value {
        Value::Json(serde_json::Value::Object(object)) => Some(object),
        _ => None,
    }
}

fn decode_base64(encoded: &str) -> Option<Vec<u8>> {
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

fn normalize_string_index(index: i64, len: i64) -> i64 {
    if index < 0 {
        (len + index).clamp(0, len)
    } else {
        index.clamp(0, len)
    }
}

#[derive(Debug)]
struct Frame {
    plugin: String,
    locals: HashMap<String, Value>,
    remaining: usize,
}

impl Frame {
    fn consume(&mut self) -> anyhow::Result<()> {
        if self.remaining == 0 {
            anyhow::bail!("Husk instruction budget exhausted");
        }
        self.remaining -= 1;
        Ok(())
    }
}

enum Flow {
    Continue(Value),
    Return(Value),
    Break,
    LoopContinue,
}

fn required_string<'a>(args: &'a [Value], index: usize, function: &str) -> anyhow::Result<&'a str> {
    args.get(index)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("`{function}` argument {index} must be a string"))
}

fn required_callback<'a>(
    args: &'a [Value],
    index: usize,
    function: &str,
) -> anyhow::Result<&'a Callback> {
    match args.get(index) {
        Some(Value::Callback(callback)) => Ok(callback),
        _ => anyhow::bail!("`{function}` argument {index} must be a function callback"),
    }
}

fn required_array<'a>(
    args: &'a [Value],
    index: usize,
    function: &str,
) -> anyhow::Result<&'a [serde_json::Value]> {
    match args.get(index) {
        Some(Value::Json(serde_json::Value::Array(values))) => Ok(values),
        _ => anyhow::bail!("`{function}` argument {index} must be an array"),
    }
}

fn value_to_log_string(value: &Value) -> String {
    match value {
        Value::Unit => "()".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Int(value) => value.to_string(),
        Value::Float(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Json(value) => value.to_string(),
        Value::Callback(callback) => format!("{}::{}", callback.plugin, callback.function),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestHost {
        logs: Vec<String>,
        actions: Vec<(String, Vec<Value>)>,
    }

    impl Host for TestHost {
        fn log(&mut self, message: &str) {
            self.logs.push(message.to_string());
        }

        fn execute(
            &mut self,
            _plugin: &str,
            action: &str,
            args: &[Value],
        ) -> anyhow::Result<Value> {
            self.actions.push((action.to_string(), args.to_vec()));
            Ok(Value::Unit)
        }
    }

    #[test]
    fn loads_and_executes_registered_command() {
        let source = r#"
            pub fn activate() {
                red::add_command("Hello", hello);
            }

            fn hello() {
                red::execute("Print", "hello from husk");
            }
        "#;
        let mut host = TestHost::default();
        let mut vm = Vm::new();

        vm.load_plugin("test", source, &mut host).unwrap();
        vm.execute_command("Hello", &mut host).unwrap();

        assert_eq!(
            host.actions,
            vec![(
                "Print".to_string(),
                vec![Value::String("hello from husk".to_string())]
            )]
        );
    }

    #[test]
    fn notifies_registered_event_listener() {
        let source = r#"
            pub fn activate() {
                red::on("editor:ready", ready);
            }

            fn ready(event: Json) {
                red::log("ready");
            }
        "#;
        let mut host = TestHost::default();
        let mut vm = Vm::new();

        vm.load_plugin("test", source, &mut host).unwrap();
        vm.notify("editor:ready", serde_json::json!({}), &mut host)
            .unwrap();

        assert_eq!(host.logs, vec!["ready".to_string()]);
    }

    #[test]
    fn evaluates_stateful_event_logic() {
        let source = r#"
            pub fn activate() {
                red::on("search:highlighted", search_highlighted);
                red::on("search:cleared", search_cleared);
                red::on("cursor:moved", cursor_moved);
            }

            fn search_highlighted(event: Json) {
                red::state_set("highlight_active", true);
            }

            fn search_cleared(event: Json) {
                red::state_set("highlight_active", false);
            }

            fn cursor_moved(event: Json) {
                if !red::state_bool("highlight_active") {
                    return;
                }

                if event.mode != "Normal" {
                    return;
                }

                if is_search_cause(event.cause) {
                    return;
                }

                clear();
            }

            fn clear() {
                if !red::state_bool("highlight_active") {
                    return;
                }

                red::state_set("highlight_active", false);
                red::execute("ClearSearchHighlight");
            }

            fn is_search_cause(cause: String) -> bool {
                return cause == "FindNext" || cause == "RepeatSearch";
            }
        "#;
        let mut host = TestHost::default();
        let mut vm = Vm::new();

        vm.load_plugin("cool_search", source, &mut host).unwrap();
        vm.notify("search:highlighted", serde_json::json!({}), &mut host)
            .unwrap();
        vm.notify(
            "cursor:moved",
            serde_json::json!({
                "mode": "Normal",
                "cause": "FindNext",
            }),
            &mut host,
        )
        .unwrap();

        assert!(host.actions.is_empty());

        vm.notify(
            "cursor:moved",
            serde_json::json!({
                "mode": "Normal",
                "cause": "MoveRight",
            }),
            &mut host,
        )
        .unwrap();

        assert_eq!(
            host.actions,
            vec![("ClearSearchHighlight".to_string(), Vec::new())]
        );

        host.actions.clear();
        vm.notify(
            "cursor:moved",
            serde_json::json!({
                "mode": "Normal",
                "cause": "MoveRight",
            }),
            &mut host,
        )
        .unwrap();

        assert!(host.actions.is_empty());
    }

    #[test]
    fn evaluates_arrays_objects_loops_assignment_and_indexing() {
        let source = r#"
            pub fn activate() {
                red::on("symbols", symbols);
            }

            fn symbols(event: Json) {
                let items = [];
                for symbol in event.symbols {
                    items = red::push(items, PickerItem {
                        id: symbol.kindName + ":" + symbol.name,
                        label: symbol.kindName + " " + symbol.name,
                        detail: symbol.file + ":" + (symbol.line + 1),
                        data: Json { symbol: symbol },
                    });
                }

                red::execute("OpenDynamicPicker", "Symbols", 7, items, PickerOptions {
                    status: red::len(items) + " symbols",
                });
                red::execute("OpenFirst", items[0]);
            }
        "#;
        let mut host = TestHost::default();
        let mut vm = Vm::new();

        vm.load_plugin("symbols", source, &mut host).unwrap();
        vm.notify(
            "symbols",
            serde_json::json!({
                "symbols": [{
                    "kindName": "Function",
                    "name": "main",
                    "file": "src/main.rs",
                    "line": 4
                }]
            }),
            &mut host,
        )
        .unwrap();

        assert_eq!(host.actions.len(), 2);
        assert_eq!(host.actions[0].0, "OpenDynamicPicker");
        assert_eq!(
            host.actions[0].1,
            vec![
                Value::String("Symbols".to_string()),
                Value::Int(7),
                Value::Json(serde_json::json!([{
                    "id": "Function:main",
                    "label": "Function main",
                    "detail": "src/main.rs:5",
                    "data": {
                        "symbol": {
                            "kindName": "Function",
                            "name": "main",
                            "file": "src/main.rs",
                            "line": 4
                        }
                    }
                }])),
                Value::Json(serde_json::json!({
                    "status": "1 symbols"
                }))
            ]
        );
        assert_eq!(
            host.actions[1],
            (
                "OpenFirst".to_string(),
                vec![Value::Json(serde_json::json!({
                    "id": "Function:main",
                    "label": "Function main",
                    "detail": "src/main.rs:5",
                    "data": {
                        "symbol": {
                            "kindName": "Function",
                            "name": "main",
                            "file": "src/main.rs",
                            "line": 4
                        }
                    }
                }))]
            )
        );
    }

    #[test]
    fn decodes_ripgrep_text_fields_and_byte_offsets() {
        let source = r#"
            pub fn activate() {
                red::on("match", matched);
            }

            fn matched(event: Json) {
                let text = red::text_field(event.field);
                red::execute("Result", text, red::utf8_byte_to_char_index(text, event.offset), red::trim_line_end(event.line));
            }
        "#;
        let mut host = TestHost::default();
        let mut vm = Vm::new();

        vm.load_plugin("match", source, &mut host).unwrap();
        vm.notify(
            "match",
            serde_json::json!({
                "field": { "bytes": "YcOpIG5lZWRsZQ==" },
                "offset": 4,
                "line": "line\r\n",
            }),
            &mut host,
        )
        .unwrap();

        assert_eq!(
            host.actions,
            vec![(
                "Result".to_string(),
                vec![
                    Value::String("aé needle".to_string()),
                    Value::Int(3),
                    Value::String("line".to_string()),
                ],
            )]
        );
    }
}
