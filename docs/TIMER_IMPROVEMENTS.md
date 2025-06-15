# Timer Implementation Improvements

This document describes the improvements made to Red's timer system to fix the "Too many timers" error and improve timer management.

## Problem

The original timer implementation had several issues:

1. **No callback execution**: The Rust-side `op_set_timeout` only tracked timer IDs but didn't execute JavaScript callbacks
2. **Missing timer ID returns**: `globalThis.setTimeout` didn't return the timer ID, making it impossible to clear timers
3. **Global timer limit**: A hard limit of 1000 timers shared across all plugins
4. **Rapid timer creation**: Plugins like fidget.js were creating timers on every LSP event without proper cleanup

## Solution

### 1. Timer Callback Execution

Added proper callback mechanism:
- Added `TimeoutCallback` variant to `PluginRequest` enum
- Modified `op_set_timeout` to send callback requests via `ACTION_DISPATCHER`
- Added event listener in JavaScript for "timeout:callback" events
- Timers now properly execute their callbacks when they fire

### 2. Fixed JavaScript Timer API

Updated `globalThis.setTimeout`:
- Now stores callbacks in a mapping
- Returns timer IDs properly
- Callbacks are cleaned up after execution

Updated `globalThis.clearTimeout`:
- Properly cleans up stored callbacks
- Prevents memory leaks

### 3. Timer Statistics and Debugging

Added timer tracking capabilities:
- New `timer_stats` module tracks timer usage per plugin
- `dt` debug command shows active timers and usage statistics
- Helps identify plugins that create excessive timers

### 4. Plugin Improvements

Updated fidget.js plugin:
- Proper debouncing to prevent timer spam
- Separate tracking of render and removal timers
- Comprehensive cleanup on deactivation
- Reduced poll rate to 100ms now that timers work correctly

## Usage

### For Plugin Developers

```javascript
// Timers now work as expected
const timerId = await red.setTimeout(() => {
    console.log("Timer fired!");
}, 1000);

// Can be cleared properly
await red.clearTimeout(timerId);

// Best practices:
// 1. Always store timer IDs
// 2. Clear timers when done
// 3. Clean up all timers in deactivate()
```

### Debugging Timer Issues

Use the `dt` command in command mode to see timer statistics:
```
:dt
```

This will log:
- Active timeouts per plugin
- Active intervals per plugin  
- Total timers created/cleared
- Overall timer usage

## Testing

Added comprehensive tests:
- `test_runtime_timer`: Validates timer scheduling and ID return
- `timer-test.js`: Example plugin demonstrating timer usage
- Stress tests for multiple concurrent timers

## Future Improvements

1. **Per-plugin timer limits**: Instead of a global limit, enforce limits per plugin
2. **Timer pooling**: Reuse timer IDs to reduce allocation overhead
3. **Built-in debouncing**: Provide debounce/throttle utilities
4. **Timer metrics**: Track timer execution times and performance

## Migration Notes

Existing plugins should:
1. Update to use the timer ID return value
2. Ensure proper cleanup in deactivate()
3. Use debouncing for high-frequency events
4. Test with the `dt` command to verify timer usage