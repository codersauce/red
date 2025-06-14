# Red Editor TypeScript Types

TypeScript type definitions for developing plugins for the Red editor.

## Installation

```bash
npm install --save-dev @red-editor/types
```

or

```bash
yarn add -D @red-editor/types
```

## Usage

In your plugin's TypeScript file:

```typescript
/// <reference types="@red-editor/types" />

export async function activate(red: Red.RedAPI) {
    // Your plugin code with full type safety
    red.addCommand("MyCommand", async () => {
        const info = await red.getEditorInfo();
        red.log(`Current buffer: ${info.buffers[info.current_buffer_index].name}`);
    });
}
```

Or with ES modules:

```typescript
import type { RedAPI } from '@red-editor/types';

export async function activate(red: RedAPI) {
    // Your plugin code
}
```

## API Documentation

See the [Plugin System Documentation](../docs/PLUGIN_SYSTEM.md) for detailed API usage.

## Type Coverage

The type definitions include:

- All Red API methods
- Event types with proper typing for event data
- Configuration structure
- Buffer and editor information interfaces
- Style and UI component types

## Contributing

If you find any issues with the type definitions or want to add missing types, please submit a pull request to the main Red editor repository.