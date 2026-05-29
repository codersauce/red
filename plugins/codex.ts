/// <reference path="../types/red.d.ts" />

const LEGACY_WINDOW_ID = "chat";
const FOLLOW_OVERLAY_ID = "codex.followChanges";
const LARGE_PASTE_CHAR_THRESHOLD = 1000;
const LEGACY_STORAGE_KEY = "codex.chat";
const STORAGE_KEY_PREFIX = "codex.chat.";
const DISCONNECTED_ACTION_HINT = "Codex is disconnected. Run codex.reconnect before sending another prompt.";

type Mode = "composer" | "transcript";
type ConnectionState = "unknown" | "connecting" | "ready" | "disconnected";

interface ContextAttachment {
  label: string;
  content: string;
  path?: string;
  startLine?: number;
  endLine?: number;
}

interface PendingCodexRequest {
  key: string;
  streamId: string;
  requestId: any;
  method: string;
  params: any;
}

interface ComposerPosition {
  line: number;
  column: number;
}

interface State {
  windowId: string;
  open: boolean;
  mode: Mode;
  composerLines: string[];
  cursorLine: number;
  cursorColumn: number;
  selectionAnchor?: ComposerPosition;
  transcriptScroll: number;
  contextAttachments: ContextAttachment[];
  threadId?: string;
  projectCwd?: string;
  inFlight: boolean;
  followChanges: boolean;
  connection: ConnectionState;
  transcript: Red.PluginWindowLine[];
  status: string;
  activeStreamId?: string;
  activeAgentLine?: number;
  activeAgentText: string;
  activeNotifications: any[];
  conflictedPaths: string[];
  pendingRequestKeys: string[];
  pendingRequests: PendingCodexRequest[];
  loadedTranscriptThreadId?: string;
  lastFollowedPath?: string;
  lastFollowedLocation?: string;
}

const states = new Map<string, State>();
const registeredWindowIds = new Set<string>();
let state = createState(LEGACY_WINDOW_ID);
states.set(state.windowId, state);

function createState(windowId: string, projectCwd?: string): State {
  return {
    windowId,
    open: false,
    mode: "composer",
    composerLines: [""],
    cursorLine: 0,
    cursorColumn: 0,
    transcriptScroll: 0,
    contextAttachments: [],
    projectCwd,
    inFlight: false,
    followChanges: false,
    connection: "unknown",
    transcript: [
      { text: "Codex Chat Window" },
      { text: "Ask Codex a question or attach editor context before sending." },
    ],
    status: "local preview",
    activeAgentText: "",
    activeNotifications: [],
    conflictedPaths: [],
    pendingRequestKeys: [],
    pendingRequests: [],
  };
}

export async function activate(red: Red.RedAPI): Promise<void> {
  registerCommands(red);
  registerWindowEvent(red, LEGACY_WINDOW_ID);

  red.on("editor:ready", () => {
    void restoreStoredThread(red, undefined, { loadTranscript: true }).catch((error) => {
      red.logWarn("Codex thread restore failed", String(error));
    });
    red.logInfo(
      "Codex plugin loaded. Run :codex.open or bind PluginCommand codex.open.",
    );
  });
}

function registerWindowEvent(red: Red.RedAPI, windowId: string): void {
  if (registeredWindowIds.has(windowId)) {
    return;
  }
  registeredWindowIds.add(windowId);
  red.onPluginWindowEvent(windowId, (event) => {
    state = stateForWindow(windowId);
    handleWindowEvent(red, event);
  });
}

async function activateCurrentWorkspaceState(
  red: Red.RedAPI,
  snapshot?: Red.EditorStateSnapshot,
): Promise<State> {
  const workspaceRoot = await currentWorkspaceRoot(red, snapshot);
  state = stateForWorkspaceRoot(workspaceRoot);
  return state;
}

function stateForWindow(windowId: string): State {
  let chatState = states.get(windowId);
  if (!chatState) {
    chatState = createState(windowId);
    states.set(windowId, chatState);
  }
  return chatState;
}

function stateForWorkspaceRoot(workspaceRoot: string): State {
  const normalizedRoot = normalizePath(workspaceRoot);
  const windowId = windowIdForWorkspaceRoot(normalizedRoot);
  const chatState = stateForWindow(windowId);
  chatState.projectCwd ??= normalizedRoot;
  return chatState;
}

function windowIdForWorkspaceRoot(workspaceRoot: string): string {
  return `chat-${stableHash(workspaceRoot)}`;
}

function storageKeyForWorkspaceRoot(workspaceRoot: string): string {
  return `${STORAGE_KEY_PREFIX}${stableHash(workspaceRoot)}`;
}

export function __testRootScopedCodexIds(workspaceRoot: string): {
  windowId: string;
  storageKey: string;
} {
  const normalizedRoot = normalizePath(workspaceRoot);
  return {
    windowId: windowIdForWorkspaceRoot(normalizedRoot),
    storageKey: storageKeyForWorkspaceRoot(normalizedRoot),
  };
}

function stableHash(value: string): string {
  let hash = 2166136261;
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return (hash >>> 0).toString(36);
}

function registerCommands(red: Red.RedAPI): void {
  registerCommand(red, "codex.open", () => openAndRestore(red), {
    title: "Open Codex Chat",
    description: "Open or focus the Codex chat window for this workspace.",
    suggestedKeys: ["Space c"],
    context: ["editor", "plugin-window"],
  });
  registerCommand(red, "codex.cancel", () => cancelActiveTurn(red), {
    title: "Cancel Codex Turn",
    description: "Cancel a pending Codex request or interrupt the active streamed turn.",
    suggestedKeys: ["Ctrl-c"],
    context: ["codex-chat"],
  });
  registerCommand(red, "codex.reconnect", () => reconnectCodex(red), {
    title: "Reconnect Codex",
    description: "Check the Codex app-server connection and clear a disconnected chat state.",
    context: ["editor", "codex-chat"],
  });
  registerCommand(red, "codex.approveRequest", () => resolveLatestCodexRequest(red, "accept"), {
    title: "Approve Codex Request",
    description: "Approve the latest pending Codex app-server request.",
    context: ["codex-chat"],
  });
  registerCommand(red, "codex.approveRequestForSession", () => resolveLatestCodexRequest(red, "acceptForSession"), {
    title: "Approve Codex Request For Session",
    description: "Approve the latest pending Codex app-server request for the session when supported.",
    context: ["codex-chat"],
  });
  registerCommand(red, "codex.declineRequest", () => resolveLatestCodexRequest(red, "decline"), {
    title: "Decline Codex Request",
    description: "Decline the latest pending Codex app-server request.",
    context: ["codex-chat"],
  });
  registerCommand(red, "codex.cancelRequest", () => resolveLatestCodexRequest(red, "cancel"), {
    title: "Cancel Codex Request",
    description: "Cancel the latest pending Codex app-server request and interrupt the turn.",
    context: ["codex-chat"],
  });
  registerCommand(red, "codex.answerRequest", () => answerLatestUserInputRequest(red), {
    title: "Answer Codex Input Request",
    description: "Use the composer text to answer the latest Codex input request.",
    context: ["codex-chat"],
  });
  registerCommand(red, "codex.attachCurrentLine", () => addCurrentLineContext(red), {
    title: "Attach Current Line",
    description: "Snapshot the current editor line as Codex context.",
    context: ["editor"],
  });
  registerCommand(red, "codex.attachCurrentFile", () => addCurrentFileContext(red), {
    title: "Attach Current File",
    description: "Snapshot the current editor file as Codex context.",
    context: ["editor"],
  });
  registerCommand(red, "codex.attachSelection", () => addSelectionContext(red), {
    title: "Attach Selection",
    description: "Snapshot the current visual selection as Codex context.",
    context: ["editor"],
  });
  registerCommand(red, "codex.attachGitDiff", () => addGitDiffContext(red), {
    title: "Attach Git Diff",
    description: "Snapshot the current workspace git diff as Codex context.",
    context: ["editor"],
  });
  registerCommand(red, "codex.attachDiagnostics", () => addDiagnosticsContext(red), {
    title: "Attach Diagnostics",
    description: "Snapshot current-buffer diagnostics as Codex context.",
    context: ["editor"],
  });
  registerCommand(red, "codex.attachOpenBuffers", () => addOpenBuffersContext(red), {
    title: "Attach Open Buffers",
    description: "Snapshot open file-backed buffers as Codex context.",
    context: ["editor"],
  });
  registerCommand(red, "codex.sessions.list", () => listProjectSessions(red), {
    title: "List Codex Sessions",
    description: "List Codex sessions stored for the current workspace root.",
    context: ["editor", "codex-chat"],
  });
  registerCommand(red, "codex.resume", () => resumeProjectSession(red), {
    title: "Resume Codex Session",
    description: "Pick and resume a Codex session for the current workspace root.",
    context: ["editor", "codex-chat"],
  });
  registerCommand(red, "codex.toggleFollowChanges", () => toggleFollowChanges(red), {
    title: "Toggle Follow Changes",
    description: "Toggle live Codex change updates in the editor overlay.",
    context: ["editor", "codex-chat"],
  });

  registerCommandAlias(red, "codex.context.currentLine", "codex.attachCurrentLine", () =>
    addCurrentLineContext(red),
  );
  registerCommandAlias(red, "codex.context.currentFile", "codex.attachCurrentFile", () =>
    addCurrentFileContext(red),
  );
  registerCommandAlias(red, "codex.context.gitDiff", "codex.attachGitDiff", () =>
    addGitDiffContext(red),
  );
  registerCommandAlias(red, "codex.context.diagnostics", "codex.attachDiagnostics", () =>
    addDiagnosticsContext(red),
  );
  registerCommandAlias(red, "codex.context.openBuffers", "codex.attachOpenBuffers", () =>
    addOpenBuffersContext(red),
  );
  registerCommandAlias(red, "codex.sessions.resume", "codex.resume", () =>
    resumeProjectSession(red),
  );
  registerCommandAlias(red, "codex.followChanges.toggle", "codex.toggleFollowChanges", () =>
    toggleFollowChanges(red),
  );
  registerCommandAlias(red, "codex.appServer.reconnect", "codex.reconnect", () =>
    reconnectCodex(red),
  );
  registerCommandAlias(red, "codex.request.approve", "codex.approveRequest", () =>
    resolveLatestCodexRequest(red, "accept"),
  );
  registerCommandAlias(red, "codex.request.approveForSession", "codex.approveRequestForSession", () =>
    resolveLatestCodexRequest(red, "acceptForSession"),
  );
  registerCommandAlias(red, "codex.request.decline", "codex.declineRequest", () =>
    resolveLatestCodexRequest(red, "decline"),
  );
  registerCommandAlias(red, "codex.request.cancel", "codex.cancelRequest", () =>
    resolveLatestCodexRequest(red, "cancel"),
  );
}

