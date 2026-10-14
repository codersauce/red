# TypeScript + ESLint LSP fixture

This fixture distills issue #382 from
`DoctorRyner/eslint-playground@acf163662c2c33255bda1e5da46691f5051195e6`.
It intentionally routes the same TypeScript document to two language servers
while keeping Prettier as the external formatter.

The compatibility baseline recorded on 2026-10-14 is:

- `vscode-langservers-extracted` 4.8.0
- `typescript-language-server` 6.0.1
- TypeScript 6.0.3
- Prettier 3.8.3

The expected result is TypeScript completion, hover, navigation, and symbols
together with ESLint's `@typescript-eslint/no-unused-vars` diagnostic and code
actions. Formatting must continue to use Prettier.
