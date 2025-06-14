const { core } = Deno;
const { ops } = core;

const print = (message) => {
  ops.op_trigger_action("Print", message);
};

const log = (...message) => {
  ops.op_log(message);
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

  getCommands() {
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
globalThis.setTimeout = async (callback, delay) => {
  core.ops.op_set_timeout(delay).then(() => callback());
};
globalThis.clearTimeout = async (id) => {
  core.ops.op_clear_timeout(id);
};
