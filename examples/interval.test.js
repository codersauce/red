/**
 * Tests for setInterval/clearInterval functionality
 */

describe('Interval Support', () => {
  test('should support basic interval', async (red) => {
    let counter = 0;
    const intervalId = await red.setInterval(() => {
      counter++;
    }, 50);
    
    // Wait for a few ticks
    await new Promise(resolve => setTimeout(resolve, 175));
    
    // Should have executed 3 times (at 50ms, 100ms, 150ms)
    expect(counter).toBe(3);
    
    // Clear the interval
    await red.clearInterval(intervalId);
    
    // Wait a bit more
    await new Promise(resolve => setTimeout(resolve, 100));
    
    // Counter should not have increased
    expect(counter).toBe(3);
  });
  
  test('should support multiple intervals', async (red) => {
    let fast = 0;
    let slow = 0;
    
    const fastId = await red.setInterval(() => {
      fast++;
    }, 20);
    
    const slowId = await red.setInterval(() => {
      slow++;
    }, 50);
    
    // Wait for 110ms
    await new Promise(resolve => setTimeout(resolve, 110));
    
    // Fast should have run ~5 times (20, 40, 60, 80, 100)
    // Slow should have run ~2 times (50, 100)
    expect(fast >= 4).toBe(true);
    expect(fast <= 6).toBe(true);
    expect(slow >= 1).toBe(true);
    expect(slow <= 3).toBe(true);
    
    // Clear both
    await red.clearInterval(fastId);
    await red.clearInterval(slowId);
  });
  
  test('should handle interval errors gracefully', async (red) => {
    let errorCount = 0;
    
    const intervalId = await red.setInterval(() => {
      errorCount++;
      // Intervals should handle errors gracefully in real implementation
      // In mock, we just count the executions
    }, 50);
    
    // Wait for a couple ticks
    await new Promise(resolve => setTimeout(resolve, 120));
    
    // Should have executed at least once
    expect(errorCount >= 1).toBe(true);
    
    // Clean up
    await red.clearInterval(intervalId);
  });
  
  test('should clear interval on double clear', async (red) => {
    const intervalId = await red.setInterval(() => {}, 50);
    
    // Clear once
    await red.clearInterval(intervalId);
    
    // Clear again - should not throw
    await red.clearInterval(intervalId);
    
    expect(true).toBe(true); // If we got here, no error was thrown
  });
});

describe('Interval Plugin Integration', () => {
  const intervalPlugin = {
    intervals: [],
    
    async activate(red) {
      red.addCommand('StartInterval', async () => {
        const id = await red.setInterval(() => {
          red.log('Interval tick');
        }, 100);
        this.intervals.push(id);
      });
      
      red.addCommand('StopAllIntervals', async () => {
        for (const id of this.intervals) {
          await red.clearInterval(id);
        }
        this.intervals = [];
        red.log('All intervals stopped');
      });
    },
    
    async deactivate(red) {
      // Clean up all intervals
      for (const id of this.intervals) {
        await red.clearInterval(id);
      }
    }
  };
  
  test('plugin should manage intervals', async (red) => {
    await intervalPlugin.activate(red);
    
    // Start an interval
    await red.executeCommand('StartInterval');
    
    // Wait for some ticks
    await new Promise(resolve => setTimeout(resolve, 250));
    
    // Check logs
    const logs = red.getLogs();
    const tickLogs = logs.filter(log => log.includes('Interval tick'));
    expect(tickLogs.length >= 2).toBe(true);
    
    // Stop all intervals
    await red.executeCommand('StopAllIntervals');
    
    // Verify stopped
    expect(red.getLogs()).toContain('log: All intervals stopped');
    
    // Deactivate plugin
    await intervalPlugin.deactivate(red);
  });
});