const STORAGE_KEY = "latest";

let redApi = null;

export async function activate(red) {
  redApi = red;

  red.on("editor:ready", async () => {
    try {
      const startupFileCount = await red.getConfig("startup_file_count");
      if (startupFileCount > 0) {
        return;
      }

      const snapshot = await red.storage.get(STORAGE_KEY);
      if (!snapshot || snapshot.version !== 1) {
        return;
      }

      const cwd = await red.getConfig("cwd");
      if (snapshot.cwd && cwd && snapshot.cwd !== cwd) {
        red.logInfo("Session restore skipped: saved cwd differs from current cwd");
        return;
      }

      const result = await red.restoreEditorState(snapshot);
      if (!result.restored) {
        red.logWarn("Session restore did not restore files", result.warnings);
      }
      for (const skipped of result.skippedFiles || []) {
        red.logWarn("Session restore skipped file", skipped.path, skipped.reason);
      }
    } catch (error) {
      red.logError("Session restore failed", error?.message || error);
    }
  });
}

export async function beforeExit(red, state) {
  const api = red || redApi;
  if (!api || !state) return;

  const cleanState = {
    ...state,
    buffers: (state.buffers || []).filter((buffer) => buffer.path && !buffer.dirty),
  };

  await api.storage.set(STORAGE_KEY, cleanState);
}

export function deactivate() {
  redApi = null;
}
