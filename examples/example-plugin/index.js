/**
 * Example plugin demonstrating metadata usage
 */

export function activate(red) {
    red.logInfo("Example plugin activated!");
    
    red.addCommand("ExampleCommand", async () => {
        const config = await red.getConfig();
        const greeting = config.plugins?.example_plugin?.greeting || "Hello from Example Plugin!";
        
        const info = await red.getEditorInfo();
        red.log(`${greeting} You have ${info.buffers.length} buffers open.`);
        
        // Show plugin list
        const choices = [
            "Show Plugin List",
            "View Logs",
            "Cancel"
        ];
        
        const choice = await red.pick("Example Plugin", choices);
        if (choice === "Show Plugin List") {
            red.execute("ListPlugins");
        } else if (choice === "View Logs") {
            red.viewLogs();
        }
    });
    
    // Example event handlers
    red.on("buffer:changed", (event) => {
        red.logDebug("Buffer changed in example plugin:", event.buffer_name);
    });
    
    red.on("file:saved", (event) => {
        red.logInfo("File saved:", event.path);
    });
}

export function deactivate(red) {
    red.logInfo("Example plugin deactivated!");
}