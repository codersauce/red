/**
 * Test suite for the buffer picker plugin
 */

describe('BufferPicker Plugin', () => {
  let mockPick;
  
  beforeEach(() => {
    // Reset mock functions
    mockPick = jest.fn();
  });
  
  test('should register BufferPicker command', async (red) => {
    expect(red.hasCommand('BufferPicker')).toBe(true);
  });
  
  test('should show picker with buffer names', async (red) => {
    // Override the pick method to capture calls
    const originalPick = red.pick.bind(red);
    red.pick = mockPick.mockImplementation(() => Promise.resolve(null));
    
    // Execute the command
    await red.executeCommand('BufferPicker');
    
    // Verify picker was called
    expect(mockPick).toHaveBeenCalled();
    expect(mockPick).toHaveBeenCalledWith('Open Buffer', ['test.js']);
    
    // Restore original method
    red.pick = originalPick;
  });
  
  test('should open selected buffer', async (red) => {
    // Mock picker to return a selection
    red.pick = jest.fn().mockImplementation(() => Promise.resolve('selected.js'));
    
    // Execute the command
    await red.executeCommand('BufferPicker');
    
    // Verify buffer was opened
    expect(red.getLogs()).toContain('openBuffer: selected.js');
  });
  
  test('should handle cancelled picker', async (red) => {
    // Mock picker to return null (cancelled)
    red.pick = jest.fn().mockImplementation(() => Promise.resolve(null));
    
    // Execute the command
    await red.executeCommand('BufferPicker');
    
    // Verify no buffer was opened
    const logs = red.getLogs();
    const openBufferLogs = logs.filter(log => log.startsWith('openBuffer:'));
    expect(openBufferLogs.length).toBe(0);
  });
  
  test('should handle multiple buffers', async (red) => {
    // Add more buffers to the mock state
    red.setMockState({
      buffers: [
        { id: 0, name: 'file1.js', path: '/tmp/file1.js', language_id: 'javascript' },
        { id: 1, name: 'file2.ts', path: '/tmp/file2.ts', language_id: 'typescript' },
        { id: 2, name: 'README.md', path: '/tmp/README.md', language_id: 'markdown' }
      ]
    });
    
    // Mock picker
    const originalPick = red.pick.bind(red);
    red.pick = mockPick.mockImplementation(() => Promise.resolve(null));
    
    // Execute the command
    await red.executeCommand('BufferPicker');
    
    // Verify all buffers were shown
    expect(mockPick).toHaveBeenCalledWith('Open Buffer', ['file1.js', 'file2.ts', 'README.md']);
    
    red.pick = originalPick;
  });
});

describe('BufferPicker Event Handling', () => {
  test('should react to buffer changes', async (red) => {
    // Simulate buffer change
    red.emit('buffer:changed', {
      buffer_id: 0,
      buffer_name: 'test.js',
      file_path: '/tmp/test.js',
      line_count: 10,
      cursor: { x: 5, y: 3 }
    });
    
    // Plugin might log or update state
    // This is where you'd test plugin's reaction to events
  });
});