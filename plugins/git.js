const WORKSPACE_ID = "git-dashboard";
const GUTTER_NAMESPACE = "git-signs";
const REFRESH_DEBOUNCE_MS = 120;
const POLL_INTERVAL_MS = 5000;
const DEFAULT_SIGNS = {
  add: "+",
  change: "~",
  delete: "_",
  topdelete: "‾",
  changedelete: "~",
};
const DEFAULT_STAGED_SIGNS = {
  add: "┃",
  change: "┃",
  delete: "▁",
  topdelete: "▔",
  changedelete: "~",
};

let redApi = null;
let repository = null;
let dashboardOpen = false;
let refreshTimer = null;
let pollTimer = null;
let watchId = null;
let operationLog = [];
let styles = null;
let refreshing = false;
let pendingRefresh = false;
let pendingSelectedRow = null;
let pendingScratch = null;
let redExecutable = "red";
let signGlyphs = DEFAULT_SIGNS;
let stagedSignGlyphs = DEFAULT_STAGED_SIGNS;

const DEFAULT_STYLE = { fg: null, bg: null, bold: false, italic: false };

function style(base, overrides = {}) {
  return { ...DEFAULT_STYLE, ...(base || {}), ...overrides };
}

function colorChannels(color) {
  if (typeof color === "string") {
    const match = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})(?:[0-9a-f]{2})?$/iu.exec(color);
    return match ? match.slice(1).map((component) => Number.parseInt(component, 16)) : null;
  }
  const value = color?.Rgb || color?.Rgba;
  return value ? [value.r, value.g, value.b] : null;
}

export function blendColor(foreground, background, amount = 0.5) {
  const fg = colorChannels(foreground);
  const bg = colorChannels(background);
  if (!fg || !bg) return foreground;
  const channels = fg.map((component, index) => Math.round(component + (bg[index] - component) * amount));
  return `#${channels.map((component) => component.toString(16).padStart(2, "0")).join("")}`;
}

function stagedStyle(normal, background) {
  return style(normal, { fg: blendColor(normal?.fg, background) });
}

export function parsePorcelainV2(output) {
  const state = {
    root: null,
    head: null,
    oid: null,
    upstream: null,
    ahead: 0,
    behind: 0,
    stashCount: 0,
    staged: [],
    unstaged: [],
    untracked: [],
    conflicted: [],
  };
  const records = String(output || "").split("\0");
  for (let index = 0; index < records.length; index += 1) {
    const record = records[index];
    if (!record) continue;
    if (record.startsWith("# ")) {
      const space = record.indexOf(" ", 2);
      const key = space === -1 ? record.slice(2) : record.slice(2, space);
      const value = space === -1 ? "" : record.slice(space + 1);
      if (key === "branch.oid") state.oid = value === "(initial)" ? null : value;
      if (key === "branch.head") state.head = value;
      if (key === "branch.upstream") state.upstream = value;
      if (key === "branch.ab") {
        const match = /^\+(\d+) -(\d+)$/.exec(value);
        if (match) {
          state.ahead = Number(match[1]);
          state.behind = Number(match[2]);
        }
      }
      if (key === "stash") state.stashCount = Number(value) || 0;
      continue;
    }
    if (record.startsWith("? ")) {
      state.untracked.push({ path: record.slice(2), x: "?", y: "?", kind: "untracked" });
      continue;
    }
    if (record.startsWith("u ")) {
      const fields = record.split(" ");
      const path = fields.slice(10).join(" ");
      state.conflicted.push({ path, x: fields[1]?.[0] || "U", y: fields[1]?.[1] || "U", kind: "conflicted" });
      continue;
    }
    if (record.startsWith("1 ") || record.startsWith("2 ")) {
      const renamed = record.startsWith("2 ");
      const fields = record.split(" ");
      const xy = fields[1] || "..";
      const pathStart = renamed ? 9 : 8;
      const path = fields.slice(pathStart).join(" ");
      const entry = { path, originalPath: renamed ? records[index + 1] || null : null, x: xy[0], y: xy[1], kind: renamed ? "renamed" : "changed" };
      if (renamed) index += 1;
      if (entry.x !== ".") state.staged.push(entry);
      if (entry.y !== ".") state.unstaged.push(entry);
    }
  }
  return state;
}

export function parseUnifiedHunks(patch) {
  const hunks = [];
  const lines = String(patch || "").split("\n");
  let header = [];
  let current = null;
  for (const line of lines) {
    const match = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/.exec(line);
    if (match) {
      if (current) hunks.push(current);
      current = {
        oldStart: Number(match[1]),
        oldCount: match[2] == null ? 1 : Number(match[2]),
        newStart: Number(match[3]),
        newCount: match[4] == null ? 1 : Number(match[4]),
        lines: [line],
        header: [...header],
      };
    } else if (current) {
      current.lines.push(line);
    } else {
      header.push(line);
    }
  }
  if (current) hunks.push(current);
  return hunks;
}

export function patchForHunk(patch, line) {
  const hunk = parseUnifiedHunks(patch).find((candidate) => {
    const start = Math.max(1, candidate.newStart);
    const end = start + Math.max(1, candidate.newCount) - 1;
    return line + 1 >= start && line + 1 <= end;
  });
  if (!hunk) return null;
  return [...hunk.header, ...hunk.lines].join("\n");
}

