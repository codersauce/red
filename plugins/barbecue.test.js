const theme = {
  name: "night",
  style: { fg: "#dddddd", bg: "#111111" },
  colors: {
    "breadcrumb.background": "#101010",
    "breadcrumb.foreground": "#aaaaaa",
    "breadcrumb.focusForeground": "#ffffff",
    "symbolIcon.folderForeground": "#89b4fa",
    "symbolIcon.functionForeground": "#a6e3a1",
  },
};

const symbols = [
  {
    id: "outer",
    parentId: null,
    name: "outer",
    kindName: "Function",
    file: "/repo/plugins/example.js",
    depth: 0,
    range: { start: { line: 2, character: 0 }, end: { line: 12, character: 1 } },
    selectionRange: { start: { line: 2, character: 9 }, end: { line: 2, character: 14 } },
  },
  {
    id: "inner",
    parentId: "outer",
    name: "inner",
    kindName: "Function",
    file: "/repo/plugins/example.js",
    depth: 1,
    range: { start: { line: 5, character: 2 }, end: { line: 8, character: 3 } },
    selectionRange: { start: { line: 5, character: 11 }, end: { line: 5, character: 16 } },
  },
];

function windowAt(line, character = 0) {
  return {
    windowId: 7,
    active: true,
    bufferIndex: 2,
    bufferPath: "/repo/plugins/example.js",
    revision: 4,
    cursor: { x: character, y: line },
    lspPosition: { line, character },
  };
}

describe("Barbecue", () => {
  test("finds the nested symbol chain", async () => {
    expect(plugin.enclosingSymbols(symbols, { line: 6, character: 1 }).map((item) => item.name))
      .toEqual(["outer", "inner"]);
    expect(plugin.enclosingSymbols(symbols, { line: 12, character: 1 })).toEqual([]);
  });

  test("builds path, file, and symbol segments with semantic theme fallbacks", async () => {
    const segments = plugin.buildSegments(
      windowAt(6, 1),
      symbols,
      { separator: "", nerdFont: true },
      { theme },
      "/repo",
    );

    expect(segments.map((item) => item.text).join(""))
      .toBe(" plugins   example.js  󰊕 outer  󰊕 inner");
    expect(segments.at(-1).style.semantic.foreground).toEqual([
      "breadcrumb.focusForeground",
      "breadcrumb.activeSelectionForeground",
      "list.activeSelectionForeground",
      "editor.foreground",
    ]);
    expect(segments.at(-1).style.semantic.background).toEqual([
      "breadcrumb.background",
      "editor.background",
    ]);
    expect(segments.at(-1).action).toBe("jump:2:inner");
  });

  test("supports plain Unicode mode and path fallback without LSP", async () => {
    const segments = plugin.buildSegments(
      windowAt(0),
      [],
      { separator: "›", nerdFont: false },
      { theme },
      "/repo",
    );
    expect(segments.map((item) => item.text).join("")).toBe("plugins › example.js");
  });

  test("renders every window and targets symbol requests by buffer revision", async (red) => {
    await plugin.deactivate(red);
    red.setMockState({
      config: {
        ...red.getMockState().config,
        cwd: "/repo",
        plugin_config: { barbecue: { separator: "" } },
      },
      theme,
      windows: [windowAt(6, 1), { ...windowAt(0), windowId: 8, bufferIndex: 3 }],
      documentSymbols: { ok: true, file: "/repo/plugins/example.js", symbols },
    });

    await plugin.activate(red);

    expect(red.getWindowBar("barbecue", 7).at(-1).text).toBe("󰊕 inner");
    expect(red.getWindowBar("barbecue", 8).map((item) => item.text).join(""))
      .toBe(" plugins   example.js");
    expect(red.getLogs()).toContain('lsp.documentSymbols: {"bufferIndex":2}');
    expect(red.getLogs()).toContain('lsp.documentSymbols: {"bufferIndex":3}');
  });

  test("renders path segments before document symbols resolve and then enriches them", async (red) => {
    await plugin.deactivate(red);
    red.setMockState({
      config: {
        ...red.getMockState().config,
        cwd: "/repo",
        plugin_config: { barbecue: { separator: "" } },
      },
      windows: [windowAt(6, 1)],
    });

    const originalDocumentSymbols = red.lsp.documentSymbols;
    let resolveSymbols;
    red.lsp.documentSymbols = () => new Promise((resolve) => {
      resolveSymbols = resolve;
    });

    try {
      await plugin.activate(red);
      expect(red.getWindowBar("barbecue", 7).map((item) => item.text).join(""))
        .toBe(" plugins   example.js");

      resolveSymbols({ ok: true, revision: 4, symbols });
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();

      expect(red.getWindowBar("barbecue", 7).at(-1).text).toBe("󰊕 inner");
    } finally {
      red.lsp.documentSymbols = originalDocumentSymbols;
    }
  });

  test("caches static context and symbols across cursor refreshes", async (red) => {
    await plugin.deactivate(red);
    const originalGetConfig = red.getConfig.bind(red);
    const originalDocumentSymbols = red.lsp.documentSymbols;
    let configRequests = 0;
    let symbolRequests = 0;
    red.getConfig = async (...args) => {
      configRequests += 1;
      return originalGetConfig(...args);
    };
    red.lsp.documentSymbols = async (...args) => {
      symbolRequests += 1;
      return originalDocumentSymbols(...args);
    };

    try {
      await plugin.activate(red);
      await Promise.resolve();
      await Promise.resolve();
      await red.__barbecueController.refreshFromCache();

      expect(configRequests).toBe(1);
      expect(symbolRequests).toBe(1);
    } finally {
      red.getConfig = originalGetConfig;
      red.lsp.documentSymbols = originalDocumentSymbols;
    }
  });

  test("jumps to a clicked symbol using UTF-16 coordinates", async (red) => {
    await red.emitWindowBarAction("barbecue", { action: "jump:2:inner" });
    expect(red.openedLocations.at(-1).location).toEqual({
      path: "/repo/plugins/example.js",
      line: 5,
      column: 11,
      columnEncoding: "utf-16",
    });
  });

  test("closes its bar on deactivation", async (red) => {
    await plugin.deactivate(red);
    expect(red.getWindowBar("barbecue")).toBe(undefined);
  });
});
