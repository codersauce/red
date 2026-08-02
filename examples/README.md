# Red plugin examples

The supported plugin example is [`example-plugin/index.hk`](example-plugin/index.hk)
with its [`package.json`](example-plugin/package.json) metadata. Red parses and
typechecks Husk `.hk` plugins against the versioned native host API; see the
[plugin guide](../docs/PLUGIN_SYSTEM.md).

The example registers its command and editor-ready handler directly with
`#[red::command(...)]` and `#[red::on(...)]`. External package authors can also
use `#[red::state]`, `#[red::config(...)]`, and `#[red::lifecycle(...)]`; see
the [external plugin guide](../docs/EXTERNAL_PLUGINS.md#husk-entrypoint) and
the [plugin API](../docs/PLUGIN_API.md#declarative-plugin-authoring).

[`external-hello-plugin`](external-hello-plugin/) demonstrates the same
declarative command in a standalone, installable package.

The `.js`, `.ts`, and JavaScript test files in this directory are historical examples
for the removed Deno plugin runtime. They are retained for migration reference, are
not loaded by Red, and should not be used as templates for new plugins.
