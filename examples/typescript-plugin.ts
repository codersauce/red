/// <reference path="../types/red.d.ts" />

/**
 * Example TypeScript plugin for Red editor
 * Demonstrates type-safe plugin development
 */

interface PluginState {
  lastCursorPosition?: Red.CursorPosition;
  bufferChangeCount: number;
}

const state: PluginState = {
  bufferChangeCount: 0
};

export async function activate(red: Red.RedAPI): Promise<void> {
  red.log("TypeScript plugin activated!");

  // Command with type-safe implementation
  red.addCommand("ShowEditorStats", async () => {
    const info = await red.getEditorInfo();
    const config = await red.getConfig();
    
    const stats = [
      `Open buffers: ${info.buffers.length}`,
      `Current buffer: ${info.buffers[info.current_buffer_index].name}`,
      `Editor size: ${info.size.cols}x${info.size.rows}`,
      `Theme: ${config.theme}`,
      `Buffer changes: ${state.bufferChangeCount}`
    ];

    const selected = await red.pick("Editor Statistics", stats);
    if (selected) {
      red.log("User selected:", selected);
    }
  });

  // Type-safe event handlers
  red.on("buffer:changed", (data: Red.BufferChangeEvent) => {
    state.bufferChangeCount++;
    red.log(`Buffer ${data.buffer_name} changed at line ${data.cursor.y}`);
  });

  red.on("cursor:moved", (data: Red.CursorMoveEvent) => {
    // Demonstrate type safety - TypeScript knows the structure
    if (state.lastCursorPosition) {
      const distance = Math.abs(data.to.x - data.from.x) + Math.abs(data.to.y - data.from.y);
      if (distance > 10) {
        red.log(`Large cursor jump: ${distance} positions`);
      }
    }
    state.lastCursorPosition = data.to;
  });

  red.on("mode:changed", (data: Red.ModeChangeEvent) => {
    red.log(`Mode changed from ${data.from} to ${data.to}`);
  });

  // Advanced example: Smart text manipulation
  red.addCommand("SmartQuotes", async () => {
    const pos = await red.getCursorPosition();
    const line = await red.getBufferText(pos.y, pos.y + 1);
    
    // Find quotes to replace
    const singleQuoteRegex = /'/g;
    const doubleQuoteRegex = /"/g;
    
    let match;
    let replacements: Array<{x: number, length: number, text: string}> = [];
    
    // Process single quotes
    while ((match = singleQuoteRegex.exec(line)) !== null) {
      replacements.push({
        x: match.index,
        length: 1,
        text: match.index === 0 || line[match.index - 1] === ' ' ? ''' : '''
      });
    }
    
    // Process double quotes  
    let quoteCount = 0;
    while ((match = doubleQuoteRegex.exec(line)) !== null) {
      replacements.push({
        x: match.index,
        length: 1,
        text: quoteCount % 2 === 0 ? '"' : '"'
      });
      quoteCount++;
    }
    
    // Apply replacements in reverse order to maintain positions
    replacements.sort((a, b) => b.x - a.x);
    for (const replacement of replacements) {
      red.replaceText(replacement.x, pos.y, replacement.length, replacement.text);
    }
  });

  // Configuration example
  red.addCommand("ShowTheme", async () => {
    const theme = await red.getConfig("theme");
    const allCommands = red.getCommands();
    
    red.log(`Current theme: ${theme}`);
    red.log(`Available plugin commands: ${allCommands.join(", ")}`);
  });
}

export function deactivate(red: Red.RedAPI): void {
  red.log("TypeScript plugin deactivated!");
  // Cleanup would go here
}