/// <reference path="../types/red.d.ts" />

const WINDOW_ID = "chat";

type Mode = "composer" | "transcript";

interface State {
  open: boolean;
  mode: Mode;
  composerLines: string[];
  cursorLine: number;
  cursorColumn: number;
  transcriptScroll: number;
  transcript: Red.PluginWindowLine[];
  status: string;
}

const state: State = {
  open: false,
  mode: "composer",
  composerLines: [""],
  cursorLine: 0,
  cursorColumn: 0,
  transcriptScroll: 0,
  transcript: [
    { text: "Codex Chat Window" },
    { text: "This is the first visual slice. Type in the composer and press Enter to add a local turn." },
  ],
  status: "local preview",
};

export async function activate(red: Red.RedAPI): Promise<void> {
  red.addCommand("codex.open", () => open(red));
  red.addCommand("codex.cancel", () => {
    state.status = "cancelled";
    state.transcript.push({ text: "Cancelled active local preview turn." });
    render(red);
  });

  red.onPluginWindowEvent(WINDOW_ID, (event) => {
    handleWindowEvent(red, event);
  });

  red.on("editor:ready", () => {
    red.logInfo("Codex plugin loaded. Run :codex.open or bind PluginCommand codex.open.");
  });
}

function open(red: Red.RedAPI): void {
  state.open = true;
  red.createPluginWindow(WINDOW_ID, { title: "Codex" });
  render(red);
  red.focusPluginWindow(WINDOW_ID);
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
      submit(red);
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
      state.status = "cancelled";
      render(red);
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

function submit(red: Red.RedAPI): void {
  if (state.mode !== "composer") {
    state.mode = "composer";
    updateModeStatus();
    render(red);
    return;
  }

  const prompt = state.composerLines.join("\n").trimEnd();
  if (!prompt) {
    return;
  }

  state.transcript.push({ text: `You: ${prompt}` });
  state.transcript.push({
    text: "Codex: app-server integration is not connected yet. This local preview proves the Plugin Window, transcript, composer, and key routing.",
  });
  state.composerLines = [""];
  state.cursorLine = 0;
  state.cursorColumn = 0;
  state.transcriptScroll = 0;
  state.status = "ready";
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
    composerCursor: {
      line: state.cursorLine,
      column: state.cursorColumn,
    },
    keyHints: [
      "Enter send",
      "Ctrl-j newline",
      state.mode === "composer" ? "Esc transcript" : "Esc composer",
      "Ctrl-f/b page",
      "Ctrl-w w focus",
    ],
  });
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

function takeChars(value: string, count: number): string {
  return chars(value).slice(0, count).join("");
}

function dropChars(value: string, count: number): string {
  return chars(value).slice(count).join("");
}
