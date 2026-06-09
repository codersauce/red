/**
 * Mock implementation of the Red editor API for plugin testing
 */

class MockRedAPI {
  constructor() {
    this.commands = new Map();
    this.eventListeners = new Map();
    this.logs = [];
    this.overlays = new Map();
    this.decorations = new Map();
    this.panels = new Map();
    this.windowBars = new Map();
    this.storageValues = new Map();
    this.spawnedProcesses = [];
    this.openedLocations = [];
    this.pickers = [];
    this.directoryListings = new Map();
    this.directoryWatches = new Map();
    this.gitStatus = { root: null, statuses: [], error: null };
    this.timeouts = new Map();
    this.nextTimeoutId = 1;
    
    // Mock state
    this.mockState = {
      buffers: [
        {
          id: 0,
          name: "test.js",
          path: "/tmp/test.js",
          language_id: "javascript"
        }
      ],
      current_buffer_index: 0,
      size: { rows: 24, cols: 80 },
      theme: {
        name: "test-theme",
        style: { fg: "#ffffff", bg: "#000000" }
      },
      cursor: { x: 0, y: 0 },
      windows: [
        {
          id: 0,
          active: true,
          bufferIndex: 0,
          bufferPath: "/tmp/test.js",
          revision: 0,
          cursor: { x: 0, y: 0 },
          lspPosition: { line: 0, character: 0 }
        }
      ],
      bufferContent: ["// Test file", "console.log('hello');", ""],
      documentSymbols: { ok: true, file: "/tmp/test.js", symbols: [] },
      inlayHints: { ok: true, file: "/tmp/test.js", hints: [] },
      viewportLayout: null,
      config: {
        theme: "test-theme",
        plugins: { "test-plugin": "test-plugin.js" },
        log_file: "/tmp/red.log",
        mouse_scroll_lines: 3,
        show_diagnostics: true,
        keys: {}
      }
    };

    this.lsp = {
      documentSymbols: async (options = {}) => {
        this.logs.push(`lsp.documentSymbols: ${JSON.stringify(options)}`);
        return this.mockState.documentSymbols;
      },
      inlayHints: async (options = {}) => {
        this.logs.push(`lsp.inlayHints: ${JSON.stringify(options)}`);
        return this.mockState.inlayHints;
      }
    };
    this.storage = {
      get: async (key) => this.storageValues.get(key) ?? null,
      set: async (key, value) => this.storageValues.set(key, value),
      delete: async (key) => this.storageValues.delete(key),
    };
    this.ui = {
      createWindowBar: (id, config) => this.createWindowBar(id, config),
      updateWindowBar: (id, windowId, segments) => this.updateWindowBar(id, windowId, segments),
      closeWindowBar: (id, windowId = null) => this.closeWindowBar(id, windowId),
    };
    this.theme = {
      resolveStyle: (spec) => this.resolveThemeStyle(spec),
    };
  }

  // Command registration
  addCommand(name, callback) {
    this.commands.set(name, callback);
  }

  // Event handling
  on(event, callback) {
    if (!this.eventListeners.has(event)) {
      this.eventListeners.set(event, []);
    }
    this.eventListeners.get(event).push(callback);
  }

  once(event, callback) {
    const wrapper = (data) => {
      this.off(event, wrapper);
      callback(data);
    };
    this.on(event, wrapper);
  }

  off(event, callback) {
    const listeners = this.eventListeners.get(event) || [];
    const index = listeners.indexOf(callback);
    if (index !== -1) {
      listeners.splice(index, 1);
    }
  }

  emit(event, data) {
    const listeners = this.eventListeners.get(event) || [];
    listeners.forEach(callback => callback(data));
  }

  async emitAsync(event, data) {
    const listeners = this.eventListeners.get(event) || [];
    await Promise.all(listeners.map(callback => callback(data)));
  }

  // API methods
  async getEditorInfo() {
    return {
      buffers: this.mockState.buffers,
      current_buffer_index: this.mockState.current_buffer_index,
      size: this.mockState.size,
      theme: this.mockState.theme
    };
  }

  async getWindows() {
    return this.mockState.windows;
  }

