/// <reference path="../types/red.d.ts" />

function symbolPrefix(kindName: string): string {
  switch (kindName) {
    case "Class":
      return "[class]";
    case "Method":
      return "[method]";
    case "Function":
      return "[fn]";
    case "Constructor":
      return "[ctor]";
    case "Interface":
      return "[iface]";
    case "Enum":
      return "[enum]";
    case "Struct":
      return "[struct]";
    case "Property":
      return "[prop]";
    case "Field":
      return "[field]";
    case "Variable":
      return "[var]";
    case "Constant":
      return "[const]";
    case "Module":
      return "[mod]";
    default:
      return `[${kindName.toLowerCase()}]`;
  }
}

function symbolLabel(symbol: Red.DocumentSymbol): string {
  const indent = "  ".repeat(symbol.depth);
  const line = symbol.selectionRange.start.line + 1;
  const character = symbol.selectionRange.start.character + 1;
  const detail = symbol.detail ? ` ${symbol.detail}` : "";
  return `${indent}${symbolPrefix(symbol.kindName)} ${symbol.name}${detail} - ${line}:${character}`;
}

function uniqueLabel(base: string, counts: Map<string, number>): string {
  const count = (counts.get(base) || 0) + 1;
  counts.set(base, count);
  return count === 1 ? base : `${base} [${count}]`;
}

export async function activate(red: Red.RedAPI): Promise<void> {
  red.addCommand("LspDocumentSymbols", async () => {
    const result = await red.lsp.documentSymbols();
    if (!result.ok) {
      red.execute("Print", `Document symbols unavailable: ${result.error}`);
      return;
    }

    if (result.symbols.length === 0) {
      red.execute("Print", "No document symbols found");
      return;
    }

    const counts = new Map<string, number>();
    const symbolsByLabel = new Map<string, Red.DocumentSymbol>();
    const labels = result.symbols.map((symbol) => {
      const label = uniqueLabel(symbolLabel(symbol), counts);
      symbolsByLabel.set(label, symbol);
      return label;
    });

    const selected = await red.pick("Document Symbols", labels);
    if (!selected) {
      return;
    }

    const symbol = symbolsByLabel.get(selected);
    if (!symbol) {
      return;
    }

    red.execute("MoveToFilePos", [
      symbol.file,
      symbol.selectionRange.start.character,
      symbol.selectionRange.start.line + 1,
    ]);
  });
}
