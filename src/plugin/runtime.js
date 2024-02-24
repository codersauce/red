const { core } = Deno;
const { ops } = core;

const print = (message) => {
  ops.op_trigger_action("Print", message);
};

const log = (...message) => {
  ops.op_log(message);
};

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
}

function execute(command, args) {
  log(`Executing command: ${command} with args: ${args}`);
  log(`Commands: ${JSON.stringify(this.commands)}`);
  const cmd = context.commands[command];
  log(`Command found: ${cmd}`);
  if (cmd) {
    const result = cmd(args);
    log(`Command result: ${result}`);
  } else {
    return `Command not found: ${command}`;
  }
}

globalThis.log = log;
globalThis.print = print;
globalThis.context = new RedContext();
globalThis.execute = execute;
