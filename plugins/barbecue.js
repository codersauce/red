const BAR_ID = "barbecue";
const DEFAULT_DEBOUNCE_MS = 120;

const DEFAULT_ICONS = {
  Array: "",
  Boolean: "󰨙",
  Class: "",
  Constant: "󰏿",
  Constructor: "",
  Enum: "",
  EnumMember: "",
  Event: "",
  Field: "",
  File: "",
  Folder: "",
  Function: "󰊕",
  Interface: "",
  Key: "",
  Method: "󰊕",
  Module: "",
  Namespace: "󰦮",
  Number: "󰎠",
  Object: "",
  Operator: "",
  Package: "",
  Property: "",
  String: "",
  Struct: "󰆼",
  TypeParameter: "",
  Variable: "󰀫",
  Unknown: "",
};

const FILE_ICONS = {
  cjs: "",
  fish: "",
  js: "",
  json: "",
  jsx: "",
  lock: "",
  lua: "",
  markdown: "",
  md: "",
  mjs: "",
  rs: "",
  sh: "",
  toml: "",
  ts: "",
  tsx: "",
  zsh: "",
};

const KIND_THEME_KEYS = {
  Array: "symbolIcon.arrayForeground",
  Boolean: "symbolIcon.booleanForeground",
  Class: "symbolIcon.classForeground",
  Constant: "symbolIcon.constantForeground",
  Constructor: "symbolIcon.constructorForeground",
  Enum: "symbolIcon.enumeratorForeground",
  EnumMember: "symbolIcon.enumeratorMemberForeground",
  Event: "symbolIcon.eventForeground",
  Field: "symbolIcon.fieldForeground",
  File: "symbolIcon.fileForeground",
  Function: "symbolIcon.functionForeground",
  Interface: "symbolIcon.interfaceForeground",
  Key: "symbolIcon.keyForeground",
  Method: "symbolIcon.methodForeground",
  Module: "symbolIcon.moduleForeground",
  Namespace: "symbolIcon.namespaceForeground",
  Number: "symbolIcon.numberForeground",
  Object: "symbolIcon.objectForeground",
  Operator: "symbolIcon.operatorForeground",
  Package: "symbolIcon.packageForeground",
  Property: "symbolIcon.propertyForeground",
  String: "symbolIcon.stringForeground",
  Struct: "symbolIcon.structForeground",
  TypeParameter: "symbolIcon.typeParameterForeground",
  Variable: "symbolIcon.variableForeground",
};

const BAR_BACKGROUND = ["breadcrumb.background", "editor.background"];

function semantic(foreground, bold = false) {
  return { semantic: { foreground, background: BAR_BACKGROUND, ...(bold ? { bold } : {}) } };
}

export function stylesFor(_info) {
  const normal = semantic(["breadcrumb.foreground", "editor.foreground"]);
  const separator = semantic([
    "breadcrumb.foreground",
    "descriptionForeground",
    "editor.foreground",
  ]);
  const directory = semantic([
    "symbolIcon.folderForeground",
    "breadcrumb.foreground",
    "editor.foreground",
  ]);
  const file = semantic([
    "symbolIcon.fileForeground",
    "breadcrumb.foreground",
    "editor.foreground",
  ]);
  const current = semantic([
    "breadcrumb.focusForeground",
    "breadcrumb.activeSelectionForeground",
    "list.activeSelectionForeground",
    "editor.foreground",
  ], true);

  return {
    current,
    directory,
    file,
    normal,
    separator,
    symbol(kind, isCurrent = false) {
      if (isCurrent) return current;
      return semantic(
        [KIND_THEME_KEYS[kind], "breadcrumb.foreground", "editor.foreground"].filter(Boolean),
      );
    },
  };
}

function normalizePath(path) {
  return String(path ?? "").replace(/\\/g, "/").replace(/\/+$/, "");
}

function basename(path) {
  const parts = normalizePath(path).split("/");
  return parts.at(-1) || "[No Name]";
}

