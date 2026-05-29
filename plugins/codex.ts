/// <reference path="../types/red.d.ts" />

const WINDOW_ID = "chat";
const FOLLOW_OVERLAY_ID = "codex.followChanges";
const LARGE_PASTE_CHAR_THRESHOLD = 1000;
const STORAGE_KEY = "codex.chat";

type Mode = "composer" | "transcript";

interface ContextAttachment {
  label: string;
  content: string;
  path?: string;
  startLine?: number;
  endLine?: number;
}

interface State {
  open: boolean;
  mode: Mode;
  composerLines: string[];
  cursorLine: number;
  cursorColumn: number;
  transcriptScroll: number;
  contextAttachments: ContextAttachment[];
  threadId?: string;
  projectCwd?: string;
  inFlight: boolean;
  followChanges: boolean;
  transcript: Red.PluginWindowLine[];
  status: string;
  activeStreamId?: string;
  activeAgentLine?: number;
  activeAgentText: string;
  activeNotifications: any[];
  loadedTranscriptThreadId?: string;
  lastFollowedPath?: string;
}

const state: State = {
  open: false,
  mode: "composer",
  composerLines: [""],
  cursorLine: 0,
  cursorColumn: 0,
  transcriptScroll: 0,
  contextAttachments: [],
  inFlight: false,
  followChanges: false,
  transcript: [
    { text: "Codex Chat Window" },
    { text: "Ask Codex a question or attach editor context before sending." },
  ],
  status: "local preview",
  activeAgentText: "",
  activeNotifications: [],
};