function registerCommand(
  red: Red.RedAPI,
  name: string,
  command: () => void | Promise<void>,
  metadata: Red.PluginCommandMetadata,
): void {
  red.addCommand(name, command, {
    category: "Codex",
    ...metadata,
  });
}

function registerCommandAlias(
  red: Red.RedAPI,
  name: string,
  canonicalName: string,
  command: () => void | Promise<void>,
): void {
  red.addCommand(name, command, {
    title: `${canonicalName} alias`,
    category: "Codex",
    description: `Compatibility alias for ${canonicalName}.`,
    context: ["compatibility"],
  });
}

export async function beforeExit(red: Red.RedAPI): Promise<void> {
  for (const chatState of states.values()) {
    state = chatState;
    await persistThread(red);
  }
}

function open(red: Red.RedAPI, chatState: State = state): void {
  state = chatState;
  state.open = true;
  registerWindowEvent(red, state.windowId);
  red.createPluginWindow(state.windowId, { title: "Codex" });
  render(red);
  red.focusPluginWindow(state.windowId);
}

async function openAndRestore(red: Red.RedAPI): Promise<void> {
  const chatState = await activateCurrentWorkspaceState(red);
  open(red, chatState);
  const previousStatus = state.status;
  state.status = "restoring";
  render(red);
  await restoreStoredThread(red, undefined, { loadTranscript: true });
  if (state.status === "restoring") {
    state.status = previousStatus === "local preview" ? "ready" : previousStatus;
  }
  render(red);
}

async function listProjectSessions(red: Red.RedAPI): Promise<void> {
  const chatState = await activateCurrentWorkspaceState(red);
  open(red, chatState);
  state.status = "loading sessions";
  state.connection = "connecting";
  render(red);

  try {
    const snapshot = await red.getEditorState();
    const workspaceRoot = await currentWorkspaceRoot(red, snapshot);
    const sessions = await fetchProjectSessions(red, workspaceRoot);
    markCodexConnected("sessions");
    state.transcript.push({ text: `Sessions for ${workspaceRoot}` });
    if (sessions.length === 0) {
      state.transcript.push({ text: "No Codex sessions found for this project." });
    } else {
      for (const session of sessions) {
        state.transcript.push({ text: sessionLabel(session) });
      }
    }
  } catch (error) {
    recordAppServerError(`Codex app-server request failed: ${String(error)}`);
  }

  render(red);
}

async function resumeProjectSession(red: Red.RedAPI): Promise<void> {
  const chatState = await activateCurrentWorkspaceState(red);
  open(red, chatState);
  state.status = "loading sessions";
  state.connection = "connecting";
  render(red);

  try {
    const snapshot = await red.getEditorState();
    const workspaceRoot = await currentWorkspaceRoot(red, snapshot);
    const sessions = await fetchProjectSessions(red, workspaceRoot);
    markCodexConnected("sessions");
    if (sessions.length === 0) {
      state.status = "sessions";
      state.transcript.push({ text: "No Codex sessions found for this project." });
      render(red);
      return;
    }

    const labels = sessions.map(sessionLabel);
    const selected = await red.pick("Codex Sessions", labels);
    if (!selected) {
      state.status = "ready";
      render(red);
      return;
    }

    const index = labels.indexOf(selected);
    const session = sessions[index];
    if (!session?.id) {
      state.status = "ready";
      render(red);
      return;
    }

    state.threadId = session.id;
    state.projectCwd = workspaceRoot;
    await persistThread(red);
    state.status = "resumed";
    await loadThreadTranscript(red, session.id);
  } catch (error) {
    recordAppServerError(`Codex session resume failed: ${String(error)}`);
  }

  render(red);
}

async function reconnectCodex(red: Red.RedAPI): Promise<void> {
  const chatState = await activateCurrentWorkspaceState(red);
  open(red, chatState);
  state.connection = "connecting";
  state.status = "reconnecting";
  render(red);

  try {
    const snapshot = await red.getEditorState();
    const workspaceRoot = await currentWorkspaceRoot(red, snapshot);
    await red.codex.request("thread/list", await withCodexAppServerEndpoint(red, {
      limit: 1,
      cwd: workspaceRoot,
      sortKey: "updated_at",
      sortDirection: "desc",
    }));
    markCodexConnected("ready");
    state.transcript.push({ text: "Codex app-server connection restored." });
  } catch (error) {
    recordAppServerError(`Codex app-server reconnect failed: ${String(error)}`);
  }

  render(red);
}

async function fetchProjectSessions(red: Red.RedAPI, cwd: string): Promise<any[]> {
  const response = await red.codex.request("thread/list", await withCodexAppServerEndpoint(red, {
    limit: 20,
    cwd,
    sortKey: "updated_at",
    sortDirection: "desc",
  }));
  return Array.isArray(response?.data) ? response.data : [];
}

async function withCodexAppServerEndpoint<T extends Record<string, any>>(
  red: Red.RedAPI,
  params: T,
): Promise<T & { appServerEndpoint?: string }> {
  const endpoint = await codexAppServerEndpoint(red);
  return endpoint ? { ...params, appServerEndpoint: endpoint } : params;
}

async function codexAppServerEndpoint(red: Red.RedAPI): Promise<string | undefined> {
  const config = await red.getConfig("codex");
  const endpoint = config?.app_server_endpoint ?? config?.appServerEndpoint;
  return typeof endpoint === "string" && endpoint.trim() ? endpoint.trim() : undefined;
}

function sessionLabel(session: any): string {
  const id = shortSessionId(session?.id);
  const preview = compactPreview(session?.preview);
  const status = sessionStatus(session?.status);
  const updated = sessionUpdatedAt(session);
  const source = sessionSource(session);
  const details = [status, updated, source].filter(Boolean).join(", ");
  return details ? `${id} ${preview} (${details})` : `${id} ${preview}`;
}

function shortSessionId(id: any): string {
  if (typeof id !== "string" || id.length === 0) {
    return "<unknown>";
  }
  return id.length > 12 ? id.slice(0, 8) : id;
}

