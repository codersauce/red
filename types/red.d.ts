export {};

/**
 * Red Editor Plugin API Type Definitions
 * 
 * This file provides TypeScript type definitions for the Red editor plugin API.
 * Plugins can reference this file to get full type safety and IntelliSense support.
 */

declare global {
namespace Red {
  type Color =
    | string
    | { Rgb: { r: number; g: number; b: number } }
    | { Rgba: { r: number; g: number; b: number; a: number } };

  /**
   * Style configuration for text rendering
   */
  interface Style {
    /** Foreground color */
    fg?: Color | null;
    /** Background color */
    bg?: Color | null;
    bold?: boolean;
    italic?: boolean;
    /** Text modifiers */
    modifiers?: Array<"bold" | "italic" | "underline">;
  }

  /**
   * A style resolved from the active theme. Entries are tried from left to
   * right. Prefix a TextMate scope with `scope:`, for example
   * `scope:entity.name.function`.
   */
  interface ThemeStyleSpec {
    foreground?: string[];
    background?: string[];
    bold?: boolean;
    italic?: boolean;
  }

  interface ThemeAPI {
    resolveStyle(spec: ThemeStyleSpec): Promise<Style>;
  }

  interface WindowBounds {
    x: number;
    y: number;
    width: number;
    height: number;
  }

  interface WindowViewport {
    top: number;
    left?: number;
  }

  interface WindowContext {
    id: number;
    active: boolean;
    bounds: WindowBounds;
    contentBounds: WindowBounds;
    bufferIndex: number;
    bufferPath?: string;
    revision: number;
    cursor: CursorPosition;
    /** Cursor position using the UTF-16 column encoding expected by LSP. */
    lspPosition: Position;
    viewport: WindowViewport;
  }

  interface WindowChangeEvent {
    windowId: number | null;
    bufferIndex: number;
    cause: string;
  }

  type WindowBarEdge = "top";
  type WindowBarOverflow = "truncate_left" | "truncate_right";

  interface WindowBarConfig {
    edge?: WindowBarEdge;
    priority?: number;
    overflow?: WindowBarOverflow;
    truncateMarker?: string;
    style?: WindowBarSegmentStyle;
  }

  interface WindowBarSegmentStyle {
    semantic?: string | ThemeStyleSpec;
    style?: Style;
  }

  interface WindowBarSegment {
    id?: string;
    text: string;
    style?: WindowBarSegmentStyle;
    tooltip?: string;
    action?: string;
  }

  interface WindowBarAPI {
    createWindowBar(id: string, config?: WindowBarConfig): void;
    updateWindowBar(id: string, windowId: number, segments: WindowBarSegment[]): void;
    closeWindowBar(id: string, windowId?: number | null): void;
  }

  type PanelSide = "left" | "right";
  type PanelRowKind = "file" | "directory";

  interface PanelConfig {
    side?: PanelSide;
    width?: number;
    title?: string | null;
  }

  interface PanelSegment {
    text: string;
    style?: Style | null;
  }

  interface PanelRow {
    id: string;
    path?: string | null;
    expanded?: boolean | null;
    kind: PanelRowKind;
    segments?: PanelSegment[];
    right_segments?: PanelSegment[];
  }

  interface PanelEvent {
    panel_id: string;
    action: string;
    selected_index: number;
    row?: PanelRow | null;
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
      colors?: Record<string, Color>;
      gutter_style?: Style;
      gutterStyle?: Style;
      ui_style?: Record<string, Color>;
      uiStyle?: Record<string, Color>;
    };
  }

  interface EditorStateSnapshot {
    version: number;
    cwd: string;
    savedAt: number;
    buffers: BufferStateSnapshot[];
    currentBufferIndex: number;
    windowLayout: any;
  }

  interface BufferStateSnapshot {
    index: number;
    path: string;
    dirty: boolean;
    cursor: CursorPosition;
    viewportTop: number;
  }

  interface RestoreResult {
    restored: boolean;
    openedFiles: string[];
    skippedFiles: Array<{ path: string; reason: string }>;
    warnings: string[];
  }

  interface PluginStorage {
    get(key: string): Promise<any>;
    set(key: string, value: any): Promise<void>;
    delete(key: string): Promise<void>;
  }

  interface PickerItem<T = any> {
    /** Stable identity used to preserve selection across item updates. */
    id: string;
    /** Primary text shown in the result list and used for local filtering. */
    label: string;
    /** Optional LSP symbol kind used to derive a semantic theme color. */
    kind?: string;
    /** Optional text rendered immediately after the label, such as `:line:column`. */
    annotation?: string;
    /** Optional secondary text shown after the label. */
    detail?: string;
    /** Plugin-owned payload returned unchanged in picker events. */
    data?: T;
    /** Byte-independent character ranges in label text to highlight. */
    matches?: Array<[start: number, end: number]>;
    /** Byte-independent character ranges in detail text to highlight. */
    detailMatches?: Array<[start: number, end: number]>;
    /** Preview shown while this item is selected. */
    preview?: PickerPreview;
  }

  type PickerPreview =
    | { text: string; language?: string }
    | {
        path: string;
        line?: number;
        column?: number;
        /** UTF-8 byte ranges to highlight on the focused line. */
        matches?: Array<[start: number, end: number]>;
      };

  interface PickerKeyAction {
    /** Key name such as `c-o`, `alt-enter`, `tab`, or `f2`. */
    key: string;
    /** Stable action identifier delivered to onAction. */
    action: string;
    label?: string;
  }

  interface PickerOptions<T = any> {
    /** Disable Red's fuzzy filter so the plugin can provide query results. */
    externalFilter?: boolean;
    placeholder?: string;
    initialQuery?: string;
    /** Initially selected item id. */
    initialSelection?: string;
    status?: string;
    actions?: PickerKeyAction[];
    preview?: PickerPreview;
    onQuery?: (query: string, picker: PickerController<T>) => void | Promise<void>;
    onChange?: (item: PickerItem<T> | null) => void;
    onSelection?: (item: PickerItem<T> | null) => void;
    onSelect?: (item: PickerItem<T>) => void;
    onCancel?: () => void;
    onClose?: (item: PickerItem<T> | null) => void;
    onAction?: (
      action: string,
      item: PickerItem<T> | null,
      query: string,
      picker: PickerController<T>,
    ) => void | Promise<void>;
  }

  interface PickerController<T = any> {
    readonly id: number;
    readonly result: Promise<PickerItem<T> | null>;
    updateItems(items: PickerItem<T>[]): void;
    updateQuery(query: string): void;
    updateStatus(status: string | null): void;
    updatePreview(preview: PickerPreview | null): void;
    close(): void;
  }

  interface ProcessResult {
    /** Exit status, or null when the process was terminated by a signal. */
    code: number | null;
    error?: string;
  }

  interface ProcessOptions {
    /** Exact executable name or path allowed by this plugin's config entry. */
    command: string;
    args?: string[];
    cwd?: string | null;
    onStdout?: (line: string) => void;
    onStderr?: (line: string) => void;
    onExit?: (result: ProcessResult) => void;
    onError?: (message: string) => void;
  }

  interface ProcessHandle {
    readonly id: string;
    readonly result: Promise<ProcessResult>;
    kill(): void;
  }

  interface Location {
    path: string;
    /** Zero-based line. */
    line: number;
    /** Zero-based UTF-8 byte offset within the line. */
    column: number;
    columnEncoding?: "utf8-byte" | "utf-16";
  }

  type OpenLocationTarget = "current" | "horizontal" | "vertical";

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
    /** Previous mode, retained for compatibility */
    old_mode?: string;
    /** New mode, retained for compatibility */
    new_mode?: string;
    /** Editor action or event that caused the transition */
    cause?: string;
  }

  /**
   * Cursor move event data
   */
  interface CursorMoveEvent {
    /** Previous position */
    from: CursorPosition;
    /** New position */
    to: CursorPosition;
    /** New column position */
    x: number;
    /** New line position */
    y: number;
    /** Editor mode after the move */
    mode: string;
    /** Editor action or event that caused the move */
    cause: string;
    /** Viewport top after the move */
    viewportTop: number;
    /** Viewport top, retained for compatibility */
    viewport_top?: number;
    /** Active buffer index after the move */
    bufferIndex: number;
    /** Active buffer index, retained for compatibility */
    buffer_index?: number;
  }

  interface SearchHighlightEvent {
    /** Current search term */
    term: string;
    /** Search direction */
    direction: "Forward" | "Backward" | string;
    /** Editor action that activated the highlights */
    source: string;
  }

  interface SearchClearedEvent {
    /** Search term that was cleared */
    term: string;
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
    /** Raw LSP WorkDoneProgress value */
    value: {
      kind: "begin" | "report" | "end";
      title?: string;
      message?: string;
      percentage?: number;
      cancellable?: boolean;
      [key: string]: any;
    };
    /** Progress kind */
    kind: "begin" | "report" | "end";
    /** Progress title */
    title?: string;
    /** Progress message */
    message?: string;
    /** Progress percentage */
    percentage?: number;
    /** Whether the LSP task says it can be cancelled */
    cancellable?: boolean;
    /** LSP client metadata attached by Red */
    lspClient?: {
      name: string;
      workspaceRoot: string;
    };
  }

  interface Position {
    line: number;
    character: number;
  }

  interface Range {
    start: Position;
    end: Position;
  }

  interface DocumentSymbol {
    id?: string;
    parentId?: string;
    name: string;
    detail?: string;
    kind: number;
    kindName: string;
    file: string;
    range: Range;
    selectionRange: Range;
    depth: number;
  }

  type DocumentSymbolsResult =
    | {
        ok: true;
        file: string;
        bufferIndex?: number;
        revision?: number;
        symbols: DocumentSymbol[];
      }
    | {
        ok: false;
        error: string;
      };

  type WorkspaceSymbolsResult =
    | {
        ok: true;
        symbols: DocumentSymbol[];
      }
    | {
        ok: false;
        error: string;
      };

  interface FileLocation {
    file: string;
    range: Range;
  }

  type ReferencesResult =
    | {
        ok: true;
        file: string;
        position: Position;
        references: FileLocation[];
      }
    | {
        ok: false;
        error: string;
      };

  interface ReferencesOptions {
    includeDeclaration?: boolean;
  }

  type InlayHintLabel = string | InlayHintLabelPart[];

  interface InlayHintLabelPart {
    value: string;
    tooltip?: string | { kind: "plaintext" | "markdown"; value: string };
    location?: {
      uri: string;
      range: Range;
    };
    command?: {
      title: string;
      command: string;
      arguments?: any[];
    };
  }

  interface InlayHint {
    position: Position;
    label: InlayHintLabel;
    /** LSP InlayHintKind: 1 = type, 2 = parameter */
    kind?: number;
    paddingLeft?: boolean;
    paddingRight?: boolean;
    tooltip?: string | { kind: "plaintext" | "markdown"; value: string };
    data?: any;
  }

  type InlayHintsResult =
    | {
        ok: true;
        file: string;
        hints: InlayHint[];
      }
    | {
        ok: false;
        error: string;
      };

  interface InlayHintsOptions {
    range?: Range;
    visible?: boolean;
  }

  interface DocumentSymbolsOptions {
    bufferIndex?: number;
  }

  interface LspAPI {
    documentSymbols(options?: DocumentSymbolsOptions): Promise<DocumentSymbolsResult>;
    workspaceSymbols(query?: string): Promise<WorkspaceSymbolsResult>;
    references(options?: ReferencesOptions): Promise<ReferencesResult>;
    inlayHints(options?: InlayHintsOptions): Promise<InlayHintsResult>;
  }

  interface ViewportRow {
    screenRow: number;
    screen_row: number;
    line: number;
    startCol: number;
    start_col: number;
    endCol: number;
    end_col: number;
    firstSegment: boolean;
    first_segment: boolean;
    text: string;
  }

  interface ViewportLayout {
    bufferIndex: number;
    buffer_index: number;
    contentWidth: number;
    content_width: number;
    rows: ViewportRow[];
  }

  type DecorationAnchor = "column" | "eol" | "right_align";

  interface Decoration {
    buffer_index?: number;
    bufferIndex?: number;
    line: number;
    column?: number;
    anchor?: DecorationAnchor;
    text: string;
    style?: Style;
    priority?: number;
    repeat_linebreak?: boolean;
    repeatLinebreak?: boolean;
    only_whitespace?: boolean;
    onlyWhitespace?: boolean;
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
    /** Plugin-owned namespaced settings. */
    plugin_config?: Record<string, any>;
    plugin_permissions?: Record<string, { process?: string[] }>;
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
    storage: PluginStorage;
    lsp: LspAPI;
    theme: ThemeAPI;
    ui: WindowBarAPI;
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
    on(event: "editor:ready", callback: (data: Record<string, never>) => void): void;
    on(
      event: "editor:stateRestored",
      callback: (data: { windows: WindowContext[]; cause: "RestoreEditorState" }) => void,
    ): void;
    on(event: "buffer:changed", callback: (data: BufferChangeEvent) => void): void;
    on(event: "mode:changed", callback: (data: ModeChangeEvent) => void): void;
    on(event: "cursor:moved", callback: (data: CursorMoveEvent) => void): void;
    on(event: "search:highlighted", callback: (data: SearchHighlightEvent) => void): void;
    on(event: "search:cleared", callback: (data: SearchClearedEvent) => void): void;
    on(event: "file:opened", callback: (data: FileEvent) => void): void;
    on(event: "file:saved", callback: (data: FileEvent) => void): void;
    on(event: "lsp:progress", callback: (data: LspProgressEvent) => void): void;
    on(event: "editor:resize", callback: (data: ResizeEvent) => void): void;
    on(event: "window:focused", callback: (data: WindowChangeEvent) => void): void;
    on(event: "window:layoutChanged", callback: (data: { windows: WindowContext[] }) => void): void;
    on(event: "window:bufferChanged", callback: (data: WindowChangeEvent) => void): void;
    on(event: "window:closed", callback: (data: { windowId: number }) => void): void;
    on(event: "theme:changed", callback: (data: { name: string }) => void): void;
    on(
      event: `windowBar:action:${string}`,
      callback: (data: { windowId: number; segmentId?: string; action: string }) => void,
    ): void;
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
     * Clear visible search highlights until the next successful search
     */
    clearSearchHighlight(): void;

    /**
     * Get editor information
     * @returns Promise resolving to editor info
     */
    getEditorInfo(): Promise<EditorInfo>;

    /** Return session-stable identities and render context for open windows. */
    getWindows(): Promise<WindowContext[]>;

    /** Resolve an ordered semantic style against the active theme. */
    resolveThemeStyle(spec: ThemeStyleSpec): Promise<Style>;

    createWindowBar(id: string, config?: WindowBarConfig): void;
    updateWindowBar(id: string, windowId: number, segments: WindowBarSegment[]): void;
    /** Omit windowId to remove the bar from every window. */
    closeWindowBar(id: string, windowId?: number | null): void;

    createPanel(id: string, config?: PanelConfig): void;
    updatePanel(id: string, rows: PanelRow[]): void;
    selectPanelRow(id: string, rowId: string): void;
    focusPanel(id: string): void;
    focusEditor(): void;
    closePanel(id: string): void;
    onPanelEvent(id: string, callback: (event: PanelEvent) => void): void;

    /**
     * Show a picker dialog
     * @param title Dialog title
     * @param values List of options to choose from
     * @returns Promise resolving to selected value or null
     */
    pick(title: string, values: string[]): Promise<string | null>;

    /** Show the legacy string picker with selection and cancellation callbacks. */
    pickLive(
      title: string,
      values: string[],
      options?: {
        initial?: string;
        onChange?: (value: string) => void;
        onCancel?: () => void;
      },
    ): Promise<string | null>;

    /** Open a structured picker and return a handle for incremental updates. */
    createPicker<T = any>(
      title: string,
      items: PickerItem<T>[],
      options?: PickerOptions<T>,
    ): PickerController<T>;

    /** Open a structured picker and wait for selection or cancellation. */
    pickDynamic<T = any>(
      title: string,
      items: PickerItem<T>[],
      options?: PickerOptions<T>,
    ): Promise<PickerItem<T> | null>;

    /** Launch an allowlisted executable without invoking a shell. */
    spawnProcess(options: ProcessOptions): ProcessHandle;

    /** Open a zero-based location and add the previous position to jump history. */
    openLocation(location: Location, options?: { target?: OpenLocationTarget }): void;

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
     * Get visible window rows for decoration plugins.
     */
    getViewportLayout(): Promise<ViewportLayout>;

    /**
     * Replace all persistent virtual text decorations in a namespace.
     */
    setDecorations(namespace: string, decorations: Decoration[]): void;

    /**
     * Clear persistent virtual text decorations in a namespace.
     */
    clearDecorations(namespace: string): void;

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
    getConfig(key: "plugin_config"): Promise<Record<string, any>>;
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
  }
}
}

export type RedAPI = Red.RedAPI;

/**
 * Plugin activation function
 * @param red The Red editor API object
 */
export function activate(red: Red.RedAPI): void | Promise<void>;

/**
 * Plugin deactivation function (optional)
 * @param red The Red editor API object
 */
export function deactivate(red: Red.RedAPI): void | Promise<void>;

export function beforeExit(
  red: Red.RedAPI,
  state: Red.EditorStateSnapshot,
): void | Promise<void>;
