# Red Editor Plugin Testing Framework

A comprehensive testing framework for Red editor plugins that provides a mock implementation of the Red API and a Jest-like test runner.

## Features

- **Mock Red API**: Complete mock implementation of all plugin APIs
- **Jest-like syntax**: Familiar testing patterns with `describe`, `test`, `expect`
- **Async support**: Full support for testing async operations
- **Event simulation**: Test event handlers and subscriptions
- **State management**: Control and inspect mock editor state

## Installation

The test harness is included with the Red editor. No additional installation required.

## Usage

### Writing Tests

Create a test file for your plugin:

```javascript
// my-plugin.test.js
describe('My Plugin', () => {
  test('should register command', async (red) => {
    expect(red.hasCommand('MyCommand')).toBe(true);
  });
  
  test('should handle buffer changes', async (red) => {
    // Simulate event
    red.emit('buffer:changed', {
      buffer_id: 0,
      buffer_name: 'test.js',
      line_count: 10,
      cursor: { x: 0, y: 0 }
    });
    
    // Check plugin reaction
    expect(red.getLogs()).toContain('log: Buffer changed');
  });
});
```

### Running Tests

```bash
node test-harness/test-runner.js <plugin-file> <test-file>

# Example
node test-harness/test-runner.js my-plugin.js my-plugin.test.js
```

## Test API

### Test Structure

- `describe(name, fn)` - Group related tests
- `test(name, fn)` or `it(name, fn)` - Define a test
- `beforeEach(fn)` - Run before each test
- `afterEach(fn)` - Run after each test
- `beforeAll(fn)` - Run once before all tests
- `afterAll(fn)` - Run once after all tests

### Assertions

- `expect(value).toBe(expected)` - Strict equality check
- `expect(value).toEqual(expected)` - Deep equality check
- `expect(array).toContain(item)` - Array/string contains
- `expect(fn).toHaveBeenCalled()` - Mock function was called
- `expect(fn).toHaveBeenCalledWith(...args)` - Mock called with args

### Mock Red API

The mock API provides all standard plugin methods plus testing utilities:

```javascript
// Test helpers
red.getLogs()           // Get all logged messages
red.clearLogs()         // Clear log history
red.hasCommand(name)    // Check if command exists
red.executeCommand(name, ...args) // Execute a command
red.setMockState(state) // Override mock state
red.getMockState()      // Get current mock state
red.emit(event, data)   // Emit an event
```

### Mock Functions

Create mock functions with Jest-like API:

```javascript
const mockFn = jest.fn();
const mockWithImpl = jest.fn(() => 'return value');

// Use in tests
mockFn('arg1', 'arg2');
expect(mockFn).toHaveBeenCalled();
expect(mockFn).toHaveBeenCalledWith('arg1', 'arg2');
```

## Examples

### Testing Commands

```javascript
test('should execute command successfully', async (red) => {
  await red.executeCommand('MyCommand', 'arg1');
  
  const logs = red.getLogs();
  expect(logs).toContain('execute: MyAction {"param":"arg1"}');
});
```

### Testing Events

```javascript
test('should handle cursor movement', async (red) => {
  // Set initial position
  red.setMockState({ cursor: { x: 0, y: 0 } });
  
  // Move cursor (triggers event)
  red.setCursorPosition(10, 5);
  
  // Wait for async handlers
  await new Promise(resolve => setTimeout(resolve, 10));
  
  // Check results
  const pos = await red.getCursorPosition();
  expect(pos).toEqual({ x: 10, y: 5 });
});
```

### Testing Async Operations

```javascript
test('should handle async operations', async (red) => {
  // Test setTimeout
  let called = false;
  await red.setTimeout(() => { called = true; }, 50);
  
  await new Promise(resolve => setTimeout(resolve, 60));
  expect(called).toBe(true);
  
  // Test async command
  const result = await red.executeCommand('AsyncCommand');
  expect(result).toEqual({ status: 'success' });
});
```

### Testing Buffer Manipulation

```javascript
test('should modify buffer content', async (red) => {
  // Insert text
  red.insertText(0, 0, 'Hello ');
  
  // Get buffer text
  const text = await red.getBufferText();
  expect(text).toContain('Hello ');
  
  // Check event was emitted
  expect(red.getLogs()).toContain('insertText: 0,0 "Hello "');
});
```

## Mock State Structure

The mock maintains the following state:

```javascript
{
  buffers: [{
    id: 0,
    name: "test.js",
    path: "/tmp/test.js",
    language_id: "javascript"
  }],
  current_buffer_index: 0,
  size: { rows: 24, cols: 80 },
  theme: {
    name: "test-theme",
    style: { fg: "#ffffff", bg: "#000000" }
  },
  cursor: { x: 0, y: 0 },
  bufferContent: ["// Test file", "console.log('hello');", ""],
  config: {
    theme: "test-theme",
    plugins: { "test-plugin": "test-plugin.js" },
    log_file: "/tmp/red.log",
    mouse_scroll_lines: 3,
    show_diagnostics: true,
    keys: {}
  }
}
```

## Best Practices

1. **Test in isolation**: Each test should be independent
2. **Use descriptive names**: Test names should explain what they verify
3. **Test edge cases**: Include tests for error conditions
4. **Mock external dependencies**: Use mock functions for external calls
5. **Clean up after tests**: Use afterEach to reset state
6. **Test async code properly**: Always await async operations

## Debugging Tests

- Use `red.log()` in your plugin to debug execution flow
- Check `red.getLogs()` to see all operations performed
- Use `console.log()` in tests for additional debugging
- The test runner shows execution time for performance issues

## Contributing

To improve the testing framework:

1. Add new mock methods to `mock-red.js`
2. Add new assertions to `test-runner.js`
3. Update this documentation
4. Add example tests demonstrating new features