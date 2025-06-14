/**
 * Red Editor Plugin API Type Definitions
 * 
 * This file provides TypeScript type definitions for the Red editor plugin API.
 * Plugins can reference this file to get full type safety and IntelliSense support.
 */

declare namespace Red {
  /**
   * Style configuration for text rendering
   */
  interface Style {
    /** Foreground color */
    fg?: string;
    /** Background color */
    bg?: string;
    /** Text modifiers */
    modifiers?: Array<"bold" | "italic" | "underline">;
  }

  /**
   * Information about a buffer
   */
  interface BufferInfo {
    /** Buffer ID */
    id: number;
    /** Buffer name (usually filename) */
    name: string;
    /** Full file path */
    path?: string;
    /** Language ID for syntax highlighting */
    language_id?: string;
  }

  /**
   * Editor information
   */
  interface EditorInfo {
    /** List of open buffers */
    buffers: BufferInfo[];
    /** Index of the currently active buffer */
    current_buffer_index: number;
    /** Editor dimensions */
    size: {
      /** Number of rows */
      rows: number;
      /** Number of columns */
      cols: number;
    };
    /** Current theme information */
    theme: {
      name: string;
      style: Style;
    };
  }

  /**
   * Cursor position
   */
  interface CursorPosition {
    /** Column position */
    x: number;
    /** Line position */
    y: number;
  }

  /**
   * Buffer change event data
   */
  interface BufferChangeEvent {
    /** Buffer ID */
    buffer_id: number;
    /** Buffer name */
    buffer_name: string;
    /** File path */
    file_path?: string;
    /** Total line count */
    line_count: number;
    /** Current cursor position */
    cursor: CursorPosition;
  }

  /**
   * Mode change event data
   */
  interface ModeChangeEvent {
    /** Previous mode */
    from: string;
    /** New mode */
    to: string;
  }

  /**
   * Cursor move event data
   */
  interface CursorMoveEvent {
    /** Previous position */
    from: CursorPosition;
    /** New position */
    to: CursorPosition;
  }

  /**
   * File event data
   */
  interface FileEvent {
    /** Buffer ID */
    buffer_id: number;
    /** File path */
    path: string;
  }

  /**
   * LSP progress event data
   */
  interface LspProgressEvent {
    /** Progress token */
    token: string | number;
    /** Progress kind */
    kind: "begin" | "report" | "end";
    /** Progress title */
    title?: string;
    /** Progress message */
    message?: string;
    /** Progress percentage */
    percentage?: number;
  }

  /**
   * Editor resize event data
   */
  interface ResizeEvent {
    /** New number of rows */
    rows: number;
    /** New number of columns */
    cols: number;
  }

  /**
   * Configuration object
   */
  interface Config {
    /** Current theme name */
    theme: string;
    /** Map of plugin names to paths */
    plugins: Record<string, string>;
    /** Log file path */
    log_file?: string;
    /** Lines to scroll with mouse wheel */
    mouse_scroll_lines?: number;
    /** Whether to show diagnostics */
    show_diagnostics: boolean;
    /** Key binding configuration */
    keys: any; // Complex nested structure
  }

  /**
   * The main Red editor API object passed to plugins
   */
  interface RedAPI {
    /**
     * Register a new command
     * @param name Command name
     * @param callback Command implementation
     */
    addCommand(name: string, callback: () => void | Promise<void>): void;

    /**
     * Subscribe to an editor event
     * @param event Event name
     * @param callback Event handler
     */
    on(event: "buffer:changed", callback: (data: BufferChangeEvent) => void): void;
    on(event: "mode:changed", callback: (data: ModeChangeEvent) => void): void;
    on(event: "cursor:moved", callback: (data: CursorMoveEvent) => void): void;
    on(event: "file:opened", callback: (data: FileEvent) => void): void;
    on(event: "file:saved", callback: (data: FileEvent) => void): void;
    on(event: "lsp:progress", callback: (data: LspProgressEvent) => void): void;
    on(event: "editor:resize", callback: (data: ResizeEvent) => void): void;
    on(event: string, callback: (data: any) => void): void;