function relativeParts(path, cwd) {
  const normalizedPath = normalizePath(path);
  const normalizedCwd = normalizePath(cwd);
  let relative = normalizedPath;
  if (normalizedCwd && normalizedPath.startsWith(`${normalizedCwd}/`)) {
    relative = normalizedPath.slice(normalizedCwd.length + 1);
  }
  const parts = relative.split("/").filter(Boolean);
  return { directories: parts.slice(0, -1), file: parts.at(-1) || "[No Name]" };
}

function extension(path) {
  const name = basename(path);
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
}

function positionFromWindow(window) {
  if (window.lspPosition) return window.lspPosition;
  const cursor = window.cursor ?? {};
  return {
    line: cursor.line ?? cursor.y ?? window.cursorLine ?? window.cursor_line ?? 0,
    character:
      cursor.character ??
      cursor.lspCharacter ??
      cursor.lsp_character ??
      cursor.utf16Character ??
      cursor.utf16_character ??
      cursor.x ??
      window.cursorCharacter ??
      window.cursor_character ??
      0,
  };
}

function comparePosition(left, right) {
  return left.line - right.line || left.character - right.character;
}

export function rangeContains(range, position) {
  if (!range?.start || !range?.end) return false;
  return comparePosition(range.start, position) <= 0 && comparePosition(position, range.end) < 0;
}

function flattenSymbols(symbols, parentId = null, depth = 0, output = []) {
  for (const [index, symbol] of (symbols ?? []).entries()) {
    const id = symbol.id ?? `${parentId ?? "root"}:${index}:${symbol.name ?? "symbol"}`;
    const flat = { ...symbol, id, parentId: symbol.parentId ?? parentId, depth: symbol.depth ?? depth };
    delete flat.children;
    output.push(flat);
    flattenSymbols(symbol.children, id, flat.depth + 1, output);
  }
  return output;
}

export function enclosingSymbols(symbols, position) {
  const flat = flattenSymbols(symbols);
  if (flat.length === 0) return [];
  const byId = new Map(flat.map((symbol) => [symbol.id, symbol]));
  const containing = flat.filter((symbol) => rangeContains(symbol.range, position));
  if (containing.length === 0) return [];
  const leaf = containing.reduce((best, symbol) =>
    (symbol.depth ?? 0) >= (best.depth ?? 0) ? symbol : best,
  );

  if (leaf.parentId != null && byId.has(leaf.parentId)) {
    const chain = [];
    const visited = new Set();
    let current = leaf;
    while (current && !visited.has(current.id)) {
      visited.add(current.id);
      chain.unshift(current);
      current = byId.get(current.parentId);
    }
    return chain.filter((symbol) => rangeContains(symbol.range, position));
  }

  const leafIndex = flat.indexOf(leaf);
  const chain = [leaf];
  let wantedDepth = (leaf.depth ?? 0) - 1;
  for (let index = leafIndex - 1; index >= 0 && wantedDepth >= 0; index -= 1) {
    const candidate = flat[index];
    if ((candidate.depth ?? 0) === wantedDepth && rangeContains(candidate.range, position)) {
      chain.unshift(candidate);
      wantedDepth -= 1;
    }
  }
  return chain;
}

function configuredIcon(kind, options) {
  if (options.nerdFont === false) return "";
  const overrides = options.icons?.overrides ?? {};
  const override = overrides[kind.toLowerCase()];
  return override == null ? DEFAULT_ICONS[kind] ?? DEFAULT_ICONS.Unknown : String(override);
}

function segment(id, text, segmentStyle, action = null) {
  return { id, text, style: segmentStyle, ...(action ? { action } : {}) };
}

