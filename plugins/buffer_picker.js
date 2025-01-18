export async function activate(red) {
  // let buffers = [];
  //
  // red.on("bufferList:change", (action, buf) => {
  //   if (action === "add") {
  //     buffers.push(buf);
  //   } else if (action === "remove") {
  //     buffers = buffers.filter((b) => b !== buf);
  //   } else if (action === "rename") {
  //     const idx = buffers.indexOf(buf.oldName);
  //     buffers[idx] = buf.newName;
  //   }
  // });

  red.addCommand(
    "BufferPicker",
    async () => {
      const info = await red.getEditorInfo();
      const buffers = info.buffers.map((buf) => buf.name);

      red.openBuffer(await red.pick("Buffers", buffers));
    },
    // {
    //   defaultKeymap: [" ", "b"],
    // },
  );
}