function compactPreview(preview: any): string {
  const text = typeof preview === "string" && preview.trim() ? preview.trim() : "Untitled Codex session";
  return text.length > 72 ? `${text.slice(0, 69)}...` : text;
}

function sessionStatus(status: any): string | undefined {
  if (typeof status === "string") {
    return status;
  }
  if (status && typeof status.type === "string") {
    return status.type;
  }
  return undefined;
}

function sessionSource(session: any): string | undefined {
  const source = session?.threadSource ?? session?.source;
  if (typeof source === "string") {
    return source;
  }
  if (source && typeof source.type === "string") {
    return source.type;
  }
  return undefined;
}

function sessionUpdatedAt(session: any): string | undefined {
  const value = session?.updatedAt ?? session?.updated_at;
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return undefined;
  }
  const date = new Date(value * 1000);
  if (Number.isNaN(date.getTime())) {
    return undefined;
  }
  return date.toISOString().slice(0, 16).replace("T", " ");
}

async function loadThreadTranscript(red: Red.RedAPI, threadId: string): Promise<void> {
  const lines: Red.PluginWindowLine[] = [
    { text: "Codex Chat Window" },
    { text: `Resumed Codex session ${threadId}` },
  ];

  try {
    const response = await red.codex.request("thread/read", await withCodexAppServerEndpoint(red, {
      threadId,
      includeTurns: true,
    }));
    markCodexConnected(state.status);
    const turns = response?.thread?.turns;
    if (!Array.isArray(turns) || turns.length === 0) {
      lines.push({ text: "No persisted turns in this session." });
    } else {
      for (const turn of turns) {
        lines.push(...transcriptLinesForTurn(turn));
      }
    }
    state.transcript = lines;
    state.transcriptScroll = 0;
    state.loadedTranscriptThreadId = threadId;
  } catch (error) {
    recordAppServerError(`Codex history load failed: ${String(error)}`);
  }
}

function transcriptLinesForTurn(turn: any): Red.PluginWindowLine[] {
  const lines: Red.PluginWindowLine[] = [];
  for (const item of Array.isArray(turn?.items) ? turn.items : []) {
    switch (item.type) {
      case "userMessage": {
        const text = userInputText(item.content);
        if (text) {
          lines.push({ text: `You: ${text}` });
        }
        break;
      }
      case "agentMessage":
        if (item.text) {
          lines.push({ text: `Codex: ${item.text}` });
        }
        break;
      case "commandExecution":
        if (item.command) {
          lines.push({ text: `$ ${item.command}` });
        }
        break;
      case "fileChange":
        if (Array.isArray(item.changes)) {
          lines.push({ text: `Codex changed ${item.changes.length} file(s).` });
        }
        break;
    }
  }
  if (turn?.status === "interrupted") {
    lines.push({ text: "Codex: turn interrupted." });
  } else if (turn?.status === "failed" && turn?.error?.message) {
    lines.push({ text: `Codex: ${turn.error.message}` });
  }
  return lines;
}

function userInputText(content: any): string {
  if (!Array.isArray(content)) {
    return "";
  }
  return content
    .filter((item) => item?.type === "text" && typeof item.text === "string")
    .map((item) => item.text)
    .join("\n");
}

function handleWindowEvent(red: Red.RedAPI, event: Red.PluginWindowKeyEvent): void {
  if (!state.open || event.kind !== "key") {
    return;
  }

  switch (event.key) {
    case "Esc":
      state.mode = state.mode === "composer" ? "transcript" : "composer";
      updateModeStatus();
      render(red);
      return;
    case "Enter":
      void submit(red);
      return;
    case "Ctrl-j":
    case "Alt-Enter":
    case "Shift-Enter":
      insertText(red, "\n");
      return;
    case "Backspace":
      deleteBackward(red);
      return;
    case "Delete":
      deleteForward(red);
      return;
    case "Left":
      moveCursor(red, "left", event.modifiers.includes("Shift"));
      return;
    case "Right":
      moveCursor(red, "right", event.modifiers.includes("Shift"));
      return;
    case "Up":
      if (state.mode === "transcript") {
        scrollTranscript(red, 1);
      } else {
        moveCursor(red, "up", event.modifiers.includes("Shift"));
      }
      return;
    case "Down":
      if (state.mode === "transcript") {
        scrollTranscript(red, -1);
      } else {
        moveCursor(red, "down", event.modifiers.includes("Shift"));
      }
      return;
    case "j":
      if (state.mode === "transcript") {
        scrollTranscript(red, -1);
        return;
      }
      if (event.text && !event.modifiers.includes("Ctrl") && !event.modifiers.includes("Alt")) {
        insertText(red, event.text);
      }
      return;
    case "k":
      if (state.mode === "transcript") {
        scrollTranscript(red, 1);
        return;
      }
      if (event.text && !event.modifiers.includes("Ctrl") && !event.modifiers.includes("Alt")) {
        insertText(red, event.text);
      }
      return;
    case "PageUp":
    case "Ctrl-b":
      scrollTranscript(red, 8);
      return;
    case "PageDown":
    case "Ctrl-f":
      scrollTranscript(red, -8);
      return;
    case "Home":
      updateSelectionForMove(event.modifiers.includes("Shift"));
      state.cursorColumn = 0;
      clearSelectionIfCollapsed();
      state.status = "editing";
      render(red);
      return;
    case "End":
      updateSelectionForMove(event.modifiers.includes("Shift"));
      state.cursorColumn = lineLength(currentLine());
      clearSelectionIfCollapsed();
      state.status = "editing";
      render(red);
      return;
    case "Ctrl-c":
      cancelActiveTurn(red);
      return;
    default:
      if (event.text && !event.modifiers.includes("Ctrl") && !event.modifiers.includes("Alt")) {
        insertText(red, event.text);
      }
  }
}

function insertText(red: Red.RedAPI, text: string): void {
  if (state.mode !== "composer") {
    return;
  }

  deleteSelectionIfPresent();
  for (const char of chars(text)) {
    if (char === "\n") {
      insertNewline();
    } else {
      const line = currentLine();
      const before = takeChars(line, state.cursorColumn);
      const after = dropChars(line, state.cursorColumn);
      state.composerLines[state.cursorLine] = before + char + after;
      state.cursorColumn += 1;
    }
  }

  state.status = "editing";
  render(red);
}

function insertNewline(): void {
  const line = currentLine();
  const before = takeChars(line, state.cursorColumn);
  const after = dropChars(line, state.cursorColumn);
  state.composerLines[state.cursorLine] = before;
  state.composerLines.splice(state.cursorLine + 1, 0, after);
  state.cursorLine += 1;
  state.cursorColumn = 0;
}

function deleteBackward(red: Red.RedAPI): void {
  if (state.mode !== "composer") {
    return;
  }

  if (deleteSelectionIfPresent()) {
    state.status = "editing";
    render(red);
    return;
  }

  if (state.cursorColumn > 0) {
    const line = currentLine();
    const before = takeChars(line, state.cursorColumn - 1);
    const after = dropChars(line, state.cursorColumn);
    state.composerLines[state.cursorLine] = before + after;
    state.cursorColumn -= 1;
  } else if (state.cursorLine > 0) {
    const previousLine = state.composerLines[state.cursorLine - 1] ?? "";
    const line = currentLine();
    state.cursorColumn = lineLength(previousLine);
    state.composerLines[state.cursorLine - 1] = previousLine + line;
    state.composerLines.splice(state.cursorLine, 1);
    state.cursorLine -= 1;
  } else {
    return;
  }

  state.status = "editing";
  render(red);
}

function deleteForward(red: Red.RedAPI): void {
  if (state.mode !== "composer") {
    return;
  }

  if (deleteSelectionIfPresent()) {
    state.status = "editing";
    render(red);
    return;
  }

  const line = currentLine();
  if (state.cursorColumn < lineLength(line)) {
    const before = takeChars(line, state.cursorColumn);
    const after = dropChars(line, state.cursorColumn + 1);
    state.composerLines[state.cursorLine] = before + after;
  } else if (state.cursorLine < state.composerLines.length - 1) {
    state.composerLines[state.cursorLine] = line + (state.composerLines[state.cursorLine + 1] ?? "");
    state.composerLines.splice(state.cursorLine + 1, 1);
  } else {
    return;
  }

  state.status = "editing";
  render(red);
}

