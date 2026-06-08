const COMMAND = "ProjectSearch";
const EXPORT_PANEL_ID = "project-search-results";
const HISTORY_STORAGE_KEY = "historyByCwd";
const HISTORY_LIMIT = 100;
const MATCH_LIMIT = 500;
const DEBOUNCE_MS = 100;

let exportedLocations = new Map();

function textField(value) {
  if (!value) return "";
  if (typeof value.text === "string") return value.text;
  if (typeof value.bytes !== "string") return "";

  try {
    const binary = globalThis.atob(value.bytes);
    const bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0));
    return new globalThis.TextDecoder("utf-8", { fatal: false }).decode(bytes);
  } catch (_) {
    return "";
  }
}

export function buildRipgrepArgs(query, options = {}) {
  const args = [
    "--json",
    "--color=never",
    "--no-heading",
    "--with-filename",
    "--line-number",
    "--column",
    "--smart-case",
    "--max-columns=500",
    "--max-columns-preview",
    "--glob=!.bare",
    "--glob=!.git",
  ];

  for (const pattern of options.exclude || []) {
    args.push("--glob", `!${pattern}`);
  }

  args.push(options.hidden ? "--hidden" : "--no-hidden");
  if (options.ignored) args.push("--no-ignore");
  if (options.follow) args.push("--follow");
  if (options.regex === false) args.push("--fixed-strings");

  for (const fileType of options.fileTypes || []) {
    args.push("--type", fileType);
  }
  for (const glob of options.globs || []) {
    args.push("--glob", glob);
  }
  args.push(...(options.args || []), "--", query);
  return args;
}

export function parseRipgrepJsonLine(line) {
  let message;
  try {
    message = JSON.parse(line);
  } catch (_) {
    return null;
  }

  if (message.type !== "match" || !message.data) return null;
  const path = textField(message.data.path);
  const text = textField(message.data.lines).replace(/\r?\n$/, "");
  if (!path || !Number.isInteger(message.data.line_number)) return null;

  const matches = (message.data.submatches || []).map((submatch) => ({
    start: submatch.start,
    end: submatch.end,
    text: textField(submatch.match),
  }));
  const submatch = matches[0];
  const column = submatch ? submatch.start : 0;
  return {
    path,
    line: message.data.line_number - 1,
    column,
    columnEncoding: "utf8-byte",
    text,
    match: submatch?.text || "",
    matches,
  };
}

function utf8ByteLength(text) {
  let byteLength = 0;
  for (const character of text) {
    const codePoint = character.codePointAt(0);
    if (codePoint <= 0x7f) byteLength += 1;
    else if (codePoint <= 0x7ff) byteLength += 2;
    else if (codePoint <= 0xffff) byteLength += 3;
    else byteLength += 4;
  }
  return byteLength;
}

function utf8ByteOffsetToCharacterIndex(text, byteOffset) {
  let consumedBytes = 0;
  let characterIndex = 0;
  for (const character of text) {
    const characterBytes = utf8ByteLength(character);
    if (consumedBytes + characterBytes > byteOffset) break;
    consumedBytes += characterBytes;
    characterIndex += 1;
  }
  return characterIndex;
}

export function createSearchState(overrides = {}) {
  return {
    generation: 0,
    query: "",
    matches: [],
    running: false,
    error: null,
    hidden: false,
    ignored: false,
    follow: false,
    regex: true,
    preview: true,
    truncated: false,
    ...overrides,
  };
}

export function beginSearch(state, query) {
  return {
    ...state,
    generation: state.generation + 1,
    query,
    matches: [],
    running: query.length > 0,
    error: null,
    truncated: false,
  };
}

export function isCurrentGeneration(state, generation) {
  return state.generation === generation;
}

export function addSearchHistory(entries, query, limit = HISTORY_LIMIT) {
  const normalized = query.trim();
  if (!normalized) return entries.slice();
  return [normalized, ...entries.filter((entry) => entry !== normalized)].slice(0, limit);
}

