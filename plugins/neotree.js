const PANEL_ID = "neotree";
const ROOT = ".";

let redApi = null;

// Directory watches live outside a single activation callback so deactivate()
// can clean them up even if the panel was closed by editor shutdown.
const watches = new Map();

export async function activate(red) {
  redApi = red;

  // `expanded` is the source of truth for tree state. `children` caches the
  // last directory listing for each loaded path so rendering can rebuild rows
  // without hitting the filesystem for every visible parent.
  const expanded = new Set([ROOT]);
  const children = new Map();
  let created = false;

  async function loadDirectory(path) {
    const result = await red.listDirectory(path);
    if (result.error) {
      red.logWarn("NeoTree failed to list directory", path, result.error);
      children.set(path, []);
      return [];
    }
    children.set(path, result.entries);
    return result.entries;
  }

  function watchDirectory(path) {
    if (watches.has(path)) return;

    // Watch only directories that have been loaded. When a watched directory
    // changes, refresh its cached listing and rebuild the panel rows.
    const watchId = red.watchDirectory(path, async () => {
      await loadDirectory(path);
      await refresh();
    });
    watches.set(path, watchId);
  }

  async function ensureLoaded(path) {
    if (!children.has(path)) {
      await loadDirectory(path);
    }

    // Loading and watching are tied together: if a directory can contribute
    // rows, changes under it should be reflected in the tree.
    watchDirectory(path);
    return children.get(path) || [];
  }

  async function buildRows(path, depth = 0, rows = []) {
    const entries = await ensureLoaded(path);
    for (const entry of entries) {
      if (entry.kind !== "directory" && entry.kind !== "file") {
        continue;
      }

      // Panel rows are intentionally flat. `depth` lets the renderer indent
      // them, while `expanded` tells it which directory icon/state to show.
      const isDirectory = entry.kind === "directory";
      rows.push({
        id: entry.path,
        label: entry.name,
        path: entry.path,
        depth,
        expanded: isDirectory ? expanded.has(entry.path) : false,
        kind: isDirectory ? "directory" : "file",
      });

      if (isDirectory && expanded.has(entry.path)) {
        await buildRows(entry.path, depth + 1, rows);
      }
    }
    return rows;
  }

  async function refresh() {
    const rows = await buildRows(ROOT);

    // The panel manager owns selection/scroll state; this call only replaces
    // the model rows that should be rendered.
    red.updatePanel(PANEL_ID, rows);
  }

  function stopWatchingDirectories() {
    for (const watchId of watches.values()) {
      red.unwatchDirectory(watchId);
    }
    watches.clear();
  }

  function close() {
    if (!created) return;
    stopWatchingDirectories();
    red.closePanel(PANEL_ID);
    red.focusEditor();
    created = false;
  }

  async function show() {
    if (!created) {
      // Create the panel lazily so the command acts as a lightweight toggle
      // and startup does not allocate UI until the user asks for the tree.
      red.createPanel(PANEL_ID, {
        side: "left",
        width: 32,
        title: "Files",
      });
      created = true;
    }

    await ensureLoaded(ROOT);
    await refresh();
    red.focusPanel(PANEL_ID);
  }

  async function toggleDirectory(path, forceExpand = null) {
    const shouldExpand = forceExpand ?? !expanded.has(path);
    if (shouldExpand) {
      expanded.add(path);
      await ensureLoaded(path);
    } else {
      expanded.delete(path);
    }
    await refresh();
  }

  red.addCommand("NeoTree", async () => {
    if (created) {
      close();
    } else {
      await show();
    }
  });

  red.onPanelEvent(PANEL_ID, async (event) => {
    const row = event.row;
    if (!row) return;

    // `activate` is the generic "open/toggle" action. Direct expand/collapse
    // events keep keyboard-driven tree navigation explicit.
    if (event.action === "activate") {
      if (row.kind === "directory") {
        await toggleDirectory(row.path);
      } else if (row.path) {
        red.openFile(row.path);
        red.focusEditor();
      }
      return;
    }

    if (row.kind === "directory" && event.action === "expand") {
      await toggleDirectory(row.path, true);
    }

    if (row.kind === "directory" && event.action === "collapse") {
      await toggleDirectory(row.path, false);
    }
  });
}

export async function deactivate() {
  if (!redApi) return;

  // Deactivation may happen without the NeoTree command closing the panel, so
  // repeat cleanup here rather than relying only on close().
  for (const watchId of watches.values()) redApi.unwatchDirectory(watchId);
  watches.clear();
  redApi.closePanel(PANEL_ID);
  redApi = null;
}
