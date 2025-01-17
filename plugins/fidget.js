// Styling and configuration
const config = {
  pollRate: 10, // ms
  maxMessages: 16,
  progressTtl: Number.POSITIVE_INFINITY,
  doneTtl: 3000,
  padding: {
    x: 1,
    y: 1,
  },
};

// Progress message formatting
function formatMessage(progress) {
  const msg = progress.value;
  let message = msg.message ||
    (msg.kind === "end" ? "Completed" : "In progress...");

  if (msg.percentage != null) {
    message = `${message} (${Math.floor(msg.percentage)}%)`;
  }
  return message;
}

function renderProgress(red, info, messages, startY) {
  log(" ===> renderProgress", startY);
  log("      messages", messages);
  let y = startY;

  // Get window dimensions
  const height = info.size[0];
  const width = info.size[1];

  // Clear progress area first
  for (let i = 0; i < messages.length + 1; i++) {
    red.drawText(0, y + i, " ".repeat(width), {});
  }

  // Render each progress message
  for (const [_token, progress] of messages.entries()) {
    if (y >= height - 2) break; // Leave space for statusline

    const message = formatMessage(progress);
    const title = progress.value.title;
    // TODO:
    // const style = progress.value.kind === "end"
    //   ? { ...info.theme.style, fg: info.theme.colors.green }
    //   : { ...info.theme.style, fg: info.theme.colors.yellow };

    // Render title + message
    if (title) {
      const msg = `${title}: ${message}`;
      const x = info.size[0] - config.padding.x - msg.length;
      log("render", x, "with title", msg);
      red.drawText(
        x,
        y,
        msg,
        info.theme.style,
      );
    } else {
      const x = info.size[0] - config.padding.x - message.length;
      log("render", x, msg);
      red.drawText(
        x,
        y,
        message,
        style,
      );
    }

    y++;
  }

  return y;
}

export async function activate(red) {
  const info = await red.getEditorInfo();
  const messages = new Map();

  log("Fidget activated", info);

  function render() {
    // TODO: use viewport size
    const startY = info.size[1] - messages.size - 2;
    renderProgress(red, info, messages, startY);
  }

  // Handle LSP progress notifications
  red.on("lsp:progress", (progress) => {
    // {"token":"rustAnalyzer/Indexing","value":{"kind":"report","cancellable":false,"message":"17/21 (unicode_width)","percentage":80}}

    const { token, value: { kind, message, percentage } } = progress;
    log(
      "token:",
      token,
      "kind:",
      kind,
      "message:",
      message,
      "percentage:",
      percentage,
    );

    if (kind === "begin") {
      log("begin, setting", token);
      messages.set(token, progress);
      render();
    } else if (kind === "report") {
      log("report, setting", token);
      const existing = messages.get(token);
      if (existing) {
        messages.set(token, {
          ...existing,
          value: { ...existing.value, ...progress.value },
        });
        render();
      }
    } else if (kind === "end") {
      log("end, setting", token);
      const existing = messages.get(token);
      if (existing) {
        messages.set(token, {
          ...existing,
          value: { ...existing.value, kind: "end" },
        });
        render();

        setTimeout(() => {
          messages.delete(token);
          render();
        }, config.doneTtl);
      }
    }

    while (messages.size > config.maxMessages) {
      const oldestToken = messages.keys().next().value;
      messages.delete(oldestToken);
    }
  });

  // Clean up on deactivate
  return () => {
    // if (renderTimer) clearTimeout(renderTimer);
  };
}
