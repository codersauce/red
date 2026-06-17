describe("Git integration", () => {
  test("parses branch headers and keeps staged and unstaged states separate", () => {
    const output = [
      "# branch.oid abcdef",
      "# branch.head feature",
      "# branch.upstream origin/feature",
      "# branch.ab +2 -3",
      "# stash 4",
      "1 MM N... 100644 100644 100644 aaaaaaa bbbbbbb src/both.rs",
      "1 .M N... 100644 100644 100644 aaaaaaa bbbbbbb src/work.rs",
      "? new file.txt",
      "",
    ].join("\0");
    const state = plugin.parsePorcelainV2(output);
    expect(state.head).toBe("feature");
    expect(state.upstream).toBe("origin/feature");
    expect(state.ahead).toBe(2);
    expect(state.behind).toBe(3);
    expect(state.stashCount).toBe(4);
    expect(state.staged.map((entry) => entry.path)).toEqual(["src/both.rs"]);
    expect(state.unstaged.map((entry) => entry.path)).toEqual(["src/both.rs", "src/work.rs"]);
    expect(state.untracked[0].path).toBe("new file.txt");
  });

  test("parses rename records with their original path", () => {
    const output = [
      "2 R. N... 100644 100644 100644 aaaaaaa bbbbbbb R100 new name.rs",
      "old name.rs",
      "",
    ].join("\0");
    const state = plugin.parsePorcelainV2(output);
    expect(state.staged[0].path).toBe("new name.rs");
    expect(state.staged[0].originalPath).toBe("old name.rs");
  });

  test("keeps relative buffer paths and rejects absolute paths outside the repository", () => {
    expect(plugin.repositoryPath("README.md", "/work/red")).toBe("README.md");
    expect(plugin.repositoryPath("./src/editor.rs", "/work/red")).toBe("src/editor.rs");
    expect(plugin.repositoryPath("/work/red/src/editor.rs", "/work/red")).toBe("src/editor.rs");
    expect(plugin.repositoryPath("/other/repo/file.rs", "/work/red")).toBe(null);
  });

  test("uses the editor cwd for a bare relative filename", () => {
    expect(plugin.searchDirectoryForBuffer("README.md", "/work/red")).toBe("/work/red");
    expect(plugin.searchDirectoryForBuffer("src/editor.rs", "/work/red")).toBe("src");
    expect(plugin.searchDirectoryForBuffer("/work/red/README.md", "/tmp")).toBe("/work/red");
  });

  test("builds added, modified, and deleted gutter signs", () => {
    const patch = [
      "diff --git a/a b/a",
      "--- a/a",
      "+++ b/a",
      "@@ -1,0 +2,2 @@",
      "+one",
      "+two",
      "@@ -8,2 +10,1 @@",
      "-old",
      "+new",
      "@@ -20,1 +21,0 @@",
      "-gone",
    ].join("\n");
    const signs = plugin.signsFromPatch(patch, 7, false);
    expect(signs.map((sign) => sign.text)).toEqual(["+", "+", "~", "_"]);
    expect(signs.map((sign) => sign.line)).toEqual([1, 2, 9, 20]);
  });

  test("matches Gitsigns top-delete and change-delete classification", () => {
    const patch = [
      "diff --git a/a b/a",
      "--- a/a",
      "+++ b/a",
      "@@ -1,2 +0,0 @@",
      "-one",
      "-two",
      "@@ -5,3 +3,1 @@",
      "-old one",
      "-old two",
      "-old three",
      "+replacement",
    ].join("\n");

    const signs = plugin.signsFromPatch(patch, 7, false);
    expect(signs.map((sign) => ({ line: sign.line, text: sign.text, kind: sign.kind }))).toEqual([
      { line: 0, text: "‾", kind: "topdelete" },
      { line: 2, text: "~", kind: "changedelete" },
    ]);
  });

  test("uses distinct staged glyphs and splits added change tails", () => {
    const patch = [
      "diff --git a/a b/a",
      "--- a/a",
      "+++ b/a",
      "@@ -4,1 +4,3 @@",
      "-old",
      "+new",
      "+extra one",
      "+extra two",
    ].join("\n");

    const signs = plugin.signsFromPatch(patch, 7, true);
    expect(signs.map((sign) => sign.text)).toEqual(["┃", "┃", "┃"]);
    expect(signs.map((sign) => sign.kind)).toEqual(["change", "add", "add"]);
    expect(signs.map((sign) => sign.line)).toEqual([3, 4, 5]);
  });

  test("blends staged sign colors toward the active editor background", () => {
    expect(plugin.blendColor("#a6e3a1", "#1e1e2e")).toBe("#628168");
    expect(plugin.blendColor({ Rgb: { r: 166, g: 227, b: 161 } }, { Rgb: { r: 30, g: 30, b: 46 } })).toBe("#628168");
  });

  test("accepts per-kind sign overrides without dropping defaults", () => {
    expect(plugin.configuredSignGlyphs({ add: "A", delete: "" }, { add: "+", delete: "_" })).toEqual({
      add: "A",
      delete: "_",
    });
  });

  test("extracts only the hunk under the cursor", () => {
    const patch = [
      "diff --git a/a b/a",
      "--- a/a",
      "+++ b/a",
      "@@ -1 +1 @@",
      "-a",
      "+b",
      "@@ -10 +10 @@",
      "-c",
      "+d",
    ].join("\n");
    const selected = plugin.patchForHunk(patch, 9);
    expect(selected).toContain("@@ -10 +10 @@");
    expect(selected.includes("@@ -1 +1 @@")).toBe(false);
    expect(selected).toContain("diff --git a/a b/a");
  });

  test("builds a patch for only the selected changed lines", () => {
    const patch = [
      "diff --git a/a b/a",
      "--- a/a",
      "+++ b/a",
      "@@ -1,3 +1,4 @@",
      " first",
      "-old",
      "+new",
      "+selected",
      " last",
    ].join("\n");
    const selected = plugin.patchForLineRange(patch, 2, 2);
    expect(selected).toContain("+new");
    expect(selected).toContain("+selected");
    expect(selected).toContain("-old");
    expect(selected).toContain("@@ -1,3 +1,4 @@");
  });

  test("safe sync never rewrites a dirty or diverged branch automatically", () => {
    expect(plugin.safeSyncDecision({ upstream: "origin/main", ahead: 0, behind: 1 }, true)).toEqual({ action: "stop", reason: "working tree is dirty" });
    expect(plugin.safeSyncDecision({ upstream: "origin/main", ahead: 2, behind: 1 }, false)).toEqual({ action: "diverged" });
    expect(plugin.safeSyncDecision({ upstream: "origin/main", ahead: 0, behind: 1 }, false)).toEqual({ action: "pull_ff" });
    expect(plugin.safeSyncDecision({ upstream: "origin/main", ahead: 1, behind: 0 }, false)).toEqual({ action: "push" });
  });

  test("registers dashboard and hunk commands", async (red) => {
    for (const command of ["GitDashboard", "GitRefresh", "GitHunkNext", "GitHunkPrevious", "GitHunkStage", "GitHunkUnstage", "GitHunkReset", "GitSubmitMessage", "GitCancelMessage"]) {
      expect(red.commands.has(command)).toBe(true);
    }
  });
});
