//! Embedded Husk runtime used by Red's plugin system.
//!
//! This crate is intentionally Red-agnostic. The VM owns Husk programs,
//! callbacks, and the small interpreter surface; the host implements editor
//! operations through [`Host`].

use std::collections::HashMap;

use husk_ast::{Expr, ExprKind, ItemKind, LiteralKind, PatternKind, Stmt, StmtKind};

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
    fn execute(&mut self, action: &str, args: &[Value]) -> anyhow::Result<Value>;
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
            StmtKind::Block(block) => {
                let value = self.eval_statements(&block.stmts, frame, host)?;
                Ok(Flow::Continue(value))
            }
            _ => anyhow::bail!("unsupported Husk statement in embedded runtime"),
        }
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
            ExprKind::Block(block) => self.eval_statements(&block.stmts, frame, host),
            _ => anyhow::bail!("unsupported Husk expression in embedded runtime"),
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
                host.execute(action, &args[1..])
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
            function if self.has_function(&frame.plugin, function) => self.call_function(
                &Callback::new(frame.plugin.clone(), function.to_string()),
                args,
                host,
            ),
            _ => anyhow::bail!("unknown Husk function `{name}`"),
        }
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

        fn execute(&mut self, action: &str, args: &[Value]) -> anyhow::Result<Value> {
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
}