  async resolveThemeStyle(spec = {}) {
    const theme = this.mockState.theme || {};
    const colors = theme.colors || {};
    const firstColor = (references, fallback) =>
      (references || []).map((key) => colors[key]).find(Boolean) ?? fallback ?? null;
    return {
      fg: firstColor(spec.foreground, theme.style?.fg),
      bg: firstColor(spec.background, theme.style?.bg),
      bold: spec.bold === true,
      italic: spec.italic === true,
    };
  }

  async pick(title, values) {
    // In tests, return the first value by default
    // Can be overridden by test setup
    return values.length > 0 ? values[0] : null;
  }

  createPicker(title, items, options = {}) {
    const picker = {
      title,
      items: [...items],
      query: options.initialQuery || "",
      status: options.status || null,
      preview: options.preview || null,
      closed: false,
    };
    const controller = {
      result: Promise.resolve(null),
      updateItems: (values) => { picker.items = [...values]; },
      updateQuery: (query) => {
        picker.query = query;
        options.onQuery?.(query, controller);
      },
      updateStatus: (status) => { picker.status = status; },
      updatePreview: (preview) => { picker.preview = preview; },
      close: () => {
        picker.closed = true;
        options.onClose?.(null);
      },
    };
    picker.controller = controller;
    picker.options = options;
    this.pickers.push(picker);
    return controller;
  }

  spawnProcess(options) {
    const process = { options, killed: false };
    const handle = {
      id: `process-${this.spawnedProcesses.length + 1}`,
      result: Promise.resolve({ code: 0 }),
      kill: () => { process.killed = true; },
    };
    process.handle = handle;
    this.spawnedProcesses.push(process);
    return handle;
  }

  async openLocation(location, options = {}) {
    this.openedLocations.push({ location, options });
  }

  openBuffer(name) {
    this.logs.push(`openBuffer: ${name}`);
    const existingIndex = this.mockState.buffers.findIndex(b => b.name === name);
    if (existingIndex !== -1) {
      this.mockState.current_buffer_index = existingIndex;
    } else {
      this.mockState.buffers.push({
        id: this.mockState.buffers.length,
        name: name,
        path: `/tmp/${name}`,
        language_id: "text"
      });
      this.mockState.current_buffer_index = this.mockState.buffers.length - 1;
    }
  }

  drawText(x, y, text, style) {
    this.logs.push(`drawText: ${x},${y} "${text}" ${JSON.stringify(style || {})}`);
  }

  insertText(x, y, text) {
    this.logs.push(`insertText: ${x},${y} "${text}"`);
    // Update mock buffer content
    const line = this.mockState.bufferContent[y] || "";
    this.mockState.bufferContent[y] = 
      line.slice(0, x) + text + line.slice(x);
    
    // Emit buffer changed event
    this.emit("buffer:changed", {
      buffer_id: this.mockState.current_buffer_index,
      buffer_name: this.mockState.buffers[this.mockState.current_buffer_index].name,
      file_path: this.mockState.buffers[this.mockState.current_buffer_index].path,
      line_count: this.mockState.bufferContent.length,
      cursor: { x, y }
    });
  }

  deleteText(x, y, length) {
    this.logs.push(`deleteText: ${x},${y} length=${length}`);
    const line = this.mockState.bufferContent[y] || "";
    this.mockState.bufferContent[y] = 
      line.slice(0, x) + line.slice(x + length);
  }

  replaceText(x, y, length, text) {
    this.logs.push(`replaceText: ${x},${y} length=${length} "${text}"`);
    const line = this.mockState.bufferContent[y] || "";
    this.mockState.bufferContent[y] = 
      line.slice(0, x) + text + line.slice(x + length);
  }

  async getCursorPosition() {
    return this.mockState.cursor;
  }

  setCursorPosition(x, y) {
    this.logs.push(`setCursorPosition: ${x},${y}`);
    const oldPos = { ...this.mockState.cursor };
    this.mockState.cursor = { x, y };
    
    // Emit cursor moved event
    this.emit("cursor:moved", {
      from: oldPos,
      to: { x, y },
      x,
      y,
      mode: "Normal",
      cause: "setCursorPosition",
      viewportTop: 0,
      viewport_top: 0,
      bufferIndex: this.mockState.current_buffer_index,
      buffer_index: this.mockState.current_buffer_index
    });
  }

