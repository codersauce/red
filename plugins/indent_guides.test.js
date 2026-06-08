const testStyle = {
  fg: { Rgb: { r: 120, g: 120, b: 120 } },
  bg: null,
  bold: false,
  italic: false,
};

function layout(rows, cursorLine = 0) {
  return {
    bufferIndex: 0,
    buffer_index: 0,
    cursor: { x: 0, y: cursorLine, screenRow: cursorLine, screen_row: cursorLine },
    indentation: { shiftWidth: 4, shift_width: 4, tabWidth: 4, tab_width: 4 },
    rows: rows.map((text, line) => ({
      line,
      text,
      screenRow: line,
      screen_row: line,
      startCol: 0,
      start_col: 0,
      endCol: text.length,
      end_col: text.length,
      firstSegment: true,
      first_segment: true,
    })),
  };
}

describe("IndentGuides", () => {
  test("measures spaces and tabs as display columns", async () => {
    expect(plugin.leadingIndentWidth("    let x = 1", 4)).toBe(4);
    expect(plugin.leadingIndentWidth("\tlet x = 1", 4)).toBe(4);
    expect(plugin.leadingIndentWidth("  \tlet x = 1", 4)).toBe(4);
    expect(plugin.leadingIndentWidth("  \t  let x = 1", 4)).toBe(6);
  });

  test("builds one decoration per visible indented line", async () => {
    const decorations = plugin.buildDecorations(layout(["fn main() {", "    let x = 1;", "}"]), {
      styles: { indent: testStyle, scope: { ...testStyle, bold: true } },
      scope: false,
    });

    expect(decorations.length).toBe(1);
    expect(decorations[0].line).toBe(1);
    expect(decorations[0].column).toBe(0);
    expect(decorations[0].text).toBe("│   ");
    expect(decorations[0].repeat_linebreak).toBe(true);
    expect(decorations[0].only_whitespace).toBe(true);
  });

  test("continues guides through blank lines inside the same block", async () => {
    const decorations = plugin.buildDecorations(layout(["fn main() {", "    let x = 1;", "", "    let y = 2;", "}"]), {
      styles: { indent: testStyle, scope: { ...testStyle, bold: true } },
      scope: false,
    });

    const blankLine = decorations.find((decoration) => decoration.line === 2);
    expect(blankLine.text).toBe("│   ");
  });

  test("does not bridge blank lines between top-level blocks", async () => {
    const decorations = plugin.buildDecorations(layout(["fn main() {", "}", "", "    let x = 1;"]), {
      styles: { indent: testStyle, scope: { ...testStyle, bold: true } },
      scope: false,
    });

    const blankLine = decorations.find((decoration) => decoration.line === 2);
    expect(blankLine).toBe(undefined);
  });

  test("highlights the indentation-based active scope", async () => {
    const decorations = plugin.buildDecorations(
      layout(["fn main() {", "    if ready {", "        run();", "    }", "}"], 2),
      {
        styles: { indent: testStyle, scope: { ...testStyle, bold: true } },
      },
    );

    const scoped = decorations.filter((decoration) => decoration.priority === 1024);
    expect(scoped.map((decoration) => decoration.line)).toEqual([2]);
    expect(scoped[0].column).toBe(4);
    expect(scoped[0].style.bold).toBe(true);
  });

  test("caps the number of lines it processes", async () => {
    const decorations = plugin.buildDecorations(layout(["    a", "    b", "    c"]), {
      styles: { indent: testStyle, scope: testStyle },
      scope: false,
      maxLines: 2,
    });

    expect(decorations.map((decoration) => decoration.line)).toEqual([0, 1]);
  });

  test("activation sets an indent guide decoration namespace", async (red) => {
    red.setMockState({
      bufferContent: ["fn main() {", "    let x = 1;", "}"],
      viewportLayout: layout(["fn main() {", "    let x = 1;", "}"], 1),
    });

    await plugin.activate(red);

    const decorations = red.getDecorations("indent-guides");
    const base = decorations.find((decoration) => decoration.priority === 1);
    expect(base.text).toBe("│   ");
  });
});
