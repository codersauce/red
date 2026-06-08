describe("ProjectSearch", () => {
  test("activation registers the default command and picker actions", async (red) => {
    expect(red.hasCommand("ProjectSearch")).toBe(true);

    await red.executeCommand("ProjectSearch");

    expect(red.pickers.length).toBe(1);
    const actions = red.pickers[0].options.actions.map((action) => action.action);
    expect(actions).toContain("open_horizontal");
    expect(actions).toContain("open_vertical");
    expect(actions).toContain("toggle_regex");
    expect(actions).toContain("toggle_preview");
    expect(actions).toContain("export");
  });

  test("streams ripgrep matches into the picker", async (red) => {
    await red.executeCommand("ProjectSearch");
    const picker = red.pickers[red.pickers.length - 1];

    picker.options.onQuery("ProjectSearch");
    await new Promise((resolve) => setTimeout(resolve, 120));
    const process = red.spawnedProcesses[red.spawnedProcesses.length - 1];
    process.options.onStdout(JSON.stringify({
      type: "match",
      data: {
        path: { text: "src/config.rs" },
        lines: { text: "ProjectSearch\n" },
        line_number: 7,
        submatches: [{ match: { text: "ProjectSearch" }, start: 0, end: 13 }],
      },
    }));
    process.options.onExit({ code: 0 });
    await new Promise((resolve) => setTimeout(resolve, 30));

    expect(picker.items.length).toBe(1);
    expect(picker.items[0].label).toBe("src/config.rs");
    expect(picker.items[0].annotation).toBe(":7:1");
    expect(picker.items[0].detailMatches).toEqual([[0, 13]]);
    expect(picker.status).toContain("1 matches");
  });

  test("builds Snacks-like ripgrep arguments", async () => {
    const args = plugin.buildRipgrepArgs("Needle", {
      hidden: true,
      ignored: true,
      exclude: ["target", "node_modules"],
    });

    expect(args).toContain("--json");
    expect(args).toContain("--smart-case");
    expect(args).toContain("--max-columns=500");
    expect(args).toContain("--hidden");
    expect(args).toContain("--no-ignore");
    expect(args).toEqual([
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
      "--glob",
      "!target",
      "--glob",
      "!node_modules",
      "--hidden",
      "--no-ignore",
      "--",
      "Needle",
    ]);
  });

  test("parses ripgrep match JSON into a location", async () => {
    const match = plugin.parseRipgrepJsonLine(JSON.stringify({
      type: "match",
      data: {
        path: { text: "src/main.rs" },
        lines: { text: "let needle = true;\n" },
        line_number: 42,
        submatches: [{ match: { text: "needle" }, start: 4, end: 10 }],
      },
    }));

    expect(match).toEqual({
      path: "src/main.rs",
      line: 41,
      column: 4,
      columnEncoding: "utf8-byte",
      text: "let needle = true;",
      match: "needle",
      matches: [{ start: 4, end: 10, text: "needle" }],
    });
  });

  test("preserves ripgrep byte offsets for openLocation", async () => {
    const match = plugin.parseRipgrepJsonLine(JSON.stringify({
      type: "match",
      data: {
        path: { text: "unicode.txt" },
        lines: { text: "a\u00e9 needle\n" },
        line_number: 3,
        submatches: [{ match: { text: "needle" }, start: 4, end: 10 }],
      },
    }));

    expect(match.line).toBe(2);
    expect(match.column).toBe(4);
    expect(match.columnEncoding).toBe("utf8-byte");
    expect(match.matches).toEqual([{ start: 4, end: 10, text: "needle" }]);
  });

  test("ignores malformed and non-match ripgrep events", async () => {
    expect(plugin.parseRipgrepJsonLine("not json")).toBe(null);
    expect(plugin.parseRipgrepJsonLine('{"type":"begin","data":{}}')).toBe(null);
  });

  test("increments generations and rejects stale output", async () => {
    const first = plugin.beginSearch(plugin.createSearchState(), "first");
    const second = plugin.beginSearch(first, "second");

    expect(first.generation).toBe(1);
    expect(second.generation).toBe(2);
    expect(second.matches).toEqual([]);
    expect(plugin.isCurrentGeneration(second, first.generation)).toBe(false);
    expect(plugin.isCurrentGeneration(second, second.generation)).toBe(true);
  });

  test("keeps unique most-recent search history", async () => {
    let entries = plugin.addSearchHistory(["alpha", "beta"], " beta ");
    entries = plugin.addSearchHistory(entries, "gamma", 2);

    expect(entries).toEqual(["gamma", "beta"]);
    expect(plugin.addSearchHistory(entries, "  ")).toEqual(entries);
  });

  test("builds picker items and export panel rows", async () => {
    const matches = [{
      path: "src/lib.rs",
      line: 6,
      column: 11,
      text: "pub fn search() {}",
      match: "search",
    }];
    const items = plugin.buildPickerItems(matches);
    const rows = plugin.buildExportPanelRows(matches);

    expect(items[0].label).toBe("src/lib.rs");
    expect(items[0].annotation).toBe(":7:12");
    expect(items[0].detail).toBe("pub fn search() {}");
    expect(items[0].detailMatches).toEqual([[11, 17]]);
    expect(items[0].data.location).toEqual({
      path: "src/lib.rs",
      line: 6,
      column: 11,
      columnEncoding: "utf8-byte",
    });
    expect(items[0].preview).toEqual({
      path: "src/lib.rs",
      line: 6,
      column: 11,
      matches: [[11, 17]],
    });
    expect(rows[0].id).toBe(items[0].id);
    expect(rows[0].path).toBe("src/lib.rs");
    expect(rows[0].segments[0].text).toBe("src/lib.rs:7:12: pub fn search() {}");
  });

  test("converts ripgrep byte ranges to character ranges for result highlighting", async () => {
    const items = plugin.buildPickerItems([{
      path: "unicode.txt",
      line: 2,
      column: 4,
      text: "a\u00e9 needle needle",
      match: "needle",
      matches: [
        { start: 4, end: 10, text: "needle" },
        { start: 11, end: 17, text: "needle" },
      ],
    }]);

    expect(items[0].detailMatches).toEqual([[3, 9], [10, 16]]);
    expect(items[0].preview.matches).toEqual([[4, 10], [11, 17]]);
  });
});
