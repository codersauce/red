# Bundled structural queries

The `.scm` files in this directory were adapted from
[`nvim-treesitter/nvim-treesitter-textobjects`](https://github.com/nvim-treesitter/nvim-treesitter-textobjects)
at commit `851e865342e5a4cb1ae23d31caf6e991e1c99f1e`.

Upstream queries are distributed under the Apache License, Version 2.0. The
complete license is included in [`LICENSE.Apache-2.0`](LICENSE.Apache-2.0).

Red explicitly composes JavaScript, JSX, TypeScript, TSX, and ECMA query
inheritance in `src/highlighter.rs`. C++ and Svelte queries already include their
C and HTML parents, respectively. Bash and Fish Lua predicates were replaced
with standard Tree-sitter predicates; unsupported Fish, Python, and YAML offset
directives were removed. Red synthesizes comment interiors and Fish function
interiors in `src/textobjects.rs`.

First-party language packs published before structural-query support use these
bundled fallbacks without reinstalling. Explicit package or user text-object
queries always take precedence, and incompatible fallbacks leave highlighting
and language-server routing available. `powershell.scm` is maintained directly
by Red because the pinned upstream revision does not supply PowerShell objects.