function moveCursor(
  red: Red.RedAPI,
  direction: "left" | "right" | "up" | "down",
  selecting = false,
): void {
  if (state.mode !== "composer") {
    return;
  }

  updateSelectionForMove(selecting);
  switch (direction) {
    case "left":
      if (state.cursorColumn > 0) {
        state.cursorColumn -= 1;
      } else if (state.cursorLine > 0) {
        state.cursorLine -= 1;
        state.cursorColumn = lineLength(currentLine());
      }
      break;
    case "right":
      if (state.cursorColumn < lineLength(currentLine())) {
        state.cursorColumn += 1;
      } else if (state.cursorLine < state.composerLines.length - 1) {
        state.cursorLine += 1;
        state.cursorColumn = 0;
      }
      break;
    case "up":
      if (state.cursorLine > 0) {
        state.cursorLine -= 1;
        state.cursorColumn = Math.min(state.cursorColumn, lineLength(currentLine()));
      }
      break;
    case "down":
      if (state.cursorLine < state.composerLines.length - 1) {
        state.cursorLine += 1;
        state.cursorColumn = Math.min(state.cursorColumn, lineLength(currentLine()));
      }
      break;
  }

  clearSelectionIfCollapsed();
  state.status = "editing";
  render(red);
}

async function submit(red: Red.RedAPI): Promise<void> {
  const chatState = state;
  if (state.mode !== "composer") {
    state.mode = "composer";
    updateModeStatus();
    render(red);
    return;
  }
  if (latestPendingUserInputRequest()) {
    answerLatestUserInputRequest(red);
    return;
  }
  if (state.inFlight) {
    return;
  }

  const prompt = state.composerLines.join("\n").trimEnd();
  if (!prompt) {
    return;
  }
  const blockedReason = promptSubmitBlockedReason(state.connection);
  if (blockedReason) {
    state.status = blockedReason;
    appendDisconnectedActionHint();
    render(red);
    return;
  }

  const additionalContext = additionalContextFromAttachments(state.contextAttachments);
  state.transcript.push({ text: `You: ${prompt}` });
  for (const attachment of state.contextAttachments) {
    state.transcript.push({ text: `Context: ${attachment.label}` });
  }
  state.composerLines = [""];
  state.cursorLine = 0;
  state.cursorColumn = 0;
  state.selectionAnchor = undefined;
  state.transcriptScroll = 0;
  state.contextAttachments = [];
  state.inFlight = true;
  state.connection = "connecting";
  state.status = "running";
  render(red);

  try {
    const snapshot = await red.getEditorState();
    const workspaceRoot = await currentWorkspaceRoot(red, snapshot);
    await restoreStoredThread(red, workspaceRoot);
    state.projectCwd = workspaceRoot;
    state.activeAgentText = "";
    state.activeNotifications = [];
    state.lastFollowedPath = undefined;
    state.lastFollowedLocation = undefined;
    state.activeAgentLine = state.transcript.push({ text: "Codex: " }) - 1;
    const turnParams: Red.CodexRunTurnParams = {
      prompt,
      cwd: workspaceRoot,
      runtimeWorkspaceRoots: [workspaceRoot],
      threadId: state.threadId,
    };
    const endpoint = await codexAppServerEndpoint(red);
    if (endpoint) {
      turnParams.appServerEndpoint = endpoint;
    }
    if (additionalContext) {
      turnParams.additionalContext = additionalContext;
    }
    const streamId = red.codex.startTurn(turnParams, (event) => {
      state = chatState;
      handleCodexTurnEvent(red, event);
    });
    state.activeStreamId ??= streamId;
  } catch (error) {
    recordAppServerError(`Codex: ${String(error)}`);
    state.inFlight = false;
  }

  render(red);
}

function render(red: Red.RedAPI): void {
  normalizeCursor();
  normalizeTranscriptScroll();

  const composerLines = state.composerLines.map((text) => ({ text }));

  red.updatePluginWindow(state.windowId, {
    kind: "chat",
    title: "Codex",
    status: renderStatus(),
    transcript: state.transcript,
    composer: composerLines,
    scroll: state.transcriptScroll,
    contextPlaceholders: contextPlaceholders(),
    composerCursor: {
      line: state.cursorLine,
      column: state.cursorColumn,
    },
    composerSelection: composerSelection(),
    keyHints: [
      "Enter send",
      "Ctrl-j newline",
      "context commands",
      state.followChanges ? "follow on" : "follow off",
      state.pendingRequestKeys.length > 0 ? "request commands" : "no pending requests",
      state.connection === "disconnected" ? "codex.reconnect" : "app-server ready",
      state.mode === "composer" ? "Esc transcript" : "Esc composer",
      "Ctrl-f/b page",
      "Ctrl-w w focus",
    ],
  });
}

function toggleFollowChanges(red: Red.RedAPI): void {
  state.followChanges = !state.followChanges;
  if (state.followChanges) {
    red.createOverlay(FOLLOW_OVERLAY_ID, {
      align: "bottom",
      relative: "editor",
      x_padding: 1,
      y_padding: 1,
    });
    red.updateOverlay(FOLLOW_OVERLAY_ID, [{ text: "Codex follow changes enabled" }]);
  } else {
    red.removeOverlay(FOLLOW_OVERLAY_ID);
  }
  state.status = state.followChanges ? "follow changes" : "ready";
  render(red);
}

function handleCodexTurnEvent(red: Red.RedAPI, event: Red.CodexTurnEvent): void {
  if (!state.activeStreamId && state.inFlight) {
    state.activeStreamId = event.streamId;
  }
  if (event.streamId !== state.activeStreamId) {
    return;
  }

  switch (event.kind) {
    case "thread":
      markCodexConnected("running");
      state.threadId = event.thread?.id ?? state.threadId;
      if (state.threadId) {
        void persistThread(red).catch((error) => red.logWarn("Codex thread persist failed", String(error)));
      }
      state.status = "running";
      break;
    case "turn":
      markCodexConnected("turn started");
      state.status = "turn started";
      break;
    case "notification":
      state.activeNotifications.push(event.notification);
      applyCodexNotification(event.notification);
      void updateFollowChanges(red, state.activeNotifications);
      break;
    case "request":
      renderInteractiveRequest(red, event);
      break;
    case "cancelled":
      state.status = "cancelling";
      updateActiveAgentLine(state.activeAgentText || "interrupting turn...");
      break;
    case "completed":
      completeCodexTurn(red, event.result);
      break;
    case "error":
      failCodexTurn(red, event.error);
      break;
  }

  render(red);
}

function applyCodexNotification(notification: any): void {
  if (notification.method === "item/agentMessage/delta") {
    const delta = notification.params?.delta;
    if (typeof delta === "string" && delta.length > 0) {
      state.activeAgentText += delta;
      updateActiveAgentLine(state.activeAgentText);
    }
  }

  if (notification.method === "item/completed") {
    const item = notification.params?.item;
    if (item?.type === "agentMessage" && typeof item.text === "string") {
      state.activeAgentText = item.text;
      updateActiveAgentLine(state.activeAgentText);
    }
  }
}

function completeCodexTurn(red: Red.RedAPI, result: Red.CodexRunTurnResult): void {
  markCodexConnected("ready");
  state.threadId = result.thread?.id ?? state.threadId;
  if (state.threadId) {
    void persistThread(red).catch((error) => red.logWarn("Codex thread persist failed", String(error)));
  }
  const interrupted = String(result.turn?.status ?? "").toLowerCase() === "interrupted";
  state.activeNotifications = result.notifications;
  void updateFollowChanges(red, result.notifications);
  if (result.agentText) {
    state.activeAgentText = result.agentText;
    updateActiveAgentLine(result.agentText);
  } else if (interrupted) {
    updateActiveAgentLine("turn interrupted.");
  } else {
    updateActiveAgentLine(state.activeAgentText || "turn completed.");
  }
  state.activeStreamId = undefined;
  state.activeAgentLine = undefined;
  state.pendingRequestKeys = [];
  state.pendingRequests = [];
  state.inFlight = false;
  state.status = interrupted ? "interrupted" : "ready";
}

function failCodexTurn(red: Red.RedAPI, error: string): void {
  state.connection = "disconnected";
  if (state.threadId) {
    const staleThreadId = state.threadId;
    state.threadId = undefined;
    void persistThread(red).catch((persistError) => red.logWarn("Codex thread persist failed", String(persistError)));
    updateActiveAgentLine(`stored thread ${staleThreadId} could not be used: ${error}`);
  } else {
    updateActiveAgentLine(error);
  }
  state.activeStreamId = undefined;
  state.activeAgentLine = undefined;
  state.activeAgentText = "";
  state.activeNotifications = [];
  state.pendingRequestKeys = [];
  state.pendingRequests = [];
  state.inFlight = false;
  state.status = "disconnected";
  appendDisconnectedActionHint();
}

