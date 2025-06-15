/**
 * Mock implementation of the Red editor API for plugin testing
 */

class MockRedAPI {
  constructor() {
    this.commands = new Map();
    this.eventListeners = new Map();
    this.logs = [];
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
      bufferContent: ["// Test file", "console.log('hello');", ""],
      config: {
        theme: "test-theme",
        plugins: { "test-plugin": "test-plugin.js" },
        log_file: "/tmp/red.log",
        mouse_scroll_lines: 3,
        show_diagnostics: true,
        keys: {}
      }
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

  // API methods
  async getEditorInfo() {
    return {
      buffers: this.mockState.buffers,
      current_buffer_index: this.mockState.current_buffer_index,
      size: this.mockState.size,
      theme: this.mockState.theme
    };
  }

  async pick(title, values) {
    // In tests, return the first value by default
    // Can be overridden by test setup
    return values.length > 0 ? values[0] : null;
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
      to: { x, y }
    });
  }

  async getBufferText(startLine, endLine) {
    const start = startLine || 0;
    const end = endLine || this.mockState.bufferContent.length;
    return this.mockState.bufferContent.slice(start, end).join("\n");
  }

  execute(command, args) {
    this.logs.push(`execute: ${command} ${JSON.stringify(args || {})}`);
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
}

// Export for use in tests
if (typeof module !== 'undefined' && module.exports) {
  module.exports = { MockRedAPI };
}