/**
 * Timer test plugin - validates that timer callbacks work correctly
 */

export async function activate(red) {
  let testResults = {
    immediateTimer: false,
    delayedTimer: false,
    clearedTimer: false,
    multipleTimers: 0,
    errorInCallback: false
  };

  red.addCommand("TestTimers", async () => {
    red.log("Starting timer tests...");
    
    // Test 1: Immediate timer (0ms delay)
    await red.setTimeout(() => {
      testResults.immediateTimer = true;
      red.log("✓ Immediate timer fired");
    }, 0);
    
    // Test 2: Delayed timer
    const timer1 = await red.setTimeout(() => {
      testResults.delayedTimer = true;
      red.log("✓ Delayed timer fired");
    }, 100);
    red.log(`Scheduled delayed timer with ID: ${timer1}`);
    
    // Test 3: Clear timer before it fires
    const timer2 = await red.setTimeout(() => {
      testResults.clearedTimer = true;
      red.log("✗ This timer should not fire!");
    }, 200);
    await red.clearTimeout(timer2);
    red.log("✓ Timer cleared");
    
    // Test 4: Multiple timers
    for (let i = 0; i < 5; i++) {
      await red.setTimeout(() => {
        testResults.multipleTimers++;
        red.log(`✓ Timer ${i + 1} of 5 fired`);
      }, 50 * (i + 1));
    }
    
    // Test 5: Timer with error in callback
    await red.setTimeout(() => {
      try {
        throw new Error("Test error in timer callback");
      } catch (e) {
        testResults.errorInCallback = true;
        red.log("✓ Error in callback handled gracefully");
      }
    }, 300);
    
    // Check results after all timers should have fired
    await red.setTimeout(() => {
      red.log("\n=== Timer Test Results ===");
      red.log(`Immediate timer: ${testResults.immediateTimer ? '✓' : '✗'}`);
      red.log(`Delayed timer: ${testResults.delayedTimer ? '✓' : '✗'}`);
      red.log(`Cleared timer prevented: ${!testResults.clearedTimer ? '✓' : '✗'}`);
      red.log(`Multiple timers: ${testResults.multipleTimers}/5`);
      red.log(`Error handling: ${testResults.errorInCallback ? '✓' : '✗'}`);
      
      const allPassed = testResults.immediateTimer && 
                       testResults.delayedTimer && 
                       !testResults.clearedTimer && 
                       testResults.multipleTimers === 5 &&
                       testResults.errorInCallback;
                       
      red.log(`\nAll tests passed: ${allPassed ? '✓' : '✗'}`);
    }, 500);
  });
  
  red.addCommand("StressTestTimers", async () => {
    red.log("Starting timer stress test...");
    const startTime = Date.now();
    const timerIds = [];
    
    // Create 100 timers
    for (let i = 0; i < 100; i++) {
      const id = await red.setTimeout(() => {
        red.logDebug(`Timer ${i} fired`);
      }, Math.random() * 1000);
      timerIds.push(id);
    }
    
    red.log(`Created ${timerIds.length} timers`);
    
    // Clear half of them
    for (let i = 0; i < 50; i++) {
      await red.clearTimeout(timerIds[i]);
    }
    
    red.log("Cleared 50 timers");
    
    // Wait for all timers to complete
    await red.setTimeout(() => {
      const elapsed = Date.now() - startTime;
      red.log(`Stress test completed in ${elapsed}ms`);
    }, 1100);
  });
}

export async function deactivate(red) {
  red.log("Timer test plugin deactivated");
}