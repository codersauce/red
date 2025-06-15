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
  constructor() {
    this.commands = {};
    this.eventSubscriptions = {};
  }

  addCommand(name, command) {
    log("Adding command", name, "with function: ", command);
    this.commands[name] = command;
  }

  getCommandList() {
    // Return command names as an array
    return Object.keys(this.commands);
  }
  
  getCommandsWithCallbacks() {
    return this.commands;
  }

  on(event, callback) {
    log("Subscribing to", event, "with callback: ", callback);
    const subs = this.eventSubscriptions[event] || [];
    subs.push(callback);
    this.eventSubscriptions[event] = subs;
  }

  notify(event, args) {
    const subs = this.eventSubscriptions[event] || [];
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

  openPicker(title, id, values) {
    ops.op_open_picker(title, id, values);
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

  getBufferText(startLine, endLine) {
    return new Promise((resolve, _reject) => {
      const handler = (data) => {
        resolve(data.text);
      };
      this.once("buffer:text", handler);
      ops.op_get_buffer_text(startLine, endLine);
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
    const subs = this.eventSubscriptions[event] || [];
    this.eventSubscriptions[event] = subs.filter(sub => sub !== callback);
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
