/// <reference path="../types/red.d.ts" />

type SymbolIconConfig = {
  enabled?: boolean;
  overrides?: Record<string, string>;
};

type SymbolPickerData = { symbol: Red.DocumentSymbol };
type ReferencePickerData = { location: Red.FileLocation };
const workspaceQueryDebounceMs = 150;

export const defaultSymbolIcons: Record<string, string> = {
  Array: "",
  Boolean: "󰨙",
  Class: "",
  Color: "",
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
  Keyword: "",
  Method: "󰊕",
  Module: "",
  Namespace: "󰦮",
  Null: "",
  Number: "󰎠",
  Object: "",
  Operator: "",
  Package: "",
  Property: "",
  Reference: "",
  Snippet: "󱄽",
  String: "",
  Struct: "󰆼",
  Text: "",
  Trait: "",
  TypeParameter: "",
  Unit: "",
  Unknown: "",
  Value: "",
  Variable: "󰀫",
};

const workspaceSymbolKinds = new Set([
  "Class",
  "Constructor",
  "Enum",
  "Field",
  "Function",
  "Interface",
  "Method",
  "Module",
  "Namespace",
  "Package",
  "Property",
  "Struct",
  "Trait",
]);

function configuredIcons(config: Record<string, any> | null | undefined): SymbolIconConfig {
  const icons = config?.lsp_symbols?.icons;
  if (!icons || typeof icons !== "object" || Array.isArray(icons)) return {};
  return icons;
}

export function symbolIcon(kindName: string, config: SymbolIconConfig = {}): string {
  if (config.enabled === false) return "";
  const key = kindName.toLowerCase();
  const overrides = config.overrides || {};
  if (Object.prototype.hasOwnProperty.call(overrides, key)) {
    return String(overrides[key] ?? "");
  }
  return defaultSymbolIcons[kindName] || defaultSymbolIcons.Unknown;
}

function iconLabel(kindName: string, label: string, config: SymbolIconConfig): string {
  const icon = symbolIcon(kindName, config);
  return icon ? `${icon} ${label}` : label;
}

function compactPath(file: string): string {
  const parts = file.split(/[\\/]/).filter(Boolean);
  return parts.slice(-2).join("/") || file;
}

function pickerId(prefix: string, file: string, range: Red.Range, suffix = ""): string {
  return [
    prefix,
    file,
    range.start.line,
    range.start.character,
    range.end.line,
    range.end.character,
    suffix,
  ].join("\u0000");
}

function symbolPickerItem(
  symbol: Red.DocumentSymbol,
  config: SymbolIconConfig,
): Red.PickerItem<SymbolPickerData> {
  const position = symbol.selectionRange.start;
  const indent = "  ".repeat(symbol.depth);
  return {
    id: pickerId(symbol.kindName, symbol.file, symbol.selectionRange, symbol.name),
    label: `${indent}${iconLabel(symbol.kindName, symbol.name, config)}`,
    kind: symbol.kindName,
    annotation: symbol.detail ? ` ${symbol.detail}` : undefined,
    detail: `${compactPath(symbol.file)}:${position.line + 1}:${position.character + 1}`,
    data: { symbol },
    preview: {
      path: symbol.file,
      line: position.line,
      column: position.character,
    },
  };
}

export function buildDocumentSymbolItems(
  symbols: Red.DocumentSymbol[],
  config: SymbolIconConfig = {},
): Array<Red.PickerItem<SymbolPickerData>> {
  return symbols.map((symbol) => symbolPickerItem(symbol, config));
}

export function buildWorkspaceSymbolItems(
  symbols: Red.DocumentSymbol[],
  config: SymbolIconConfig = {},
): Array<Red.PickerItem<SymbolPickerData>> {
  return symbols
    .filter((symbol) => workspaceSymbolKinds.has(symbol.kindName))
    .map((symbol) => symbolPickerItem(symbol, config));
}

export function isCurrentReference(
  location: Red.FileLocation,
  file: string,
  position: Red.Position,
): boolean {
  return (
    location.file === file &&
    location.range.start.line <= position.line &&
    location.range.end.line >= position.line
  );
}

