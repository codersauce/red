/**
 * Simple buffer picker plugin for testing
 */

async function activate(red) {
  red.addCommand("BufferPicker", async () => {
    const info = await red.getEditorInfo();
    const bufferNames = info.buffers.map(b => b.name);
    const selected = await red.pick("Open Buffer", bufferNames);
    
    if (selected) {
      red.openBuffer(selected);
    }
  });
}

module.exports = { activate };