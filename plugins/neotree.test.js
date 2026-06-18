function testStyles() {
  const base = { fg: { Rgb: { r: 255, g: 255, b: 255 } } };
  return {
    root: base,
    normal: base,
    guide: base,
    directory: base,
    ignored: base,
    status: {
      modified: base,
      untracked: base,
      ignored: base,
    },
  };
}

describe("NeoTree", () => {
  test("fallback styles are complete Rust Style payloads", async () => {
    const styles = plugin.stylesFor(null);

    expect(styles.normal).toEqual({
      fg: null,
      bg: null,
      bold: false,
      italic: false,
    });
    expect(styles.status.modified.bold).toBe(false);
    expect(styles.status.modified.italic).toBe(false);
  });

  test("muted styles keep the panel background", async () => {
    const panelBackground = { Rgb: { r: 10, g: 20, b: 30 } };
    const popupBackground = { Rgb: { r: 40, g: 50, b: 60 } };
    const mutedForeground = { Rgb: { r: 70, g: 80, b: 90 } };
    const styles = plugin.stylesFor({
      theme: {
        style: {
          fg: null,
          bg: panelBackground,
          bold: false,
          italic: false,
        },
        ui_style: {
          muted: {
            fg: mutedForeground,
            bg: popupBackground,
            bold: false,
            italic: false,
          },
        },
      },
    });

    expect(styles.guide.fg).toEqual(mutedForeground);
    expect(styles.guide.bg).toEqual(panelBackground);
    expect(styles.ignored.bg).toEqual(panelBackground);
    expect(styles.status.untracked.bg).toEqual(panelBackground);
  });

  test("builds root, icons, guides, and right git badges", async () => {
    const children = new Map();
    children.set(".", [
      { name: "src", path: "./src", kind: "directory" },
      { name: "README.md", path: "./README.md", kind: "file" },
    ]);
    children.set("./src", [
      { name: "editor", path: "./src/editor", kind: "directory" },
      { name: "editor.rs", path: "./src/editor.rs", kind: "file" },
    ]);

    const rows = plugin.buildNeoTreeRows({
      root: ".",
      cwd: "/Users/fcoury/code/red",
      children,
      expanded: new Set([".", "./src"]),
      statusIndex: {
        root: "/Users/fcoury/code/red",
        entries: [
          {
            absolutePath: "/Users/fcoury/code/red/src/editor.rs",
            status: "modified",
          },
          {
            absolutePath: "/Users/fcoury/code/red/README.md",
            status: "untracked",
          },
        ],
      },
      styles: testStyles(),
    });

    expect(rows[0].segments.map((segment) => segment.text).join("")).toContain(" red");
    expect(rows[1].segments.map((segment) => segment.text).join("")).toContain(" src");
    expect(rows[1].right_segments[0].text).toBe("");
    const nestedDirectoryText = rows[2].segments.map((segment) => segment.text).join("");
    expect(nestedDirectoryText).toContain("  ├ ");
    expect(nestedDirectoryText.includes("│ ├ ")).toBe(false);
    expect(nestedDirectoryText).toContain(" editor");
    const nestedFileText = rows[3].segments.map((segment) => segment.text).join("");
    expect(nestedFileText).toContain("  └ ");
    expect(nestedFileText.includes("│ └ ")).toBe(false);
    expect(nestedFileText).toContain(" editor.rs");
    expect(rows[4].right_segments[0].text).toBe("");
  });

  test("converts current file paths to tree row ids", async () => {
    expect(
      plugin.treePathForFile(
        "/Users/fcoury/code/red/src/editor/rendering.rs",
        "/Users/fcoury/code/red",
      ),
    ).toBe("./src/editor/rendering.rs");
    expect(plugin.treePathForFile("/tmp/other/file.rs", "/Users/fcoury/code/red")).toBe(null);
  });

  test("creates the panel and closes it after opening a file", async (red) => {
    red.setMockState({
      config: {
        ...red.getMockState().config,
        cwd: "/Users/fcoury/code/red",
      },
      theme: {
        ...red.getMockState().theme,
        colors: {
          "gitDecoration.modifiedResourceForeground": { Rgb: { r: 1, g: 2, b: 3 } },
        },
      },
    });
    red.setDirectoryListing(".", [
      { name: "README.md", path: "./README.md", kind: "file" },
    ]);
    red.setGitStatus({
      root: "/Users/fcoury/code/red",
      statuses: [
        {
          path: "README.md",
          absolute_path: "/Users/fcoury/code/red/README.md",
          status: "modified",
        },
      ],
      error: null,
    });

    await red.executeCommand("NeoTree");
    const panel = red.getPanel("neotree");

    expect(panel.config.width).toBe(30);
    expect(panel.rows[1].right_segments[0].text).toBe("");
    expect(panel.rows[1].right_segments[0].style.fg).toEqual({ Rgb: { r: 1, g: 2, b: 3 } });

    await red.emitPanelEvent("neotree", {
      action: "activate",
      row: panel.rows[1],
    });

    expect(red.getLogs()).toContain("openFile: ./README.md");
    expect(red.getLogs()).toContain("closePanel: neotree");
  });

  test("reveals and selects the active file when opening", async (red) => {
    red.setMockState({
      config: {
        ...red.getMockState().config,
        cwd: "/Users/fcoury/code/red",
      },
      windows: [
        {
          ...red.getMockState().windows[0],
          active: true,
          bufferPath: "/Users/fcoury/code/red/src/editor/rendering.rs",
        },
      ],
    });
    red.setDirectoryListing(".", [
      { name: "src", path: "./src", kind: "directory" },
      { name: "README.md", path: "./README.md", kind: "file" },
    ]);
    red.setDirectoryListing("./src", [
      { name: "editor", path: "./src/editor", kind: "directory" },
      { name: "lib.rs", path: "./src/lib.rs", kind: "file" },
    ]);
    red.setDirectoryListing("./src/editor", [
      { name: "rendering.rs", path: "./src/editor/rendering.rs", kind: "file" },
    ]);

    await red.executeCommand("NeoTree");

    const panel = red.getPanel("neotree");
    const selected = panel.rows[panel.selected];
    expect(red.getLogs()).toContain("selectPanelRow: neotree ./src/editor/rendering.rs");
    expect(selected.id).toBe("./src/editor/rendering.rs");
    expect(panel.rows.find((row) => row.id === "./src").expanded).toBe(true);
    expect(panel.rows.find((row) => row.id === "./src/editor").expanded).toBe(true);

    await red.executeCommand("NeoTree");
  });
});