function renderInteractiveRequest(red: Red.RedAPI, request: Extract<Red.CodexTurnEvent, { kind: "request" }>): void {
  const method = request.method;
  if (typeof method !== "string" || !isInteractiveRequestMethod(method)) {
    return;
  }

  const params = request.params ?? {};
  const key = `${request.streamId}:${JSON.stringify(request.requestId)}`;
  if (state.pendingRequestKeys.includes(key)) {
    return;
  }
  state.pendingRequestKeys.push(key);
  state.pendingRequests.push({
    key,
    streamId: request.streamId,
    requestId: request.requestId,
    method,
    params,
  });
  state.status = method === "item/tool/requestUserInput" ? "input requested" : "approval requested";

  state.transcript.push({ text: interactiveRequestTitle(method) });
  for (const line of interactiveRequestDetails(method, params)) {
    state.transcript.push({ text: line });
  }
  state.transcript.push({ text: "Actions:" });
  for (const line of interactiveRequestActionLines(method, params)) {
    state.transcript.push({ text: line });
  }
  render(red);
}

function isInteractiveRequestMethod(method: string): boolean {
  return method === "item/commandExecution/requestApproval"
    || method === "item/fileChange/requestApproval"
    || method === "item/permissions/requestApproval"
    || method === "item/tool/requestUserInput";
}

function interactiveRequestActionLines(method: string, params: any): string[] {
  if (method === "item/tool/requestUserInput") {
    return [
      "  Answer: type in the composer and press Enter",
      "  Cancel: run codex.cancelRequest",
    ];
  }

  const actions = [
    "  Approve: run codex.approveRequest",
  ];
  if (
    method === "item/permissions/requestApproval"
    || supportsDecision({ params } as PendingCodexRequest, "acceptForSession")
  ) {
    actions.push("  Approve for session: run codex.approveRequestForSession");
  }
  actions.push("  Decline: run codex.declineRequest");
  actions.push("  Cancel turn: run codex.cancelRequest");
  return actions;
}

export function __testInteractiveRequestActionLines(method: string, params: any = {}): string[] {
  return interactiveRequestActionLines(method, params);
}

function interactiveRequestTitle(method: string): string {
  switch (method) {
    case "item/commandExecution/requestApproval":
      return "Codex needs approval to run a command.";
    case "item/fileChange/requestApproval":
      return "Codex needs approval to change files.";
    case "item/permissions/requestApproval":
      return "Codex is requesting additional permissions.";
    case "item/tool/requestUserInput":
      return "Codex is requesting input.";
    default:
      return "Codex needs user action.";
  }
}

function interactiveRequestDetails(method: string, params: any): string[] {
  switch (method) {
    case "item/commandExecution/requestApproval":
      return compactLines([
        params.command ? `$ ${params.command}` : undefined,
        params.cwd ? `cwd: ${params.cwd}` : undefined,
        params.reason ? `reason: ${params.reason}` : undefined,
        availableDecisionLine(params.availableDecisions),
      ]);
    case "item/fileChange/requestApproval":
      return compactLines([
        params.grantRoot ? `root: ${params.grantRoot}` : undefined,
        params.reason ? `reason: ${params.reason}` : undefined,
      ]);
    case "item/permissions/requestApproval":
      return compactLines([
        params.cwd ? `cwd: ${params.cwd}` : undefined,
        params.reason ? `reason: ${params.reason}` : undefined,
        params.permissions ? `permissions: ${JSON.stringify(params.permissions)}` : undefined,
      ]);
    case "item/tool/requestUserInput":
      return userInputRequestLines(params.questions);
    default:
      return [];
  }
}

function userInputRequestLines(questions: any): string[] {
  if (!Array.isArray(questions) || questions.length === 0) {
    return ["No question details were provided."];
  }

  const lines: string[] = [];
  for (const question of questions) {
    const header = typeof question?.header === "string" ? question.header.trim() : "";
    const text = typeof question?.question === "string" ? question.question.trim() : "";
    lines.push(header ? `${header}: ${text}` : text || "Question");
    if (Array.isArray(question?.options) && question.options.length > 0) {
      for (const option of question.options.slice(0, 4)) {
        const label = typeof option?.label === "string" ? option.label : "";
        const description = typeof option?.description === "string" ? option.description : "";
        lines.push(`- ${label}${description ? `: ${description}` : ""}`);
      }
    }
  }
  return lines;
}

function availableDecisionLine(decisions: any): string | undefined {
  return Array.isArray(decisions) && decisions.length > 0
    ? `available decisions: ${decisions.join(", ")}`
    : undefined;
}

function compactLines(lines: Array<string | undefined>): string[] {
  return lines.filter((line): line is string => Boolean(line));
}

type RequestDecision = "accept" | "acceptForSession" | "decline" | "cancel";

function resolveLatestCodexRequest(red: Red.RedAPI, decision: RequestDecision): void {
  const request = latestPendingRequest();
  if (!request) {
    state.status = "no pending request";
    render(red);
    return;
  }

  const response = responseForDecision(request, decision);
  if (!response) {
    state.transcript.push({ text: `Codex request ${request.method} cannot be approved by this command.` });
    state.status = "request still pending";
    render(red);
    return;
  }

  resolvePendingRequest(red, request, response, decision);
}

function answerLatestUserInputRequest(red: Red.RedAPI): void {
  const request = latestPendingUserInputRequest();
  if (!request) {
    state.status = "no input request";
    render(red);
    return;
  }

  const answer = state.composerLines.join("\n").trimEnd();
  if (!answer) {
    state.status = "empty answer";
    render(red);
    return;
  }

  const questions = Array.isArray(request.params?.questions) ? request.params.questions : [];
  const answers: Record<string, { answers: string[] }> = {};
  for (const question of questions) {
    if (typeof question?.id === "string") {
      answers[question.id] = { answers: [answer] };
    }
  }
  if (Object.keys(answers).length === 0) {
    state.status = "invalid input request";
    render(red);
    return;
  }

  state.composerLines = [""];
  state.cursorLine = 0;
  state.cursorColumn = 0;
  state.selectionAnchor = undefined;
  resolvePendingRequest(red, request, { answers }, "accept");
}

function latestPendingRequest(): PendingCodexRequest | undefined {
  return state.pendingRequests[state.pendingRequests.length - 1];
}

function latestPendingUserInputRequest(): PendingCodexRequest | undefined {
  return [...state.pendingRequests]
    .reverse()
    .find((pending) => pending.method === "item/tool/requestUserInput");
}

function responseForDecision(request: PendingCodexRequest, decision: RequestDecision): any | undefined {
  if (
    request.method === "item/commandExecution/requestApproval"
    || request.method === "item/fileChange/requestApproval"
  ) {
    if (decision === "acceptForSession") {
      return { decision: supportsDecision(request, "acceptForSession") ? "acceptForSession" : "accept" };
    }
    return { decision };
  }
  if (request.method === "item/permissions/requestApproval") {
    if (decision === "accept" || decision === "acceptForSession") {
      return {
        permissions: grantablePermissions(request.params?.permissions),
        scope: decision === "acceptForSession" ? "session" : "turn",
      };
    }
    return { permissions: {}, scope: "turn" };
  }
  if (request.method === "item/tool/requestUserInput" && decision === "cancel") {
    return { answers: {} };
  }
  return undefined;
}

function grantablePermissions(permissions: any): any {
  if (!permissions || typeof permissions !== "object" || Array.isArray(permissions)) {
    return {};
  }

  const granted: any = {};
  if (permissions.network && typeof permissions.network === "object" && !Array.isArray(permissions.network)) {
    granted.network = permissions.network;
  }
  if (
    permissions.fileSystem
    && typeof permissions.fileSystem === "object"
    && !Array.isArray(permissions.fileSystem)
  ) {
    granted.fileSystem = permissions.fileSystem;
  }
  return granted;
}

function supportsDecision(request: PendingCodexRequest, decision: string): boolean {
  const decisions = request.params?.availableDecisions;
  return Array.isArray(decisions) && decisions.includes(decision);
}

