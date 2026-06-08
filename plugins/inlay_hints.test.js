const testStyle = {
  fg: { Rgb: { r: 108, g: 112, b: 134 } },
  bg: null,
  bold: false,
  italic: true,
};

function layout(rows) {
  return {
    bufferIndex: 0,
    buffer_index: 0,
    contentWidth: 80,
    content_width: 80,
    rows: rows.map((text, line) => ({
      screenRow: line,
      screen_row: line,
      line,
      startCol: 0,
      start_col: 0,
      endCol: text.length,
      end_col: text.length,
      firstSegment: true,
      first_segment: true,
      text,
    })),
  };
}

describe("InlayHints", () => {
  test("formats type hints with the loved arrow style", async () => {
    const text = plugin.formatLineHints([
      {
        kind: 1,
        position: { line: 0, character: 12 },
        label: ": PathBuf",
      },
    ]);

    expect(text).toBe("=> PathBuf");
  });

  test("keeps parameter hints hidden by default", async () => {
    const text = plugin.formatLineHints([
      {
        kind: 2,
        position: { line: 0, character: 4 },
        label: "file:",
      },
      {
        kind: 1,
        position: { line: 0, character: 8 },
        label: ": String",
      },
    ]);

    expect(text).toBe("=> String");
  });

  test("can include parameter hints when configured", async () => {
    const text = plugin.formatLineHints(
      [
        {
          kind: 2,
          position: { line: 0, character: 4 },
          label: "file:",
        },
        {
          kind: 1,
          position: { line: 0, character: 8 },
          label: ": String",
        },
      ],
      { parameterHints: true },
    );

    expect(text).toBe("<- (file) => String");
  });

  test("fades the theme hint color toward the editor background", async () => {
    const styles = plugin.stylesFor(
      {
        theme: {
          colors: {
            "editorInlayHint.typeForeground": "#c8c8c8",
            "editor.background": "#0a141e",
          },
        },
      },
      { opacity: 0.5 },
    );

    expect(styles.hint.fg).toEqual({ Rgb: { r: 105, g: 110, b: 115 } });
  });

  test("builds eol decorations for visible hint lines", async () => {
    const decorations = plugin.buildDecorations(
      layout(["let path = Config::path(\"config.toml\");", "let x = 1;"]),
      {
        ok: true,
        file: "/tmp/main.rs",
        hints: [
          {
            kind: 1,
            position: { line: 0, character: 16 },
            label: [{ value: ": PathBuf" }],
          },
        ],
      },
      { style: testStyle },
    );

    expect(decorations.length).toBe(1);
    expect(decorations[0].anchor).toBe("eol");
    expect(decorations[0].text).toBe(" => PathBuf");
    expect(decorations[0].priority).toBe(1001);
  });

  test("activation requests visible range and sets decorations", async (red) => {
    red.setMockState({
      bufferContent: ["let path = Config::path(\"config.toml\");"],
      viewportLayout: layout(["let path = Config::path(\"config.toml\");"]),
      inlayHints: {
        ok: true,
        file: "/tmp/main.rs",
        hints: [
          {
            kind: 1,
            position: { line: 0, character: 16 },
            label: ": PathBuf",
          },
        ],
      },
    });

    await plugin.deactivate(red);
    await plugin.activate(red);

    const decorations = red.getDecorations("inlay-hints");
    expect(decorations[0].text).toBe(" => PathBuf");
    expect(red.getLogs()).toContain(
      'lsp.inlayHints: {"range":{"start":{"line":0,"character":0},"end":{"line":1,"character":0}}}',
    );
  });
});