export function patchForLineRange(patch, startLine, endLine) {
  const selectedHunks = [];
  let commonHeader = null;
  for (const hunk of parseUnifiedHunks(patch)) {
    const source = hunk.lines.slice(1);
    const output = [];
    let newLine = Math.max(0, hunk.newStart - 1);
    for (let index = 0; index < source.length;) {
      const line = source[index];
      if (!line.startsWith("+") && !line.startsWith("-")) {
        output.push(line);
        if (line.startsWith(" ")) newLine += 1;
        index += 1;
        continue;
      }
      const removed = [];
      const added = [];
      while (index < source.length && source[index].startsWith("-")) removed.push(source[index++]);
      const additionStart = newLine;
      while (index < source.length && source[index].startsWith("+")) {
        added.push(source[index++]);
        newLine += 1;
      }
      const additionEnd = additionStart + Math.max(0, added.length - 1);
      const selected = added.length > 0
        ? additionStart <= endLine && additionEnd >= startLine
        : additionStart >= startLine && additionStart <= endLine + 1;
      if (selected) output.push(...removed, ...added);
      else output.push(...removed.map((removedLine) => ` ${removedLine.slice(1)}`));
    }
    const hasChange = output.some((line) => line.startsWith("+") || line.startsWith("-"));
    if (!hasChange) continue;
    const oldCount = output.filter((line) => line.startsWith(" ") || line.startsWith("-")).length;
    const newCount = output.filter((line) => line.startsWith(" ") || line.startsWith("+")).length;
    const suffix = hunk.lines[0].replace(/^@@ .*? @@/, "");
    const headerLine = `@@ -${hunk.oldStart},${oldCount} +${hunk.newStart},${newCount} @@${suffix}`;
    commonHeader ||= hunk.header;
    selectedHunks.push([headerLine, ...output].join("\n"));
  }
  return selectedHunks.length > 0 ? [...(commonHeader || []), ...selectedHunks].join("\n") : null;
}

export function configuredSignGlyphs(configured, defaults) {
  return Object.fromEntries(Object.entries(defaults).map(([kind, fallback]) => {
    const value = configured?.[kind];
    return [kind, typeof value === "string" && value.length > 0 ? value : fallback];
  }));
}

function hunkType(hunk) {
  if (hunk.oldCount === 0) return "add";
  if (hunk.newCount === 0) return "delete";
  return "change";
}

function hunkChangeEnd(hunk) {
  const type = hunkType(hunk);
  if (type === "delete") return hunk.newStart;
  if (type === "add") return hunk.newStart + hunk.newCount - 1;
  return hunk.newStart + Math.min(hunk.newCount, hunk.oldCount) - 1;
}

export function signsFromPatch(patch, bufferIndex, staged = false, glyphs = staged ? DEFAULT_STAGED_SIGNS : DEFAULT_SIGNS) {
  const signs = [];
  const hunks = parseUnifiedHunks(patch);
  for (let index = 0; index < hunks.length; index += 1) {
    const hunk = hunks[index];
    const previous = hunks[index - 1];
    const next = hunks[index + 1];
    const type = hunkType(hunk);
    const priority = staged ? 5 : 10;
    const changeEnd = hunkChangeEnd(hunk);
    const topdelete = type === "delete"
      && (hunk.newStart === 0 || (previous && hunkChangeEnd(previous) === hunk.newStart))
      && (!next || next.newStart !== hunk.newStart + 1);

    for (let line = hunk.newStart; line <= changeEnd; line += 1) {
      const changedelete = type === "change"
        && ((hunk.oldCount > hunk.newCount && line === changeEnd) || previous?.newStart === 0);
      const kind = topdelete ? "topdelete" : changedelete ? "changedelete" : type;
      const oneBasedLine = line + (topdelete ? 1 : 0);
      signs.push({
        bufferIndex,
        line: Math.max(0, oneBasedLine - 1),
        text: glyphs[kind],
        priority,
        kind,
        staged,
      });
    }

    if (type === "change" && hunk.newCount > hunk.oldCount) {
      for (let line = changeEnd + 1; line < hunk.newStart + hunk.newCount; line += 1) {
        signs.push({
          bufferIndex,
          line: Math.max(0, line - 1),
          text: glyphs.add,
          priority,
          kind: "add",
          staged,
        });
      }
    }
  }
  return signs;
}

export function safeSyncDecision(state, dirty) {
  if (dirty) return { action: "stop", reason: "working tree is dirty" };
  if (!state.upstream) return { action: "choose_upstream" };
  if (state.ahead === 0 && state.behind === 0) return { action: "noop" };
  if (state.ahead === 0) return { action: "pull_ff" };
  if (state.behind === 0) return { action: "push" };
  return { action: "diverged" };
}

async function runGit(args, options = {}) {
  const stdout = [];
  const stderr = [];
  const handle = redApi.spawnProcess({
    command: "git",
    args,
    cwd: options.cwd || repository?.root || options.fallbackCwd || null,
    stdin: options.stdin ?? null,
    env: {
      GIT_PAGER: "cat",
      GIT_TERMINAL_PROMPT: "0",
      LC_ALL: "C",
      ...(options.env || {}),
    },
    rawOutput: true,
    onStdout: (line) => stdout.push(line),
    onStderr: (line) => stderr.push(line),
  });
  const result = await handle.result;
  const response = { code: result.code, stdout: stdout.join("\n"), stderr: stderr.join("\n"), args };
  operationLog.push(response);
  operationLog = operationLog.slice(-50);
  if (result.code !== 0 && !options.allowFailure) {
    throw new Error(response.stderr || `git ${args.join(" ")} exited with ${result.code}`);
  }
  return response;
}

function relativePath(path, root) {
  const normalizedRoot = String(root || "").replace(/[\\/]+$/, "");
  const normalizedPath = String(path || "");
  if (normalizedPath === normalizedRoot) return ".";
  if (normalizedPath.startsWith(`${normalizedRoot}/`) || normalizedPath.startsWith(`${normalizedRoot}\\`)) {
    return normalizedPath.slice(normalizedRoot.length + 1).replaceAll("\\", "/");
  }
  return normalizedPath.replaceAll("\\", "/");
}