export function buildPickerItems(matches, options = {}) {
  return matches.map((match, index) => {
    const byteMatches = match.matches || (match.match ? [{
      start: match.column,
      end: match.column + utf8ByteLength(match.match),
    }] : []);
    const detailMatches = byteMatches.map(({ start, end }) => [
      utf8ByteOffsetToCharacterIndex(match.text, start),
      utf8ByteOffsetToCharacterIndex(match.text, end),
    ]);
    return {
      id: `${match.path}\u0000${match.line}\u0000${match.column}\u0000${index}`,
      label: match.path,
      annotation: `:${match.line + 1}:${match.column + 1}`,
      detail: match.text,
      detailMatches,
      data: {
        location: {
          path: match.path,
          line: match.line,
          column: match.column,
          columnEncoding: match.columnEncoding || "utf8-byte",
        },
      },
      preview: options.preview === false ? undefined : {
        path: match.path,
        line: match.line,
        column: match.column,
        matches: byteMatches.map(({ start, end }) => [start, end]),
      },
    };
  });
}

export function buildExportPanelRows(matches) {
  return buildPickerItems(matches).map((item) => ({
    id: item.id,
    path: item.data.location.path,
    expanded: null,
    kind: "file",
    segments: [{ text: `${item.label}${item.annotation}: ${item.detail}`, style: null }],
    right_segments: [],
  }));
}

function statusFor(state) {
  if (state.error) return state.error;
  const flags = [
    state.regex ? "regex" : "literal",
    state.hidden ? "hidden" : null,
    state.ignored ? "ignored" : null,
    state.follow ? "follow" : null,
    state.preview ? "preview" : null,
  ].filter(Boolean).join(" ");
  if (state.running) return `Searching (${state.matches.length}/${MATCH_LIMIT}) [${flags}]`;
  if (!state.query) return "Type to search";
  return `${state.matches.length} matches${state.truncated ? " (limit reached)" : ""} [${flags}]`;
}

function exportResults(red, matches) {
  const items = buildPickerItems(matches);
  exportedLocations = new Map(items.map((item) => [item.id, item.data.location]));
  red.createPanel(EXPORT_PANEL_ID, {
    side: "right",
    width: 64,
    title: "Project Search Results",
  });
  red.updatePanel(EXPORT_PANEL_ID, buildExportPanelRows(matches));
  red.focusPanel(EXPORT_PANEL_ID);
}

