/// <reference path="../types/red.d.ts" />

const WINDOW_ID = "chat";

type Mode = "composer" | "transcript";

interface State {
  open: boolean;
  mode: Mode;
  composer: string;
  transcript: Red.PluginWindowLine[];
  status: string;
}

const state: State = {
  open: false,
  mode: "composer",
  composer: "",
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
      state.status = state.mode;
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
      if (state.composer.length > 0) {
        state.composer = state.composer.slice(0, -1);
        render(red);
      }
      return;
    case "Ctrl-c":
      state.status = "cancelled";
      render(red);
      return;
    default:
      if (event.text && event.modifiers.length === 0) {
        insertText(red, event.text);
      }
  }
}

function insertText(red: Red.RedAPI, text: string): void {
  if (state.mode !== "composer") {
    return;
  }
  state.composer += text;
  state.status = "editing";
  render(red);
}

function submit(red: Red.RedAPI): void {
  if (state.mode !== "composer") {
    state.mode = "composer";
    state.status = "composer";
    render(red);
    return;
  }

  const prompt = state.composer.trimEnd();
  if (!prompt) {
    return;
  }

  state.transcript.push({ text: `You: ${prompt}` });
  state.transcript.push({
    text: "Codex: app-server integration is not connected yet. This local preview proves the Plugin Window, transcript, composer, and key routing.",
  });
  state.composer = "";
  state.status = "ready";
  render(red);
}

function render(red: Red.RedAPI): void {
  const composerLines = state.composer.length === 0
    ? [{ text: "" }]
    : state.composer.split("\n").map((text) => ({ text }));

  red.updatePluginWindow(WINDOW_ID, {
    kind: "chat",
    title: "Codex",
    status: state.status,
    transcript: state.transcript,
    composer: composerLines,
    composerCursor: {
      line: composerLines.length - 1,
      column: composerLines[composerLines.length - 1]?.text.length ?? 0,
    },
    keyHints: [
      "Enter send",
      "Ctrl-j newline",
      "Esc mode",
      "Ctrl-w w focus",
    ],
  });
}
