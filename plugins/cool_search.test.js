describe("CoolSearch", () => {
  test("arms on search highlight and clears on normal movement", async (red) => {
    const controller = plugin.createCoolSearchController(red);

    controller.onSearchHighlighted();
    controller.onCursorMoved({ mode: "Normal", cause: "MoveToNextWord" });

    expect(red.getLogs()).toContain("execute: ClearSearchHighlight {}");
    expect(controller.isHighlightActive()).toBe(false);
  });

  test("does not clear on search-caused movement", async (red) => {
    const controller = plugin.createCoolSearchController(red);

    controller.onSearchHighlighted();
    controller.onCursorMoved({ mode: "Normal", cause: "RepeatSearch" });

    expect(red.getLogs().length).toBe(0);
    expect(controller.isHighlightActive()).toBe(true);
  });

  test("clears when entering insert mode", async (red) => {
    const controller = plugin.createCoolSearchController(red);

    controller.onSearchHighlighted();
    controller.onModeChanged({ to: "Insert" });

    expect(red.getLogs()).toContain("execute: ClearSearchHighlight {}");
    expect(controller.isHighlightActive()).toBe(false);
  });

  test("ignores repeated clear events while inactive", async (red) => {
    const controller = plugin.createCoolSearchController(red);

    controller.onCursorMoved({ mode: "Normal", cause: "MoveRight" });
    controller.onModeChanged({ to: "Insert" });

    expect(red.getLogs().length).toBe(0);
  });

  test("activation wires editor events", async (red) => {
    red.emit("search:highlighted", { term: "alpha" });
    red.emit("cursor:moved", { mode: "Normal", cause: "MoveRight" });

    expect(red.getLogs()).toContain("execute: ClearSearchHighlight {}");
  });
});