function resolvePendingRequest(
  red: Red.RedAPI,
  request: PendingCodexRequest,
  response: any,
  label: string,
): void {
  if (!red.codex.resolveRequest(request.streamId, request.requestId, response)) {
    state.status = "request expired";
    state.transcript.push({ text: "Codex request could not be resolved; it may have expired." });
    render(red);
    return;
  }

  state.pendingRequests = state.pendingRequests.filter((pending) => pending.key !== request.key);
  state.pendingRequestKeys = state.pendingRequestKeys.filter((key) => key !== request.key);
  state.status = state.pendingRequests.length > 0 ? "request pending" : "running";
  state.transcript.push({ text: `Codex request resolved: ${label}.` });
  render(red);
}

function markCodexConnected(status: string): void {
  state.connection = "ready";
  state.status = status;
}

function recordAppServerError(message: string): void {
  state.connection = "disconnected";
  state.status = "disconnected";
  state.transcript.push({ text: message });
  appendDisconnectedActionHint();
}

function renderStatus(): string {
  if (state.connection === "disconnected") {
    return "disconnected";
  }
  if (state.connection === "connecting") {
    return `${state.status} (connecting)`;
  }
  return state.status;
}

function promptSubmitBlockedReason(connection: ConnectionState): string | undefined {
  return connection === "disconnected" ? "disconnected" : undefined;
}

function appendDisconnectedActionHint(): void {
  if (state.transcript[state.transcript.length - 1]?.text !== DISCONNECTED_ACTION_HINT) {
    state.transcript.push({ text: DISCONNECTED_ACTION_HINT });
  }
}

export function __testPromptSubmitBlockedReason(connection: ConnectionState): string | undefined {
  return promptSubmitBlockedReason(connection);
}

export function __testDisconnectedActionHint(): string {
  return DISCONNECTED_ACTION_HINT;
}

function updateActiveAgentLine(text: string): void {
  const index = state.activeAgentLine;
  if (index === undefined || !state.transcript[index]) {
    state.activeAgentLine = state.transcript.push({ text: `Codex: ${text}` }) - 1;
    return;
  }
  state.transcript[index] = { text: `Codex: ${text}` };
}

function cancelActiveTurn(red: Red.RedAPI): void {
  if (latestPendingRequest()) {
    resolveLatestCodexRequest(red, "cancel");
    return;
  }

  const streamId = state.activeStreamId;
  if (!streamId || !state.inFlight) {
    state.status = "ready";
    render(red);
    return;
  }

  if (red.codex.cancelTurn(streamId)) {
    state.status = "cancelling";
    updateActiveAgentLine(state.activeAgentText || "interrupting turn...");
  } else {
    state.activeStreamId = undefined;
    state.activeAgentLine = undefined;
    state.activeAgentText = "";
    state.activeNotifications = [];
    state.inFlight = false;
    state.status = "cancelled";
    state.transcript.push({ text: "Codex: active turn was already stopped." });
  }
  render(red);
}

async function updateFollowChanges(red: Red.RedAPI, notifications: any[]): Promise<void> {
  if (!state.followChanges) {
    return;
  }

  const conflictPath = await followLatestChangedFile(red, notifications)
    ?? state.conflictedPaths[state.conflictedPaths.length - 1];

  const lines: Red.OverlayLine[] = [];
  const latestDiff = [...notifications]
    .reverse()
    .find((notification) => notification.method === "turn/diff/updated")
    ?.params?.diff;
  const latestPlan = [...notifications]
    .reverse()
    .find((notification) => notification.method === "turn/plan/updated")
    ?.params?.plan;

  lines.push({ text: `Codex ${state.threadId ?? ""}`.trim() });
  if (conflictPath) {
    lines.push({ text: `dirty conflict ${relativePath(state.projectCwd ?? "", conflictPath)}` });
  }
  if (Array.isArray(latestPlan)) {
    for (const item of latestPlan.slice(0, 3)) {
      lines.push({ text: `${item.status ?? "pending"}: ${item.step ?? ""}` });
    }
  }
  for (const notification of notifications) {
    if (notification.method !== "item/completed") {
      continue;
    }
    const item = notification.params?.item;
    if (item?.type === "fileChange" && Array.isArray(item.changes)) {
      for (const change of item.changes) {
        lines.push({ text: `${change.kind ?? "changed"} ${change.path ?? ""}` });
      }
    }
  }
  if (typeof latestDiff === "string" && latestDiff.length > 0) {
    lines.push({ text: latestDiff.split("\n").find((line) => line.startsWith("diff --git")) ?? "diff updated" });
  }
  if (lines.length === 1) {
    lines.push({ text: "No file changes in last turn" });
  }

  red.updateOverlay(FOLLOW_OVERLAY_ID, lines.slice(0, 8));
}

async function followLatestChangedFile(
  red: Red.RedAPI,
  notifications: any[],
): Promise<string | undefined> {
  const changed = latestChangedLocation(notifications);
  if (!changed?.path || !state.projectCwd) {
    return undefined;
  }

  const filePath = absolutePath(state.projectCwd, changed.path);
  const snapshot = await red.getEditorState();
  if (snapshot.buffers.some((buffer) => normalizePath(buffer.path) === filePath && buffer.dirty)) {
    recordDirtyConflict(red, filePath);
    return filePath;
  }

  const locationKey = `${filePath}:${changed.line ?? 1}`;
  if (state.lastFollowedPath === filePath && state.lastFollowedLocation === locationKey) {
    return undefined;
  }

  state.conflictedPaths = state.conflictedPaths.filter((path) => path !== filePath);
  if (state.lastFollowedPath !== filePath) {
    red.openFile(filePath);
    state.lastFollowedPath = filePath;
  }
  if (changed.line !== undefined) {
    red.centerCursorPosition(0, Math.max(0, changed.line - 1));
  }
  state.lastFollowedLocation = locationKey;
  return undefined;
}

function recordDirtyConflict(red: Red.RedAPI, filePath: string): void {
  if (!state.conflictedPaths.includes(filePath)) {
    state.conflictedPaths.push(filePath);
    state.transcript.push({
      text: `Codex changed ${relativePath(state.projectCwd ?? "", filePath)}, but the open buffer has unsaved edits. Auto-open skipped.`,
    });
  }
  state.status = "dirty conflict";
  render(red);
}

function latestChangedLocation(notifications: any[]): { path: string; line?: number } | undefined {
  for (const notification of [...notifications].reverse()) {
    if (
      notification.method === "item/fileChange/patchUpdated"
      && Array.isArray(notification.params?.changes)
    ) {
      const change = [...notification.params.changes].reverse()
        .find((candidate) => typeof candidate?.path === "string");
      if (change?.path) {
        return { path: change.path, line: changedLineFromDiff(change.diff) };
      }
    }

    if (notification.method !== "item/completed") {
      continue;
    }
    const item = notification.params?.item;
    if (item?.type !== "fileChange" || !Array.isArray(item.changes)) {
      continue;
    }
    const change = [...item.changes].reverse()
      .find((candidate) => typeof candidate?.path === "string");
    if (change?.path) {
      return { path: change.path, line: changedLineFromDiff(change.diff) };
    }
  }

  return undefined;
}

function changedLineFromDiff(diff: any): number | undefined {
  if (typeof diff !== "string") {
    return undefined;
  }
  const match = diff.match(/^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@/m);
  if (!match) {
    return undefined;
  }
  const line = Number.parseInt(match[1] ?? "", 10);
  return Number.isFinite(line) && line > 0 ? line : undefined;
}

async function restoreStoredThread(
  red: Red.RedAPI,
  cwd?: string,
  options: { loadTranscript?: boolean } = {},
): Promise<void> {
  const projectCwd = cwd ?? await currentWorkspaceRoot(red);
  state = stateForWorkspaceRoot(projectCwd);
  const stored = await readStoredChatState(red, projectCwd);
  if (!stored || stored.cwd !== projectCwd || (stored.version !== 1 && stored.version !== 2)) {
    return;
  }

  if (typeof stored.threadId === "string") {
    state.threadId = stored.threadId;
  }
  state.projectCwd = stored.cwd;
  if (stored.version >= 2) {
    restoreDraft(stored);
  }
  if (
    options.loadTranscript
    && typeof stored.threadId === "string"
    && state.loadedTranscriptThreadId !== stored.threadId
  ) {
    await loadThreadTranscript(red, stored.threadId);
    if (state.connection !== "disconnected") {
      state.status = "resumed";
    }
  }
}

