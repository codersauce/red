const PANEL_ID = "neotree";
const ROOT = ".";

const STATUS_PRECEDENCE = [
  "conflict",
  "renamed",
  "deleted",
  "added",
  "modified",
  "untracked",
  "ignored",
  "staged",
];

const STATUS_SYMBOLS = {
  added: "✚",
  deleted: "✖",
  modified: "",
  renamed: "",
  untracked: "",
  ignored: "",
  staged: "",
  conflict: "",
};

const FILE_ICONS = {
  js: "",
  mjs: "",
  cjs: "",
  ts: "",
  tsx: "",
  jsx: "",
  json: "",
  toml: "",
  rs: "",
  lua: "",
  md: "",
  markdown: "",
  lock: "",
  sh: "",
  zsh: "",
  fish: "",
  txt: "󰈙",
};

let redApi = null;
const watches = new Map();
const DEFAULT_STYLE = {
  fg: null,
  bg: null,
  bold: false,
  italic: false,
};

function rgb(r, g, b) {
  return { Rgb: { r, g, b } };
}

function style(base, overrides = {}) {
  return { ...DEFAULT_STYLE, ...(base || {}), ...overrides };
}

function colorStyle(colors, keys, base, overrides = {}) {
  for (const key of keys) {
    const fg = colors?.[key];
    if (fg) return style(base, { ...overrides, fg });
  }
  return null;
}

export function stylesFor(info) {
  const theme = info?.theme ?? {};
  const fallback = style(theme.style);
  const ui = theme.ui_style ?? theme.uiStyle ?? {};
  const colors = theme.colors ?? {};
  const normal = colorStyle(colors, ["sideBar.foreground"], fallback) ?? fallback;
  const guide =
    colorStyle(colors, ["tree.indentGuidesStroke"], fallback) ??
    ui.muted ??
    style(fallback, { fg: rgb(108, 112, 134) });
  const directory =
    colorStyle(
      colors,
      [
        "symbolIcon.folderForeground",
        "sideBarTitle.foreground",
        "list.highlightForeground",
      ],
      fallback,
    ) ?? style(fallback, { fg: rgb(137, 180, 250) });
  const root =
    colorStyle(
      colors,
      [
        "sideBarTitle.foreground",
        "symbolIcon.folderForeground",
        "list.highlightForeground",
      ],
      fallback,
      { bold: true },
    ) ?? style(fallback, { fg: rgb(198, 160, 246), bold: true });
  const ignored =
    colorStyle(colors, ["gitDecoration.ignoredResourceForeground"], fallback) ??
    ui.muted ??
    style(fallback, { fg: rgb(108, 112, 134) });

  return {
    root,
    normal,
    guide,
    directory,
    ignored,
    status: {
      added:
        colorStyle(
          colors,
          [
            "gitDecoration.addedResourceForeground",
            "gitDecoration.untrackedResourceForeground",
          ],
          fallback,
        ) ??
        style(fallback, { fg: rgb(166, 227, 161) }),
      deleted:
        colorStyle(
          colors,
          [
            "gitDecoration.deletedResourceForeground",
            "gitDecoration.stageDeletedResourceForeground",
          ],
          fallback,
        ) ??
        style(fallback, { fg: rgb(243, 139, 168) }),
      modified:
        colorStyle(
          colors,
          [
            "gitDecoration.modifiedResourceForeground",
            "gitDecoration.stageModifiedResourceForeground",
          ],
          fallback,
        ) ??
        style(fallback, { fg: rgb(249, 226, 175) }),
      renamed:
        colorStyle(
          colors,
          [
            "gitDecoration.renamedResourceForeground",
            "gitDecoration.modifiedResourceForeground",
          ],
          fallback,
        ) ??
        style(fallback, { fg: rgb(137, 220, 235) }),
      untracked:
        colorStyle(
          colors,
          [
            "gitDecoration.untrackedResourceForeground",
            "gitDecoration.addedResourceForeground",
          ],
          fallback,
        ) ??
        ui.muted ??
        style(fallback, { fg: rgb(108, 112, 134) }),
      ignored,
      staged:
        colorStyle(
          colors,
          [
            "gitDecoration.stageModifiedResourceForeground",
            "gitDecoration.addedResourceForeground",
          ],
          fallback,
        ) ??
        style(fallback, { fg: rgb(166, 227, 161) }),
      conflict:
        colorStyle(colors, ["gitDecoration.conflictingResourceForeground"], fallback, { bold: true }) ??
        style(fallback, { fg: rgb(243, 139, 168), bold: true }),
    },
  };
}

function normalizePath(path) {
  return String(path || ".").replace(/\\/g, "/").replace(/\/+/g, "/").replace(/\/$/, "");
}

