export function activate(red) {
  log("Red is: ", red);

  red.addCommand("MoveTenDown", () => {
    print("Command worked!");
    red.execute("FilePicker");
  });

  red.on("buffer:changed", (buffer) => {
    print(`Buffer has ${buffer.length} bytes`);
  });
}
