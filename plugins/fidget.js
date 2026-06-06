/**
 * Fidget-style LSP progress indicator for Red.
 *
 * This mirrors the LSP-progress side of j-hui/fidget.nvim: progress messages
 * are grouped by LSP server, updated in place by token, rendered bottom-up,
 * and completed items linger briefly before disappearing.
 */

export const fidgetDefaults = {
  doneTtl: 3000,
  overlayId: "fidget-progress",
  renderLimit: 16,
  spinnerDelay: 100,
  spinnerFrames: ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
  doneIcon: "✔",
  groupSeparator: "--",
};

function mergeOptions(options = {}) {
  return { ...fidgetDefaults, ...options };
}

function tokenKey(token) {
  return typeof token === "number" ? String(token) : token;
}

function normalizeProgress(progress) {
  if (!progress || progress.token == null) {
    return null;
  }

  const value = progress.value && typeof progress.value === "object"
    ? progress.value
    : progress;
  const kind = progress.kind ?? value.kind;
  if (!["begin", "report", "end"].includes(kind)) {
    return null;
  }

  const lspClient = progress.lspClient ?? progress.lsp_client ?? null;
  return {
    token: tokenKey(progress.token),
    kind,
    title: progress.title ?? value.title,
    message: progress.message ?? value.message,
    percentage: progress.percentage ?? value.percentage,
    cancellable: progress.cancellable ?? value.cancellable ?? false,
    lspClient,
    done: kind === "end",
  };
}

function groupNameFor(progress) {
  const name = progress.lspClient?.name;
  if (!name) {
    return "LSP";
  }
  return name === "rust_analyzer" ? "rust-analyzer" : name;
}

export function formatProgressMessage(progress) {
  let message = progress.message;
  if (!message) {
    message = progress.done ? "Completed" : "In progress...";
  }

  if (!progress.done && progress.percentage != null) {
    const percentage = Number(progress.percentage);
    if (Number.isFinite(percentage)) {
      message = `${message} (${Math.round(percentage)}%)`;
    } else {
      message = `${message} (${progress.percentage})`;
    }
  }

  return message;
}

function getColumns(info) {
  if (Array.isArray(info?.size)) {
    return info.size[0] ?? 80;
  }
  return info?.size?.cols ?? info?.size?.columns ?? 80;
}

function truncateLine(text, width) {
  if (width <= 0) {
    return "";
  }
  if (text.length <= width) {
    return text;
  }
  return text.slice(0, width);
}

function stylesFor(info) {
  const theme = info?.theme ?? {};
  const ui = theme.ui_style ?? theme.uiStyle ?? {};
  const fallback = theme.style ?? {};

  return {
    header: ui.popup_title ?? fallback,
    message: ui.muted ?? fallback,
    done: ui.muted ?? fallback,
    separator: ui.muted ?? fallback,
  };
}

