const { core } = Deno;
const { ops } = core;

const print = (message) => {
  ops.op_trigger_action("Print", message);
};

const log = (...message) => {
  ops.op_log(null, message);
};

// Log level functions
const logDebug = (...message) => {
  ops.op_log("debug", message);
};

const logInfo = (...message) => {
  ops.op_log("info", message);
};

const logWarn = (...message) => {
  ops.op_log("warn", message);
};

const logError = (...message) => {
  ops.op_log("error", message);
};

let nextReqId = 0;
class RedContext {
  constructor(pluginName = null, root = null) {
    this.pluginName = pluginName;
    this.root = root || this;
    if (!root) {
      this.commands = {};
      this.commandOwners = {};
      this.eventSubscriptions = {};
      this.eventOwners = {};
    }
    this.storage = {
      get: async (key) => ops.op_plugin_storage_get(this.requirePluginName(), key),
      set: async (key, value) => ops.op_plugin_storage_set(this.requirePluginName(), key, value),
      delete: async (key) => ops.op_plugin_storage_delete(this.requirePluginName(), key),
    };
    this.lsp = {
      documentSymbols: () => this.documentSymbols(),
      inlayHints: (options = {}) => this.inlayHints(options),
    };
  }

  requirePluginName() {
    if (!this.pluginName) {
      throw new Error("Plugin storage requires a plugin-specific context");
    }
    return this.pluginName;
  }

  addCommand(name, command) {
    log("Adding command", name, "with function: ", command);
    this.root.commands[name] = command;
    this.root.commandOwners[name] = this.pluginName;
  }

  getCommandList() {
    // Return command names as an array
    return Object.keys(this.root.commands);
  }
  
  getCommandsWithCallbacks() {
    return this.root.commands;
  }

  on(event, callback) {
    log("Subscribing to", event, "with callback: ", callback);
    const subs = this.root.eventSubscriptions[event] || [];
    subs.push(callback);
    this.root.eventSubscriptions[event] = subs;
    const owners = this.root.eventOwners[event] || [];
    owners.push({ callback, pluginName: this.pluginName });
    this.root.eventOwners[event] = owners;
  }

  notify(event, args) {
    const subs = this.root.eventSubscriptions[event] || [];
    if (subs.length > 0) {
      log("Notifying event", event);
    }
    subs.forEach((sub) => sub(args));
  }

  execute(command, args) {
    log(`Command: ${command}`);
    log(`Args: ${args}`);
    ops.op_trigger_action(command, args);
  }

  clearSearchHighlight() {
    this.execute("ClearSearchHighlight");
  }

  getEditorInfo() {
    return new Promise((resolve, _reject) => {
      const reqId = nextReqId++;
      this.on(`editor:info:${reqId}`, (info) => {
        resolve(info, null);
      });
      this.requestEditorInfo(reqId);
    });
  }

  requestEditorInfo(id) {
    ops.op_editor_info(id);
  }

  pick(title, values) {
    return new Promise((resolve, _reject) => {
      const reqId = nextReqId++;
      this.on(`picker:selected:${reqId}`, (selected) => {
        resolve(selected);
      });
      this.openPicker(title, reqId, values);
    });
  }

  pickLive(title, values, options = {}) {
    return new Promise((resolve, _reject) => {
      const reqId = nextReqId++;
      const selectedEvent = `picker:selected:${reqId}`;
      const changedEvent = `picker:changed:${reqId}`;
      const cancelledEvent = `picker:cancelled:${reqId}`;

      const cleanup = () => {
        this.off(selectedEvent, selectedHandler);
        this.off(changedEvent, changedHandler);
        this.off(cancelledEvent, cancelledHandler);
      };
      const selectedHandler = (selected) => {
        cleanup();
        resolve(selected);
      };
      const changedHandler = (selected) => {
        if (options.onChange) {
          options.onChange(selected);
        }
      };
      const cancelledHandler = () => {
        cleanup();
        if (options.onCancel) {
          options.onCancel();
        }
        resolve(null);
      };

      this.on(selectedEvent, selectedHandler);
      this.on(changedEvent, changedHandler);
      this.on(cancelledEvent, cancelledHandler);
      this.openLivePicker(title, reqId, values, options.initial || null);
    });
  }

