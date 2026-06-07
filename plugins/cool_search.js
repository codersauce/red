const SEARCH_CAUSES = new Set([
  "CommitSearch",
  "FindNext",
  "FindPrevious",
  "RepeatSearch",
  "RepeatSearchOpposite",
  "SearchWordUnderCursor",
]);

export function createCoolSearchController(red) {
  let highlightActive = false;

  const clear = () => {
    if (!highlightActive) return;
    highlightActive = false;
    red.clearSearchHighlight();
  };

  const onSearchHighlighted = () => {
    highlightActive = true;
  };

  const onSearchCleared = () => {
    highlightActive = false;
  };

  const onModeChanged = (event = {}) => {
    const mode = event.to || event.new_mode;
    if (mode === "Insert") clear();
  };

  const onCursorMoved = (event = {}) => {
    if (!highlightActive) return;
    if (event.mode && event.mode !== "Normal") return;
    if (SEARCH_CAUSES.has(event.cause)) return;
    clear();
  };

  const isHighlightActive = () => highlightActive;

  return {
    clear,
    isHighlightActive,
    onCursorMoved,
    onModeChanged,
    onSearchCleared,
    onSearchHighlighted,
  };
}

export function activate(red) {
  const controller = createCoolSearchController(red);

  red.on("search:highlighted", controller.onSearchHighlighted);
  red.on("search:cleared", controller.onSearchCleared);
  red.on("mode:changed", controller.onModeChanged);
  red.on("cursor:moved", controller.onCursorMoved);
}