  async getBufferText(startLine, endLine) {
    const start = startLine || 0;
    const end = endLine || this.mockState.bufferContent.length;
    return this.mockState.bufferContent.slice(start, end).join("\n");
  }

  async getViewportLayout() {
    if (this.mockState.viewportLayout) {
      return this.mockState.viewportLayout;
    }

    return {
      bufferIndex: this.mockState.current_buffer_index,
      buffer_index: this.mockState.current_buffer_index,
      windowId: 0,
      window_id: 0,
      width: this.mockState.size.cols,
      height: this.mockState.size.rows,
      contentStart: 4,
      content_start: 4,
      contentWidth: this.mockState.size.cols - 4,
      content_width: this.mockState.size.cols - 4,
      vtop: 0,
      vleft: 0,
      skipcol: 0,
      wrap: true,
      cursor: {
        x: this.mockState.cursor.x,
        y: this.mockState.cursor.y,
        screenRow: this.mockState.cursor.y,
        screen_row: this.mockState.cursor.y
      },
      indentation: {
        shiftWidth: 4,
        shift_width: 4,
        tabWidth: 4,
        tab_width: 4
      },
      lineCount: this.mockState.bufferContent.length,
      line_count: this.mockState.bufferContent.length,
      rows: this.mockState.bufferContent.map((text, line) => ({
        screenRow: line,
        screen_row: line,
        line,
        startCol: 0,
        start_col: 0,
        endCol: text.length,
        end_col: text.length,
        firstSegment: true,
        first_segment: true,
        text
      }))
    };
  }

  setDecorations(namespace, decorations = []) {
    this.logs.push(`setDecorations: ${namespace} ${JSON.stringify(decorations)}`);
    this.decorations.set(namespace, decorations);
  }

  clearDecorations(namespace) {
    this.logs.push(`clearDecorations: ${namespace}`);
    this.decorations.delete(namespace);
  }

  execute(command, args) {
    this.logs.push(`execute: ${command} ${JSON.stringify(args || {})}`);
  }

  clearSearchHighlight() {
    this.execute("ClearSearchHighlight");
  }

  getCommands() {
    return Array.from(this.commands.keys());
  }

  async getConfig(key) {
    if (key) {
      return this.mockState.config[key];
    }
    return this.mockState.config;
  }

  log(...messages) {
    this.logs.push(`log: ${messages.join(" ")}`);
  }

  logDebug(...messages) {
    this.logs.push(`log:debug: ${messages.join(" ")}`);
  }

  logInfo(...messages) {
    this.logs.push(`log:info: ${messages.join(" ")}`);
  }

  logWarn(...messages) {
    this.logs.push(`log:warn: ${messages.join(" ")}`);
  }

  logError(...messages) {
    this.logs.push(`log:error: ${messages.join(" ")}`);
  }

  async setTimeout(callback, delay) {
    const id = `timeout-${this.nextTimeoutId++}`;
    const handle = globalThis.setTimeout(() => {
      this.timeouts.delete(id);
      callback();
    }, delay);
    this.timeouts.set(id, handle);
    return id;
  }

  async clearTimeout(id) {
    const handle = this.timeouts.get(id);
    if (handle) {
      globalThis.clearTimeout(handle);
      this.timeouts.delete(id);
    }
  }

  async setInterval(callback, delay) {
    const id = `interval-${this.nextTimeoutId++}`;
    const handle = globalThis.setInterval(() => {
      callback();
    }, delay);
    this.timeouts.set(id, handle);
    return id;
  }

  async clearInterval(id) {
    const handle = this.timeouts.get(id);
    if (handle) {
      globalThis.clearInterval(handle);
      this.timeouts.delete(id);
    }
  }

  createOverlay(id, config = {}) {
    this.logs.push(`createOverlay: ${id} ${JSON.stringify(config)}`);
    this.overlays.set(id, { config, lines: [] });
  }

  updateOverlay(id, lines) {
    this.logs.push(`updateOverlay: ${id} ${JSON.stringify(lines)}`);
    const overlay = this.overlays.get(id) || { config: {}, lines: [] };
    overlay.lines = lines;
    this.overlays.set(id, overlay);
  }

