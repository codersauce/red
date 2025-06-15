/**
 * Test suite demonstrating async plugin testing
 */

// Example async plugin for testing
const asyncPlugin = {
  async activate(red) {
    red.addCommand("DelayedGreeting", async () => {
      red.log("Starting delayed greeting...");
      await new Promise(resolve => setTimeout(resolve, 100));
      red.log("Hello after delay!");
    });
    
    red.addCommand("FetchData", async () => {
      // Simulate async data fetching
      const data = await new Promise(resolve => {
        setTimeout(() => resolve({ status: "success", count: 42 }), 50);
      });
      red.log(`Fetched data: ${JSON.stringify(data)}`);
      return data;
    });
    
    // Event handler with async processing
    red.on("buffer:changed", async (event) => {
      await new Promise(resolve => setTimeout(resolve, 10));
      red.log(`Processed buffer change for ${event.buffer_name}`);
    });
  }
};

describe('Async Plugin Tests', () => {
  test('should handle async command execution', async (red) => {
    await asyncPlugin.activate(red);
    
    // Execute async command
    await red.executeCommand('DelayedGreeting');
    
    // Check logs
    const logs = red.getLogs();
    expect(logs).toContain('log: Starting delayed greeting...');
    expect(logs).toContain('log: Hello after delay!');
  });
  
  test('should return data from async commands', async (red) => {
    await asyncPlugin.activate(red);
    
    // Execute command that returns data
    const result = await red.executeCommand('FetchData');
    
    expect(result).toEqual({ status: "success", count: 42 });
    expect(red.getLogs()).toContain('log: Fetched data: {"status":"success","count":42}');
  });
  
  test('should handle async event processing', async (red) => {
    await asyncPlugin.activate(red);
    
    // Emit event
    red.emit('buffer:changed', {
      buffer_id: 0,
      buffer_name: 'async-test.js',
      file_path: '/tmp/async-test.js',
      line_count: 5,
      cursor: { x: 0, y: 0 }
    });
    
    // Wait for async processing
    await new Promise(resolve => setTimeout(resolve, 20));
    
    // Check that event was processed
    expect(red.getLogs()).toContain('log: Processed buffer change for async-test.js');
  });
  
  test('should handle setTimeout/clearTimeout', async (red) => {
    await asyncPlugin.activate(red);
    
    let timerFired = false;
    const timerId = await red.setTimeout(() => {
      timerFired = true;
    }, 50);
    
    // Timer should not have fired yet
    expect(timerFired).toBe(false);
    
    // Wait for timer
    await new Promise(resolve => setTimeout(resolve, 60));
    
    // Timer should have fired
    expect(timerFired).toBe(true);
  });
  
  test('should cancel timers with clearTimeout', async (red) => {
    await asyncPlugin.activate(red);
    
    let timerFired = false;
    const timerId = await red.setTimeout(() => {
      timerFired = true;
    }, 50);
    
    // Cancel timer
    await red.clearTimeout(timerId);
    
    // Wait past timer duration
    await new Promise(resolve => setTimeout(resolve, 60));
    
    // Timer should not have fired
    expect(timerFired).toBe(false);
  });
});

describe('Error Handling', () => {
  test('should handle command errors gracefully', async (red) => {
    const errorPlugin = {
      async activate(red) {
        red.addCommand("FailingCommand", async () => {
          throw new Error("Command failed!");
        });
      }
    };
    
    await errorPlugin.activate(red);
    
    // Execute failing command
    try {
      await red.executeCommand('FailingCommand');
      // Should not reach here
      expect(true).toBe(false);
    } catch (error) {
      expect(error.message).toBe('Command failed!');
    }
  });
  
  test('should handle missing commands', async (red) => {
    try {
      await red.executeCommand('NonExistentCommand');
      // Should not reach here
      expect(true).toBe(false);
    } catch (error) {
      expect(error.message).toBe('Command not found: NonExistentCommand');
    }
  });
});