  openPicker(title, id, values) {
    ops.op_open_picker(title, id, values);
  }

  openLivePicker(title, id, values, initial = null) {
    ops.op_open_live_picker(title, id, values, initial);
  }

  listThemes() {
    return ops.op_list_themes();
  }

  previewTheme(name) {
    this.execute("PreviewTheme", name);
  }

  setTheme(name) {
    this.execute("SetTheme", name);
  }

  openBuffer(name) {
    this.execute("OpenBuffer", name);
  }

  drawText(x, y, text, style) {
    this.execute("BufferText", { x, y, text, style });
  }

  // Buffer manipulation APIs
  insertText(x, y, text) {
    ops.op_buffer_insert(x, y, text);
  }

  deleteText(x, y, length) {
    ops.op_buffer_delete(x, y, length);
  }

  replaceText(x, y, length, text) {
    ops.op_buffer_replace(x, y, length, text);
  }

  getCursorPosition() {
    return new Promise((resolve, _reject) => {
      const handler = (pos) => {
        resolve(pos);
      };
      this.once("cursor:position", handler);
      ops.op_get_cursor_position();
    });
  }

  setCursorPosition(x, y) {
    ops.op_set_cursor_position(x, y);
  }

  getCursorDisplayColumn() {
    return new Promise((resolve, _reject) => {
      const handler = (data) => {
        resolve(data.column);
      };
      this.once("cursor:display_position", handler);
      ops.op_get_cursor_display_column();
    });
  }

  setCursorDisplayColumn(column, y) {
    ops.op_set_cursor_display_column(column, y);
  }

  getTextDisplayWidth(text) {
    return new Promise((resolve, _reject) => {
      const handler = (data) => {
        resolve(data.width);
      };
      this.once("text:display_width", handler);
      ops.op_get_text_display_width(text);
    });
  }

  charIndexToDisplayColumn(x, y) {
    return new Promise((resolve, _reject) => {
      const handler = (data) => {
        resolve(data.column);
      };
      this.once("char:display_column", handler);
      ops.op_char_index_to_display_column(x, y);
    });
  }

  displayColumnToCharIndex(column, y) {
    return new Promise((resolve, _reject) => {
      const handler = (data) => {
        resolve(data.index);
      };
      this.once("display:char_index", handler);
      ops.op_display_column_to_char_index(column, y);
    });
  }

  getBufferText(startLine, endLine) {
    return new Promise((resolve, _reject) => {
      const handler = (data) => {
        resolve(data.text);
      };
      this.once("buffer:text", handler);
      ops.op_get_buffer_text(startLine, endLine);
    });
  }

  getViewportLayout() {
    return new Promise((resolve, _reject) => {
      const reqId = nextReqId++;
      this.once(`viewport:layout:${reqId}`, (layout) => {
        resolve(layout);
      });
      ops.op_get_viewport_layout(reqId);
    });
  }

  setDecorations(namespace, decorations) {
    ops.op_set_decorations(namespace, decorations || []);
  }

  clearDecorations(namespace) {
    ops.op_clear_decorations(namespace);
  }

  documentSymbols() {
    return new Promise((resolve, _reject) => {
      const reqId = nextReqId++;
      this.once(`lsp:document_symbols:${reqId}`, (result) => {
        resolve(result);
      });
      ops.op_lsp_document_symbols(reqId);
    });
  }

