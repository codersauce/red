# Red plugin examples

The supported plugin example is [`example-plugin/index.hk`](example-plugin/index.hk)
with its [`package.json`](example-plugin/package.json) metadata. Red parses and
typechecks Husk `.hk` plugins against the versioned native host API; see the
[plugin guide](../docs/PLUGIN_SYSTEM.md).

The `.js`, `.ts`, and JavaScript test files in this directory are historical examples
for the removed Deno plugin runtime. They are retained for migration reference, are
not loaded by Red, and should not be used as templates for new plugins.
