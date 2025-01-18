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

/**
 * Renders progress messages in the editor
 * @param {Object} red - Renderer object with drawText method
 * @param {Object} info - Editor information
 * @param {[number, number]} info.size - Editor dimensions [width, height]
 * @param {Object} info.theme - Theme configuration
 * @param {Object} info.theme.style - Theme style properties
 * @param {Object} info.theme.colors - Theme colors
 * @param {Map<string, Object>} messages - Map of progress messages
 * @param {number} startY - Starting Y position for rendering
 * @returns {number} Final Y position after rendering
 */
function renderProgress(red, info, messages, startY) {
  log(" ===> renderProgress", startY);
  log("      messages", messages.size);
  let y = startY;

  // Get window dimensions
  const width = info.size[0];
  const height = info.size[1];

  // Clear progress area first
  // for (let i = 0; i < messages.size; i++) {
  //   log("clearing", y + i);
  //   red.drawText(0, y + i, " ".repeat(width), info.theme.style);
  // }

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
  let renderTimer = null;

  log("Fidget activated", info);

  function scheduleRender() {
    if (renderTimer) clearTimeout(renderTimer);
    renderTimer = setTimeout(() => {
      const startY = info.size[1] - messages.size - 2;
      renderProgress(red, info, messages, startY);
    }, Math.floor(1000 / config.pollRate));
  }

  // function render() {
  //   // TODO: use viewport size
  //   const startY = info.size[1] - messages.size - 2;
  //   renderProgress(red, info, messages, startY);
  // }

  red.on("editor:resize", (newSize) => {
    info.size = newSize;
    scheduleRender();
  });

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
      scheduleRender();
    } else if (kind === "report") {
      log("report, setting", token);
      const existing = messages.get(token);
      if (existing) {
        messages.set(token, {
          ...existing,
          value: { ...existing.value, ...progress.value },
        });
        scheduleRender();
      }
    } else if (kind === "end") {
      log("end, setting", token);
      const existing = messages.get(token);
      if (existing) {
        messages.set(token, {
          ...existing,
          value: { ...existing.value, kind: "end" },
        });
        scheduleRender();

        // Remove after delay
        const removeTimer = setTimeout(() => {
          messages.delete(token);
          scheduleRender();
        }, config.doneTtl);

        // messages.delete(token);
        // scheduleRender();

        existing.removeTimer = removeTimer;
      }
    }

    while (messages.size > config.maxMessages) {
      const oldestToken = messages.keys().next().value;
      const oldest = messages.get(oldestToken);
      if (oldest.removeTimer) {
        clearTimeout(oldest.removeTimer);
      }
      messages.delete(oldestToken);
    }
  });

  // Cleanup on deactivate
  return () => {
    if (renderTimer) clearTimeout(renderTimer);
    // Clean up any pending removal timers
    for (const progress of messages.values()) {
      if (progress.removeTimer) {
        clearTimeout(progress.removeTimer);
      }
    }
  };
}
