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
}

async function execute(command, args) {
  const cmd = context.commands[command];
  if (cmd) {
    return cmd(args);
  }

  return `Command not found: ${command}`;
}

globalThis.log = log;
globalThis.print = print;
globalThis.context = new RedContext();
globalThis.execute = execute;