function displayPath(path, cwd) {
  const normalized = normalizePath(path);
  const home = globalThis.Deno?.env?.get?.("HOME");
  if (home && normalized.startsWith(normalizePath(home) + "/")) {
    return `~/${normalized.slice(normalizePath(home).length + 1)}`;
  }
  if (cwd && normalized === normalizePath(cwd)) {
    return normalized.split("/").filter(Boolean).pop() || normalized;
  }
  return normalized;
}

function fileIcon(name) {
  const lower = name.toLowerCase();
  const extension = lower.includes(".") ? lower.split(".").pop() : lower;
  return FILE_ICONS[extension] ?? "󰈙";
}

function statusRank(status) {
  const index = STATUS_PRECEDENCE.indexOf(status);
  return index === -1 ? STATUS_PRECEDENCE.length : index;
}

function preferredStatus(statuses) {
  let best = null;
  for (const status of statuses) {
    if (!best || statusRank(status) < statusRank(best)) {
      best = status;
    }
  }
  return best;
}

function makeStatusIndex(result) {
  const entries = Array.isArray(result?.statuses) ? result.statuses : [];
  const root = normalizePath(result?.root || "");
  return {
    root,
    entries: entries.map((entry) => ({
      path: normalizePath(entry.path),
      absolutePath: normalizePath(entry.absolute_path ?? entry.absolutePath ?? entry.path),
      status: entry.status,
    })),
  };
}