export async function activate(red: Red.RedAPI): Promise<void> {
  registerCommands(red);

  red.onPluginWindowEvent(WINDOW_ID, (event) => {
    handleWindowEvent(red, event);
  });

  red.on("editor:ready", () => {
    void restoreStoredThread(red, undefined, { loadTranscript: true }).catch((error) => {
      red.logWarn("Codex thread restore failed", String(error));
    });
    red.logInfo(
      "Codex plugin loaded. Run :codex.open or bind PluginCommand codex.open.",
    );
  });
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
    description: "Interrupt the active streamed Codex turn.",
    suggestedKeys: ["Ctrl-c"],
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
  registerCommandAlias(red, "codex.sessions.resume", "codex.resume", () =>
    resumeProjectSession(red),
  );
  registerCommandAlias(red, "codex.followChanges.toggle", "codex.toggleFollowChanges", () =>
    toggleFollowChanges(red),
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
  await persistThread(red);
}

function open(red: Red.RedAPI): void {
  state.open = true;
  red.createPluginWindow(WINDOW_ID, { title: "Codex" });
  render(red);
  red.focusPluginWindow(WINDOW_ID);
}

async function openAndRestore(red: Red.RedAPI): Promise<void> {
  open(red);
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
  open(red);
  state.status = "loading sessions";
  render(red);

  try {
    const snapshot = await red.getEditorState();
    const workspaceRoot = await currentWorkspaceRoot(red, snapshot);
    const sessions = await fetchProjectSessions(red, workspaceRoot);
    state.transcript.push({ text: `Sessions for ${workspaceRoot}` });
    if (sessions.length === 0) {
      state.transcript.push({ text: "No Codex sessions found for this project." });
    } else {
      for (const session of sessions) {
        const preview = session.preview ? ` - ${session.preview}` : "";
        state.transcript.push({ text: `${session.id}${preview}` });
      }
    }
    state.status = "sessions";
  } catch (error) {
    state.status = "app-server error";
    state.transcript.push({ text: `Codex app-server request failed: ${String(error)}` });
  }

  render(red);
}

async function resumeProjectSession(red: Red.RedAPI): Promise<void> {
  open(red);
  state.status = "loading sessions";
  render(red);

  try {
    const snapshot = await red.getEditorState();
    const workspaceRoot = await currentWorkspaceRoot(red, snapshot);
    const sessions = await fetchProjectSessions(red, workspaceRoot);
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
    state.status = "app-server error";
    state.transcript.push({ text: `Codex session resume failed: ${String(error)}` });
  }

  render(red);
}

async function fetchProjectSessions(red: Red.RedAPI, cwd: string): Promise<any[]> {
  const response = await red.codexAppServerRequest("thread/list", {
    limit: 20,
    cwd,
    sortKey: "updated_at",
    sortDirection: "desc",
  });
  return Array.isArray(response?.data) ? response.data : [];
}

function sessionLabel(session: any): string {
  const preview = session.preview ? ` - ${session.preview}` : "";
  return `${session.id}${preview}`;
}

async function loadThreadTranscript(red: Red.RedAPI, threadId: string): Promise<void> {
  const lines: Red.PluginWindowLine[] = [
    { text: "Codex Chat Window" },
    { text: `Resumed Codex session ${threadId}` },
  ];

  try {
    const response = await red.codexAppServerRequest("thread/read", {
      threadId,
      includeTurns: true,
    });
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
    state.transcript.push({ text: `Codex history load failed: ${String(error)}` });
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
      moveCursor(red, "left");
      return;
    case "Right":
      moveCursor(red, "right");
      return;
    case "Up":
      if (state.mode === "transcript") {
        scrollTranscript(red, 1);
      } else {
        moveCursor(red, "up");
      }
      return;
    case "Down":
      if (state.mode === "transcript") {
        scrollTranscript(red, -1);
      } else {
        moveCursor(red, "down");
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
      state.cursorColumn = 0;
      state.status = "editing";
      render(red);
      return;
    case "End":
      state.cursorColumn = lineLength(currentLine());
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

function moveCursor(red: Red.RedAPI, direction: "left" | "right" | "up" | "down"): void {
  if (state.mode !== "composer") {
    return;
  }

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

  state.status = "editing";
  render(red);
}

async function submit(red: Red.RedAPI): Promise<void> {
  if (state.mode !== "composer") {
    state.mode = "composer";
    updateModeStatus();
    render(red);
    return;
  }
  if (state.inFlight) {
    return;
  }

  const prompt = state.composerLines.join("\n").trimEnd();
  if (!prompt) {
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
  state.transcriptScroll = 0;
  state.contextAttachments = [];
  state.inFlight = true;
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
    state.activeAgentLine = state.transcript.push({ text: "Codex: " }) - 1;
    const turnParams: Red.CodexRunTurnParams = {
      prompt,
      cwd: workspaceRoot,
      runtimeWorkspaceRoots: [workspaceRoot],
      threadId: state.threadId,
    };
    if (additionalContext) {
      turnParams.additionalContext = additionalContext;
    }
    const streamId = red.codexStartTurn(turnParams, (event) => handleCodexTurnEvent(red, event));
    state.activeStreamId ??= streamId;
  } catch (error) {
    state.status = "app-server error";
    state.transcript.push({ text: `Codex: ${String(error)}` });
    state.inFlight = false;
  }

  render(red);
}

function render(red: Red.RedAPI): void {
  normalizeCursor();
  normalizeTranscriptScroll();

  const composerLines = state.composerLines.map((text) => ({ text }));

  red.updatePluginWindow(WINDOW_ID, {
    kind: "chat",
    title: "Codex",
    status: state.status,
    transcript: state.transcript,
    composer: composerLines,
    scroll: state.transcriptScroll,
    contextPlaceholders: contextPlaceholders(),
    composerCursor: {
      line: state.cursorLine,
      column: state.cursorColumn,
    },
    keyHints: [
      "Enter send",
      "Ctrl-j newline",
      "context commands",
      state.followChanges ? "follow on" : "follow off",
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
      state.threadId = event.thread?.id ?? state.threadId;
      if (state.threadId) {
        void persistThread(red).catch((error) => red.logWarn("Codex thread persist failed", String(error)));
      }
      state.status = "running";
      break;
    case "turn":
      state.status = "turn started";
      break;
    case "notification":
      state.activeNotifications.push(event.notification);
      applyCodexNotification(event.notification);
      updateFollowChanges(red, state.activeNotifications);
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
  state.threadId = result.thread?.id ?? state.threadId;
  if (state.threadId) {
    void persistThread(red).catch((error) => red.logWarn("Codex thread persist failed", String(error)));
  }
  const interrupted = String(result.turn?.status ?? "").toLowerCase() === "interrupted";
  state.activeNotifications = result.notifications;
  updateFollowChanges(red, result.notifications);
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
  state.inFlight = false;
  state.status = interrupted ? "interrupted" : "ready";
}

function failCodexTurn(red: Red.RedAPI, error: string): void {
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
  state.inFlight = false;
  state.status = "app-server error";
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
  const streamId = state.activeStreamId;
  if (!streamId || !state.inFlight) {
    state.status = "ready";
    render(red);
    return;
  }

  if (red.codexCancelTurn(streamId)) {
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

function updateFollowChanges(red: Red.RedAPI, notifications: any[]): void {
  if (!state.followChanges) {
    return;
  }

  followLatestChangedFile(red, notifications);

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

function followLatestChangedFile(red: Red.RedAPI, notifications: any[]): void {
  const changedPath = latestChangedPath(notifications);
  if (!changedPath || !state.projectCwd) {
    return;
  }

  const filePath = absolutePath(state.projectCwd, changedPath);
  if (state.lastFollowedPath === filePath) {
    return;
  }

  state.lastFollowedPath = filePath;
  red.openFile(filePath);
}

function latestChangedPath(notifications: any[]): string | undefined {
  for (const notification of [...notifications].reverse()) {
    if (
      notification.method === "item/fileChange/patchUpdated"
      && Array.isArray(notification.params?.changes)
    ) {
      const change = [...notification.params.changes].reverse()
        .find((candidate) => typeof candidate?.path === "string");
      if (change?.path) {
        return change.path;
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
      return change.path;
    }
  }

  return undefined;
}

async function restoreStoredThread(
  red: Red.RedAPI,
  cwd?: string,
  options: { loadTranscript?: boolean } = {},
): Promise<void> {
  const projectCwd = cwd ?? await currentWorkspaceRoot(red);
  const stored = await red.storage.get(STORAGE_KEY);
  if (!stored || stored.version !== 1 || stored.cwd !== projectCwd || !stored.threadId) {
    return;
  }

  state.threadId = stored.threadId;
  state.projectCwd = stored.cwd;
  if (options.loadTranscript && state.loadedTranscriptThreadId !== stored.threadId) {
    await loadThreadTranscript(red, stored.threadId);
    state.status = "resumed";
  }
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

async function persistThread(red: Red.RedAPI): Promise<void> {
  if (!state.threadId || !state.projectCwd) {
    await red.storage.delete(STORAGE_KEY);
    return;
  }

  await red.storage.set(STORAGE_KEY, {
    version: 1,
    cwd: state.projectCwd,
    threadId: state.threadId,
  });
}

async function addCurrentLineContext(red: Red.RedAPI): Promise<void> {
  open(red);

  const snapshot = await red.getEditorState();
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
  open(red);

  const snapshot = await red.getEditorState();
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
  open(red);

  const snapshot = await red.getEditorState();
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

function normalizeCursor(): void {
  if (state.composerLines.length === 0) {
    state.composerLines = [""];
  }

  state.cursorLine = Math.max(0, Math.min(state.cursorLine, state.composerLines.length - 1));
  state.cursorColumn = Math.max(0, Math.min(state.cursorColumn, lineLength(currentLine())));
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
