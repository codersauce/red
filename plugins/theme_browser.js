export function buildThemePickerModel(themes) {
  const entries = themes
    .map((theme) => {
      if (typeof theme === "string") {
        return { name: theme, file: theme };
      }

      const file = theme.file || theme.name;
      return {
        name: theme.name || file,
        file,
      };
    })
    .filter((theme) => theme.name && theme.file);

  const counts = new Map();
  for (const theme of entries) {
    counts.set(theme.name, (counts.get(theme.name) || 0) + 1);
  }

  const labels = [];
  const filesByLabel = new Map();
  const labelsByFile = new Map();
  for (const theme of entries) {
    const label =
      counts.get(theme.name) > 1 ? `${theme.name} (${theme.file})` : theme.name;
    labels.push(label);
    filesByLabel.set(label, theme.file);
    labelsByFile.set(theme.file, label);
  }

  return { labels, filesByLabel, labelsByFile };
}

export async function activate(red) {
  red.addCommand("ThemeBrowser", async () => {
    const originalTheme = await red.getConfig("theme");
    const themes = red.listThemes();
    const model = buildThemePickerModel(themes);

    if (!model.labels.length) {
      red.execute("Print", "No themes found");
      return;
    }

    const selected = await red.pickLive("Themes", model.labels, {
      initial: model.labelsByFile.get(originalTheme) || originalTheme,
      onChange: (theme) => red.previewTheme(model.filesByLabel.get(theme) || theme),
      onCancel: () => {
        if (originalTheme) {
          red.previewTheme(originalTheme);
        }
      },
    });

    if (selected) {
      red.setTheme(model.filesByLabel.get(selected) || selected);
    }
  });
}