async function readStoredChatState(red: Red.RedAPI, projectCwd: string): Promise<any> {
  const stored = await red.storage.get(storageKeyForWorkspaceRoot(projectCwd));
  if (stored) {
    return stored;
  }

  const legacy = await red.storage.get(LEGACY_STORAGE_KEY);
  return legacy?.cwd === projectCwd ? legacy : undefined;
}

function restoreDraft(stored: any): void {
  if (
    Array.isArray(stored.composerLines)
    && stored.composerLines.every((line: unknown) => typeof line === "string")
  ) {
    state.composerLines = stored.composerLines.length > 0 ? stored.composerLines : [""];
  }

  if (
    Array.isArray(stored.contextAttachments)
    && stored.contextAttachments.every(isContextAttachment)
  ) {
    state.contextAttachments = stored.contextAttachments;
  }

  state.cursorLine = Number.isInteger(stored.cursorLine) ? stored.cursorLine : state.cursorLine;
  state.cursorColumn = Number.isInteger(stored.cursorColumn) ? stored.cursorColumn : state.cursorColumn;
  normalizeCursor();
}

function isContextAttachment(value: any): value is ContextAttachment {
  return value
    && typeof value.label === "string"
    && typeof value.content === "string"
    && (value.path === undefined || typeof value.path === "string")
    && (value.startLine === undefined || Number.isInteger(value.startLine))
    && (value.endLine === undefined || Number.isInteger(value.endLine));
}

async function currentWorkspaceRoot(
  red: Red.RedAPI,
  snapshot?: Red.EditorStateSnapshot,
): Promise<string> {
  const editorState = snapshot ?? await red.getEditorState();
  return await resolveWorkspaceRoot(red, editorState.cwd);
}

async function resolveWorkspaceRoot(red: Red.RedAPI, cwd: string): Promise<string> {
  let dir = normalizePath(cwd);
  const fallback = dir;
  const seen = new Set<string>();

  while (dir && !seen.has(dir)) {
    seen.add(dir);
    const listing = await red.listDirectory(dir);
    if (!listing.error && listing.entries.some((entry) => entry.name === ".git")) {
      return dir;
    }
    const parent = parentPath(dir);
    if (!parent || parent === dir) {
      break;
    }
    dir = parent;
  }

  return fallback;
}

function normalizePath(path: string): string {
  if (path.length > 1) {
    return path.replace(/\/+$/, "");
  }
  return path;
}

function parentPath(path: string): string | undefined {
  const normalized = normalizePath(path);
  if (normalized === "/") {
    return undefined;
  }
  const index = normalized.lastIndexOf("/");
  if (index <= 0) {
    return "/";
  }
  return normalized.slice(0, index);
}

function isPathInsideRoot(path: string, root: string): boolean {
  const normalizedPath = normalizePath(path);
  const normalizedRoot = normalizePath(root);
  return normalizedRoot === "/"
    || normalizedPath === normalizedRoot
    || normalizedPath.startsWith(`${normalizedRoot}/`);
}

function absolutePath(root: string, path: string): string {
  if (path.startsWith("/")) {
    return normalizePath(path);
  }
  return `${normalizePath(root)}/${path.replace(/^\/+/, "")}`;
}

function relativePath(root: string, path: string): string {
  const normalizedRoot = normalizePath(root);
  const normalizedPath = normalizePath(path);
  if (normalizedPath === normalizedRoot) {
    return ".";
  }
  if (normalizedPath.startsWith(`${normalizedRoot}/`)) {
    return normalizedPath.slice(normalizedRoot.length + 1);
  }
  return normalizedPath;
}

async function persistThread(red: Red.RedAPI): Promise<void> {
  state.projectCwd ??= await currentWorkspaceRoot(red);
  const storageKey = storageKeyForWorkspaceRoot(state.projectCwd);
  if (!state.threadId && !hasDraftState()) {
    await red.storage.delete(storageKey);
    return;
  }

  await red.storage.set(storageKey, {
    version: 2,
    cwd: state.projectCwd,
    threadId: state.threadId,
    composerLines: state.composerLines,
    cursorLine: state.cursorLine,
    cursorColumn: state.cursorColumn,
    contextAttachments: state.contextAttachments,
  });
}

function hasDraftState(): boolean {
  return state.contextAttachments.length > 0
    || state.composerLines.length > 1
    || (state.composerLines[0] ?? "") !== "";
}

async function addCurrentLineContext(red: Red.RedAPI): Promise<void> {
  const snapshot = await red.getEditorState();
  open(red, await activateCurrentWorkspaceState(red, snapshot));
  const buffer = currentSnapshotBuffer(snapshot);
  const position = buffer?.cursor ?? await red.getCursorPosition();
  const text = await red.getBufferText(position.y, position.y + 1);
  const line = text.replace(/\n$/, "");
  const path = buffer?.path;
  if (!await ensureAttachmentInWorkspace(red, snapshot, path)) {
    return;
  }
  const label = `[Current Line ${shortPath(path)}:${position.y + 1}]`;

  addContextAttachment({
    label,
    content: line,
    path,
    startLine: position.y + 1,
    endLine: position.y + 1,
  });
  render(red);
}

async function addCurrentFileContext(red: Red.RedAPI): Promise<void> {
  const snapshot = await red.getEditorState();
  open(red, await activateCurrentWorkspaceState(red, snapshot));
  const buffer = currentSnapshotBuffer(snapshot);
  const content = await red.getBufferText();
  const count = charCount(content);
  const path = buffer?.path;
  if (!await ensureAttachmentInWorkspace(red, snapshot, path)) {
    return;
  }
  const label = count > LARGE_PASTE_CHAR_THRESHOLD
    ? `[Pasted Content ${count} chars] ${shortPath(path)}`
    : `[Current File ${shortPath(path)}]`;

  addContextAttachment({
    label,
    content,
    path,
    startLine: 1,
    endLine: content.split("\n").length,
  });
  render(red);
}

async function addSelectionContext(red: Red.RedAPI): Promise<void> {
  const snapshot = await red.getEditorState();
  open(red, await activateCurrentWorkspaceState(red, snapshot));
  const selection = snapshot.selection;
  if (!selection?.text) {
    state.status = "no selection";
    state.transcript.push({ text: "No active editor selection to attach." });
    render(red);
    return;
  }

  const buffer = currentSnapshotBuffer(snapshot);
  const path = buffer?.path;
  if (!await ensureAttachmentInWorkspace(red, snapshot, path)) {
    return;
  }
  const startLine = Math.min(selection.start.y, selection.end.y) + 1;
  const endLine = Math.max(selection.start.y, selection.end.y) + 1;
  const count = charCount(selection.text);
  const lineSuffix = startLine === endLine ? `${startLine}` : `${startLine}-${endLine}`;
  const label = count > LARGE_PASTE_CHAR_THRESHOLD
    ? `[Pasted Content ${count} chars] ${shortPath(path)}:${lineSuffix}`
    : `[Selection ${shortPath(path)}:${lineSuffix}]`;

  addContextAttachment({
    label,
    content: selection.text,
    path,
    startLine,
    endLine,
  });
  render(red);
}

async function addGitDiffContext(red: Red.RedAPI): Promise<void> {
  const snapshot = await red.getEditorState();
  open(red, await activateCurrentWorkspaceState(red, snapshot));
  const workspaceRoot = await currentWorkspaceRoot(red, snapshot);
  const diff = await red.getGitDiff(workspaceRoot);
  if (diff.error) {
    state.status = "git diff error";
    state.transcript.push({ text: `Git diff failed: ${diff.error}` });
    render(red);
    return;
  }

  const content = diff.text.trimEnd();
  if (!content) {
    state.status = "no git diff";
    state.transcript.push({ text: "No workspace git diff to attach." });
    render(red);
    return;
  }

  const count = charCount(content);
  const label = count > LARGE_PASTE_CHAR_THRESHOLD
    ? `[Pasted Content ${count} chars] git diff`
    : "[Git Diff]";

  addContextAttachment({
    label,
    content,
    path: workspaceRoot,
  });
  render(red);
}