  async inlayHints(options = {}) {
    const requestOptions = { ...(options || {}) };
    if (requestOptions.visible && !requestOptions.range) {
      const layout = await this.getViewportLayout();
      const rows = Array.isArray(layout?.rows) ? layout.rows : [];
      if (rows.length > 0) {
        const startLine = rows.reduce(
          (line, row) => Math.min(line, row.line ?? line),
          rows[0].line ?? 0,
        );
        const endLine = rows.reduce(
          (line, row) => Math.max(line, row.line ?? line),
          rows[0].line ?? 0,
        );
        requestOptions.range = {
          start: { line: startLine, character: 0 },
          end: { line: endLine + 1, character: 0 },
        };
      }
    }
    delete requestOptions.visible;

    return new Promise((resolve, _reject) => {
      const reqId = nextReqId++;
      this.once(`lsp:inlay_hints:${reqId}`, (result) => {
        resolve(result);
      });
      ops.op_lsp_inlay_hints(reqId, requestOptions);
    });
  }

  // Helper method for one-time event listeners
  once(event, callback) {
    const wrapper = (data) => {
      this.off(event, wrapper);
      callback(data);
    };
    this.on(event, wrapper);
  }

  // Method to remove event listeners
  off(event, callback) {
    const subs = this.root.eventSubscriptions[event] || [];
    this.root.eventSubscriptions[event] = subs.filter(sub => sub !== callback);
    const owners = this.root.eventOwners[event] || [];
    this.root.eventOwners[event] = owners.filter(owner => owner.callback !== callback);
  }
  
  // Get list of available commands
  getCommands() {
    // Return plugin commands synchronously
    // In the future, we could make this async to fetch built-in commands too
    return this.getCommandList();
  }

  // Get configuration values
  getConfig(key) {
    return new Promise((resolve, _reject) => {
      const handler = (data) => {
        resolve(data.value);
      };
      this.once("config:value", handler);
      ops.op_get_config(key);
    });
  }

  getEditorState() {
    return new Promise((resolve, _reject) => {
      const reqId = nextReqId++;
      this.once(`editor:state:${reqId}`, (state) => {
        resolve(state);
      });
      ops.op_get_editor_state(reqId);
    });
  }

  restoreEditorState(snapshot) {
    return new Promise((resolve, _reject) => {
      const reqId = nextReqId++;
      this.once(`editor:restore:${reqId}`, (result) => {
        resolve(result);
      });
      ops.op_restore_editor_state(reqId, snapshot);
    });
  }

  // Logging with levels
  log(...messages) {
    log(...messages);
  }

  logDebug(...messages) {
    logDebug(...messages);
  }

  logInfo(...messages) {
    logInfo(...messages);
  }

  logWarn(...messages) {
    logWarn(...messages);
  }

  logError(...messages) {
    logError(...messages);
  }

  // View logs in editor
  viewLogs() {
    ops.op_trigger_action("ViewLogs");
  }

  // Timer functions
  async setInterval(callback, delay) {
    return await globalThis.setInterval(callback, delay);
  }

  async clearInterval(id) {
    return await globalThis.clearInterval(id);
  }

  async setTimeout(callback, delay) {
    return await globalThis.setTimeout(callback, delay);
  }

  async clearTimeout(id) {
    return await globalThis.clearTimeout(id);
  }

  // Overlay API
  createOverlay(id, config = {}) {
    ops.op_create_overlay(id, config);
  }

  updateOverlay(id, lines) {
    ops.op_update_overlay(id, lines);
  }

  removeOverlay(id) {
    ops.op_remove_overlay(id);
  }

  // Persistent panel API
  createPanel(id, config = {}) {
    ops.op_create_panel(id, config);
  }

  updatePanel(id, rows) {
    ops.op_update_panel(id, rows);
  }

  focusPanel(id) {
    ops.op_focus_panel(id);
  }

  focusEditor() {
    ops.op_focus_editor();
  }

  closePanel(id) {
    ops.op_close_panel(id);
  }

  onPanelEvent(id, callback) {
    this.on(`panel:event:${id}`, callback);
  }

  listDirectory(path) {
    return new Promise((resolve, _reject) => {
      const reqId = nextReqId++;
      this.once(`filesystem:directory:${reqId}`, (result) => {
        resolve(result);
      });
      ops.op_list_directory(path, reqId);
    });
  }

