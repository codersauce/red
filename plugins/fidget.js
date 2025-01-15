export async function activate(red) {
  const messages = {};
  let lastUpdated = null;
  let doneTtl = 0;
  let position = [0, 0];

  let info = await red.getEditorInfo();
  log("Fidget activated! ", info);

  red.on("lsp:progress", (progress) => {
    log(" *** LSP progress", progress);
    red.drawText(0, 20, progress.token, info.theme.style);
  });
}
