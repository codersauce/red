export async function activate(red) {
  red.addCommand("ThemeBrowser", async () => {
    const originalTheme = await red.getConfig("theme");
    const themes = red.listThemes();

    if (!themes.length) {
      red.execute("Print", "No themes found");
      return;
    }

    const selected = await red.pickLive("Themes", themes, {
      initial: originalTheme,
      onChange: (theme) => red.previewTheme(theme),
      onCancel: () => {
        if (originalTheme) {
          red.previewTheme(originalTheme);
        }
      },
    });

    if (selected) {
      red.setTheme(selected);
    }
  });
}