export function repositoryPath(path, root) {
  const value = String(path || "");
  const isAbsolute = value.startsWith("/") || value.startsWith("\\") || /^[A-Za-z]:[\\/]/u.test(value);
  const relative = relativePath(value, root);
  if (isAbsolute && relative === value.replaceAll("\\", "/")) return null;
  return relative.replace(/^\.\//u, "");
}

export function searchDirectoryForBuffer(path, fallbackCwd) {
  const value = String(path || "");
  const separator = Math.max(value.lastIndexOf("/"), value.lastIndexOf("\\"));
  if (separator < 0) return fallbackCwd || ".";
  if (separator === 0) return value.slice(0, 1);
  return value.slice(0, separator);
}

function gitPatchPath(path, side) {
  const value = `${side}/${path}`;
  return /[\s"\\]/u.test(value) ? JSON.stringify(value) : value;
}

async function findRepository() {
  const info = await redApi.getEditorInfo();
  const active = info.buffers.find((buffer) => buffer.id === info.current_buffer_index) || info.buffers[info.current_buffer_index];
  const configCwd = await redApi.getConfig("cwd");
  const cwd = searchDirectoryForBuffer(active?.path, configCwd || ".");
  const result = await runGit(["rev-parse", "--show-toplevel"], { cwd, fallbackCwd: cwd, allowFailure: true });
  if (result.code !== 0) return null;
  const root = result.stdout.trim();
  const gitDir = await runGit(["rev-parse", "--absolute-git-dir"], { cwd: root, allowFailure: true });
  return { root, gitDir: gitDir.stdout.trim(), info };
}

async function detectOperation() {
  for (const [name, ref] of [["merge", "MERGE_HEAD"], ["cherry-pick", "CHERRY_PICK_HEAD"], ["revert", "REVERT_HEAD"]]) {
    const result = await runGit(["rev-parse", "-q", "--verify", ref], { allowFailure: true });
    if (result.code === 0) return name;
  }
  const rebase = await runGit(["rebase", "--show-current-patch"], { allowFailure: true });
  return rebase.code === 0 ? "rebase" : null;
}

async function readState() {
  const found = await findRepository();
  if (!found) return null;
  repository = { root: found.root, gitDir: found.gitDir };
  const result = await runGit(["status", "--porcelain=v2", "--branch", "--show-stash", "-z", "--untracked-files=all"]);
  const state = parsePorcelainV2(result.stdout);
  state.root = repository.root;
  state.gitDir = repository.gitDir;
  state.operation = await detectOperation();
  repository = state;
  return state;
}

async function refreshSigns() {
  if (!repository) return;
  const windows = await redApi.getWindows();
  const info = await redApi.getEditorInfo();
  const signs = [];
  for (const window of windows) {
    if (!window.bufferPath) continue;
    const path = repositoryPath(window.bufferPath, repository.root);
    if (!path || path.startsWith("../")) continue;
    const [working, staged] = await Promise.all([
      runGit(["diff", "--no-ext-diff", "--unified=0", "--", path], { allowFailure: true }),
      runGit(["diff", "--cached", "--no-ext-diff", "--unified=0", "--", path], { allowFailure: true }),
    ]);
    for (const sign of [
      ...signsFromPatch(staged.stdout, window.bufferIndex, true, stagedSignGlyphs),
      ...signsFromPatch(working.stdout, window.bufferIndex, false, signGlyphs),
    ]) {
      const palette = sign.staged ? styles.sign.staged : styles.sign.normal;
      sign.style = palette[sign.kind] || palette.change;
      delete sign.kind;
      delete sign.staged;
      signs.push(sign);
    }
    if (window.bufferIndex === info.current_buffer_index) {
      const currentText = await redApi.getBufferText();
      if (currentText.split("\n").length <= 40_000) {
        const unsaved = await runGit(["diff", "--no-index", "--no-ext-diff", "--unified=0", "--", window.bufferPath, "-"], {
          stdin: currentText,
          allowFailure: true,
        });
        for (const sign of signsFromPatch(unsaved.stdout, window.bufferIndex, false, signGlyphs)) {
          sign.priority = 20;
          sign.style = styles.sign.normal[sign.kind] || styles.sign.normal.change;
          delete sign.kind;
          delete sign.staged;
          signs.push(sign);
        }
      }
    }
  }
  redApi.setGutterSigns(GUTTER_NAMESPACE, signs);
}

function statusLabel(entry) {
  if (entry.kind === "untracked") return "?";
  if (entry.kind === "conflicted") return "!";
  const code = `${entry.x}${entry.y}`;
  if (code.includes("R")) return "R";
  if (code.includes("A")) return "+";
  if (code.includes("D")) return "−";
  return "~";
}

function sectionRows(title, entries, section) {
  if (entries.length === 0) return [];
  return [
    { id: `section:${section}`, selectable: false, segments: [{ text: `${title}  ${entries.length}`, style: styles.heading }] },
    ...entries.map((entry) => ({
      id: `${section}:${entry.path}`,
      selectable: true,
      depth: 1,
      segments: [
        { text: `${statusLabel(entry)} `, style: styles.status[section] || styles.normal },
        { text: entry.path, style: styles.normal },
      ],
      data: { section, path: entry.path, entry },
    })),
  ];
}

function dashboardRows(state) {
  const rows = [];
  if (state.operation) {
    rows.push({ id: "section:operation", selectable: false, segments: [{ text: `Ongoing ${state.operation}`, style: styles.warning }] });
  }
  rows.push(...sectionRows("Conflicts", state.conflicted, "conflicted"));
  rows.push(...sectionRows("Staged", state.staged, "staged"));
  rows.push(...sectionRows("Unstaged", state.unstaged, "unstaged"));
  rows.push(...sectionRows("Untracked", state.untracked, "untracked"));
  if (rows.length === 0) rows.push({ id: "clean", selectable: false, segments: [{ text: "Working tree clean", style: styles.success }] });
  return rows;
}

function headerSegments(state) {
  const branch = state.head === "(detached)" ? `detached ${String(state.oid || "").slice(0, 8)}` : state.head || "unborn";
  const tracking = state.upstream ? ` → ${state.upstream}` : " (no upstream)";
  const counts = `  ↑${state.ahead} ↓${state.behind}`;
  const operation = state.operation ? `  ${state.operation.toUpperCase()}` : "";
  return [
    { text: branch, style: styles.branch },
    { text: tracking, style: styles.muted },
    { text: counts, style: state.ahead || state.behind ? styles.warning : styles.muted },
    { text: operation, style: styles.warning },
  ];
}

async function detailFor(row) {
  if (!row?.data?.path || !repository) return [[{ text: "Select a change to inspect its diff.", style: styles.muted }]];
  const cached = row.data.section === "staged";
  const args = ["diff", ...(cached ? ["--cached"] : []), "--no-ext-diff", "--color=never", "--", row.data.path];
  const result = await runGit(args, { allowFailure: true });
  if (!result.stdout) return [[{ text: row.data.path, style: styles.heading }], [{ text: "No textual diff available.", style: styles.muted }]];
  return result.stdout.split("\n").slice(0, 1000).map((line) => [{
    text: line,
    style: line.startsWith("+") && !line.startsWith("+++") ? styles.diffAdded
      : line.startsWith("-") && !line.startsWith("---") ? styles.diffDeleted
        : line.startsWith("@@") ? styles.diffHunk : styles.normal,
  }]);
}

async function renderDashboard(selectedRow = null) {
  if (!dashboardOpen || !repository) return;
  const rows = dashboardRows(repository);
  const detailRow = selectedRow || rows.find((row) => row.selectable) || null;
  redApi.updateWorkspace(WORKSPACE_ID, {
    header: headerSegments(repository),
    rows,
    detail: await detailFor(detailRow),
    footer: [{ text: "s stage  u unstage  x discard  c commit  y sync  b branch  t tag  z stash  w worktree  l log  i rebase  ? help  q close", style: styles.muted }],
  });
}

async function refresh(selectedRow = null) {
  if (refreshing) {
    pendingRefresh = true;
    pendingSelectedRow = selectedRow || pendingSelectedRow;
    return;
  }
  refreshing = true;
  try {
    const state = await readState();
    if (!state) {
      redApi.clearGutterSigns(GUTTER_NAMESPACE);
      if (dashboardOpen) {
        redApi.updateWorkspace(WORKSPACE_ID, { rows: [{ id: "not-repo", selectable: false, segments: [{ text: "The active buffer is not inside a Git repository.", style: styles.warning }] }] });
      }
      return;
    }
    await Promise.all([refreshSigns(), renderDashboard(selectedRow)]);
  } catch (error) {
    redApi.logError?.(`Git refresh failed: ${error.message}`);
  } finally {
    refreshing = false;
    if (pendingRefresh) {
      const queuedRow = pendingSelectedRow;
      pendingRefresh = false;
      pendingSelectedRow = null;
      await refresh(queuedRow);
    }
  }
}

function scheduleRefresh() {
  if (refreshTimer != null) redApi.clearTimeout(refreshTimer);
  redApi.setTimeout(async () => {
    refreshTimer = null;
    await refresh();
  }, REFRESH_DEBOUNCE_MS).then((id) => { refreshTimer = id; });
}

async function confirmAction(title, impact) {
  const choice = await redApi.pick(`${title}\n${impact}`, ["Cancel", "Proceed"]);
  return choice === "Proceed";
}

function promptText(title, initialQuery = "") {
  return new Promise((resolve) => {
    let settled = false;
    const finish = (value) => {
      if (settled) return;
      settled = true;
      controller.close();
      resolve(value);
    };
    const controller = redApi.createPicker(title, [{ id: "help", label: "Type above, then press Ctrl-Enter", data: null }], {
      externalFilter: true,
      initialQuery,
      actions: [{ key: "c-enter", action: "submit", label: "Submit" }],
      onAction: (action, _item, query) => { if (action === "submit") finish(query); },
      onCancel: () => finish(null),
    });
  });
}

async function promptScratch(title, initialText = "") {
  if (pendingScratch) return null;
  const snapshot = await redApi.getEditorState();
  const reopenDashboard = dashboardOpen;
  if (reopenDashboard) closeDashboard();
  const template = `${initialText}${initialText && !initialText.endsWith("\n") ? "\n" : ""}\n# ${title}\n# Edit this buffer normally. Space c c submits; Space c q cancels.\n# Lines beginning with # are removed.\n`;
  const opened = await redApi.openScratchBuffer("[Git Commit].gitcommit", template);
  return new Promise((resolve) => {
    pendingScratch = { resolve, snapshot, reopenDashboard, bufferIndex: opened.bufferIndex };
  });
}

async function finishScratch(submit) {
  const pending = pendingScratch;
  if (!pending) return;
  pendingScratch = null;
  const text = submit ? await redApi.getBufferText() : null;
  redApi.closeScratchBuffer(pending.bufferIndex);
  await redApi.restoreEditorState(pending.snapshot);
  if (pending.reopenDashboard) {
    dashboardOpen = true;
    redApi.openWorkspace(WORKSPACE_ID, { title: "Git", detailRatio: 58, minTwoPaneWidth: 96 });
    await refresh();
  }
  const cleaned = text
    ?.split("\n")
    .filter((line) => !line.startsWith("#"))
    .join("\n")
    .trim();
  pending.resolve(cleaned || null);
}

async function stageOrUnstage(row, stage) {
  if (!row?.data?.path) return;
  const args = stage
    ? ["add", "--", row.data.path]
    : repository.oid
      ? ["restore", "--staged", "--", row.data.path]
      : ["rm", "--cached", "--", row.data.path];
  await runGit(args);
  await refresh();
}

async function discard(row) {
  if (!row?.data?.path) return;
  const untracked = row.data.section === "untracked";
  if (!(await confirmAction("Discard change", `${row.data.path}\nThis cannot be undone by Red.`))) return;
  await runGit(untracked ? ["clean", "-f", "--", row.data.path] : ["restore", "--worktree", "--", row.data.path]);
  await refresh();
}

async function commitMenu() {
  const action = await redApi.pick("Commit", ["Create commit", "Amend commit", "Amend without editing", "Cancel"]);
  if (!action || action === "Cancel") return;
  if (action === "Amend without editing") {
    if (!(await confirmAction("Amend commit", "Rewrite HEAD while keeping its message."))) return;
    await runGit(["commit", "--amend", "--no-edit"]);
  } else {
    let message = await promptScratch(action, "");
    if (!message?.trim()) return;
    if (action === "Amend commit" && !(await confirmAction("Amend commit", "Rewrite HEAD with the new message."))) return;
    for (;;) {
      try {
        await runGit(["commit", ...(action === "Amend commit" ? ["--amend"] : []), "--cleanup=strip", "-F", "-"], { stdin: `${message.trim()}\n` });
        break;
      } catch (error) {
        operationLog.push({ args: ["commit"], code: null, stdout: "", stderr: error.message });
        message = await promptScratch(`${action} failed: ${error.message}`, message);
        if (!message?.trim()) return;
      }
    }
  }
  await refresh();
}

async function safeSync() {
  await runGit(["fetch", "--prune"]);
  await readState();
  const dirty = repository.staged.length + repository.unstaged.length + repository.untracked.length + repository.conflicted.length > 0;
  const decision = safeSyncDecision(repository, dirty);
  if (decision.action === "noop") return renderDashboard();
  if (decision.action === "pull_ff") await runGit(["pull", "--ff-only"]);
  else if (decision.action === "push") await runGit(["push"]);
  else if (decision.action === "choose_upstream") await pushMenu();
  else if (decision.action === "diverged") {
    const choice = await redApi.pick("Branch histories diverged", ["Rebase onto upstream", "Merge upstream", "Cancel"]);
    if (choice === "Rebase onto upstream") await runGit(["rebase", repository.upstream]);
    if (choice === "Merge upstream") await runGit(["merge", "--no-edit", repository.upstream]);
  } else {
    throw new Error(decision.reason);
  }
  await refresh();
}

async function pushMenu() {
  if (repository.upstream) await runGit(["push"]);
  else {
    const remote = await redApi.pick("Set upstream", ["origin", "Cancel"]);
    if (remote === "origin") await runGit(["push", "--set-upstream", remote, repository.head]);
  }
  await refresh();
}

async function operationMenu() {
  if (!repository.operation) return;
  const operation = repository.operation;
  const choices = operation === "rebase" ? ["Continue", "Skip", "Abort", "Cancel"] : ["Continue", "Abort", "Cancel"];
  const choice = await redApi.pick(`${operation}`, choices);
  if (!choice || choice === "Cancel") return;
  if (choice === "Abort" && !(await confirmAction(`Abort ${operation}`, "Discard the operation's in-progress state."))) return;
  const action = choice.toLowerCase();
  if (operation === "merge") await runGit(["merge", `--${action}`]);
  else if (operation === "cherry-pick") await runGit(["cherry-pick", `--${action}`]);
  else if (operation === "revert") await runGit(["revert", `--${action}`]);
  else await runGit(["rebase", `--${action}`], { env: { GIT_EDITOR: "true" } });
  await refresh();
}

async function refs(prefix, format = "%(refname:short)") {
  const result = await runGit(["for-each-ref", `--format=${format}`, prefix]);
  return result.stdout.split("\n").filter(Boolean);
}

async function branchMenu() {
  const action = await redApi.pick("Branch", ["Checkout", "Create", "Rename current", "Delete", "Merge", "Rebase onto", "Cancel"]);
  if (action === "Checkout") {
    const branch = await redApi.pick("Checkout branch", [...await refs("refs/heads"), "Cancel"]);
    if (branch && branch !== "Cancel") await runGit(["switch", branch]);
  } else if (action === "Create") {
    const name = await promptText("New branch name");
    if (name) await runGit(["switch", "-c", name]);
  } else if (action === "Rename current") {
    const name = await promptText("Rename branch", repository.head || "");
    if (name) await runGit(["branch", "-m", name]);
  } else if (action === "Delete") {
    const branch = await redApi.pick("Delete branch", [...await refs("refs/heads"), "Cancel"]);
    if (branch && branch !== "Cancel" && await confirmAction("Delete branch", branch)) await runGit(["branch", "-d", branch]);
  } else if (action === "Merge" || action === "Rebase onto") {
    const branch = await redApi.pick(action, [...(await refs("refs/heads")), ...(await refs("refs/remotes")), "Cancel"]);
    if (branch && branch !== "Cancel") await runGit(action === "Merge" ? ["merge", "--no-edit", branch] : ["rebase", branch]);
  }
  await refresh();
}

async function stashMenu() {
  const action = await redApi.pick("Stash", ["Push", "Push including untracked", "Apply", "Pop", "Drop", "Cancel"]);
  if (action?.startsWith("Push")) {
    const message = await promptText("Stash message");
    const args = ["stash", "push", ...(action.includes("untracked") ? ["--include-untracked"] : []), ...(message ? ["-m", message] : [])];
    await runGit(args);
  } else if (["Apply", "Pop", "Drop"].includes(action)) {
    const list = await runGit(["stash", "list", "--format=%gd%x09%s"]);
    const stashes = list.stdout.split("\n").filter(Boolean);
    const stash = await redApi.pick(action, [...stashes, "Cancel"]);
    if (stash && stash !== "Cancel") {
      const ref = stash.split("\t", 1)[0];
      if (action !== "Drop" || await confirmAction("Drop stash", stash)) await runGit(["stash", action.toLowerCase(), ref]);
    }
  }
  await refresh();
}

async function tagMenu() {
  const action = await redApi.pick("Tag", ["Create lightweight", "Create annotated", "Delete", "Push", "Cancel"]);
  if (action?.startsWith("Create")) {
    const name = await promptText("Tag name");
    if (!name) return;
    if (action === "Create annotated") {
      const message = await promptText("Tag message", name);
      if (message) await runGit(["tag", "-a", name, "-m", message]);
    } else await runGit(["tag", name]);
  } else if (action === "Delete") {
    const tag = await redApi.pick("Delete tag", [...await refs("refs/tags"), "Cancel"]);
    if (tag && tag !== "Cancel" && await confirmAction("Delete tag", tag)) await runGit(["tag", "-d", tag]);
  } else if (action === "Push") {
    const tag = await redApi.pick("Push tag", [...await refs("refs/tags"), "All tags", "Cancel"]);
    if (tag === "All tags") await runGit(["push", "--tags"]);
    else if (tag && tag !== "Cancel") await runGit(["push", "origin", `refs/tags/${tag}`]);
  }
  await refresh();
}

async function remoteMenu() {
  const action = await redApi.pick("Remote", ["List", "Add", "Rename", "Remove", "Cancel"]);
  const list = await runGit(["remote"]);
  const remotes = list.stdout.split("\n").filter(Boolean);
  if (action === "List") await redApi.pick("Remotes", [...remotes, "Close"]);
  else if (action === "Add") {
    const name = await promptText("Remote name", "origin");
    const url = name ? await promptText("Remote URL") : null;
    if (name && url) await runGit(["remote", "add", name, url]);
  } else if (action === "Rename") {
    const oldName = await redApi.pick("Rename remote", [...remotes, "Cancel"]);
    const newName = oldName && oldName !== "Cancel" ? await promptText("New remote name", oldName) : null;
    if (newName) await runGit(["remote", "rename", oldName, newName]);
  } else if (action === "Remove") {
    const name = await redApi.pick("Remove remote", [...remotes, "Cancel"]);
    if (name && name !== "Cancel" && await confirmAction("Remove remote", name)) await runGit(["remote", "remove", name]);
  }
  await refresh();
}

async function resetMenu() {
  const target = await promptText("Reset target", "HEAD~1");
  if (!target) return;
  const mode = await redApi.pick("Reset mode", ["Soft", "Mixed", "Hard", "Cancel"]);
  if (!mode || mode === "Cancel") return;
  if (mode === "Hard" && !(await confirmAction("Hard reset", `${target}\nWorking tree and index changes will be discarded.`))) return;
  await runGit(["reset", `--${mode.toLowerCase()}`, target]);
  await refresh();
}

async function interactiveRebase() {
  const base = await promptText("Interactive rebase base", repository.upstream || "HEAD~5");
  if (!base) return;
  const result = await runGit(["log", "--reverse", "--format=%H%x09%s", `${base}..HEAD`]);
  const commits = result.stdout.split("\n").filter(Boolean).map((line) => {
    const [oid, ...subject] = line.split("\t");
    return { oid, subject: subject.join("\t") };
  });
  if (commits.length === 0) return;
  const todo = [];
  let hasPreviousCommit = false;
  for (const commit of commits) {
    const choices = ["pick", "edit", ...(hasPreviousCommit ? ["squash", "fixup"] : []), "drop", "Cancel rebase"];
    const choice = await redApi.pick(`${commit.oid.slice(0, 8)} ${commit.subject}`, choices);
    if (!choice || choice === "Cancel rebase") return;
    todo.push(`${choice} ${commit.oid} ${commit.subject}`);
    if (choice !== "drop") hasPreviousCommit = true;
  }
  const impact = `${commits.length} commits after ${base}\n${todo.join("\n")}`;
  if (!(await confirmAction("Rewrite history", impact))) return;
  await runGit(["rebase", "-i", base], {
    env: {
      GIT_SEQUENCE_EDITOR: `${redExecutable} --process-editor-replace`,
      GIT_EDITOR: "true",
      RED_PROCESS_EDITOR_CONTENT: `${todo.join("\n")}\n`,
    },
  });
  await refresh();
}

async function worktreeMenu() {
  const action = await redApi.pick("Worktree", ["List", "Add", "Move", "Lock", "Unlock", "Remove", "Prune", "Cancel"]);
  if (action === "List") {
    const result = await runGit(["worktree", "list", "--porcelain"]);
    await redApi.pick("Worktrees", [...result.stdout.split("\n\n").filter(Boolean), "Close"]);
  } else if (action === "Add") {
    const path = await promptText("Worktree path");
    if (!path) return;
    const branch = await promptText("New branch (leave empty for detached)");
    await runGit(["worktree", "add", ...(branch ? ["-b", branch] : ["--detach"]), path]);
  } else if (action === "Remove") {
    const result = await runGit(["worktree", "list", "--porcelain"]);
    const paths = result.stdout.split("\n").filter((line) => line.startsWith("worktree ")).map((line) => line.slice(9));
    const path = await redApi.pick("Remove worktree", [...paths, "Cancel"]);
    if (path && path !== "Cancel" && await confirmAction("Remove worktree", path)) await runGit(["worktree", "remove", path]);
  } else if (["Move", "Lock", "Unlock"].includes(action)) {
    const result = await runGit(["worktree", "list", "--porcelain"]);
    const paths = result.stdout.split("\n").filter((line) => line.startsWith("worktree ")).map((line) => line.slice(9));
    const path = await redApi.pick(action, [...paths, "Cancel"]);
    if (!path || path === "Cancel") return;
    if (action === "Move") {
      const destination = await promptText("Move worktree to", path);
      if (destination) await runGit(["worktree", "move", path, destination]);
    } else await runGit(["worktree", action.toLowerCase(), path]);
  } else if (action === "Prune" && await confirmAction("Prune worktrees", "Remove stale worktree administrative records.")) {
    await runGit(["worktree", "prune"]);
  }
  await refresh();
}

async function logView() {
  const result = await runGit(["log", "--graph", "--decorate", "--date=short", "--pretty=format:%H%x09%h%x09%ad%x09%an%x09%s", "-n", "200"]);
  const items = result.stdout.split("\n").filter(Boolean).map((line, index) => {
    const clean = line.replace(/^[|*\\/ .-]+/, "");
    const [oid, short, date, author, ...subject] = clean.split("\t");
    return { id: oid || String(index), label: `${short || ""} ${subject.join(" ")}`, detail: `${date || ""} ${author || ""}`, data: { oid } };
  });
  const selected = await redApi.pickDynamic("Git log", items, { placeholder: "Filter commits" });
  if (!selected?.data?.oid) return;
  const action = await redApi.pick(selected.label, ["Show details", "Cherry-pick", "Revert", "Create branch", "Create tag", "Cancel"]);
  if (action === "Show details") {
    const show = await runGit(["show", "--stat", "--patch", "--color=never", selected.data.oid]);
    await redApi.pick("Commit details", [...show.stdout.split("\n").slice(0, 500), "Close"]);
  } else if (action === "Cherry-pick") await runGit(["cherry-pick", selected.data.oid]);
  else if (action === "Revert" && await confirmAction("Revert commit", selected.data.oid)) await runGit(["revert", "--no-edit", selected.data.oid]);
  else if (action === "Create branch") {
    const name = await promptText("Branch name");
    if (name) await runGit(["branch", name, selected.data.oid]);
  } else if (action === "Create tag") {
    const name = await promptText("Tag name");
    if (name) await runGit(["tag", name, selected.data.oid]);
  }
  await refresh();
}

async function currentHunkAction(action) {
  const info = await redApi.getEditorInfo();
  const buffer = info.buffers.find((item) => item.id === info.current_buffer_index) || info.buffers[info.current_buffer_index];
  if (!buffer?.path || !repository) return;
  const path = repositoryPath(buffer.path, repository.root);
  if (!path) return;
  const cursor = await redApi.getCursorPosition();
  const selection = await redApi.getSelection();
  const cached = action === "unstage";
  let diff = await runGit(["diff", ...(cached ? ["--cached"] : []), "--no-ext-diff", "--unified=3", "--", path], { allowFailure: true });
  if (action === "stage" && !repository.staged.some((entry) => entry.path === path)) {
    const currentText = await redApi.getBufferText();
    const unsaved = await runGit(["diff", "--no-index", "--no-ext-diff", "--unified=3", "--", buffer.path, "-"], {
      stdin: currentText,
      allowFailure: true,
    });
    if (unsaved.stdout) {
      const lines = unsaved.stdout.split("\n");
      lines[0] = `diff --git ${gitPatchPath(path, "a")} ${gitPatchPath(path, "b")}`;
      const oldHeader = lines.findIndex((line) => line.startsWith("--- "));
      const newHeader = lines.findIndex((line) => line.startsWith("+++ "));
      if (oldHeader >= 0) lines[oldHeader] = `--- ${gitPatchPath(path, "a")}`;
      if (newHeader >= 0) lines[newHeader] = `+++ ${gitPatchPath(path, "b")}`;
      diff = { ...unsaved, stdout: lines.join("\n") };
    }
  }
  const patch = selection?.bufferIndex === info.current_buffer_index
    ? patchForLineRange(diff.stdout, selection.start.y, selection.end.y)
    : patchForHunk(diff.stdout, cursor.y);
  if (!patch) return;
  if (action === "reset" && !(await confirmAction("Reset hunk", path))) return;
  await runGit(["apply", ...(action === "stage" || action === "unstage" ? ["--cached"] : []), ...(action === "unstage" || action === "reset" ? ["--reverse"] : []), "--unidiff-zero", "-"], { stdin: `${patch}\n` });
  await refresh();
}

async function navigateHunk(direction) {
  const info = await redApi.getEditorInfo();
  const buffer = info.buffers.find((item) => item.id === info.current_buffer_index) || info.buffers[info.current_buffer_index];
  if (!buffer?.path || !repository) return;
  const path = repositoryPath(buffer.path, repository.root);
  if (!path) return;
  const diff = await runGit(["diff", "--no-ext-diff", "--unified=0", "--", path], { allowFailure: true });
  const hunks = parseUnifiedHunks(diff.stdout);
  const cursor = await redApi.getCursorPosition();
  const lines = hunks.map((hunk) => Math.max(0, hunk.newStart - 1));
  const target = direction > 0 ? lines.find((line) => line > cursor.y) ?? lines[0] : [...lines].reverse().find((line) => line < cursor.y) ?? lines.at(-1);
  if (target != null) redApi.setCursorPosition(0, target);
}

async function handleWorkspaceEvent(event) {
  const row = event.row || null;
  if (["up", "down", "page_up", "page_down"].includes(event.action)) return renderDashboard(row);
  try {
    if (event.action === "q" || event.action === "escape") return closeDashboard();
    if (event.action === "r") return refresh(row);
    if (event.action === "s") return stageOrUnstage(row, true);
    if (event.action === "u") return stageOrUnstage(row, false);
    if (event.action === "x") return discard(row);
    if (event.action === "c") return commitMenu();
    if (event.action === "y") return safeSync();
    if (event.action === "p") return pushMenu();
    if (event.action === "P") { await runGit(["pull", "--ff-only"]); return refresh(); }
    if (event.action === "f") { await runGit(["fetch", "--prune"]); return refresh(); }
    if (event.action === "o") return operationMenu();
    if (event.action === "b") return branchMenu();
    if (event.action === "t") return tagMenu();
    if (event.action === "z") return stashMenu();
    if (event.action === "w") return worktreeMenu();
    if (event.action === "l") return logView();
    if (event.action === "i") return interactiveRebase();
    if (event.action === "R") return resetMenu();
    if (event.action === "e") return remoteMenu();
    if (event.action === "$") return redApi.pick("Git command log", operationLog.flatMap((entry) => [`git ${entry.args.join(" ")} [${entry.code}]`, ...entry.stderr.split("\n").filter(Boolean)]));
    if (event.action === "?" ) return redApi.pick("Git keys", ["s stage", "u unstage", "x discard", "c commit", "f fetch", "p push", "P pull --ff-only", "y safe sync", "o operation", "b branch", "t tag", "e remote", "z stash", "w worktree", "l log", "i interactive rebase", "R reset", "q close"]);
    if (event.action === "activate" && row?.data?.path) {
      redApi.openLocation({ path: `${repository.root}/${row.data.path}`, line: 0, column: 0 });
      closeDashboard();
    }
  } catch (error) {
    operationLog.push({ args: [], code: null, stdout: "", stderr: error.message });
    redApi.logError?.(`Git action failed: ${error.message}`);
    await renderDashboard(row);
  }
}

async function openDashboard() {
  dashboardOpen = true;
  redApi.openWorkspace(WORKSPACE_ID, { title: "Git", detailRatio: 58, minTwoPaneWidth: 96 });
  await refresh();
}

function closeDashboard() {
  dashboardOpen = false;
  redApi.closeWorkspace(WORKSPACE_ID);
}

async function resolveStyles(red) {
  const info = await red.getEditorInfo();
  const base = style(info.theme?.style);
  const resolve = async (foreground, overrides = {}) => style(await red.resolveThemeStyle({ foreground }), overrides);
  const sign = {
    add: await resolve(["gitDecoration.addedResourceForeground", "scope:markup.inserted"]),
    change: await resolve(["gitDecoration.modifiedResourceForeground", "scope:markup.changed"]),
    delete: await resolve(["gitDecoration.deletedResourceForeground", "scope:markup.deleted"]),
  };
  const normalSigns = {
    add: sign.add,
    change: sign.change,
    delete: sign.delete,
    topdelete: sign.delete,
    changedelete: sign.change,
  };
  return {
    normal: base,
    muted: await resolve(["descriptionForeground", "editorLineNumber.foreground"]),
    heading: await resolve(["sideBarSectionHeader.foreground", "editor.foreground"], { bold: true }),
    branch: await resolve(["gitDecoration.modifiedResourceForeground", "symbolIcon.variableForeground"], { bold: true }),
    warning: await resolve(["editorWarning.foreground", "gitDecoration.modifiedResourceForeground"]),
    success: await resolve(["gitDecoration.addedResourceForeground"]),
    diffAdded: await resolve(["gitDecoration.addedResourceForeground"]),
    diffDeleted: await resolve(["gitDecoration.deletedResourceForeground"]),
    diffHunk: await resolve(["gitDecoration.modifiedResourceForeground"]),
    status: {
      staged: await resolve(["gitDecoration.addedResourceForeground"]),
      unstaged: await resolve(["gitDecoration.modifiedResourceForeground"]),
      untracked: await resolve(["gitDecoration.untrackedResourceForeground", "gitDecoration.addedResourceForeground"]),
      conflicted: await resolve(["gitDecoration.conflictingResourceForeground", "editorError.foreground"]),
    },
    sign: {
      normal: normalSigns,
      staged: Object.fromEntries(Object.entries(normalSigns).map(([kind, normal]) => [kind, stagedStyle(normal, base.bg)])),
    },
  };
}

export async function activate(red) {
  redApi = red;
  const executable = await red.getConfig("executable");
  const pluginConfig = (await red.getConfig("plugin_config"))?.git || {};
  if (/^[A-Za-z0-9_./\\:-]+$/u.test(executable || "")) redExecutable = executable;
  signGlyphs = configuredSignGlyphs(pluginConfig.signs, DEFAULT_SIGNS);
  stagedSignGlyphs = configuredSignGlyphs(pluginConfig.signs_staged, DEFAULT_STAGED_SIGNS);
  styles = await resolveStyles(red);
  red.addCommand("GitDashboard", async () => dashboardOpen ? closeDashboard() : openDashboard());
  red.addCommand("GitRefresh", refresh);
  red.addCommand("GitHunkNext", () => navigateHunk(1));
  red.addCommand("GitHunkPrevious", () => navigateHunk(-1));
  red.addCommand("GitHunkStage", () => currentHunkAction("stage"));
  red.addCommand("GitHunkUnstage", () => currentHunkAction("unstage"));
  red.addCommand("GitHunkReset", () => currentHunkAction("reset"));
  red.addCommand("GitSubmitMessage", () => finishScratch(true));
  red.addCommand("GitCancelMessage", () => finishScratch(false));
  red.onWorkspaceEvent(WORKSPACE_ID, handleWorkspaceEvent);
  for (const event of ["buffer:changed", "file:opened", "file:saved", "window:buffer_changed", "window:focus_changed"]) red.on(event, scheduleRefresh);
  await refresh();
  if (repository?.gitDir) watchId = red.watchDirectory(repository.gitDir, scheduleRefresh, { recursive: false, intervalMs: 1000 });
  pollTimer = await red.setInterval(refresh, POLL_INTERVAL_MS);
}

export async function deactivate() {
  if (!redApi) return;
  if (watchId != null) redApi.unwatchDirectory(watchId);
  if (pollTimer != null) await redApi.clearInterval(pollTimer);
  redApi.clearGutterSigns(GUTTER_NAMESPACE);
  redApi.closeWorkspace(WORKSPACE_ID);
  redApi = null;
}
