const DEFAULT_NAMESPACE = "indent-guides";
const DEFAULT_CHAR = "│";
const DEFAULT_DEBOUNCE_MS = 50;
const DEFAULT_MAX_LINES = 500;

function scopeNamespace(namespace) {
  return `${namespace}:scope`;
}

const rgb = (r, g, b) => ({ Rgb: { r, g, b } });

const EMPTY_STYLE = {
  fg: null,
  bg: null,
  bold: false,
  italic: false,
};

function style(base = {}, overrides = {}) {
  return {
    fg: base.fg ?? null,
    bg: base.bg ?? null,
    bold: base.bold ?? false,
    italic: base.italic ?? false,
    ...overrides,
  };
}

function themedStyle(foreground, fallback, options = {}) {
  return {
    semantic: { foreground, ...options },
    style: fallback,
  };
}

function decorationStylePayload(value) {
  if (value && (Object.hasOwn(value, "semantic") || Object.hasOwn(value, "style"))) {
    return value;
  }
  return { style: value ?? EMPTY_STYLE };
}

export function stylesFor(info) {
  const theme = info?.theme ?? {};
  const base = style(theme.style ?? EMPTY_STYLE);
  const ui = theme.ui_style ?? theme.uiStyle ?? {};
  const guide =
    ui["editorIndentGuide.background1"] ??
    ui["editorIndentGuide.background"] ??
    rgb(88, 91, 112);
  const activeGuide =
    ui["editorIndentGuide.activeBackground1"] ??
    ui["editorIndentGuide.activeBackground"] ??
    theme.line_highlight_style?.fg ??
    rgb(186, 194, 222);

  return {
    indent: themedStyle(
      ["editorIndentGuide.background1", "editorIndentGuide.background"],
      style(base, { fg: guide }),
    ),
    scope: themedStyle(
      [
        "editorIndentGuide.activeBackground1",
        "editorIndentGuide.activeBackground",
        "editor.lineHighlightForeground",
      ],
      style(base, { fg: activeGuide, bold: true }),
      { bold: true },
    ),
  };
}

export function leadingIndentWidth(line, tabWidth = 4) {
  const width = Math.max(1, Number(tabWidth) || 4);
  let col = 0;
  for (const ch of line ?? "") {
    if (ch === " ") {
      col += 1;
    } else if (ch === "\t") {
      col += width - (col % width);
    } else if (/\s/u.test(ch)) {
      col += 1;
    } else {
      break;
    }
  }
  return col;
}

function uniqueFirstSegmentRows(rows = []) {
  const lines = new Map();
  for (const row of rows) {
    const first = row.firstSegment ?? row.first_segment ?? true;
    if (!first || lines.has(row.line)) {
      continue;
    }
    lines.set(row.line, row);
  }
  return [...lines.values()];
}

function isBlankLine(row) {
  return (row?.text ?? "").trim().length === 0;
}

export function inferBlankIndentWidths(rows, rawWidths) {
  const widths = new Map(rawWidths);
  const previous = [];
  let previousIndent = null;

  for (let index = 0; index < rows.length; index += 1) {
    previous[index] = previousIndent;
    if (!isBlankLine(rows[index])) {
      previousIndent = rawWidths.get(rows[index].line) ?? 0;
    }
  }

  let nextIndent = null;
  for (let index = rows.length - 1; index >= 0; index -= 1) {
    const row = rows[index];
    if (isBlankLine(row)) {
      const before = previous[index];
      let inferred = 0;
      if (before != null && nextIndent != null) {
        inferred = Math.min(before, nextIndent);
      } else {
        inferred = before ?? nextIndent ?? 0;
      }
      widths.set(row.line, inferred);
    } else {
      nextIndent = rawWidths.get(row.line) ?? 0;
    }
  }

  return widths;
}

export function activeScope(layout, widths) {
  if (!layout || !Array.isArray(layout.rows) || widths.size === 0) {
    return null;
  }

  const cursorLine = layout.cursor?.y ?? 0;
  const currentIndent = widths.get(cursorLine) ?? 0;
  const shiftWidth =
    layout.indentation?.shiftWidth ??
    layout.indentation?.shift_width ??
    layout.indentation?.tabWidth ??
    layout.indentation?.tab_width ??
    4;

  if (currentIndent < shiftWidth) {
    return null;
  }

  const level = Math.floor(currentIndent / shiftWidth) * shiftWidth;
  let start = cursorLine;
  let end = cursorLine;

  // Blank lines inside a block have inferred widths, so zero is a real scope boundary.
  for (let line = cursorLine - 1; widths.has(line); line -= 1) {
    const width = widths.get(line);
    if (width < level) {
      break;
    }
    start = line;
  }

  const visibleLines = [...widths.keys()].sort((a, b) => a - b);
  const maxLine = visibleLines[visibleLines.length - 1];
  for (let line = cursorLine + 1; line <= maxLine && widths.has(line); line += 1) {
    const width = widths.get(line);
    if (width < level) {
      break;
    }
    end = line;
  }

  return { column: level - shiftWidth, start, end };
}

