/**
 * Demo plugin showing improved logging capabilities
 */

export function activate(red) {
  red.addCommand("LogDemo", async () => {
    red.logDebug("This is a debug message - useful for detailed tracing");
    red.logInfo("This is an info message - general information");
    red.logWarn("This is a warning - something might be wrong");
    red.logError("This is an error - something definitely went wrong");
    
    // Regular log still works (defaults to info level)
    red.log("This is a regular log message");
    
    // Log with multiple arguments
    const data = { count: 42, status: "active" };
    red.logInfo("Processing data:", data);
    
    // Offer to open the log viewer
    const result = await red.pick("Logging Demo Complete", [
      "View Logs",
      "Close"
    ]);
    
    if (result === "View Logs") {
      red.viewLogs();
    }
  });
  
  // Example: Log different levels based on events
  red.on("buffer:changed", (event) => {
    red.logDebug("Buffer changed:", event.buffer_name, "at line", event.cursor.y);
  });
  
  red.on("mode:changed", (event) => {
    red.logInfo(`Mode changed from ${event.from} to ${event.to}`);
  });
  
  red.on("file:saved", (event) => {
    red.logInfo("File saved:", event.path);
  });
  
  // Example: Error handling with proper logging
  red.addCommand("ErrorExample", async () => {
    try {
      // Simulate some operation that might fail
      const result = await someRiskyOperation();
      red.logInfo("Operation succeeded:", result);
    } catch (error) {
      red.logError("Operation failed:", error.message);
      red.logDebug("Full error details:", error.stack);
    }
  });
}

async function someRiskyOperation() {
  // Simulate a 50% chance of failure
  if (Math.random() > 0.5) {
    throw new Error("Random failure occurred");
  }
  return { success: true, value: Math.floor(Math.random() * 100) };
}