export function buildReferenceItems(
  locations: Red.FileLocation[],
  config: SymbolIconConfig = {},
): Array<Red.PickerItem<ReferencePickerData>> {
  return locations.map((location) => {
    const position = location.range.start;
    return {
      id: pickerId("Reference", location.file, location.range),
      label: iconLabel("Reference", compactPath(location.file), config),
      kind: "Reference",
      annotation: `:${position.line + 1}:${position.character + 1}`,
      data: { location },
      preview: {
        path: location.file,
        line: position.line,
        column: position.character,
      },
    };
  });
}

function jumpTo(red: Red.RedAPI, location: Red.FileLocation): void {
  red.openLocation({
    path: location.file,
    line: location.range.start.line,
    column: location.range.start.character,
    columnEncoding: "utf-16",
  });
}

function jumpToSymbol(red: Red.RedAPI, symbol: Red.DocumentSymbol): void {
  jumpTo(red, {
    file: symbol.file,
    range: symbol.selectionRange,
  });
}

async function loadIconConfig(red: Red.RedAPI): Promise<SymbolIconConfig> {
  return configuredIcons(await red.getConfig("plugin_config"));
}

export async function activate(red: Red.RedAPI): Promise<void> {
  red.addCommand("LspDocumentSymbols", async () => {
    const [result, icons] = await Promise.all([
      red.lsp.documentSymbols(),
      loadIconConfig(red),
    ]);
    if (!result.ok) {
      red.execute("Print", `Document symbols unavailable: ${result.error}`);
      return;
    }

    const items = buildDocumentSymbolItems(result.symbols, icons);
    if (items.length === 0) {
      red.execute("Print", "No document symbols found");
      return;
    }

    const selected = await red.pickDynamic<SymbolPickerData>("Document Symbols", items, {
      placeholder: "Filter document symbols",
      status: `${items.length} symbols`,
    });
    if (selected?.data?.symbol) jumpToSymbol(red, selected.data.symbol);
  });

  red.addCommand("LspWorkspaceSymbols", async () => {
    const icons = await loadIconConfig(red);
    let queryGeneration = 0;
    let debounceGeneration = 0;
    let picker: Red.PickerController<SymbolPickerData>;

    const runQuery = async (query: string, generation = ++queryGeneration) => {
      picker.updateStatus("Searching workspace symbols...");
      const result = await red.lsp.workspaceSymbols(query);
      if (generation !== queryGeneration) return;
      if (!result.ok) {
        picker.updateItems([]);
        picker.updateStatus(`Workspace symbols unavailable: ${result.error}`);
        return;
      }
      const items = buildWorkspaceSymbolItems(result.symbols, icons);
      picker.updateItems(items);
      picker.updateStatus(`${items.length} symbols`);
    };

    const scheduleQuery = (query: string) => {
      const generation = ++queryGeneration;
      const debounce = ++debounceGeneration;
      picker.updateStatus("Waiting for workspace symbol query...");
      globalThis.setTimeout(() => {
        if (debounce === debounceGeneration) void runQuery(query, generation);
      }, workspaceQueryDebounceMs);
    };

    picker = red.createPicker<SymbolPickerData>("Workspace Symbols", [], {
      externalFilter: true,
      placeholder: "Type to search workspace symbols",
      status: "Loading workspace symbols...",
      onQuery: scheduleQuery,
    });
    void runQuery("");

    const selected = await picker.result;
    debounceGeneration += 1;
    queryGeneration += 1;
    if (selected?.data?.symbol) jumpToSymbol(red, selected.data.symbol);
  });

  red.addCommand("LspReferences", async () => {
    const [result, icons] = await Promise.all([
      red.lsp.references({ includeDeclaration: true }),
      loadIconConfig(red),
    ]);
    if (!result.ok) {
      red.execute("Print", `References unavailable: ${result.error}`);
      return;
    }

    const references = result.references.filter(
      (location) => !isCurrentReference(location, result.file, result.position),
    );
    if (references.length === 0) {
      red.execute("Print", "No references found");
      return;
    }
    if (references.length === 1) {
      jumpTo(red, references[0]);
      return;
    }

    const items = buildReferenceItems(references, icons);
    const selected = await red.pickDynamic<ReferencePickerData>("References", items, {
      placeholder: "Filter references",
      status: `${items.length} references`,
    });
    if (selected?.data?.location) jumpTo(red, selected.data.location);
  });
}
