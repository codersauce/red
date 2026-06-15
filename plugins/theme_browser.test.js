describe("ThemeBrowser", () => {
  test("opens the theme picker with compact presentation", async (red) => {
    red.setMockState({
      themes: [
        { name: "Mocha", file: "mocha.json" },
        { name: "Kanso Ink", file: "kanso.json" },
      ],
      config: {
        ...red.mockState.config,
        theme: "kanso.json",
      },
    });

    await red.executeCommand("ThemeBrowser");

    expect(red.pickers.length).toBe(1);
    expect(red.pickers[0].title).toBe("Themes");
    expect(red.pickers[0].options.presentation).toBe("compact");
    expect(red.pickers[0].options.initial).toBe("Kanso Ink");
  });
});