function guideText(indentWidth, shiftWidth, guideChar) {
  let text = "";
  for (let col = 0; col < indentWidth; col += 1) {
    if (col % shiftWidth === 0) {
      text += guideChar;
    } else {
      text += " ";
    }
  }

  return text;
}

export function buildDecorations(layout, options = {}) {
  const shiftWidth = Math.max(
    1,
    Number(
      layout?.indentation?.shiftWidth ??
        layout?.indentation?.shift_width ??
        layout?.indentation?.tabWidth ??
        layout?.indentation?.tab_width ??
        options.shiftWidth ??
        4,
    ) || 4,
  );
  const tabWidth = Math.max(
    1,
    Number(layout?.indentation?.tabWidth ?? layout?.indentation?.tab_width ?? shiftWidth) || shiftWidth,
  );
  const guideChar = options.char ?? DEFAULT_CHAR;
  const styles = options.styles ?? stylesFor(options.info);
  const maxLines = Math.max(1, Number(options.maxLines ?? DEFAULT_MAX_LINES));
  const rows = uniqueFirstSegmentRows(layout?.rows).slice(0, maxLines);
  const rawWidths = new Map();

  for (const row of rows) {
    rawWidths.set(row.line, leadingIndentWidth(row.text ?? "", tabWidth));
  }

  const widths = inferBlankIndentWidths(rows, rawWidths);
  const scope = options.scope === false ? null : activeScope(layout, widths);
  const decorations = [];
  const bufferIndex = layout?.bufferIndex ?? layout?.buffer_index ?? 0;

  for (const row of rows) {
    const indentWidth = widths.get(row.line) ?? 0;
    if (indentWidth < shiftWidth) {
      continue;
    }

    const inScope =
      scope &&
      row.line >= scope.start &&
      row.line <= scope.end &&
      indentWidth > scope.column;
    decorations.push({
      buffer_index: bufferIndex,
      line: row.line,
      column: 0,
      text: guideText(indentWidth, shiftWidth, guideChar),
      ...decorationStylePayload(styles.indent),
      priority: 1,
      repeat_linebreak: true,
      only_whitespace: true,
    });

    if (inScope) {
      decorations.push({
        buffer_index: bufferIndex,
        line: row.line,
        column: scope.column,
        text: guideChar,
        ...decorationStylePayload(styles.scope),
        priority: 1024,
        repeat_linebreak: true,
        only_whitespace: true,
      });
    }
  }

  return decorations;
}

export function buildScopeDecorations(layout, options = {}) {
  const shiftWidth = Math.max(
    1,
    Number(
      layout?.indentation?.shiftWidth ??
        layout?.indentation?.shift_width ??
        layout?.indentation?.tabWidth ??
        layout?.indentation?.tab_width ??
        options.shiftWidth ??
        4,
    ) || 4,
  );
  const tabWidth = Math.max(
    1,
    Number(layout?.indentation?.tabWidth ?? layout?.indentation?.tab_width ?? shiftWidth) || shiftWidth,
  );
  const guideChar = options.char ?? DEFAULT_CHAR;
  const styles = options.styles ?? stylesFor(options.info);
  const maxLines = Math.max(1, Number(options.maxLines ?? DEFAULT_MAX_LINES));
  const rows = uniqueFirstSegmentRows(layout?.rows).slice(0, maxLines);
  const rawWidths = new Map();

  for (const row of rows) {
    rawWidths.set(row.line, leadingIndentWidth(row.text ?? "", tabWidth));
  }

  const widths = inferBlankIndentWidths(rows, rawWidths);
  const scope = options.scope === false ? null : activeScope(layout, widths);
  if (!scope) {
    return [];
  }

  const decorations = [];
  const bufferIndex = layout?.bufferIndex ?? layout?.buffer_index ?? 0;
  for (const row of rows) {
    const indentWidth = widths.get(row.line) ?? 0;
    const inScope =
      row.line >= scope.start &&
      row.line <= scope.end &&
      indentWidth > scope.column;
    if (!inScope) {
      continue;
    }

    decorations.push({
      buffer_index: bufferIndex,
      line: row.line,
      column: scope.column,
      text: guideChar,
      ...decorationStylePayload(styles.scope),
      priority: 1024,
      repeat_linebreak: true,
      only_whitespace: true,
    });
  }

  return decorations;
}