  removeOverlay(id) {
    this.logs.push(`removeOverlay: ${id}`);
    this.overlays.delete(id);
  }

  getOverlay(id) {
    return this.overlays.get(id);
  }

  getDecorations(namespace) {
    return this.decorations.get(namespace);
  }

  createPanel(id, config = {}) {
    this.logs.push(`createPanel: ${id} ${JSON.stringify(config)}`);
    this.panels.set(id, { config, rows: [], focused: false });
  }

  updatePanel(id, rows) {
    this.logs.push(`updatePanel: ${id} ${JSON.stringify(rows)}`);
    const panel = this.panels.get(id) || { config: {}, rows: [], focused: false };
    panel.rows = rows;
    this.panels.set(id, panel);
  }

  focusPanel(id) {
    this.logs.push(`focusPanel: ${id}`);
    for (const panel of this.panels.values()) {
      panel.focused = false;
    }
    const panel = this.panels.get(id);
    if (panel) panel.focused = true;
  }

  focusEditor() {
    this.logs.push("focusEditor");
    for (const panel of this.panels.values()) {
      panel.focused = false;
    }
  }

  closePanel(id) {
    this.logs.push(`closePanel: ${id}`);
    this.panels.delete(id);
  }

  onPanelEvent(id, callback) {
    this.on(`panel:event:${id}`, callback);
  }

  async listDirectory(path) {
    return this.directoryListings.get(path) || { path, entries: [], error: null };
  }

  async getGitStatus(_path = ".") {
    return this.gitStatus;
  }

  watchDirectory(path, callback) {
    const id = `watch-${this.directoryWatches.size + 1}`;
    this.directoryWatches.set(id, { path, callback });
    return id;
  }

  unwatchDirectory(id) {
    this.directoryWatches.delete(id);
  }

  openFile(path) {
    this.logs.push(`openFile: ${path}`);
  }

  getPanel(id) {
    return this.panels.get(id);
  }

  createWindowBar(id, config = {}) {
    this.logs.push(`createWindowBar: ${id} ${JSON.stringify(config)}`);
    this.windowBars.set(id, { config, windows: new Map() });
  }

  updateWindowBar(id, windowId, segments = []) {
    this.logs.push(`updateWindowBar: ${id} ${windowId} ${JSON.stringify(segments)}`);
    const bar = this.windowBars.get(id) || { config: {}, windows: new Map() };
    bar.windows.set(windowId, segments);
    this.windowBars.set(id, bar);
  }

  closeWindowBar(id, windowId = null) {
    this.logs.push(`closeWindowBar: ${id} ${windowId ?? "all"}`);
    if (windowId == null) {
      this.windowBars.delete(id);
    } else {
      this.windowBars.get(id)?.windows.delete(windowId);
    }
  }

  getWindowBar(id, windowId = null) {
    const bar = this.windowBars.get(id);
    return windowId == null ? bar : bar?.windows.get(windowId);
  }

  async emitWindowBarAction(id, event) {
    const listeners = this.eventListeners.get(`windowBar:action:${id}`) || [];
    await Promise.all(listeners.map((callback) => callback(event)));
  }

  // Test helper methods
  getLogs() {
    return this.logs;
  }

  clearLogs() {
    this.logs = [];
  }

  hasCommand(name) {
    return this.commands.has(name);
  }

  async executeCommand(name, ...args) {
    const command = this.commands.get(name);
    if (command) {
      return await command(...args);
    }
    throw new Error(`Command not found: ${name}`);
  }

  setMockState(state) {
    this.mockState = { ...this.mockState, ...state };
  }

  getMockState() {
    return this.mockState;
  }

  setDirectoryListing(path, entries, error = null) {
    this.directoryListings.set(path, { path, entries, error });
  }

  setGitStatus(status) {
    this.gitStatus = status;
  }

  async emitPanelEvent(id, event) {
    const listeners = this.eventListeners.get(`panel:event:${id}`) || [];
    await Promise.all(listeners.map((callback) => callback(event)));
  }
}

// Export for use in tests
if (typeof module !== 'undefined' && module.exports) {
  module.exports = { MockRedAPI };
}