  getGitStatus(path = ".") {
    return new Promise((resolve, _reject) => {
      const reqId = nextReqId++;
      this.once(`git:status:${reqId}`, (result) => {
        resolve(result);
      });
      ops.op_get_git_status(path, reqId);
    });
  }

  watchDirectory(path, callback) {
    const watchId = nextReqId++;
    this.on(`filesystem:changed:${watchId}`, callback);
    ops.op_watch_directory(path, watchId);
    return watchId;
  }

  unwatchDirectory(watchId) {
    ops.op_unwatch_directory(watchId);
  }

  openFile(path) {
    this.execute("OpenFile", path);
  }
}

async function execute(command, args) {
  const cmd = context.commands[command];
  if (cmd) {
    try {
      return await cmd(args);
    } catch (error) {
      log(`Error executing command ${command}:`, error);
      throw error;
    }
  }

  return `Command not found: ${command}`;
}

globalThis.log = log;
globalThis.print = print;
globalThis.context = new RedContext();
globalThis.createPluginContext = (pluginName) => new RedContext(pluginName, globalThis.context);
globalThis.execute = execute;

// Timer functions
let intervalCallbacks = {};
let intervalIdToCallbackId = {};
let timeoutCallbacks = {};
let callbackIdCounter = 0;

globalThis.setTimeout = async (callback, delay) => {
  try {
    const timerId = await core.ops.op_set_timeout(delay);
    log(`[TIMER] Created timeout ${timerId} with delay ${delay}ms`);
    // Store the callback to execute when timer fires
    timeoutCallbacks[timerId] = callback;
    return timerId;
  } catch (error) {
    log("[TIMER] Error creating timeout:", error);
    throw error;
  }
};

globalThis.clearTimeout = async (id) => {
  await core.ops.op_clear_timeout(id);
  // Clean up the callback
  delete timeoutCallbacks[id];
};

globalThis.setInterval = async (callback, delay) => {
  // Generate a unique callback ID
  const callbackId = `interval_cb_${callbackIdCounter++}`;
  
  // Store the callback
  intervalCallbacks[callbackId] = callback;
  
  // Register for interval callbacks and get the interval ID
  const intervalId = await ops.op_set_interval(delay, callbackId);
  
  // Map interval ID to callback ID
  intervalIdToCallbackId[intervalId] = callbackId;
  
  return intervalId;
};

globalThis.clearInterval = async (id) => {
  // Clear the interval
  await ops.op_clear_interval(id);
  
  // Clean up our mappings
  const callbackId = intervalIdToCallbackId[id];
  if (callbackId) {
    delete intervalCallbacks[callbackId];
    delete intervalIdToCallbackId[id];
  }
};

// Listen for interval callbacks
globalThis.context.on("interval:callback", async (data) => {
  const intervalId = data.intervalId;
  
  try {
    // Get the callback ID from the interval ID
    const callbackId = await ops.op_get_interval_callback_id(intervalId);
    
    // Look up and execute the callback
    const callback = intervalCallbacks[callbackId];
    if (callback) {
      try {
        callback();
      } catch (error) {
        log("Error in interval callback:", error);
      }
    }
  } catch (error) {
    // Interval might have been cleared
    log("Failed to get interval callback:", error);
  }
});

// Listen for timeout callbacks
globalThis.context.on("timeout:callback", (data) => {
  const timerId = data.timerId;
  log("[TIMER] Received timeout callback for timer:", timerId);
  
  // Look up and execute the callback
  const callback = timeoutCallbacks[timerId];
  if (callback) {
    log("[TIMER] Executing callback for timer:", timerId);
    // Clean up the callback before executing
    delete timeoutCallbacks[timerId];
    
    try {
      callback();
    } catch (error) {
      log("[TIMER] Error in timeout callback:", error);
    }
  } else {
    log("[TIMER] No callback found for timer:", timerId);
  }
});
