/**
 * Demo plugin showing setInterval/clearInterval usage
 */

let statusUpdateInterval = null;
let clickCounter = 0;
let lastUpdate = new Date();

export async function activate(red) {
  red.logInfo("Interval demo plugin activated!");
  
  // Command that starts a status update interval
  red.addCommand("StartStatusUpdates", async () => {
    if (statusUpdateInterval) {
      red.log("Status updates already running");
      return;
    }
    
    red.log("Starting status updates every 2 seconds...");
    
    statusUpdateInterval = await red.setInterval(() => {
      clickCounter++;
      const now = new Date();
      const elapsed = Math.floor((now - lastUpdate) / 1000);
      
      red.logDebug(`Status update #${clickCounter} - ${elapsed}s since activation`);
      
      // Stop after 10 updates
      if (clickCounter >= 10) {
        red.log("Reached 10 updates, stopping automatically");
        red.clearInterval(statusUpdateInterval);
        statusUpdateInterval = null;
        clickCounter = 0;
      }
    }, 2000);
  });
  
  // Command that stops the status updates
  red.addCommand("StopStatusUpdates", async () => {
    if (!statusUpdateInterval) {
      red.log("No status updates running");
      return;
    }
    
    await red.clearInterval(statusUpdateInterval);
    statusUpdateInterval = null;
    red.log(`Stopped status updates after ${clickCounter} updates`);
    clickCounter = 0;
  });
  
  // Example: Multiple intervals with different frequencies
  red.addCommand("MultipleIntervals", async () => {
    const intervals = [];
    
    // Fast interval (500ms)
    intervals.push(await red.setInterval(() => {
      red.logDebug("Fast interval tick");
    }, 500));
    
    // Medium interval (1s)
    intervals.push(await red.setInterval(() => {
      red.logInfo("Medium interval tick");
    }, 1000));
    
    // Slow interval (3s)
    intervals.push(await red.setInterval(() => {
      red.logWarn("Slow interval tick");
    }, 3000));
    
    red.log("Started 3 intervals with different frequencies");
    
    // Stop all after 10 seconds
    await red.setTimeout(async () => {
      for (const id of intervals) {
        await red.clearInterval(id);
      }
      red.log("Stopped all intervals");
    }, 10000);
  });
  
  // Example: Progress indicator using interval
  red.addCommand("ShowProgress", async () => {
    let progress = 0;
    const total = 20;
    
    const progressInterval = await red.setInterval(() => {
      progress++;
      const bar = "=".repeat(progress) + "-".repeat(total - progress);
      red.log(`Progress: [${bar}] ${Math.floor((progress / total) * 100)}%`);
      
      if (progress >= total) {
        red.clearInterval(progressInterval);
        red.log("Task completed!");
      }
    }, 200);
  });
}

export async function deactivate(red) {
  // Clean up any running intervals
  if (statusUpdateInterval) {
    await red.clearInterval(statusUpdateInterval);
    red.logInfo("Cleaned up status update interval");
  }
  
  red.logInfo("Interval demo plugin deactivated!");
}