function createController(red, options = {}) {
  let timer = null;
  let refreshInFlight = false;
  let pendingRefresh = false;
  let lastGuidePayload = "";
  let lastScopePayload = "";
  let lastLayout = null;
  let currentStyles = stylesFor(null);
  const namespace = options.namespace ?? DEFAULT_NAMESPACE;
  const activeScopeNamespace = options.scopeNamespace ?? scopeNamespace(namespace);
  const debounceMs = Math.max(0, Number(options.debounceMs ?? DEFAULT_DEBOUNCE_MS));

  function publishGuides(layout) {
    const decorations = buildDecorations(layout, {
      ...options,
      styles: currentStyles,
      scope: false,
    });
    const payload = JSON.stringify(decorations);
    if (payload !== lastGuidePayload) {
      lastGuidePayload = payload;
      red.setDecorations(namespace, decorations);
    }
  }

  function publishScope(layout) {
    const decorations = buildScopeDecorations(layout, {
      ...options,
      styles: currentStyles,
    });
    const payload = JSON.stringify(decorations);
    if (payload !== lastScopePayload) {
      lastScopePayload = payload;
      red.setDecorations(activeScopeNamespace, decorations);
    }
  }

  async function refresh() {
    if (refreshInFlight) {
      pendingRefresh = true;
      return;
    }

    refreshInFlight = true;
    try {
      const [info, layout] = await Promise.all([
        red.getEditorInfo ? red.getEditorInfo() : Promise.resolve(null),
        red.getViewportLayout(),
      ]);
      currentStyles = stylesFor(info);
      lastLayout = layout;
      publishGuides(layout);
      publishScope(layout);
    } finally {
      refreshInFlight = false;
      if (pendingRefresh) {
        pendingRefresh = false;
        scheduleRefresh();
      }
    }
  }

  async function scheduleRefresh() {
    if (timer != null) {
      await red.clearTimeout(timer);
    }
    timer = await red.setTimeout(async () => {
      timer = null;
      await refresh();
    }, debounceMs);
  }

  function refreshScopeForCursor(event = {}) {
    if (!lastLayout) {
      scheduleRefresh();
      return;
    }

    const viewportTop = event.viewportTop ?? event.viewport_top;
    const bufferIndex = event.bufferIndex ?? event.buffer_index;
    if (
      viewportTop != null &&
      lastLayout.vtop != null &&
      viewportTop !== lastLayout.vtop
    ) {
      scheduleRefresh();
      return;
    }
    if (
      bufferIndex != null &&
      (lastLayout.bufferIndex ?? lastLayout.buffer_index) !== bufferIndex
    ) {
      scheduleRefresh();
      return;
    }

    lastLayout = {
      ...lastLayout,
      cursor: {
        ...(lastLayout.cursor ?? {}),
        x: event.x ?? lastLayout.cursor?.x ?? 0,
        y: event.y ?? lastLayout.cursor?.y ?? 0,
        screenRow: event.screenRow ?? event.screen_row ?? lastLayout.cursor?.screenRow,
        screen_row: event.screenRow ?? event.screen_row ?? lastLayout.cursor?.screen_row,
      },
    };
    publishScope(lastLayout);
  }

  async function stop() {
    if (timer != null) {
      await red.clearTimeout(timer);
      timer = null;
    }
    red.clearDecorations(namespace);
    red.clearDecorations(activeScopeNamespace);
  }

  return { refresh, scheduleRefresh, refreshScopeForCursor, stop };
}

export async function activate(red) {
  const controller = createController(red);
  red.on("editor:ready", () => controller.refresh());
  red.on("editor:stateRestored", () => controller.refresh());
  red.on("buffer:changed", () => controller.scheduleRefresh());
  red.on("cursor:moved", (event) => controller.refreshScopeForCursor(event));
  red.on("viewport:changed", () => controller.scheduleRefresh());
  red.on("mode:changed", () => controller.scheduleRefresh());
  red.on("theme:changed", () => controller.scheduleRefresh());
  red.__indentGuidesController = controller;
  await controller.refresh();
}

export async function deactivate(red) {
  await red.__indentGuidesController?.stop();
  red.__indentGuidesController = null;
}

export function indentGuidesDefaults() {
  return {
    namespace: DEFAULT_NAMESPACE,
    char: DEFAULT_CHAR,
    debounceMs: DEFAULT_DEBOUNCE_MS,
    maxLines: DEFAULT_MAX_LINES,
  };
}

export function createIndentGuidesController(red, options = {}) {
  return createController(red, options);
}
