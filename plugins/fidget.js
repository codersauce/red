/**
 * Fidget-style progress indicator plugin for Red editor
 * 
 * Displays LSP progress notifications in the editor viewport.
 * Uses the overlay system for flicker-free rendering.
 */

// Styling and configuration
const config = {
  maxMessages: 16,
  progressTtl: Number.POSITIVE_INFINITY,
  doneTtl: 3000,
  overlayId: "fidget-progress",
  icons: {
    progress: "⣾⣽⣻⢿⡿⣟⣯⣷", // Spinner animation frames
    done: "✓"
  }
};

// Progress message formatting
function formatMessage(progress) {
  const msg = progress.value;
  
  // For rust-analyzer, the message already contains the progress info
  if (msg.message) {
    return msg.message;
  }
  
  // Fallback for other LSP servers
  let message = msg.kind === "end" ? "Done" : "Loading...";
  if (msg.percentage != null) {
    message = `${message} (${Math.floor(msg.percentage)}%)`;
  }
  return message;
}

// Extract a clean title from the token
function formatTitle(token) {
  // Convert tokens like "rustAnalyzer/Indexing" to "Indexing"
  const parts = token.split('/');
  const title = parts[parts.length - 1];
  
  // Convert camelCase to space-separated words
  return title
    .replace(/([A-Z])/g, ' $1')
    .replace(/^rust Analyzer/, 'rust-analyzer')
    .trim();
}

/**
 * Updates overlay with progress messages
 * @param {Object} red - Red editor API
 * @param {Object} info - Editor information
 * @param {Map<string, Object>} messages - Map of progress messages
 * @param {string} spinnerIcon - Current spinner icon
 */
function updateOverlay(red, info, messages, spinnerIcon) {
  log("[FIDGET] Updating overlay with", messages.size, "messages");
  
  const lines = [];
  
  // Convert messages to overlay lines (stack from bottom up)
  const messageArray = Array.from(messages.entries()).slice(0, config.maxMessages);
  
  // Process messages and determine which ones are still in progress
  let lastInProgressIndex = -1;
  const processedMessages = [];
  
  for (let i = 0; i < messageArray.length; i++) {
    const [token, progress] = messageArray[i];
    const message = formatMessage(progress);
    const title = formatTitle(token);
    
    // Create display text with proper formatting
    let displayText;
    
    if (progress.value.kind === "end") {
      // For completed tasks, show with done icon on the left
      displayText = `${config.icons.done} ${title}`;
    } else {
      // For in-progress tasks, remember the last one
      lastInProgressIndex = i;
      // Format the message more cleanly
      if (message.includes('/')) {
        // Format like "Indexing: 17/21 (unicode_width)"
        displayText = `${title}: ${message}`;
      } else {
        displayText = `${title}: ${message}`;
      }
    }
    
    processedMessages.push({
      text: displayText,
      isInProgress: progress.value.kind !== "end"
    });
  }
  
  // Convert to overlay lines with spinner on the last in-progress item
  for (let i = 0; i < processedMessages.length; i++) {
    const msg = processedMessages[i];
    let displayText = msg.text;
    
    // Add spinner to the right of the last in-progress message
    if (i === lastInProgressIndex && msg.isInProgress) {
      // Add spacing and spinner at the end
      displayText = `${msg.text} ${spinnerIcon}`;
    }
    
    lines.push({
      text: displayText,
      style: info.theme.style
    });
  }
  
  // Update the overlay
  if (lines.length > 0) {
    red.updateOverlay(config.overlayId, lines);
  } else {
    // Clear overlay when no messages
    red.updateOverlay(config.overlayId, []);
  }
}

export async function activate(red) {
  const info = await red.getEditorInfo();
  const messages = new Map();
  const removeTimers = new Map(); // Track removal timers separately
  let isActive = true; // Track if plugin is still active
  let spinnerFrame = 0; // Track spinner animation frame
  let animationTimer = null;

  log("Fidget activated", info);

  // Create the overlay with bottom-right positioning
  red.createOverlay(config.overlayId, {
    align: "bottom",
    x_padding: 2,
    y_padding: 1,
    relative: "editor"
  });

  // Get current spinner icon based on frame
  function getSpinnerIcon() {
    return config.icons.progress[spinnerFrame % config.icons.progress.length];
  }

  // Update the overlay whenever messages change
  function refreshOverlay() {
    if (!isActive) return;
    updateOverlay(red, info, messages, getSpinnerIcon());
  }

  // Start spinner animation
  async function startAnimation() {
    if (animationTimer || !isActive) return;
    
    const animate = async () => {
      if (!isActive || messages.size === 0) {
        animationTimer = null;
        return;
      }
      
      spinnerFrame++;
      refreshOverlay();
      
      try {
        animationTimer = await red.setTimeout(() => animate(), 100);
      } catch (e) {
        log("Error in animation:", e);
        animationTimer = null;
      }
    };
    
    animate();
  }

  red.on("editor:resize", (newSize) => {
    info.size = newSize;
    // Update overlay on resize
    refreshOverlay();
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
      log("[FIDGET] begin, setting", token);
      messages.set(token, progress);
      refreshOverlay();
      startAnimation();
    } else if (kind === "report") {
      log("[FIDGET] report, setting", token);
      const existing = messages.get(token);
      if (existing) {
        messages.set(token, {
          ...existing,
          value: { ...existing.value, ...progress.value },
        });
        refreshOverlay();
      }
    } else if (kind === "end") {
      log("[FIDGET] end, setting", token);
      const existing = messages.get(token);
      if (existing) {
        messages.set(token, {
          ...existing,
          value: { ...existing.value, kind: "end" },
        });
        refreshOverlay();

        // Remove after delay - handle async timer creation
        Promise.resolve().then(async () => {
          try {
            const timer = await red.setTimeout(() => {
              messages.delete(token);
              removeTimers.delete(token);
              refreshOverlay();
            }, config.doneTtl);
            
            // Clean up any existing timer for this token
            const oldTimer = removeTimers.get(token);
            if (oldTimer) {
              red.clearTimeout(oldTimer).catch(() => {});
            }
            
            removeTimers.set(token, timer);
          } catch (e) {
            log("Error creating removal timer:", e);
          }
        });
      }
    }

    while (messages.size > config.maxMessages) {
      const oldestToken = messages.keys().next().value;
      const oldTimer = removeTimers.get(oldestToken);
      if (oldTimer) {
        red.clearTimeout(oldTimer).catch(() => {});
        removeTimers.delete(oldestToken);
      }
      messages.delete(oldestToken);
    }
  });

  // Initial render if we have messages
  if (messages.size > 0) {
    refreshOverlay();
  }

  // Cleanup on deactivate
  return async () => {
    // Stop the plugin
    isActive = false;
    
    // Stop animation
    if (animationTimer) {
      try {
        await red.clearTimeout(animationTimer);
      } catch (e) {
        log("Error clearing animation timer:", e);
      }
    }
    
    // Clear all removal timers
    for (const [token, timer] of removeTimers.entries()) {
      try {
        await red.clearTimeout(timer);
      } catch (e) {
        log("Error clearing removal timer for", token, ":", e);
      }
    }
    
    removeTimers.clear();
    messages.clear();
    
    // Remove the overlay
    red.removeOverlay(config.overlayId);
  };
}