export function buildSegments(window, symbols, options = {}, info = null, cwd = "") {
  const windowPath =
    window.bufferPath ?? window.buffer_path ?? window.path ?? window.file ?? window.filePath ??
    window.file_path ?? "";
  const path = relativeParts(windowPath, cwd);
  const styles = stylesFor(info);
  const separatorText = options.separator ?? "";
  const segments = [];
  const addSeparator = () => {
    if (segments.length > 0) {
      segments.push(segment(`separator:${segments.length}`, ` ${separatorText} `, styles.separator));
    }
  };

  if (options.showDirectory !== false) {
    path.directories.forEach((directory, index) => {
      addSeparator();
      const icon = options.nerdFont === false ? "" : DEFAULT_ICONS.Folder;
      const label = index === path.directories.length - 1 && icon ? `${icon} ${directory}` : directory;
      segments.push(segment(`directory:${index}:${directory}`, label, styles.directory));
    });
  }

  if (options.showFile !== false) {
    addSeparator();
    const icon = options.nerdFont === false
      ? ""
      : FILE_ICONS[extension(path.file)] ?? DEFAULT_ICONS.File;
    segments.push(segment("file", icon ? `${icon} ${path.file}` : path.file, styles.file));
  }

  if (options.showSymbols !== false) {
    const chain = enclosingSymbols(symbols, positionFromWindow(window));
    chain.forEach((symbol, index) => {
      addSeparator();
      const kind = symbol.kindName ?? symbol.kind_name ?? "Unknown";
      const icon = configuredIcon(kind, options);
      const current = index === chain.length - 1;
      segments.push(
        segment(
          `symbol:${symbol.id}`,
          icon ? `${icon} ${symbol.name}` : symbol.name,
          styles.symbol(kind, current),
          `jump:${bufferIndex(window)}:${symbol.id}`,
        ),
      );
    });
  }

  return segments;
}

function windowId(window) {
  return window.windowId ?? window.window_id ?? window.id;
}

function bufferIndex(window) {
  return window.bufferIndex ?? window.buffer_index ?? window.bufferId ?? window.buffer_id ?? 0;
}

function bufferRevision(window) {
  return window.revision ?? window.bufferRevision ?? window.buffer_revision ?? 0;
}

function barApi(red) {
  const ui = red.ui ?? red;
  return {
    create: (id, config) => ui.createWindowBar(id, config),
    update: (id, idOrWindow, segments) => ui.updateWindowBar(id, idOrWindow, segments),
    close: (id) => ui.closeWindowBar(id),
  };
}

function optionsFromConfig(config) {
  const options = config?.barbecue ?? {};
  return {
    debounceMs: options.debounce_ms ?? options.debounceMs ?? DEFAULT_DEBOUNCE_MS,
    enabled: options.enabled !== false,
    icons: options.icons ?? {},
    nerdFont: options.nerd_font ?? options.nerdFont ?? true,
    separator: options.separator ?? "",
    showDirectory: options.show_directory ?? options.showDirectory ?? true,
    showFile: options.show_file ?? options.showFile ?? true,
    showSymbols: options.show_symbols ?? options.showSymbols ?? true,
    truncateMarker: options.truncate_marker ?? options.truncateMarker ?? "…",
  };
}