async function addDiagnosticsContext(red: Red.RedAPI): Promise<void> {
  const snapshot = await red.getEditorState();
  open(red, await activateCurrentWorkspaceState(red, snapshot));
  const diagnostics = snapshot.diagnostics ?? [];
  if (diagnostics.length === 0) {
    state.status = "no diagnostics";
    state.transcript.push({ text: "No current-buffer diagnostics to attach." });
    render(red);
    return;
  }

  const buffer = currentSnapshotBuffer(snapshot);
  const path = buffer?.path;
  if (!await ensureAttachmentInWorkspace(red, snapshot, path)) {
    return;
  }

  const content = diagnostics
    .map((diagnostic) => {
      const severity = diagnostic.severity ? `${diagnostic.severity}: ` : "";
      return `${shortPath(path)}:${diagnostic.line + 1}:${diagnostic.character + 1} ${severity}${diagnostic.message}`;
    })
    .join("\n");
  const label = `[Diagnostics ${shortPath(path)} ${diagnostics.length}]`;

  addContextAttachment({
    label,
    content,
    path,
  });
  render(red);
}

async function addOpenBuffersContext(red: Red.RedAPI): Promise<void> {
  const snapshot = await red.getEditorState();
  open(red, await activateCurrentWorkspaceState(red, snapshot));
  const workspaceRoot = await currentWorkspaceRoot(red, snapshot);
  const buffers = snapshot.buffers.filter((buffer) =>
    isPathInsideRoot(buffer.path, workspaceRoot)
  );

  if (buffers.length === 0) {
    state.status = "no open buffers";
    state.transcript.push({ text: "No open workspace buffers to attach." });
    render(red);
    return;
  }

  const content = buffers
    .map((buffer) => {
      const dirty = buffer.dirty ? " dirty" : "";
      return `${relativePath(workspaceRoot, buffer.path)}:${buffer.cursor.y + 1}:${buffer.cursor.x + 1}${dirty}`;
    })
    .join("\n");

  addContextAttachment({
    label: `[Open Buffers ${buffers.length}]`,
    content,
    path: workspaceRoot,
  });
  render(red);
}

async function ensureAttachmentInWorkspace(
  red: Red.RedAPI,
  snapshot: Red.EditorStateSnapshot,
  path: string | undefined,
): Promise<boolean> {
  if (!path) {
    return true;
  }

  const workspaceRoot = await currentWorkspaceRoot(red, snapshot);
  if (isPathInsideRoot(path, workspaceRoot)) {
    return true;
  }

  state.status = "context outside workspace";
  state.transcript.push({
    text: `Context not attached: ${path} is outside ${workspaceRoot}.`,
  });
  render(red);
  return false;
}

function addContextAttachment(attachment: ContextAttachment): void {
  const duplicate = state.contextAttachments.some(({ label }) => label === attachment.label);
  if (!duplicate) {
    state.contextAttachments.push(attachment);
    appendComposerLine(attachment.label);
  }
  state.mode = "composer";
  state.status = "context added";
}

function additionalContextFromAttachments(
  attachments: ContextAttachment[],
): Record<string, { value: string; kind: "untrusted" | "application" }> | undefined {
  if (attachments.length === 0) {
    return undefined;
  }

  const entries: Record<string, { value: string; kind: "untrusted" | "application" }> = {};
  attachments.forEach((attachment, index) => {
    const source = [
      attachment.path ?? "buffer",
      attachment.startLine ?? 1,
      attachment.endLine ?? attachment.startLine ?? 1,
      index,
    ].join(":");
    entries[source] = {
      value: attachment.content,
      kind: "untrusted",
    };
  });
  return entries;
}

function appendComposerLine(text: string): void {
  if (state.composerLines.length === 1 && state.composerLines[0] === "") {
    state.composerLines[0] = text;
  } else {
    state.composerLines.push(text);
  }
  state.cursorLine = state.composerLines.length - 1;
  state.cursorColumn = lineLength(text);
}

function contextPlaceholders(): Red.PluginWindowContextPlaceholder[] {
  return state.contextAttachments
    .map((attachment) => {
      const line = state.composerLines.findIndex((value) => value === attachment.label);
      if (line < 0) {
        return undefined;
      }
      return {
        line,
        start: 0,
        end: lineLength(attachment.label),
        label: attachment.label,
      };
    })
    .filter((placeholder): placeholder is Red.PluginWindowContextPlaceholder => Boolean(placeholder));
}

function composerSelection(): Red.PluginWindowSelection | undefined {
  const anchor = state.selectionAnchor;
  if (!anchor || (anchor.line === state.cursorLine && anchor.column === state.cursorColumn)) {
    return undefined;
  }
  return {
    startLine: anchor.line,
    startColumn: anchor.column,
    endLine: state.cursorLine,
    endColumn: state.cursorColumn,
  };
}

function currentSnapshotBuffer(snapshot: Red.EditorStateSnapshot): Red.BufferStateSnapshot | undefined {
  return snapshot.buffers.find((buffer) => buffer.index === snapshot.currentBufferIndex)
    ?? snapshot.buffers[0];
}

function shortPath(path: string | undefined): string {
  if (!path) {
    return "<buffer>";
  }
  return path.split("/").filter(Boolean).pop() ?? path;
}

function currentLine(): string {
  return state.composerLines[state.cursorLine] ?? "";
}

function currentPosition(): ComposerPosition {
  return { line: state.cursorLine, column: state.cursorColumn };
}

function updateSelectionForMove(selecting: boolean): void {
  if (selecting) {
    state.selectionAnchor ??= currentPosition();
  } else {
    state.selectionAnchor = undefined;
  }
}

function clearSelectionIfCollapsed(): void {
  if (
    state.selectionAnchor
    && state.selectionAnchor.line === state.cursorLine
    && state.selectionAnchor.column === state.cursorColumn
  ) {
    state.selectionAnchor = undefined;
  }
}

function deleteSelectionIfPresent(): boolean {
  const selection = normalizedSelection();
  if (!selection) {
    return false;
  }

  const { start, end } = selection;
  if (start.line === end.line) {
    const line = state.composerLines[start.line] ?? "";
    state.composerLines[start.line] = takeChars(line, start.column) + dropChars(line, end.column);
  } else {
    const firstLine = state.composerLines[start.line] ?? "";
    const lastLine = state.composerLines[end.line] ?? "";
    state.composerLines.splice(
      start.line,
      end.line - start.line + 1,
      takeChars(firstLine, start.column) + dropChars(lastLine, end.column),
    );
  }

  state.cursorLine = start.line;
  state.cursorColumn = start.column;
  state.selectionAnchor = undefined;
  normalizeCursor();
  return true;
}

function normalizedSelection(): { start: ComposerPosition; end: ComposerPosition } | undefined {
  const anchor = state.selectionAnchor;
  if (!anchor || (anchor.line === state.cursorLine && anchor.column === state.cursorColumn)) {
    return undefined;
  }
  const cursor = currentPosition();
  return comparePositions(anchor, cursor) <= 0
    ? { start: anchor, end: cursor }
    : { start: cursor, end: anchor };
}

function comparePositions(left: ComposerPosition, right: ComposerPosition): number {
  if (left.line !== right.line) {
    return left.line - right.line;
  }
  return left.column - right.column;
}

function normalizeCursor(): void {
  if (state.composerLines.length === 0) {
    state.composerLines = [""];
  }

  state.cursorLine = Math.max(0, Math.min(state.cursorLine, state.composerLines.length - 1));
  state.cursorColumn = Math.max(0, Math.min(state.cursorColumn, lineLength(currentLine())));
  if (state.selectionAnchor) {
    state.selectionAnchor.line = Math.max(0, Math.min(state.selectionAnchor.line, state.composerLines.length - 1));
    state.selectionAnchor.column = Math.max(
      0,
      Math.min(state.selectionAnchor.column, lineLength(state.composerLines[state.selectionAnchor.line] ?? "")),
    );
  }
}

function scrollTranscript(red: Red.RedAPI, delta: number): void {
  state.mode = "transcript";
  state.transcriptScroll += delta;
  normalizeTranscriptScroll();
  updateModeStatus();
  render(red);
}

function normalizeTranscriptScroll(): void {
  const maxScroll = Math.max(0, state.transcript.length - 1);
  state.transcriptScroll = Math.max(0, Math.min(state.transcriptScroll, maxScroll));
}

function updateModeStatus(): void {
  if (state.mode === "composer") {
    state.status = "composer";
  } else if (state.transcriptScroll === 0) {
    state.status = "transcript";
  } else {
    state.status = `transcript +${state.transcriptScroll}`;
  }
}

function chars(value: string): string[] {
  return Array.from(value);
}

function lineLength(value: string): number {
  return chars(value).length;
}

function charCount(value: string): number {
  return chars(value).length;
}

function takeChars(value: string, count: number): string {
  return chars(value).slice(0, count).join("");
}

function dropChars(value: string, count: number): string {
  return chars(value).slice(count).join("");
}
