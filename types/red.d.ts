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

  interface EditorStateSnapshot {
    version: number;
    cwd: string;
    savedAt: number;
    buffers: BufferStateSnapshot[];
    currentBufferIndex: number;
    windowLayout: any;
    selection?: SelectionStateSnapshot | null;
  }

  interface BufferStateSnapshot {
    index: number;
    path: string;
    dirty: boolean;
    cursor: CursorPosition;
    viewportTop: number;
  }

  interface SelectionStateSnapshot {
    kind: "charwise" | "linewise" | "blockwise" | string;
    text: string;
    start: CursorPosition;
    end: CursorPosition;
  }

  interface RestoreResult {
    restored: boolean;
    openedFiles: string[];
    skippedFiles: Array<{ path: string; reason: string }>;
    warnings: string[];
  }

  interface DirectoryEntry {
    name: string;
    path: string;
    kind: "directory" | "file" | "other";
  }

  interface DirectoryListing {
    path: string;
    entries: DirectoryEntry[];
    error?: string | null;
  }

  interface PluginStorage {
    get(key: string): Promise<any>;
    set(key: string, value: any): Promise<void>;
    delete(key: string): Promise<void>;
  }

  interface PluginWindowConfig {
    title?: string;
  }

  interface PluginWindowRenderState {
    kind?: "chat";
    title?: string;
    status?: string;
    transcript?: PluginWindowLine[];
    composer?: PluginWindowLine[];
    composerCursor?: PluginWindowCursor;
    contextPlaceholders?: PluginWindowContextPlaceholder[];
    scroll?: number;
    keyHints?: string[];
  }

  interface PluginWindowLine {
    text: string;
    style?: Style;
  }

  interface PluginWindowCursor {
    line: number;
    column: number;
  }

  interface PluginWindowContextPlaceholder {
    line: number;
    start: number;
    end: number;
    label: string;
  }

  interface PluginWindowKeyEvent {
    plugin: string;
    window: string;
    kind: "key";
    key: string;
    code: string;
    modifiers: string[];
    text?: string;
  }

  interface OverlayConfig {
    align?: "top" | "bottom" | "avoid_cursor";
    x_padding?: number;
    y_padding?: number;
    relative?: string;
  }

  interface OverlayLine {
    text: string;
    style?: Style;
  }

  interface CodexRunTurnParams {
    prompt: string;
    cwd?: string;
    threadId?: string;
    additionalContext?: Record<string, { value: string; kind: "untrusted" | "application" }>;
  }

  interface CodexRunTurnResult {
    thread: any;
    turn: any;
    agentText: string;
    notifications: any[];
  }

  type CodexTurnEvent =
    | { streamId: string; kind: "thread"; thread: any }
    | { streamId: string; kind: "turn"; turn: any }
    | { streamId: string; kind: "notification"; notification: any }
    | { streamId: string; kind: "cancelled" }
    | { streamId: string; kind: "completed"; result: CodexRunTurnResult }
    | { streamId: string; kind: "error"; error: string };

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

  interface PluginCommandMetadata {
    /** Stable command name */
    name?: string;
    /** Plugin name that registered the command */
    owner?: string | null;
    /** Short human-readable command title */
    title?: string;
    /** Optional command grouping for command palettes or config UIs */
    category?: string;
    /** Longer description of what the command does */
    description?: string;
    /** Suggested key bindings, expressed as display strings */
    suggestedKeys?: string[];
    /** Context where this command is most useful */
    context?: string[];
  }

  /**
   * The main Red editor API object passed to plugins
   */
  interface RedAPI {
    storage: PluginStorage;
    /**
     * Register a new command
     * @param name Command name
     * @param callback Command implementation
     * @param metadata Optional command metadata for command palettes and keybinding UIs
     */
    addCommand(
      name: string,
      callback: () => void | Promise<void>,
      metadata?: PluginCommandMetadata,
    ): void;

    /**
     * Return metadata for one plugin command, or all registered plugin commands.
     */
    getCommandMetadata(name: string): PluginCommandMetadata | null;
    getCommandMetadata(): Record<string, PluginCommandMetadata>;
    getCommandsDetailed(): Record<string, PluginCommandMetadata>;

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
     * Get cursor display column (accounts for wide characters)
     * @returns Promise resolving to display column position
     */
    getCursorDisplayColumn(): Promise<number>;

    /**
     * Set cursor position by display column
     * @param column Display column position
     * @param y Line position
     */
    setCursorDisplayColumn(column: number, y: number): void;

    /**
     * Get the display width of a text string
     * @param text Text to measure
     * @returns Promise resolving to display width
     */
    getTextDisplayWidth(text: string): Promise<number>;

    /**
     * Convert character index to display column for a specific line
     * @param x Character index
     * @param y Line number
     * @returns Promise resolving to display column
     */
    charIndexToDisplayColumn(x: number, y: number): Promise<number>;

    /**
     * Convert display column to character index for a specific line
     * @param column Display column
     * @param y Line number
     * @returns Promise resolving to character index
     */
    displayColumnToCharIndex(column: number, y: number): Promise<number>;

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
    getConfig(key: "startup_file_count"): Promise<number>;
    getConfig(key: "cwd"): Promise<string | undefined>;
    getConfig(key: "keys"): Promise<any>;
    getConfig(key: string): Promise<any>;

    getEditorState(): Promise<EditorStateSnapshot>;
    restoreEditorState(snapshot: EditorStateSnapshot): Promise<RestoreResult>;

    /**
     * Send one JSON-RPC request to `codex app-server`.
     *
     * This is the low-level bridge used by Codex-aware plugins while Red's
     * persistent app-server session layer is being built.
     */
    codexAppServerRequest(method: string, params?: any): Promise<any>;

    /**
     * Start or resume a Codex thread and run one user turn to completion.
     */
    codexRunTurn(params: CodexRunTurnParams): Promise<CodexRunTurnResult>;

    /**
     * Start or resume a Codex thread and stream turn events to a callback.
     */
    codexStartTurn(params: CodexRunTurnParams, callback: (event: CodexTurnEvent) => void): string;

    /**
     * Interrupt an active streamed Codex turn.
     */
    codexCancelTurn(streamId: string): boolean;

    /**
     * Log messages to the debug log (info level)
     * @param messages Messages to log
     */
    log(...messages: any[]): void;

    /**
     * Log debug messages
     * @param messages Messages to log
     */
    logDebug(...messages: any[]): void;

    /**
     * Log info messages
     * @param messages Messages to log
     */
    logInfo(...messages: any[]): void;

    /**
     * Log warning messages
     * @param messages Messages to log
     */
    logWarn(...messages: any[]): void;

    /**
     * Log error messages
     * @param messages Messages to log
     */
    logError(...messages: any[]): void;

    /**
     * Open the log viewer in the editor
     */
    viewLogs(): void;

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

    /**
     * Set an interval
     * @param callback Function to execute repeatedly
     * @param delay Delay between executions in milliseconds
     * @returns Interval ID
     */
    setInterval(callback: () => void, delay: number): Promise<string>;

    /**
     * Clear an interval
     * @param id Interval ID
     */
    clearInterval(id: string): Promise<void>;

    createOverlay(id: string, config?: OverlayConfig): void;
    updateOverlay(id: string, lines: OverlayLine[]): void;
    removeOverlay(id: string): void;

    /**
     * Create or reveal a plugin-owned split window.
     * @param id Window ID scoped to the current plugin
     * @param config Optional window configuration
     */
    createPluginWindow(id: string, config?: PluginWindowConfig): void;

    /**
     * Focus a plugin-owned split window.
     * @param id Window ID scoped to the current plugin
     */
    focusPluginWindow(id: string): void;

    /**
     * Replace the render state for a plugin-owned split window.
     * @param id Window ID scoped to the current plugin
     * @param renderState Semantic render state for the window
     */
    updatePluginWindow(id: string, renderState: PluginWindowRenderState): void;

    /**
     * Close a plugin-owned split window.
     * @param id Window ID scoped to the current plugin
     */
    closePluginWindow(id: string): void;

    /**
     * Subscribe to key events routed to a plugin-owned split window.
     * @param id Window ID scoped to the current plugin
     * @param callback Event handler
     */
    onPluginWindowEvent(id: string, callback: (event: PluginWindowKeyEvent) => void): void;

    /**
     * List a directory on the local filesystem.
     */
    listDirectory(path: string): Promise<DirectoryListing>;
  }
}

/**
 * Plugin activation function
 * @param red The Red editor API object
 */
declare function activate(red: Red.RedAPI): void | Promise<void>;

/**
 * Plugin deactivation function (optional)
 * @param red The Red editor API object
 */
declare function deactivate(red: Red.RedAPI): void | Promise<void>;

declare function beforeExit(
  red: Red.RedAPI,
  state: Red.EditorStateSnapshot,
): void | Promise<void>;