function statusForPath(statusIndex, path, kind) {
  const normalized = normalizePath(path);
  const relative = normalized.replace(/^\.\//, "");
  const absolute = normalized.startsWith("/")
    ? normalized
    : relative === "."
      ? statusIndex.root
      : normalizePath(`${statusIndex.root}/${relative}`);

  const matches = [];
  for (const entry of statusIndex.entries) {
    if (entry.absolutePath === absolute) {
      matches.push(entry.status);
      continue;
    }

    if (kind === "directory" && entry.absolutePath.startsWith(`${absolute}/`)) {
      matches.push(entry.status);
    }
  }

  return preferredStatus(matches);
}

function statusSegments(status, styles) {
  const symbol = STATUS_SYMBOLS[status];
  if (!symbol) return [];
  return [{ text: symbol, style: styles.status[status] ?? styles.normal }];
}

function loadingRows(styles) {
  return [
    {
      id: "loading",
      path: ROOT,
      kind: "directory",
      expanded: false,
      segments: [
        { text: " ", style: styles.normal },
        { text: "Loading...", style: styles.ignored ?? styles.normal },
      ],
    },
  ];
}

function branchPrefix(ancestors, isLast, styles) {
  const segments = [];
  for (const ancestor of ancestors) {
    segments.push({
      text: ancestor.visible && !ancestor.last ? "│ " : "  ",
      style: styles.guide,
    });
  }
  if (ancestors.length > 0) {
    segments.push({
      text: isLast ? "└ " : "├ ",
      style: styles.guide,
    });
  } else {
    segments.push({ text: "  ", style: styles.guide });
  }
  return segments;
}

export function buildNeoTreeRows({
  root,
  cwd,
  children,
  expanded,
  statusIndex,
  styles,
}) {
  const rows = [];
  const rootStatus = statusForPath(statusIndex, root, "directory");
  rows.push({
    id: root,
    path: root,
    kind: "directory",
    expanded: expanded.has(root),
    segments: [
      { text: " ", style: styles.root },
      { text: " ", style: styles.directory },
      { text: displayPath(cwd || root, cwd), style: styles.root },
    ],
    right_segments: statusSegments(rootStatus, styles),
  });

  if (!expanded.has(root)) return rows;

  appendRows(root, [], rows, { children, expanded, statusIndex, styles });
  return rows;
}

function appendRows(path, ancestors, rows, context) {
  const entries = (context.children.get(path) || []).filter((entry) =>
    entry.kind === "directory" || entry.kind === "file"
  );

  entries.forEach((entry, index) => {
    const isLast = index === entries.length - 1;
    const isDirectory = entry.kind === "directory";
    const isExpanded = isDirectory && context.expanded.has(entry.path);
    const entryStatus = statusForPath(context.statusIndex, entry.path, entry.kind);
    const rowStyle = entryStatus === "ignored" ? context.styles.ignored : context.styles.normal;
    const icon = isDirectory ? (isExpanded ? "" : "") : fileIcon(entry.name);

    rows.push({
      id: entry.path,
      path: entry.path,
      kind: entry.kind,
      expanded: isDirectory ? isExpanded : false,
      segments: [
        ...branchPrefix(ancestors, isLast, context.styles),
        {
          text: `${icon} `,
          style: isDirectory ? context.styles.directory : rowStyle,
        },
        {
          text: entry.name,
          style: isDirectory ? context.styles.directory : rowStyle,
        },
      ],
      right_segments: statusSegments(entryStatus, context.styles),
    });

    if (isExpanded) {
      appendRows(
        entry.path,
        [...ancestors, { last: isLast, visible: ancestors.length > 0 }],
        rows,
        context,
      );
    }
  });
}

export async function activate(red) {
  redApi = red;

  const expanded = new Set([ROOT]);
  const children = new Map();
  let statusIndex = makeStatusIndex(null);
  let created = false;
  let cwd = ROOT;
  let currentStyles = stylesFor(null);

  async function updateEditorContext() {
    const [info, configCwd] = await Promise.all([
      red.getEditorInfo(),
      red.getConfig("cwd"),
    ]);
    cwd = configCwd || cwd;
    currentStyles = stylesFor(info);
  }

  async function loadDirectory(path) {
    const result = await red.listDirectory(path);
    if (result.error) {
      red.logWarn("NeoTree failed to list directory", path, result.error);
      children.set(path, []);
      return [];
    }
    children.set(path, result.entries);
    return result.entries;
  }

  async function refreshGitStatus() {
    const result = await red.getGitStatus(ROOT);
    if (result?.error) {
      red.logWarn("NeoTree failed to read git status", result.error);
    }
    statusIndex = makeStatusIndex(result);
  }

  function watchDirectory(path) {
    if (watches.has(path)) return;

    const watchId = red.watchDirectory(path, async () => {
      await loadDirectory(path);
      await refreshGitStatus();
      await refresh();
    });
    watches.set(path, watchId);
  }

  async function ensureLoaded(path) {
    if (!children.has(path)) {
      await loadDirectory(path);
    }
    watchDirectory(path);
    return children.get(path) || [];
  }

  async function ensureExpandedLoaded(path) {
    await ensureLoaded(path);
    const entries = children.get(path) || [];
    for (const entry of entries) {
      if (entry.kind === "directory" && expanded.has(entry.path)) {
        await ensureExpandedLoaded(entry.path);
      }
    }
  }

  async function refresh() {
    await updateEditorContext();
    const rows = buildNeoTreeRows({
      root: ROOT,
      cwd,
      children,
      expanded,
      statusIndex,
      styles: currentStyles,
    });
    red.updatePanel(PANEL_ID, rows);
  }

  async function reloadVisibleTree() {
    await ensureExpandedLoaded(ROOT);
    await refreshGitStatus();
    await refresh();
  }

  function stopWatchingDirectories() {
    for (const watchId of watches.values()) {
      red.unwatchDirectory(watchId);
    }
    watches.clear();
  }

  function close() {
    if (!created) return;
    stopWatchingDirectories();
    red.closePanel(PANEL_ID);
    red.focusEditor();
    created = false;
  }

  async function show() {
    if (!created) {
      red.createPanel(PANEL_ID, {
        side: "left",
        width: 30,
      });
      red.updatePanel(PANEL_ID, loadingRows(currentStyles));
      created = true;
    }

    await reloadVisibleTree();
    red.focusPanel(PANEL_ID);
  }

  async function toggleDirectory(path, forceExpand = null) {
    const shouldExpand = forceExpand ?? !expanded.has(path);
    if (shouldExpand) {
      expanded.add(path);
      await ensureLoaded(path);
    } else {
      expanded.delete(path);
    }
    await refreshGitStatus();
    await refresh();
  }

  red.addCommand("NeoTree", async () => {
    if (created) {
      close();
    } else {
      await show();
    }
  });

  red.onPanelEvent(PANEL_ID, async (event) => {
    const row = event.row;

    if (event.action === "close") {
      close();
      return;
    }

    if (event.action === "refresh") {
      await reloadVisibleTree();
      return;
    }

    if (!row) return;

    if (event.action === "activate" || event.action === "toggle") {
      if (row.kind === "directory") {
        await toggleDirectory(row.path);
      } else if (row.path) {
        red.openFile(row.path);
        close();
      }
      return;
    }

    if (row.kind === "directory" && event.action === "expand") {
      await toggleDirectory(row.path, true);
    }

    if (row.kind === "directory" && event.action === "collapse") {
      await toggleDirectory(row.path, false);
    }
  });
}

export async function deactivate() {
  if (!redApi) return;

  for (const watchId of watches.values()) redApi.unwatchDirectory(watchId);
  watches.clear();
  redApi.closePanel(PANEL_ID);
  redApi = null;
}
