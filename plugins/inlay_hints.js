const DEFAULT_NAMESPACE = "inlay-hints";
const DEFAULT_DEBOUNCE_MS = 120;
const DEFAULT_OPACITY = 0.42;
const TYPE_HINT_KIND = 1;
const PARAMETER_HINT_KIND = 2;

const EMPTY_STYLE = {
  fg: null,
  bg: null,
  bold: false,
  italic: true,
};

const rgb = (r, g, b) => ({ Rgb: { r, g, b } });
const clampByte = (value) => Math.max(0, Math.min(255, Math.round(value)));
const clampOpacity = (value) => {
  const opacity = Number(value);
  if (!Number.isFinite(opacity)) {
    return DEFAULT_OPACITY;
  }
  return Math.max(0, Math.min(1, opacity));
};

function colorChannels(value) {
  const color = colorFromTheme(value);
  return color?.Rgb ?? color?.Rgba ?? null;
}

function blendTowardBackground(foreground, background, opacity) {
  const fg = colorChannels(foreground);
  const bg = colorChannels(background);
  if (!fg || !bg) {
    return foreground;
  }

  const alpha = clampOpacity(opacity);
  return rgb(
    clampByte(bg.r + (fg.r - bg.r) * alpha),
    clampByte(bg.g + (fg.g - bg.g) * alpha),
    clampByte(bg.b + (fg.b - bg.b) * alpha),
  );
}

function colorFromTheme(value) {
  if (!value) {
    return null;
  }
  if (typeof value !== "string") {
    return value;
  }
  const hex = value.replace(/^#/, "");
  if (hex.length < 6) {
    return null;
  }
  return rgb(
    Number.parseInt(hex.slice(0, 2), 16),
    Number.parseInt(hex.slice(2, 4), 16),
    Number.parseInt(hex.slice(4, 6), 16),
  );
}

export function stylesFor(info, options = {}) {
  const theme = info?.theme ?? {};
  const colors = theme.colors ?? {};
  const baseFg =
    colorFromTheme(colors["editorInlayHint.typeForeground"]) ??
    colorFromTheme(colors["editorInlayHint.foreground"]) ??
    theme.gutter_style?.fg ??
    theme.gutterStyle?.fg ??
    rgb(108, 112, 134);
  const bg =
    colorFromTheme(colors["editor.background"]) ??
    theme.style?.bg ??
    rgb(0, 0, 0);
  const opacity = options.opacity ?? DEFAULT_OPACITY;
  const fg = blendTowardBackground(baseFg, bg, opacity);

  return {
    hint: {
      ...EMPTY_STYLE,
      fg,
    },
  };
}

export function labelText(label) {
  if (typeof label === "string") {
    return label;
  }
  if (Array.isArray(label)) {
    return label.map((part) => part?.value ?? "").join("");
  }
  return "";
}

export function formatLineHints(lineHints = [], options = {}) {
  const typeHints = [];
  const parameterHints = [];
  const showParameterHints = options.parameterHints === true;

  for (const hint of [...lineHints].sort(
    (a, b) => (a.position?.character ?? 0) - (b.position?.character ?? 0),
  )) {
    const text = labelText(hint.label).trim();
    if (!text) {
      continue;
    }

    if (hint.kind === TYPE_HINT_KIND) {
      typeHints.push(text.replace(/^:\s*/, ""));
    } else if (hint.kind === PARAMETER_HINT_KIND && showParameterHints) {
      parameterHints.push(text.replace(/:$/, ""));
    }
  }

  const parts = [];
  if (parameterHints.length > 0) {
    parts.push(`<- (${parameterHints.join(",")})`);
  }
  if (typeHints.length > 0) {
    parts.push(`=> ${typeHints.join(",")}`);
  }
  return parts.join(" ");
}

export function visibleRange(layout) {
  const rows = Array.isArray(layout?.rows) ? layout.rows : [];
  if (rows.length === 0) {
    return null;
  }

  let startLine = rows[0].line ?? 0;
  let endLine = startLine;
  for (const row of rows) {
    const line = row.line ?? startLine;
    startLine = Math.min(startLine, line);
    endLine = Math.max(endLine, line);
  }

  return {
    start: { line: startLine, character: 0 },
    end: { line: endLine + 1, character: 0 },
  };
}

function uniqueLines(rows = []) {
  const seen = new Set();
  const lines = [];
  for (const row of rows) {
    const line = row.line;
    if (line == null || seen.has(line)) {
      continue;
    }
    seen.add(line);
    lines.push(row);
  }
  return lines;
}

export function buildDecorations(layout, hintsResult, options = {}) {
  if (!hintsResult?.ok) {
    return [];
  }

  const hintsByLine = new Map();
  for (const hint of hintsResult.hints ?? []) {
    const line = hint.position?.line;
    if (line == null) {
      continue;
    }
    const hints = hintsByLine.get(line) ?? [];
    hints.push(hint);
    hintsByLine.set(line, hints);
  }

  const style = options.style ?? stylesFor(options.info, options).hint;
  const bufferIndex = layout?.bufferIndex ?? layout?.buffer_index ?? 0;
  const decorations = [];

  for (const row of uniqueLines(layout?.rows)) {
    const text = formatLineHints(hintsByLine.get(row.line) ?? [], options);
    if (!text) {
      continue;
    }

    decorations.push({
      buffer_index: bufferIndex,
      line: row.line,
      anchor: options.anchor ?? "eol",
      column: 0,
      text: ` ${text}`,
      style,
      priority: options.priority ?? 1001,
    });
  }

  return decorations;
}

function createController(red, options = {}) {
  let timer = null;
  let refreshInFlight = false;
  let pendingRefresh = false;
  let lastPayload = "";
  const namespace = options.namespace ?? DEFAULT_NAMESPACE;
  const debounceMs = Math.max(0, Number(options.debounceMs ?? DEFAULT_DEBOUNCE_MS));

  async function refresh() {
    if (refreshInFlight) {
      pendingRefresh = true;
      return;
    }

    refreshInFlight = true;
    try {
      const layout = await red.getViewportLayout();
      const range = visibleRange(layout);
      if (!range) {
        red.clearDecorations(namespace);
        lastPayload = "";
        return;
      }

      const [info, hintsResult] = await Promise.all([
        red.getEditorInfo ? red.getEditorInfo() : Promise.resolve(null),
        red.lsp.inlayHints({ range }),
      ]);
      const decorations = buildDecorations(layout, hintsResult, {
        ...options,
        info,
      });
      const payload = JSON.stringify(decorations);
      if (payload !== lastPayload) {
        lastPayload = payload;
        red.setDecorations(namespace, decorations);
      }
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

  async function stop() {
    if (timer != null) {
      await red.clearTimeout(timer);
      timer = null;
    }
    red.clearDecorations(namespace);
  }

  return { refresh, scheduleRefresh, stop };
}

export async function activate(red) {
  const controller = createController(red);
  red.on("buffer:changed", () => controller.scheduleRefresh());
  red.on("file:opened", () => controller.scheduleRefresh());
  red.on("viewport:changed", () => controller.scheduleRefresh());
  red.on("theme:changed", () => controller.scheduleRefresh());
  red.__inlayHintsController = controller;
  await controller.refresh();
}

export async function deactivate(red) {
  await red.__inlayHintsController?.stop();
  red.__inlayHintsController = null;
}

export function inlayHintsDefaults() {
  return {
    namespace: DEFAULT_NAMESPACE,
    debounceMs: DEFAULT_DEBOUNCE_MS,
    anchor: "eol",
    parameterHints: false,
  };
}
