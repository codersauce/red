//! Test-only driver for the worker's message handlers. It feeds Codex JSON-RPC
//! responses to the real handlers and returns the events they emit, so tests
//! elsewhere in the crate can pin what the plugin sees without naming the
//! payload types.

use super::*;

struct NoHost;

#[async_trait]
impl CodexToolHost for NoHost {
    async fn read_file(&mut self, _: &str, _: &str, _: usize, _: usize) -> Result<Value> {
        unreachable!("golden support never runs tools")
    }

    async fn write_file(&mut self, _: &str, _: &str, _: u64, _: String) -> Result<Value> {
        unreachable!("golden support never runs tools")
    }

    async fn editor_tool(&mut self, _: EditorToolRequest) -> Result<Value> {
        unreachable!("golden support never runs tools")
    }
}

pub(crate) struct Harness {
    events: mpsc::Sender<CodexEvent>,
    received: mpsc::Receiver<CodexEvent>,
    pending: HashMap<String, Pending>,
    sessions: HashMap<String, Session>,
    next_id: u64,
    output: Vec<u8>,
    answered: usize,
    host: Arc<Mutex<NoHost>>,
    internal: mpsc::Sender<InternalEvent>,
}

impl Harness {
    pub(crate) fn new() -> Self {
        let (events, received) = mpsc::channel(1024);
        let (internal, _) = mpsc::channel(8);
        Self {
            events,
            received,
            pending: HashMap::new(),
            sessions: HashMap::new(),
            next_id: 1,
            output: Vec::new(),
            answered: 0,
            host: Arc::new(Mutex::new(NoHost)),
            internal,
        }
    }

    /// Registers a visible agent conversation with a running turn.
    pub(crate) fn add_agent_session(
        &mut self,
        id: &str,
        cwd: &str,
        active_turn: Option<&str>,
        model: Option<(&str, &str)>,
    ) {
        self.sessions.insert(
            id.to_string(),
            Session {
                model_info: model.map(|(model, effort)| ModelInfo {
                    model: model.to_string(),
                    provider: None,
                    effort: Some(effort.to_string()),
                }),
                cwd: PathBuf::from(cwd),
                active_turn: active_turn.map(str::to_string),
                pending_interrupt_turn_id: None,
                cancelled: Arc::new(AtomicBool::new(false)),
                allow_sensitive_paths: false,
                kind: SessionKind::Agent,
            },
        );
    }

    pub(crate) async fn command(&mut self, command: CodexCommand) {
        handle_command(
            command,
            &mut self.output,
            &self.events,
            &mut self.pending,
            &mut self.sessions,
            &mut self.next_id,
        )
        .await
        .unwrap();
    }

    /// Delivers one app-server message (response or notification).
    pub(crate) async fn message(&mut self, message: Value) {
        handle_message(
            message,
            &mut self.output,
            &self.events,
            &mut self.pending,
            &mut self.sessions,
            &mut self.next_id,
            &AgentRuntimePolicy::default(),
            Arc::clone(&self.host),
            self.internal.clone(),
        )
        .await
        .unwrap();
    }

    /// Answers every request the worker writes, until it writes no more.
    /// `responder` gets the method and params and returns the response body:
    /// `{"result": ...}` or `{"error": ...}`.
    pub(crate) async fn serve(&mut self, mut responder: impl FnMut(&str, &Value) -> Value) {
        loop {
            let requests = String::from_utf8(self.output.clone())
                .unwrap()
                .lines()
                .skip(self.answered)
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            let Some(request) = requests.into_iter().next() else {
                return;
            };
            self.answered += 1;
            let method = request["method"].as_str().unwrap().to_string();
            let mut response = responder(&method, &request["params"]);
            response["id"] = request["id"].clone();
            self.message(response).await;
        }
    }

    pub(crate) fn events(&mut self) -> Vec<CodexEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.received.try_recv() {
            events.push(event);
        }
        events
    }
}
