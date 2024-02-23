const { core } = Deno;
const { ops } = core;

const print = (message) => {
  ops.op_trigger_action("Print", [message]);
};

globalThis.print = print;