export function createFidgetModel(options = {}) {
  const config = mergeOptions(options);
  const groups = new Map();

  function ensureGroup(groupKey) {
    let group = groups.get(groupKey);
    if (!group) {
      group = {
        key: groupKey,
        name: groupKey,
        items: new Map(),
        order: [],
      };
      groups.set(groupKey, group);
    }
    return group;
  }

  function pruneGroup(groupKey) {
    const group = groups.get(groupKey);
    if (group && group.order.length === 0) {
      groups.delete(groupKey);
    }
  }

  function remove(groupKey, token) {
    const group = groups.get(groupKey);
    if (!group) {
      return false;
    }
    if (!group.items.delete(token)) {
      return false;
    }
    group.order = group.order.filter((candidate) => candidate !== token);
    pruneGroup(groupKey);
    return true;
  }

  function handleProgress(rawProgress) {
    const progress = normalizeProgress(rawProgress);
    if (!progress) {
      return { ignored: true };
    }

    const groupKey = groupNameFor(progress);
    const group = ensureGroup(groupKey);
    let item = group.items.get(progress.token);
    if (!item) {
      item = {
        token: progress.token,
        message: "",
        annote: undefined,
        done: false,
        lastUpdated: Date.now(),
      };
      group.items.set(progress.token, item);
      group.order.push(progress.token);
    }

    item.message = formatProgressMessage(progress);
    item.annote = progress.title ?? item.annote;
    item.done = progress.done;
    item.lastUpdated = Date.now();

    return {
      changed: true,
      done: item.done,
      groupKey,
      token: progress.token,
    };
  }

  function hasActive() {
    for (const group of groups.values()) {
      for (const item of group.items.values()) {
        if (!item.done) {
          return true;
        }
      }
    }
    return false;
  }

  function isEmpty() {
    return groups.size === 0;
  }

  function render(info, spinnerIcon) {
    const chunks = [];
    const styles = stylesFor(info);
    const width = Math.max(0, getColumns(info) - 2);

    let groupIndex = 0;
    for (const group of groups.values()) {
      if (groupIndex > 0 && config.groupSeparator) {
        chunks.push({
          text: config.groupSeparator,
          style: styles.separator,
        });
      }
      groupIndex += 1;

      const icon = hasGroupActive(group) ? spinnerIcon : config.doneIcon;
      chunks.push({
        text: `${group.name} ${icon}`,
        style: styles.header,
      });

      const visibleTokens = group.order.slice(0, config.renderLimit);
      for (const token of visibleTokens) {
        const item = group.items.get(token);
        if (!item) {
          continue;
        }

        const annote = item.annote ? ` ${item.annote}` : "";
        chunks.push({
          text: `${item.message}${annote}`,
          style: item.done ? styles.done : styles.message,
        });
      }
    }

    return chunks.reverse().map((line) => ({
      text: truncateLine(line.text, width),
      style: line.style,
    }));
  }

  function hasGroupActive(group) {
    for (const item of group.items.values()) {
      if (!item.done) {
        return true;
      }
    }
    return false;
  }

  function clear() {
    groups.clear();
  }

  return {
    clear,
    handleProgress,
    hasActive,
    isEmpty,
    remove,
    render,
  };
}

export async function activate(red) {
  const config = mergeOptions();
  const model = createFidgetModel(config);
  const info = await red.getEditorInfo();
  const doneTimers = new Map();
  let active = true;
  let animationTimer = null;
  let spinnerFrame = 0;

  red.createOverlay(config.overlayId, {
    align: "bottom",
    x_padding: 1,
    y_padding: 0,
    relative: "editor",
  });

  function doneTimerKey(groupKey, token) {
    return `${groupKey}\u0000${token}`;
  }

  function spinnerIcon() {
    return config.spinnerFrames[spinnerFrame % config.spinnerFrames.length];
  }

  function refreshOverlay() {
    if (!active) {
      return;
    }
    red.updateOverlay(config.overlayId, model.render(info, spinnerIcon()));
  }

  async function clearDoneTimer(groupKey, token) {
    const key = doneTimerKey(groupKey, token);
    const timer = doneTimers.get(key);
    if (timer) {
      doneTimers.delete(key);
      await red.clearTimeout(timer);
    }
  }

  async function scheduleDoneRemoval(groupKey, token) {
    await clearDoneTimer(groupKey, token);
    const key = doneTimerKey(groupKey, token);
    const timer = await red.setTimeout(() => {
      doneTimers.delete(key);
      model.remove(groupKey, token);
      refreshOverlay();
    }, config.doneTtl);
    doneTimers.set(key, timer);
  }

  async function startAnimation() {
    if (animationTimer || !active || !model.hasActive()) {
      return;
    }

    const animate = async () => {
      if (!active || !model.hasActive()) {
        animationTimer = null;
        refreshOverlay();
        return;
      }

      spinnerFrame += 1;
      refreshOverlay();
      animationTimer = await red.setTimeout(animate, config.spinnerDelay);
    };

    await animate();
  }

  red.on("editor:resize", (newSize) => {
    info.size = newSize;
    refreshOverlay();
  });

  red.on("lsp:progress", async (progress) => {
    const result = model.handleProgress(progress);
    if (result.ignored) {
      return;
    }

    if (result.done) {
      await scheduleDoneRemoval(result.groupKey, result.token);
    } else {
      await clearDoneTimer(result.groupKey, result.token);
    }

    refreshOverlay();
    await startAnimation();
  });

  refreshOverlay();

  return async () => {
    active = false;

    if (animationTimer) {
      await red.clearTimeout(animationTimer);
      animationTimer = null;
    }

    for (const timer of doneTimers.values()) {
      await red.clearTimeout(timer);
    }
    doneTimers.clear();
    model.clear();
    red.removeOverlay(config.overlayId);
  };
}