export function createController(red) {
  const api = barApi(red);
  const symbolsByBuffer = new Map();
  const pendingByBuffer = new Map();
  const symbolsById = new Map();
  let generation = 0;
  let timer = null;
  let options = optionsFromConfig(null);
  let stopped = false;
  let barOpen = false;
  let barMarker = null;
  let cwd = "";
  let contextPromise = null;

  function configureBar() {
    api.create(BAR_ID, {
      edge: "top",
      overflow: "truncate_left",
      priority: 100,
      style: semantic(["breadcrumb.foreground", "editor.foreground"]),
      truncateMarker: options.truncateMarker,
    });
    barOpen = true;
    barMarker = options.truncateMarker;
  }

  async function loadContext() {
    if (!contextPromise) {
      contextPromise = red.getConfig().then((config) => {
        options = optionsFromConfig(config?.plugin_config);
        cwd = config?.cwd ?? "";
      });
    }
    return contextPromise;
  }

  function symbolCacheKey(window) {
    return `${bufferIndex(window)}:${bufferRevision(window)}`;
  }

  async function symbolsFor(window) {
    if (!options.showSymbols) return [];
    const index = bufferIndex(window);
    const revision = bufferRevision(window);
    const key = symbolCacheKey(window);
    if (symbolsByBuffer.has(key)) return symbolsByBuffer.get(key);
    let pending = pendingByBuffer.get(key);
    if (!pending) {
      pending = red.lsp.documentSymbols({ bufferIndex: index });
      pendingByBuffer.set(key, pending);
    }
    const result = await pending;
    pendingByBuffer.delete(key);
    if (stopped || !result?.ok) return [];
    if (result.revision != null && result.revision !== revision) return [];
    const symbols = flattenSymbols(result.symbols ?? []);
    for (const cachedKey of symbolsByBuffer.keys()) {
      if (cachedKey.startsWith(`${index}:`) && cachedKey !== key) symbolsByBuffer.delete(cachedKey);
    }
    symbolsByBuffer.set(key, symbols);
    for (const symbol of symbols) symbolsById.set(`${index}:${symbol.id}`, symbol);
    return symbols;
  }

  async function refresh() {
    const requestGeneration = ++generation;
    const [, windows] = await Promise.all([
      loadContext(),
      red.getWindows(),
    ]);
    if (stopped || requestGeneration !== generation) return;
    if (!options.enabled) {
      if (barOpen) api.close(BAR_ID);
      barOpen = false;
      return;
    }
    if (!barOpen || barMarker !== options.truncateMarker) configureBar();

    for (const window of windows ?? []) {
      const key = symbolCacheKey(window);
      const cachedSymbols = symbolsByBuffer.get(key) ?? [];
      api.update(BAR_ID, windowId(window), buildSegments(window, cachedSymbols, options, null, cwd));

      if (!options.showSymbols || symbolsByBuffer.has(key)) continue;
      void symbolsFor(window).then((symbols) => {
        if (stopped || requestGeneration !== generation) return;
        api.update(BAR_ID, windowId(window), buildSegments(window, symbols, options, null, cwd));
      }).catch((error) => {
        red.logWarn?.("Barbecue document symbols failed", error?.message ?? error);
      });
    }
  }

  async function scheduleRefresh() {
    if (timer != null) await red.clearTimeout(timer);
    timer = await red.setTimeout(async () => {
      timer = null;
      await refresh();
    }, Number(options.debounceMs));
  }

  function refreshFromCache() {
    return refresh();
  }

  async function handleAction(event) {
    const action = event?.action ?? event?.actionId ?? event?.action_id;
    if (!String(action).startsWith("jump:")) return;
    const symbol = symbolsById.get(String(action).slice(5));
    if (!symbol?.selectionRange) return;
    await red.openLocation({
      path: symbol.file,
      line: symbol.selectionRange.start.line,
      column: symbol.selectionRange.start.character,
      columnEncoding: "utf-16",
    });
  }

  async function stop() {
    stopped = true;
    generation += 1;
    if (timer != null) await red.clearTimeout(timer);
    api.close(BAR_ID);
  }

  configureBar();

  return { handleAction, refresh, refreshFromCache, scheduleRefresh, stop };
}

export async function activate(red) {
  const controller = createController(red);
  red.on("cursor:moved", controller.refreshFromCache);
  red.on("viewport:changed", controller.refreshFromCache);
  red.on("window:focused", controller.refreshFromCache);
  red.on("window:layoutChanged", controller.refreshFromCache);
  red.on("window:bufferChanged", controller.refresh);
  red.on("window:closed", controller.refresh);
  red.on("buffer:changed", controller.scheduleRefresh);
  red.on("file:opened", controller.refresh);
  red.on("theme:changed", controller.refreshFromCache);
  red.on(`windowBar:action:${BAR_ID}`, controller.handleAction);
  red.__barbecueController = controller;
  await controller.refresh();
}

export async function deactivate(red) {
  await red.__barbecueController?.stop();
  red.__barbecueController = null;
}