    /**
     * Subscribe to an event for one-time execution
     * @param event Event name
     * @param callback Event handler
     */
    once(event: string, callback: (data: any) => void): void;

    /**
     * Unsubscribe from an event
     * @param event Event name
     * @param callback Event handler to remove
     */
    off(event: string, callback: (data: any) => void): void;

    /**
     * Get editor information
     * @returns Promise resolving to editor info
     */
    getEditorInfo(): Promise<EditorInfo>;

    /**
     * Show a picker dialog
     * @param title Dialog title
     * @param values List of options to choose from
     * @returns Promise resolving to selected value or null
     */
    pick(title: string, values: string[]): Promise<string | null>;

    /**
     * Open a buffer by name
     * @param name Buffer name or file path
     */
    openBuffer(name: string): void;

    /**
     * Draw text at specific coordinates
     * @param x Column position
     * @param y Row position
     * @param text Text to draw
     * @param style Optional style configuration
     */
    drawText(x: number, y: number, text: string, style?: Style): void;

    /**
     * Insert text at position
     * @param x Column position
     * @param y Line position
     * @param text Text to insert
     */
    insertText(x: number, y: number, text: string): void;

    /**
     * Delete text at position
     * @param x Column position
     * @param y Line position
     * @param length Number of characters to delete
     */
    deleteText(x: number, y: number, length: number): void;

    /**
     * Replace text at position
     * @param x Column position
     * @param y Line position
     * @param length Number of characters to replace
     * @param text Replacement text
     */
    replaceText(x: number, y: number, length: number, text: string): void;

    /**
     * Get current cursor position
     * @returns Promise resolving to cursor position
     */
    getCursorPosition(): Promise<CursorPosition>;

    /**
     * Set cursor position
     * @param x Column position
     * @param y Line position
     */
    setCursorPosition(x: number, y: number): void;

    /**
     * Get buffer text
     * @param startLine Optional start line (0-indexed)
     * @param endLine Optional end line (exclusive)
     * @returns Promise resolving to buffer text
     */
    getBufferText(startLine?: number, endLine?: number): Promise<string>;

    /**
     * Execute an editor action
     * @param command Action name
     * @param args Optional action arguments
     */
    execute(command: string, args?: any): void;

    /**
     * Get list of available plugin commands
     * @returns Array of command names
     */
    getCommands(): string[];

    /**
     * Get configuration value
     * @param key Optional configuration key
     * @returns Promise resolving to config value or entire config
     */
    getConfig(): Promise<Config>;
    getConfig(key: "theme"): Promise<string>;
    getConfig(key: "plugins"): Promise<Record<string, string>>;
    getConfig(key: "log_file"): Promise<string | undefined>;
    getConfig(key: "mouse_scroll_lines"): Promise<number | undefined>;
    getConfig(key: "show_diagnostics"): Promise<boolean>;
    getConfig(key: "keys"): Promise<any>;
    getConfig(key: string): Promise<any>;

    /**
     * Log messages to the debug log
     * @param messages Messages to log
     */
    log(...messages: any[]): void;

    /**
     * Set a timeout
     * @param callback Function to execute
     * @param delay Delay in milliseconds
     * @returns Timer ID
     */
    setTimeout(callback: () => void, delay: number): Promise<string>;

    /**
     * Clear a timeout
     * @param id Timer ID
     */
    clearTimeout(id: string): Promise<void>;
  }
}

/**
 * Plugin activation function
 * @param red The Red editor API object
 */
export function activate(red: Red.RedAPI): void | Promise<void>;

/**
 * Plugin deactivation function (optional)
 * @param red The Red editor API object
 */
export function deactivate?(red: Red.RedAPI): void | Promise<void>;