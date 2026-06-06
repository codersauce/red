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
});