async function showProjectSearch(red) {
  const cwd = (await red.getConfig("cwd")) || ".";
  const historyByCwd = (await red.storage.get(HISTORY_STORAGE_KEY)) || {};
  let history = Array.isArray(historyByCwd[cwd]) ? historyByCwd[cwd] : [];
  let state = createSearchState();
  let process = null;
  let picker = null;
  let stderr = "";
  let historyIndex = -1;
  let historyDraft = "";
  let debounceGeneration = 0;
  let refreshScheduled = false;

  const refresh = () => {
    refreshScheduled = false;
    if (!picker) return;
    picker.updateItems(buildPickerItems(state.matches, { preview: state.preview }));
    picker.updateStatus(statusFor(state));
  };

  const scheduleRefresh = () => {
    if (refreshScheduled) return;
    refreshScheduled = true;
    globalThis.setTimeout(refresh, 16);
  };

  const cancelProcess = () => {
    debounceGeneration += 1;
    process?.kill();
    process = null;
  };

  const run = (query) => {
    cancelProcess();
    state = beginSearch(state, query);
    stderr = "";
    const generation = state.generation;
    refresh();
    if (!query) return;

    const failSearch = (message) => {
      if (!isCurrentGeneration(state, generation)) return;
      state.running = false;
      state.error = String(message);
      scheduleRefresh();
    };

    try {
      process = red.spawnProcess({
        command: "rg",
        args: buildRipgrepArgs(query, {
          hidden: state.hidden,
          ignored: state.ignored,
          follow: state.follow,
          regex: state.regex,
        }),
        cwd,
        onStdout(line) {
          if (!isCurrentGeneration(state, generation)) return;
          const match = parseRipgrepJsonLine(line);
          if (match && state.matches.length < MATCH_LIMIT) state.matches.push(match);
          if (state.matches.length >= MATCH_LIMIT) {
            state.truncated = true;
            state.running = false;
            process?.kill();
          }
          scheduleRefresh();
        },
        onStderr(line) {
          if (isCurrentGeneration(state, generation)) stderr += `${line}\n`;
        },
        onError: failSearch,
        onExit(result) {
          if (!isCurrentGeneration(state, generation)) return;
          state.running = false;
          if (!state.truncated && result.code !== 0 && result.code !== 1) {
            state.error = stderr.trim() || `rg exited with code ${result.code}`;
          }
          scheduleRefresh();
        },
      });
    } catch (error) {
      failSearch(error);
    }
  };

  const scheduleRun = (query) => {
    const debounce = ++debounceGeneration;
    globalThis.setTimeout(() => {
      if (debounce === debounceGeneration) run(query);
    }, DEBOUNCE_MS);
  };

  const remember = async (query) => {
    history = addSearchHistory(history, query);
    historyByCwd[cwd] = history;
    await red.storage.set(HISTORY_STORAGE_KEY, historyByCwd);
  };

  const openItem = async (item, target) => {
    const location = item?.data?.location;
    if (!location) return;
    await remember(state.query);
    picker.close();
    await red.openLocation(location, { target });
  };

  const moveHistory = (delta) => {
    if (!history.length) return;
    if (historyIndex === -1 && delta > 0) historyDraft = state.query;
    historyIndex = Math.max(-1, Math.min(history.length - 1, historyIndex + delta));
    const query = historyIndex === -1 ? historyDraft : history[historyIndex];
    picker.updateQuery(query);
    run(query);
  };

  picker = red.createPicker("Find in Files", [], {
    placeholder: "Search with ripgrep",
    externalFilter: true,
    actions: [
      { action: "open_horizontal", key: "Ctrl-s", label: "Open horizontal split" },
      { action: "open_vertical", key: "Ctrl-v", label: "Open vertical split" },
      { action: "export", key: "Ctrl-q", label: "Export results" },
      { action: "toggle_regex", key: "Alt-r", label: "Toggle regex" },
      { action: "toggle_hidden", key: "Alt-h", label: "Toggle hidden" },
      { action: "toggle_ignored", key: "Alt-i", label: "Toggle ignored" },
      { action: "toggle_follow", key: "Alt-f", label: "Toggle symlink following" },
      { action: "toggle_all", key: "Ctrl-e", label: "Toggle hidden and ignored" },
      { action: "toggle_preview", key: "Alt-p", label: "Toggle preview" },
      { action: "history_back", key: "Ctrl-h", label: "Previous search" },
      { action: "history_forward", key: "Ctrl-l", label: "Next search" },
    ],
    onQuery(query) {
      historyIndex = -1;
      historyDraft = query;
      scheduleRun(query);
    },
    async onAction(action, item) {
      if (action === "open_horizontal") {
        await openItem(item, "horizontal");
      } else if (action === "open_vertical") {
        await openItem(item, "vertical");
      } else if (action === "toggle_regex") {
        state.regex = !state.regex;
        run(state.query);
      } else if (action === "toggle_hidden") {
        state.hidden = !state.hidden;
        run(state.query);
      } else if (action === "toggle_ignored") {
        state.ignored = !state.ignored;
        run(state.query);
      } else if (action === "toggle_follow") {
        state.follow = !state.follow;
        run(state.query);
      } else if (action === "toggle_all") {
        state.hidden = !state.hidden;
        state.ignored = state.hidden;
        run(state.query);
      } else if (action === "toggle_preview") {
        state.preview = !state.preview;
        refresh();
      } else if (action === "history_back") {
        moveHistory(1);
      } else if (action === "history_forward") {
        moveHistory(-1);
      } else if (action === "export") {
        exportResults(red, state.matches);
      }
    },
    onClose: cancelProcess,
  });
  picker.updateStatus(statusFor(state));

  try {
    const selected = await picker.result;
    if (selected) await openItem(selected, "current");
  } finally {
    cancelProcess();
    picker.close();
  }
}

export async function activate(red) {
  red.addCommand(COMMAND, () => showProjectSearch(red));
  red.onPanelEvent(EXPORT_PANEL_ID, async (event) => {
    if (event.action === "close") {
      red.closePanel(EXPORT_PANEL_ID);
      return;
    }
    if (event.action !== "activate" || !event.row) return;
    const location = exportedLocations.get(event.row.id);
    if (location) await red.openLocation(location, { target: "current" });
  });
}

export async function deactivate(red) {
  exportedLocations.clear();
  red.closePanel(EXPORT_PANEL_ID);
}
