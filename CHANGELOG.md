# Changelog

All notable changes to Red are documented in this file.

## [0.7.0](https://github.com/codersauce/red/compare/v0.6.0...v0.7.0)

### Features

- **release:** Sharpen next release announcements ([#358](https://github.com/codersauce/red/issues/358)) ([b7c49b2](https://github.com/codersauce/red/commit/b7c49b2af3f0edadd3212e2fa2d09d71a38694db))
- **tree:** Move reusable tree rendering into rust ([#357](https://github.com/codersauce/red/issues/357)) ([e6ccc99](https://github.com/codersauce/red/commit/e6ccc991f93dcae0cc2a3142dba0099ceadcda44))
- **release:** Curate cross-channel release campaigns ([#350](https://github.com/codersauce/red/issues/350)) ([afc256a](https://github.com/codersauce/red/commit/afc256a6178c709657ca85ef176fb27dd4319bbd))
- **agent:** Add lsp diagnostics and semantic rename tools ([#345](https://github.com/codersauce/red/issues/345)) ([b9fc1ff](https://github.com/codersauce/red/commit/b9fc1ffb91fe76cd4d66f3579109fa904c538aac))
- **commands:** Add vim-style shell range filters ([#340](https://github.com/codersauce/red/issues/340)) ([7a4b344](https://github.com/codersauce/red/commit/7a4b3448607704b5d5ce5816e4f95536c11ccd32))
- **picker:** Support file line navigation ([#339](https://github.com/codersauce/red/issues/339)) ([9fcf795](https://github.com/codersauce/red/commit/9fcf795854b29312fd12429fa57a1be586cf2c2a))
- **commands:** Add vim-style shell execution ([#338](https://github.com/codersauce/red/issues/338)) ([f5cff19](https://github.com/codersauce/red/commit/f5cff1992c87dd65f230d9f8d4ac39c0565b7899))
- **git:** Browse committed files in dashboard ([#337](https://github.com/codersauce/red/issues/337)) ([4cfc5ba](https://github.com/codersauce/red/commit/4cfc5badffdf672fe0461e8a4cd443707e48b91c))
- **editor:** Format pasted text by default ([#334](https://github.com/codersauce/red/issues/334)) ([73bd541](https://github.com/codersauce/red/commit/73bd541cbc4370fb7bb9a5943218e7133f6893e4))
- **picker:** Add configurable symbol tree guides ([#333](https://github.com/codersauce/red/issues/333)) ([def39e6](https://github.com/codersauce/red/commit/def39e64ba5431866ebb9c0371c5650c8acbf77d))
- **editor:** Detect and resolve external file conflicts ([#330](https://github.com/codersauce/red/issues/330)) ([0c27121](https://github.com/codersauce/red/commit/0c27121ca5702bcf5931ac01948db52fd1c8a999))
- **editor:** Add vim-style multi-cursor editing ([#326](https://github.com/codersauce/red/issues/326)) ([94f71e7](https://github.com/codersauce/red/commit/94f71e7449beb1dac2bb45d3c18ef4d9aae03235))
- **editor:** Warn at bracket navigation boundaries ([#322](https://github.com/codersauce/red/issues/322)) ([b65a427](https://github.com/codersauce/red/commit/b65a427f89bed87356593e3beca0f4a3859ef5c0))
- **editor:** Wrap directional window navigation ([#324](https://github.com/codersauce/red/issues/324)) ([cc7ff18](https://github.com/codersauce/red/commit/cc7ff18e6268f6c69a294cd8495aed9172cd6063))
- **editor:** Support newlines in substitute replacements ([#320](https://github.com/codersauce/red/issues/320)) ([2cf819e](https://github.com/codersauce/red/commit/2cf819eb121157292b4417266ab8f98d4d19fba0))
- **fidget:** Improve progress overlay rendering ([#319](https://github.com/codersauce/red/issues/319)) ([cec6375](https://github.com/codersauce/red/commit/cec6375fa7fbba99ae7a3e39be5ec5c3f71eac82))
- **commands:** Add save-all command ([#317](https://github.com/codersauce/red/issues/317)) ([12fee08](https://github.com/codersauce/red/commit/12fee08e2bf3b6f2fc304bebf2900473586718f0))
- **neotree:** Open file tree from insert mode ([#312](https://github.com/codersauce/red/issues/312)) ([9c461e8](https://github.com/codersauce/red/commit/9c461e85adfff27a599f11765d9a5530ea18012a))
- **commands:** Add bufdo support ([#311](https://github.com/codersauce/red/issues/311)) ([5d1d34a](https://github.com/codersauce/red/commit/5d1d34aa2fd3a34e884eb52aee9b608bbc84be49))
- **editing:** Add Vim-style comment formatting ([#307](https://github.com/codersauce/red/issues/307)) ([68b4a43](https://github.com/codersauce/red/commit/68b4a43095f19cd06e320d3568a78f641bb26abb))
- **lsp:** Improve references loading feedback ([99454cd](https://github.com/codersauce/red/commit/99454cd68e1901e34eead9b3ffe9afe0cc482705))
- **commands:** Add vim-compatible command abbreviations ([#304](https://github.com/codersauce/red/issues/304)) ([d92777c](https://github.com/codersauce/red/commit/d92777c8196fe95117459a5c4ce39a6ada26ed4c))
- **picker:** Modernize buffer navigation ([#302](https://github.com/codersauce/red/issues/302)) ([85d225e](https://github.com/codersauce/red/commit/85d225e3fb24666563bf3d90c723d6b4edef9275))
- **vim:** Add paragraph and sentence motions ([#303](https://github.com/codersauce/red/issues/303)) ([816abab](https://github.com/codersauce/red/commit/816abab28c51bb251d51c87cd19e9b16fe347815))
- **buffers:** Add unnamed buffer creation ([#299](https://github.com/codersauce/red/issues/299)) ([8d86c47](https://github.com/codersauce/red/commit/8d86c47f49e5dcc8aedec7d4b87644b1015d9bc4))
- **lsp:** Cache diagnostics across editor restarts ([#296](https://github.com/codersauce/red/issues/296)) ([e06bf54](https://github.com/codersauce/red/commit/e06bf54ee69b2b88f699629bf8d911b3f3b343b6))
- **lsp:** Apply project-specific rustfmt settings ([#295](https://github.com/codersauce/red/issues/295)) ([83329b6](https://github.com/codersauce/red/commit/83329b6f1d4e40447320f187871c6873e9c3185b))
- **lsp:** Show code action loading picker ([#292](https://github.com/codersauce/red/issues/292)) ([7c9ba3a](https://github.com/codersauce/red/commit/7c9ba3a01ab03ea2e6052435b012fbc19e952ce2))
- **lsp:** Auto-import rust types from completions ([853aa21](https://github.com/codersauce/red/commit/853aa21e93b4ef38b3b8a814064b70d9466c8345))
- **neotree:** Virtualize unbounded file trees ([#289](https://github.com/codersauce/red/issues/289)) ([dbced21](https://github.com/codersauce/red/commit/dbced21f35e6c397904d4ce6b25516644273fd87))
- **agent:** Relax agent guardrails ([#288](https://github.com/codersauce/red/issues/288)) ([7380cba](https://github.com/codersauce/red/commit/7380cba5af5828ffcc387ecd0ab1c4882e910275))
- **agent:** Add composer history search ([#287](https://github.com/codersauce/red/issues/287)) ([54a6253](https://github.com/codersauce/red/commit/54a62539beec4941812ea3128221661500e86495))
- **agent:** Add source annotation walkthroughs ([#282](https://github.com/codersauce/red/issues/282)) ([94c7628](https://github.com/codersauce/red/commit/94c7628ae1119702503abb0039bb4ba83da16dcf))
- **completion:** Add opt-in coordinated suggestions ([#281](https://github.com/codersauce/red/issues/281)) ([9181829](https://github.com/codersauce/red/commit/9181829b3e2837f6e34df3e48b0a21a0b3bb88a9))
- **agent:** Show compact progress and create workspace directories ([#280](https://github.com/codersauce/red/issues/280)) ([ef9fc74](https://github.com/codersauce/red/commit/ef9fc744d2ae3d4c1ea40000de6a85d310cfc2d4))
- **onboarding:** Add interactive first-run tutorial ([#275](https://github.com/codersauce/red/issues/275)) ([b281951](https://github.com/codersauce/red/commit/b2819516eeae05fa5ca5c9caba25ab78779f5b48))
- **agent:** Add model selection to the agent pane ([f9a9801](https://github.com/codersauce/red/commit/f9a98011b5a3d62e58ce318d0af4f1c746b860e6))
- **lsp:** Show signature help while typing ([792522e](https://github.com/codersauce/red/commit/792522e4774ed979ffea7f38b8749df025af3b0c))
- **indent:** Add query-driven language indentation ([d91648e](https://github.com/codersauce/red/commit/d91648e2dfe6e4e5f7412463b1e2ea977795c67f))
- **completion:** Accept selected item with enter ([#263](https://github.com/codersauce/red/issues/263)) ([b66b89b](https://github.com/codersauce/red/commit/b66b89befbe9d42bfa298a69019baac52f418b48))
- **completion:** Add interactive snippet placeholders ([#262](https://github.com/codersauce/red/issues/262)) ([cb95bdb](https://github.com/codersauce/red/commit/cb95bdb44683dc49ecac4a4041c69d109d37b25c))
- **formatting:** Enable format on save by default ([c25254b](https://github.com/codersauce/red/commit/c25254b8f00215e083b1bdefa1d3c062cf85d25f))
- **neotree:** Activate items on double click ([46169f4](https://github.com/codersauce/red/commit/46169f412faa772920e7d0c5920b7c0e2aebc689))

### Bug Fixes

- **statusline:** Preserve styles when sections are hidden ([#356](https://github.com/codersauce/red/issues/356)) ([144da39](https://github.com/codersauce/red/commit/144da391b61468ccaedf51a2b2bb1949750d211d))
- **editor:** Repeat visual indentation by line span ([#355](https://github.com/codersauce/red/issues/355)) ([0c8a8f8](https://github.com/codersauce/red/commit/0c8a8f80a2b4521f3e4b620f8b529e2e1fad36b2))
- **editor:** Seed multi-cursor from visual selection ([#354](https://github.com/codersauce/red/issues/354)) ([f7ea02d](https://github.com/codersauce/red/commit/f7ea02d33835df7829d187d682c72e4da35842ec))
- **git:** Avoid repeated full repository scans ([#353](https://github.com/codersauce/red/issues/353)) ([873bbae](https://github.com/codersauce/red/commit/873bbae1f69d58f4e0516d77f0908e58635101b3))
- **agent:** Stabilize dynamic tool contracts ([#349](https://github.com/codersauce/red/issues/349)) ([3eb36ad](https://github.com/codersauce/red/commit/3eb36ad45a238a8c667e76cf7e071700b6d91a36))
- **neotree:** Keep large workspaces responsive ([#344](https://github.com/codersauce/red/issues/344)) ([5862875](https://github.com/codersauce/red/commit/5862875ed0f391797945e8ffd9f008b73b811a63))
- **editor:** Reindent pasted code without range formatting ([#341](https://github.com/codersauce/red/issues/341)) ([ae9c323](https://github.com/codersauce/red/commit/ae9c3231a03fdcced5a0f8f5ba22a200b9fffa97))
- **editor:** Resolve recovered external-file warnings ([#336](https://github.com/codersauce/red/issues/336)) ([bec67fb](https://github.com/codersauce/red/commit/bec67fb7cbc59b8265947842619057c0bd55a3d0))
- **session:** Scope restore state by workspace ([#335](https://github.com/codersauce/red/issues/335)) ([c821c12](https://github.com/codersauce/red/commit/c821c124a82949ae3c3c5db13e606ee585172a67))
- **picker:** Prioritize distinctive primary labels ([#331](https://github.com/codersauce/red/issues/331)) ([183bf83](https://github.com/codersauce/red/commit/183bf83710383534ba0b163a4649cd553e5b4201))
- **editor:** Prevent overwriting external file changes ([51b1831](https://github.com/codersauce/red/commit/51b18310fc0fb2bdb3ceacbce49a30e91782cfe3))
- **lsp:** Refresh stale auto-import completions ([#329](https://github.com/codersauce/red/issues/329)) ([d1442f5](https://github.com/codersauce/red/commit/d1442f5be9c501c01f303c69c08d3c3c6272c071))
- **neotree:** Restore recursive tree search ([#327](https://github.com/codersauce/red/issues/327)) ([8d09bfd](https://github.com/codersauce/red/commit/8d09bfd3d7b16420d823f5046127376fdf55adc8))
- **editor:** Preserve indent after compound closers ([#328](https://github.com/codersauce/red/issues/328)) ([92a4af9](https://github.com/codersauce/red/commit/92a4af9b4e8f598b372e5137f2744c83db505a66))
- **editor:** Smooth distant cursor reveals ([#325](https://github.com/codersauce/red/issues/325)) ([be96c53](https://github.com/codersauce/red/commit/be96c53bf1506623d71af4ebbe65130ce3995bb7))
- **notifications:** Clarify command and lsp outcomes ([#321](https://github.com/codersauce/red/issues/321)) ([c4c22bc](https://github.com/codersauce/red/commit/c4c22bc22908200901a6ca9bdbf138d9d015ad76))
- **completion:** Select top result after lsp updates ([#314](https://github.com/codersauce/red/issues/314)) ([61107d5](https://github.com/codersauce/red/commit/61107d5ce011a90f6159ea7e01d7a6b94b6632b2))
- **notifications:** Make routine errors non-intrusive ([#318](https://github.com/codersauce/red/issues/318)) ([15bfb36](https://github.com/codersauce/red/commit/15bfb36ff21053f81203dcf8ee083eb440cf8b30))
- **commands:** Allow bufdo to delete buffers ([#316](https://github.com/codersauce/red/issues/316)) ([4dc4054](https://github.com/codersauce/red/commit/4dc4054eec083683335523db791fb1599c868cec))
- **lsp:** Retain diagnostics across code actions ([#315](https://github.com/codersauce/red/issues/315)) ([3ab0632](https://github.com/codersauce/red/commit/3ab0632832ca17fcdc153ede23769431afc9db13))
- **completion:** Rebase refreshed edits from their request ([#313](https://github.com/codersauce/red/issues/313)) ([100ae37](https://github.com/codersauce/red/commit/100ae37904c6a75fb3502d901fc846d0044bcc31))
- **editor:** Refresh plugins after buffer replacement ([#310](https://github.com/codersauce/red/issues/310)) ([61956bc](https://github.com/codersauce/red/commit/61956bc2b455d1da60b4423a9fb30617944546aa))
- **lsp:** Improve workspace symbol filtering ([#306](https://github.com/codersauce/red/issues/306)) ([f4ebad3](https://github.com/codersauce/red/commit/f4ebad351639ce7fab6ed26ea43ca38ec90ebef7))
- **editor:** Preserve navigation jump history ([#305](https://github.com/codersauce/red/issues/305)) ([d778dbe](https://github.com/codersauce/red/commit/d778dbeb79d2523103c71ae0bbe50524fe23b5e6))
- **lsp:** Recover save formatting and inlay hints ([2e7702d](https://github.com/codersauce/red/commit/2e7702dcb097dab03fbb5b480b2831dcd8c02dc7))
- **formatting:** Trim whitespace before format on save ([4b0cd5f](https://github.com/codersauce/red/commit/4b0cd5fc8e707f52aa21059225a4a8057c833c8e))
- **lsp:** Ignore stale inlay hint responses ([4db98ba](https://github.com/codersauce/red/commit/4db98ba5ebdea484c06b7cec3b94c16a39873155))
- **editor:** Reveal wrapped final lines ([#301](https://github.com/codersauce/red/issues/301)) ([c7408b2](https://github.com/codersauce/red/commit/c7408b2a21ee94e800a25a5c90f79000af3364ec))
- **picker:** Prioritize filename matches ([#294](https://github.com/codersauce/red/issues/294)) ([db6c678](https://github.com/codersauce/red/commit/db6c678dc46ef402cc08915ad8c6b3ab9b4d192b))
- **lsp:** Restore work-done progress ([#293](https://github.com/codersauce/red/issues/293)) ([9934a1a](https://github.com/codersauce/red/commit/9934a1a4266a28110be401a026f363496412aed5))
- **diagnostics:** Preserve wrapped source text ([#291](https://github.com/codersauce/red/issues/291)) ([cb99f13](https://github.com/codersauce/red/commit/cb99f139346ec4f73712e943373dc359343c3c40))
- **neotree:** Prevent instruction budget exhaustion in deep workspaces ([ceafbc9](https://github.com/codersauce/red/commit/ceafbc9d668a1c3521de6a4d43bd0655b5e5e48a))
- **lsp:** Refresh and relocate stale diagnostics ([8e81a06](https://github.com/codersauce/red/commit/8e81a06faf81d0ca03eb042e2bc7a6ee8ee451b3))
- **lsp:** Tolerate language server failures ([#286](https://github.com/codersauce/red/issues/286)) ([9dfc862](https://github.com/codersauce/red/commit/9dfc862c85726c1d9d073ae90ee08c81dd0c7899))
- **agent:** Finish interrupted turns promptly ([#285](https://github.com/codersauce/red/issues/285)) ([ed140dd](https://github.com/codersauce/red/commit/ed140dd30ffc6a2dade180001a02d8b2865860c7))
- **agent:** Make transcript links navigable ([#283](https://github.com/codersauce/red/issues/283)) ([fd9b8c6](https://github.com/codersauce/red/commit/fd9b8c69043e61ec1f1fe0417cddcfe479f8f78b))
- **statusline:** Compact displayed file paths ([#284](https://github.com/codersauce/red/issues/284)) ([c7af566](https://github.com/codersauce/red/commit/c7af566882467401ad6d8f976e351df5e758fede))
- **copilot:** Keep autocomplete enabled alongside suggestions ([#279](https://github.com/codersauce/red/issues/279)) ([6d1af35](https://github.com/codersauce/red/commit/6d1af359f8fd96e278088f25a4899f474e26bbd6))
- **editor:** Restore the terminal promptly on quit ([#278](https://github.com/codersauce/red/issues/278)) ([5e39d4b](https://github.com/codersauce/red/commit/5e39d4bd29faf0b7220839e8c8be959c8f5c591d))
- **copilot:** Persist enablement across restarts ([#277](https://github.com/codersauce/red/issues/277)) ([78195ba](https://github.com/codersauce/red/commit/78195ba1dfaea668ba06da3457a3bd36c6db749d))
- **buffers:** Toggle between recently used buffers ([#276](https://github.com/codersauce/red/issues/276)) ([9b7b604](https://github.com/codersauce/red/commit/9b7b60408250e54d92e72854306546ecc7e09565))
- **keymap:** Honor command-mode aliases across navigation contexts ([#274](https://github.com/codersauce/red/issues/274)) ([7a668a4](https://github.com/codersauce/red/commit/7a668a4cd812da88d53049f320e5c8dfcc04a09c))
- **breadcrumbs:** Restore symbol context and abbreviate home paths ([de5420a](https://github.com/codersauce/red/commit/de5420ae63760dcafa119db28e6646b5ef89cc2f))
- **matchit:** Handle rust lifetimes in percent motions ([eb76dc3](https://github.com/codersauce/red/commit/eb76dc314c122455b54321b29fcf9a32fad92254))
- **copilot:** Accept inline suggestions with ctrl-l ([13659cc](https://github.com/codersauce/red/commit/13659cce06bba6f7280413e0b5884c6f0cc4420e))
- **completion:** Preserve snippet navigation with empty popups ([af35b19](https://github.com/codersauce/red/commit/af35b195c35f973bb2fa2b98d854fcdd4b12f6de))
- **clipboard:** Isolate editor tests from system clipboard ([ce86e79](https://github.com/codersauce/red/commit/ce86e796fbf84fe1b09fa279fef3c7557df24a95))
- **picker:** Prioritize exact name matches ([a807fb9](https://github.com/codersauce/red/commit/a807fb98dc68d13330c01694db98af2ddd1eec6a))
- **lsp:** Retry cancelled diagnostics quietly ([d2d9c5c](https://github.com/codersauce/red/commit/d2d9c5cf905439753326f69631c7cbaa9e1d9897))
- **config:** Accept signature help settings ([0e82af1](https://github.com/codersauce/red/commit/0e82af1b8dd2654ed605510a139c9983afb0ba2f))
- **lsp:** Refresh and preserve diagnostics after saving ([c338ed8](https://github.com/codersauce/red/commit/c338ed8b79ba8c45472d1250ca05724bae66dc9f))
- **neotree:** Show symlinks as their targets ([#260](https://github.com/codersauce/red/issues/260)) ([b5add6d](https://github.com/codersauce/red/commit/b5add6db3d1fef430f5a0560c2d1a49daaa0c1e2))
- **editor:** Keep cursor visible on matching brackets ([#259](https://github.com/codersauce/red/issues/259)) ([d80caf9](https://github.com/codersauce/red/commit/d80caf9581e1a54f4673d9fe240b1a386ef0b395))

### Performance

- **picker:** Stream and cache file discovery ([#352](https://github.com/codersauce/red/issues/352)) ([6af198e](https://github.com/codersauce/red/commit/6af198ebb2c294e73564010a7988452ae7dac1cf))
- **editor:** Accelerate large repository interactions ([#332](https://github.com/codersauce/red/issues/332)) ([ae803e0](https://github.com/codersauce/red/commit/ae803e0970dcd47ad76d1e148f813d754fe779b2))
- **runtime:** Accelerate editor and plugin hot paths ([#309](https://github.com/codersauce/red/issues/309)) ([2518f3f](https://github.com/codersauce/red/commit/2518f3fb96c72f47ce4c3c829935f71038e64203))
- **picker:** Accelerate large workspace file searches ([#298](https://github.com/codersauce/red/issues/298)) ([e0b3093](https://github.com/codersauce/red/commit/e0b30930f0173520af896fa261afaec6c039f95d))
- **lsp:** Batch progress updates and coalesce redraws ([#297](https://github.com/codersauce/red/issues/297)) ([f30f0b0](https://github.com/codersauce/red/commit/f30f0b00fba60f8ce241d078b67fd4b962db179c))
- **editor:** Speed up repeated edits ([#267](https://github.com/codersauce/red/issues/267)) ([ac89827](https://github.com/codersauce/red/commit/ac89827e1a87118fe0083a08aec00fcd349c97de))

### Documentation

- **editor:** Clarify agent workflows and upcoming release features ([#351](https://github.com/codersauce/red/issues/351)) ([10210e8](https://github.com/codersauce/red/commit/10210e831d0ef3728b0f3dfa5792e42d03406434))
- **perf:** Record edit replay optimization plan ([122c0b8](https://github.com/codersauce/red/commit/122c0b84996feff7ed1e69e302802d8c956edd1f))

### Testing

- **lsp:** Make diagnostic close fixture portable ([6ae7bf5](https://github.com/codersauce/red/commit/6ae7bf5721f1b148d2f5f2a3dc3690ea2d0ee1df))

### Continuous Integration

- Use smaller macOS test runners ([#343](https://github.com/codersauce/red/issues/343)) ([b232fef](https://github.com/codersauce/red/commit/b232fef10c419158c85662596979f7e669b91c3c))
- Reduce repeated validation spend ([#342](https://github.com/codersauce/red/issues/342)) ([e8472e7](https://github.com/codersauce/red/commit/e8472e7f1436ea6cf1cedc6f8c77ec70bb69f088))

### Maintenance

- **branch:** Sync main before snippet fix publication ([992b69e](https://github.com/codersauce/red/commit/992b69eb1ea26730890f97dd9318f9e1fb6c0f70))

### Other

- Improve graph navigation ([3b6c167](https://github.com/codersauce/red/commit/3b6c167b57a67a7bb6c7f3d040518bd005e32805))
- Document learning tutorial boundary ([e1b0193](https://github.com/codersauce/red/commit/e1b0193c76d07401786ca7a46e4ea32f5078ece1))
- Record visual repeat and picker tab gotchas ([f02bd5f](https://github.com/codersauce/red/commit/f02bd5f687e3ae8e3d4c13e847dd9c84bac05c6c))
- Refresh plugin package guidance ([8fd5b2e](https://github.com/codersauce/red/commit/8fd5b2e6fcbc83cb8a080df1f3ae38db6f35f3ef))
- Document binary skew check ([097d131](https://github.com/codersauce/red/commit/097d131d140a0767f9261fcfc14eabae8603cd06))
- Note website implementation starting point ([41a350c](https://github.com/codersauce/red/commit/41a350c3ab8230ce5d967e217903c6d0e0c56e93))
- Clarify website positioning ([81b27b0](https://github.com/codersauce/red/commit/81b27b02185ab0d11e10e02918f214e53a631f23))
- Tighten agent and plugin wiki graph ([63d56e0](https://github.com/codersauce/red/commit/63d56e07869a757def4cb17cbe805e3fa7d07244))
- Document release campaign workflow ([6a3b58d](https://github.com/codersauce/red/commit/6a3b58deed9bf003380f2cbcf6a304a7e7cd87b6))
- Refresh config and session references ([f82bccb](https://github.com/codersauce/red/commit/f82bccb2556f0cac7ab244f9ce4aa989ff5cdd79))
- Refresh CI validation reference ([4a3a1af](https://github.com/codersauce/red/commit/4a3a1affd22a0902c01e6e4398695688186b9659))
- Refresh validation and config references ([318871b](https://github.com/codersauce/red/commit/318871b701126b904f90b383d4829e195297ecb8))
- Document CI runner selection ([d8cb895](https://github.com/codersauce/red/commit/d8cb8953204c1155d99adb6938b90a22af2a5e09))
- Connect ai completion config reference ([5dae16b](https://github.com/codersauce/red/commit/5dae16baa7e851670e7fc957702486b5085e7ce1))
- Update copilot completion guide ([c632834](https://github.com/codersauce/red/commit/c6328341199d9e6e65e54531b08d7144238d84a8))
- Clarify completion and host api guidance ([7fb8d03](https://github.com/codersauce/red/commit/7fb8d0361b32f07b604d11fedcc5df2354595e15))
- Clarify agent and copilot boundaries ([695dddd](https://github.com/codersauce/red/commit/695ddddf3c642cbd9154d75da36e7c05651480db))
- Update agent tool and validation graph ([70df993](https://github.com/codersauce/red/commit/70df993af33697131c5212b9364132da12252c3c))
- Use worktrunk for features ([c1b4601](https://github.com/codersauce/red/commit/c1b4601a29148c0ed10fb53eea74e5684dbb0cbc))
- Refresh plugin and agent references ([fa487e4](https://github.com/codersauce/red/commit/fa487e4280e611b09ca7d9c65371e20b1d696182))

## [0.6.0](https://github.com/codersauce/red/compare/v0.5.0...v0.6.0)

### Features

- **inline:** Retain contextual assist and reviewable outcomes ([#256](https://github.com/codersauce/red/issues/256)) ([3b1020c](https://github.com/codersauce/red/commit/3b1020c15cff0c59734ebfd0590696dcf91b3071))
- **learn:** Add track-based learning hub ([#255](https://github.com/codersauce/red/issues/255)) ([2c56008](https://github.com/codersauce/red/commit/2c5600881795f628a4679ba5f05dc7cb82255a95))
- **ui:** Add contextual keyboard shortcut explorer ([#254](https://github.com/codersauce/red/issues/254)) ([103759b](https://github.com/codersauce/red/commit/103759b1ba26c9e8164c718b31a7530788519941))
- **copilot:** Improve setup and sign-in flow ([#249](https://github.com/codersauce/red/issues/249)) ([ef36b2b](https://github.com/codersauce/red/commit/ef36b2bac269365fcb83ec00d2b25bd522e9c6cc))
- **notifications:** Distinguish attention from routine feedback ([#252](https://github.com/codersauce/red/issues/252)) ([b833221](https://github.com/codersauce/red/commit/b833221c4546142790701fc05adc1b82a072728e))
- **commands:** Add argument completion ([#250](https://github.com/codersauce/red/issues/250)) ([a2589a6](https://github.com/codersauce/red/commit/a2589a6e30984009ffa671831b25b42e965ea3df))
- **ui:** Support word backspace in dialogs ([#251](https://github.com/codersauce/red/issues/251)) ([963c5a2](https://github.com/codersauce/red/commit/963c5a2cd46bab7a55bda37b2cbc63580fb6e0fc))
- **notifications:** Add bottom-line summary and message history ([#245](https://github.com/codersauce/red/issues/245)) ([5b74520](https://github.com/codersauce/red/commit/5b74520f43ce37d4e04b1229e5aa452240b87586))
- **git:** Improve diff highlighting ([#244](https://github.com/codersauce/red/issues/244)) ([6942906](https://github.com/codersauce/red/commit/694290640e632c87127054d9186d077ce14c7b97))
- **keymap:** Add ctrl-w d side-by-side split alias ([#243](https://github.com/codersauce/red/issues/243)) ([af36bab](https://github.com/codersauce/red/commit/af36babe4b59d871b26552b502958da10f033a48))
- **search:** Add persistent search history ([#242](https://github.com/codersauce/red/issues/242)) ([7e4b3de](https://github.com/codersauce/red/commit/7e4b3ded29173f2b8bdec76f8d4dc7e9b547c6fd))
- **agent:** Improve transcript navigation and prompt actions ([#241](https://github.com/codersauce/red/issues/241)) ([24e60c5](https://github.com/codersauce/red/commit/24e60c51f26937baf462fac173026282371cf1e0))
- **window:** Add reversible pane zoom ([#240](https://github.com/codersauce/red/issues/240)) ([94ca274](https://github.com/codersauce/red/commit/94ca274b272064e07842388f9e0eebc07ca42295))
- **git:** Polish workspace and standardize action strips ([#239](https://github.com/codersauce/red/issues/239)) ([89d061c](https://github.com/codersauce/red/commit/89d061cac9b5ce6aa457e7d0f77809168744a617))
- **composer:** Wrap words with source-backed text layout ([#237](https://github.com/codersauce/red/issues/237)) ([4542a95](https://github.com/codersauce/red/commit/4542a9511380a7e2ce111cbfbf624dad6a4346f3))
- **agent:** Submit composer prompts with enter ([#236](https://github.com/codersauce/red/issues/236)) ([4a208f5](https://github.com/codersauce/red/commit/4a208f5822c8e0247d58d1e512d7ba7cb342c279))
- **inline-assist:** Add source-linked comments and history ([#233](https://github.com/codersauce/red/issues/233)) ([8881e8f](https://github.com/codersauce/red/commit/8881e8fbbdbf32a17d14ff37f75de44cb2ef9c20))
- **ui:** Add vim dialog button navigation ([a822561](https://github.com/codersauce/red/commit/a8225610c3e024352019d3dca52a430855234a27))
- **editor:** Add tree-sitter text objects and motions ([#231](https://github.com/codersauce/red/issues/231)) ([f6bab4e](https://github.com/codersauce/red/commit/f6bab4eaa0eb2588a9de29348155ca84c8c75519))
- **editor:** Support visual command ranges ([#226](https://github.com/codersauce/red/issues/226)) ([3be17ae](https://github.com/codersauce/red/commit/3be17aecfd3648a7d6e4aea307f8b67dd561099a))
- **git:** Report operation progress ([#220](https://github.com/codersauce/red/issues/220)) ([037e461](https://github.com/codersauce/red/commit/037e46106a8618a47bf008ea328a4be8b1439ca3))
- **formatting:** Support language pack formatters ([#222](https://github.com/codersauce/red/issues/222)) ([49d062b](https://github.com/codersauce/red/commit/49d062b7025cb0b8712341a6bd2c6026de0febaf))
- **session:** Restore plugin pane state ([#221](https://github.com/codersauce/red/issues/221)) ([1e308f9](https://github.com/codersauce/red/commit/1e308f92c6642e389bec9984cf211ed3949f6a2a))
- **agent:** Add bounded inline assist ([#216](https://github.com/codersauce/red/issues/216)) ([b08a9e1](https://github.com/codersauce/red/commit/b08a9e1e8af783b06b5b21a0de569c7c1be41331))
- **git:** Confirm pushes with progress ([#218](https://github.com/codersauce/red/issues/218)) ([86f407e](https://github.com/codersauce/red/commit/86f407ed750a041360483e5f7ea035bb93451229))
- **git:** Highlight commit message buffers ([6ed6baa](https://github.com/codersauce/red/commit/6ed6baa0cac8678a36d885d4cc1dfd48b031c81f))
- **git:** Generate commit message drafts with Codex ([96d6e36](https://github.com/codersauce/red/commit/96d6e36f8208e942a68eff3020d388cff61027aa))
- **codex:** Add bounded commit message generation ([cd58292](https://github.com/codersauce/red/commit/cd58292342ce2abbd8c1d1990cffe983cba3860b))
- **agent:** Restore persisted conversations ([#210](https://github.com/codersauce/red/issues/210)) ([f8a8f36](https://github.com/codersauce/red/commit/f8a8f36c1f32e69ce59d7354c9ef1d4adceeb7d2))
- **agent:** Replace proposals with followed live edits ([#207](https://github.com/codersauce/red/issues/207)) ([a380296](https://github.com/codersauce/red/commit/a3802967bf8878b2e5842b7f23225dbeca69d1c1))
- **picker:** Add command area icons ([#206](https://github.com/codersauce/red/issues/206)) ([3ae2688](https://github.com/codersauce/red/commit/3ae2688e8808d1d976c5015c15508013d80f8f12))
- **editor:** Restore and indent visual selections ([#205](https://github.com/codersauce/red/issues/205)) ([aed51c3](https://github.com/codersauce/red/commit/aed51c3b8ad8f5b88c6afbf32ed02b6a3fba77c0))
- **editing:** Reuse vim behavior in plugin textareas ([#204](https://github.com/codersauce/red/issues/204)) ([5a6d245](https://github.com/codersauce/red/commit/5a6d24545b16441945a34307ed2d633f375c8a58))
- **agent:** Overhaul pane interaction and reliability ([#197](https://github.com/codersauce/red/issues/197)) ([8dc919b](https://github.com/codersauce/red/commit/8dc919b985bf223d07cc9cc270d4e0784e44adce))
- **lsp:** Add diagnostic navigation ([#202](https://github.com/codersauce/red/issues/202)) ([a6e9e0e](https://github.com/codersauce/red/commit/a6e9e0e79ed93275b1c3da7000a52415c6cc357e))
- **editor:** Show release notes after version upgrades ([#201](https://github.com/codersauce/red/issues/201)) ([02675ce](https://github.com/codersauce/red/commit/02675ceea4c608398191b8ad2dab36dbef5a4fe6))
- **editor:** Add diagnostic gutter signs ([#191](https://github.com/codersauce/red/issues/191)) ([ff54130](https://github.com/codersauce/red/commit/ff54130f89b3020f65bf84b784d87cc00c0ad400))
- **composer:** Add modal editing to agent pane ([#193](https://github.com/codersauce/red/issues/193)) ([fd09c4a](https://github.com/codersauce/red/commit/fd09c4aaf632de95388975b006d79489cce945e1))
- **lsp:** Add diagnostics picker ([#192](https://github.com/codersauce/red/issues/192)) ([adcb18d](https://github.com/codersauce/red/commit/adcb18dc53154ca2aa2d04624657dc3e77a46955))
- **lsp:** Show line diagnostics popup ([#194](https://github.com/codersauce/red/issues/194)) ([e15dbe5](https://github.com/codersauce/red/commit/e15dbe515e46d977c74513cc0b542c428e197ec4))
- **statusline:** Show LSP diagnostic counts ([#190](https://github.com/codersauce/red/issues/190)) ([e2cb202](https://github.com/codersauce/red/commit/e2cb202dbf2874c94ba6dbc13022c2139f4e4a01))

### Bug Fixes

- **agent:** Focus composer when opening conversation ([#253](https://github.com/codersauce/red/issues/253)) ([02de40e](https://github.com/codersauce/red/commit/02de40e329e88e18c766fd4ad937b396d0d3eb34))
- **editor:** Exit empty command mode on backspace ([#248](https://github.com/codersauce/red/issues/248)) ([d3bdaff](https://github.com/codersauce/red/commit/d3bdafffdf58189d8290778d11fb13de0f9ef164))
- **editor:** Clear dirty state when contents match saved text ([#246](https://github.com/codersauce/red/issues/246)) ([f01ec5e](https://github.com/codersauce/red/commit/f01ec5e7b14c347c287fc7de59c8311d3ae89a78))
- **editor:** Preserve viewport at insertion line ends ([24393f3](https://github.com/codersauce/red/commit/24393f3ac00d399a0151aa4682e70b3ed149d5dc))
- **completion:** Align completion item labels ([9fd1977](https://github.com/codersauce/red/commit/9fd19774634cace8d14895d6f00d17de6ef3697a))
- **completion:** Use icon for text items ([704bf8d](https://github.com/codersauce/red/commit/704bf8d022d263ffda2459109eb4039de7f5002c))
- **editor:** Prevent stale commit buffer cache reuse ([333ce8a](https://github.com/codersauce/red/commit/333ce8ad77d2a08db54ab3ebf66053a87e5de783))
- **editor:** Surface workspace action errors ([765fcc5](https://github.com/codersauce/red/commit/765fcc59a1ce53ac06ffa7e7ca456c6f458782c9))
- **editor:** Match neovim jumplist semantics ([#228](https://github.com/codersauce/red/issues/228)) ([6c6f5d0](https://github.com/codersauce/red/commit/6c6f5d0305401b8deae020d86e23b5c7cf996bcc))
- **agent:** Follow transcript after prompt submission ([#229](https://github.com/codersauce/red/issues/229)) ([d60c949](https://github.com/codersauce/red/commit/d60c949dee3be39e189df4578f2f55b89faed85c))
- **editor:** Reuse buffers for file aliases ([#227](https://github.com/codersauce/red/issues/227)) ([75d55b3](https://github.com/codersauce/red/commit/75d55b37294ca76efc9360bf0fab51369aaf92c7))
- **editor:** Accept command keys as replacements ([#225](https://github.com/codersauce/red/issues/225)) ([5b5e474](https://github.com/codersauce/red/commit/5b5e47457c8fd96627593121889256f58b1ae0ca))
- **editor:** Preserve undo across buffer overrides ([#224](https://github.com/codersauce/red/issues/224)) ([b852ab4](https://github.com/codersauce/red/commit/b852ab44dc05dd05ed72249323c2ad64d86aaf20))
- **plugin:** Remove global test serialization ([fd22348](https://github.com/codersauce/red/commit/fd223483e04cca8c6b996254fb62930426bc93af))
- **ci:** Serialize shared dispatcher tests ([107119a](https://github.com/codersauce/red/commit/107119a1191bf12c28226870d2de14622830c713))
- **ci:** Serialize Codex mock processes ([62b2212](https://github.com/codersauce/red/commit/62b221299162a72f59bfa2fc93c78043e09b98d0))
- **session:** Restore clean workspaces on restart ([#219](https://github.com/codersauce/red/issues/219)) ([6afc3e5](https://github.com/codersauce/red/commit/6afc3e5a0a7dda90e7a91b835576eb5e01e820db))
- **git:** Report commit outcomes ([0de6903](https://github.com/codersauce/red/commit/0de690368d95f10aefa504392948f09dd8bbdf0b))
- **git:** Make dashboard staging reliable ([a837109](https://github.com/codersauce/red/commit/a837109c1b749224bd823b24c35edfc3f06311c8))
- **agent:** Separate submitted prompts from busy status ([#208](https://github.com/codersauce/red/issues/208)) ([7bb2a1e](https://github.com/codersauce/red/commit/7bb2a1e4a117d8375a57a963ddb036bc1dd58d40))
- **agent:** Focus composer on first toggle ([#209](https://github.com/codersauce/red/issues/209)) ([81a8626](https://github.com/codersauce/red/commit/81a862675c92a831ea6a85e29394cdbbc6fc31da))
- **editor:** Refresh visual block edits ([#203](https://github.com/codersauce/red/issues/203)) ([872775d](https://github.com/codersauce/red/commit/872775deb13f3f8d07b2c8deba951a17aa105f0f))
- **neotree:** Highlight newly created files ([#195](https://github.com/codersauce/red/issues/195)) ([be089bb](https://github.com/codersauce/red/commit/be089bb19f62b8bc99aafd360dc9e0c82a90f730))

### Performance

- **editor:** Improve terminal resize rendering ([#247](https://github.com/codersauce/red/issues/247)) ([1042586](https://github.com/codersauce/red/commit/1042586a301b97d29814768b199d9d1c3f89fffe))
- **editor:** Coalesce keyboard navigation frames ([#238](https://github.com/codersauce/red/issues/238)) ([fd3a298](https://github.com/codersauce/red/commit/fd3a298498e1912526f935a2c8c952e2f3659e4f))
- **editor:** Coalesce scrolling and reuse unchanged surfaces ([#234](https://github.com/codersauce/red/issues/234)) ([bd15fea](https://github.com/codersauce/red/commit/bd15feac54a1517648db00b7ed9ea776a84d9bd4))
- **ci:** Speed up test and validation workflows ([#232](https://github.com/codersauce/red/issues/232)) ([0477a6e](https://github.com/codersauce/red/commit/0477a6eba85043a2d3e7c923ba403747e9ea5ad9))

### Testing

- **whats-new:** Use stable release notes fixtures ([06e95db](https://github.com/codersauce/red/commit/06e95dbfe7d94e7ae84b6448a24d68428a82d6bc))

### Continuous Integration

- Streamline pull request validation ([#223](https://github.com/codersauce/red/issues/223)) ([661032f](https://github.com/codersauce/red/commit/661032f505b641209d6b414a1b3af4f02b31a62c))
- **release:** Generate contributor-friendly release notes ([#217](https://github.com/codersauce/red/issues/217)) ([80bfcd2](https://github.com/codersauce/red/commit/80bfcd2008def5e98b10dd1b00efbd82b5498854))

### Other

- Clarify editor syntax routing ([01e7cec](https://github.com/codersauce/red/commit/01e7cec23dbbaeb1fd9a40ba1ee6ad864365f703))
- Document completion and dialog UI invariants ([6ff19cf](https://github.com/codersauce/red/commit/6ff19cf2b67c2854e2ccd7c8b1025f5fcb75f418))
- Capture editor UI and syntax constraints ([617e7a2](https://github.com/codersauce/red/commit/617e7a2d457e22065985a224d64cfafb35056873))
- Document pane restore and jumplist semantics ([7c35e98](https://github.com/codersauce/red/commit/7c35e9819a30bd7cc1f249b2c821313e084ba180))
- Document host api and release notes updates ([b8d7590](https://github.com/codersauce/red/commit/b8d759092c2b1f5f4c09248e09753a20a33b7062))
- Update agent edit model ([83228b0](https://github.com/codersauce/red/commit/83228b0fc9b49050cca734378b8248a9308697a5))
- Refresh agent and config references ([3eb82a9](https://github.com/codersauce/red/commit/3eb82a97e6c69c8390ed089b2327b6c74fb2a785))
- Document visual selection recovery ([eca0ad3](https://github.com/codersauce/red/commit/eca0ad394d92af5c00905a3d329125197269820c))
- Document diagnostics ui ([f59c82a](https://github.com/codersauce/red/commit/f59c82ac7a234acb6ea333588a74e67ece8c8e88))
- Add concepts hub ([fe1b59a](https://github.com/codersauce/red/commit/fe1b59a27a0ae707e45236fd35a66eb6c83167a2))
- Update release guide checks ([b2dcaa6](https://github.com/codersauce/red/commit/b2dcaa61e2eef04dd416ae3d5eb25d2471189b35))

## [0.5.0](https://github.com/codersauce/red/compare/v0.4.0...v0.5.0)

### Features

- **completion:** Add compact async completion menu ([#188](https://github.com/codersauce/red/issues/188)) ([21d16ba](https://github.com/codersauce/red/commit/21d16ba889cc44b97d3ba9ca41412fc280f03c60))
- **editor:** Add language-aware autoindent ([#184](https://github.com/codersauce/red/issues/184)) ([e72f7d2](https://github.com/codersauce/red/commit/e72f7d2db37d75fd564bc4d52c02edc458139afc))
- **completion:** Add buffer and automatic suggestions ([f987558](https://github.com/codersauce/red/commit/f987558706604314ab2f8ba9ce4be229bf77b67e))

### Bug Fixes

- **completion:** Exit insert mode on escape ([#187](https://github.com/codersauce/red/issues/187)) ([e10465a](https://github.com/codersauce/red/commit/e10465ae1b6ca40688b0858e31df5bac21ccf0f7))
- **completion:** Keep autocomplete valid while typing ([#186](https://github.com/codersauce/red/issues/186)) ([9d89583](https://github.com/codersauce/red/commit/9d89583e69afcb34810c9418725d1c926b3d295b))
- **completion:** Filter by existing identifier prefix ([1a15171](https://github.com/codersauce/red/commit/1a151719a82630600bbb28423970821345ca833e))
- **completion:** Insert newline when no matches remain ([#182](https://github.com/codersauce/red/issues/182)) ([b2cabfe](https://github.com/codersauce/red/commit/b2cabfe5e1bee01f6893604d724ba95a11b974cb))
- **editor:** Refresh cursor after inserting tab ([7fa7fcd](https://github.com/codersauce/red/commit/7fa7fcda312849b3d4c3a63a028a25436135fca7))

### Refactoring

- **languages:** Move Python support to official pack ([#185](https://github.com/codersauce/red/issues/185)) ([a152c33](https://github.com/codersauce/red/commit/a152c33db75472b6c63ddb6a6a379cee63c8b1e5))

### Continuous Integration

- **windows:** Pin ripgrep release archive ([96795fe](https://github.com/codersauce/red/commit/96795fe0e63f2582e51294578e010ff1b224d591))
- **windows:** Retry ripgrep installation ([11912e0](https://github.com/codersauce/red/commit/11912e060c47b64e0cf6d337d503e2c8d445ec9e))

### Other

- Update completion and CI validation ([59334f2](https://github.com/codersauce/red/commit/59334f2f59b57ea5693a0b74cea1eebb17c93824))
- Improve guide and cli navigation ([e2f2371](https://github.com/codersauce/red/commit/e2f2371110bf0da0d5327322778077b16d04bead))
- Improve wiki navigation and stale claims ([334c507](https://github.com/codersauce/red/commit/334c507bf3fefa2d2f06b97a79665c2b37615ef0))
- Refresh plugin api and graph links ([04b7e79](https://github.com/codersauce/red/commit/04b7e796a8515a4d84d60964b3216b95642de691))
- Document statusline defaults ([b080dbd](https://github.com/codersauce/red/commit/b080dbd5d470bf9358eb5c67b772668f8fafd32c))

## [0.4.0](https://github.com/codersauce/red/compare/v0.3.0...v0.4.0)

### Features

- **keymap:** Add statusline and plugin shortcuts ([a1fb15c](https://github.com/codersauce/red/commit/a1fb15c815ef472049e6bb385a2edb11877d6657))
- **statusline:** Add configurable layout and field catalog ([#178](https://github.com/codersauce/red/issues/178)) ([bf6f82f](https://github.com/codersauce/red/commit/bf6f82fea09617769830c43d56ca2fa63025a59b))
- **editor:** Refresh splash screen copy ([dd3b1e4](https://github.com/codersauce/red/commit/dd3b1e4f19e210083b796b50bc33fe2eb90edc3e))
- **languages:** Support static injections and incremental validation ([049cf21](https://github.com/codersauce/red/commit/049cf212c4235c860b7affec78386529420fb8d2))
- **languages:** Redesign language pack manager ([#175](https://github.com/codersauce/red/issues/175)) ([b1baf5e](https://github.com/codersauce/red/commit/b1baf5e10bb010edb7425ce808329facba27d365))
- **languages:** Add curated pack catalog ([#174](https://github.com/codersauce/red/issues/174)) ([6a40f85](https://github.com/codersauce/red/commit/6a40f8595849cae859e55a19cfeff643745df87c))
- **languages:** Identify standalone language packages ([e9cd71f](https://github.com/codersauce/red/commit/e9cd71f26401b959c4cfdbff53e60d1aa1fe291d))
- **languages:** Support extensible syntax and language servers ([3e66142](https://github.com/codersauce/red/commit/3e66142c09921164161e3388f75323559539878c))
- **editor:** Match Neovim line number behavior ([#172](https://github.com/codersauce/red/issues/172)) ([37e0e40](https://github.com/codersauce/red/commit/37e0e409d486cb23dd3caef74f3d38be71515b5c))
- **editor:** Add relative line numbers ([#171](https://github.com/codersauce/red/issues/171)) ([1d3637d](https://github.com/codersauce/red/commit/1d3637d69e79575fbacbac6c66682bebccba23c5))
- **plugins:** Decode host payloads into native husk types ([dd05341](https://github.com/codersauce/red/commit/dd05341dc3b3fae758d82ae027a4ac88b1df3903))
- **plugins:** Complete declarative plugin authoring ([e614f8d](https://github.com/codersauce/red/commit/e614f8d4ea3a61b5f5928927b3a60c55abe5680d))
- **husk:** Add declarative plugin command and event attributes ([2eb4630](https://github.com/codersauce/red/commit/2eb4630262aab9fc96411a4ea66ee2f1921ae57f))
- **fish:** Add syntax highlighting and language server support ([#170](https://github.com/codersauce/red/issues/170)) ([4328b5c](https://github.com/codersauce/red/commit/4328b5c73fb2909f0388267205a927da0e0a88e4))

### Bug Fixes

- **editor:** Restore terminal cursor visibility on exit ([56e4862](https://github.com/codersauce/red/commit/56e4862f27e1b14445257bbfd1b6975866d32217))
- **language-packs:** Accept supported host API versions ([#177](https://github.com/codersauce/red/issues/177)) ([9c27f7c](https://github.com/codersauce/red/commit/9c27f7c52be4315e872d88653e5361b0ab89ffd9))
- **editor:** Make global hotkeys work from panels ([#160](https://github.com/codersauce/red/issues/160)) ([931b7a0](https://github.com/codersauce/red/commit/931b7a02f5d864f5631da3cefc77aeacb1248c85))
- **languages:** Harden downloads and make reload rollback safe ([9908565](https://github.com/codersauce/red/commit/9908565f1e42e5d107046c61b05350185411497c))
- **languages:** Make package approval and server reloads robust ([4f20924](https://github.com/codersauce/red/commit/4f20924a9974c08f148e28736bee898c5f64d50b))
- **languages:** Harden grammar trust and route reloads ([267a070](https://github.com/codersauce/red/commit/267a070a4084ea2643904e3ec82240f9b1980f10))
- **languages:** Address trust and reload review feedback ([e273d93](https://github.com/codersauce/red/commit/e273d93a7bcbd7b8dfa9ab92f0a5b0ebf88d46e1))

### Refactoring

- **plugins:** Replace dynamic workspace and progress payloads ([4cc2b2a](https://github.com/codersauce/red/commit/4cc2b2ae04cacdd007d25b8576d693a1a8ffe6ab))
- **plugins:** Migrate bundled plugins to typed state ([895e29c](https://github.com/codersauce/red/commit/895e29c1b1019e1df3d5d8d356c81e2cf0019d21))
- **plugins:** Replace dynamic json with typed models ([7a16ec8](https://github.com/codersauce/red/commit/7a16ec81642ff2a6f4e02119113f48b8a37f03a4))

### Testing

- **editor:** Update global leader expectations ([e7feb7a](https://github.com/codersauce/red/commit/e7feb7aa776b1444bae0756110d1d3cd3c92afd4))
- **editor:** Gate terminal cleanup bytes to Unix ([3952f58](https://github.com/codersauce/red/commit/3952f587ea14cd940e9d70933a93056792b041d8))

### Maintenance

- **repo:** Rename default branch references ([53338f0](https://github.com/codersauce/red/commit/53338f02743f12db3797805af55a7dfac3dfce48))

### Other

- Improve Git commit editor workflow ([#179](https://github.com/codersauce/red/issues/179)) ([8d1342b](https://github.com/codersauce/red/commit/8d1342bcc7f7299fa3be3262a6fd04e53f84fa1d))
- Update main branch release guidance ([e31544e](https://github.com/codersauce/red/commit/e31544e8ed354a73b328ac02ed874b8ea2d2b9f0))
- Document terminal output test portability ([ac17c39](https://github.com/codersauce/red/commit/ac17c391284f969857f420d2c9493e2ff460a573))
- Improve wiki graph navigation ([de2d52d](https://github.com/codersauce/red/commit/de2d52dcd4796788ae840e23c571d97eaf48d7d2))
- Document Rio cursor trail rendering gotcha ([e9d5512](https://github.com/codersauce/red/commit/e9d55120ec7149a0eb0d46afd40311a6452d09af))
- Refresh host api compatibility ([577a01b](https://github.com/codersauce/red/commit/577a01bb4f2fe8d7c096317b1c321941d2f42788))
- Improve wiki graph routing ([06f38ec](https://github.com/codersauce/red/commit/06f38ec5709f25eb0a363423862b43f44d88f4f8))
- Document arborium language-pack source ([9bf51ee](https://github.com/codersauce/red/commit/9bf51eef57ba2850324161ecd8aba142d2bbea97))
- Add agent architecture hub ([f2f4222](https://github.com/codersauce/red/commit/f2f422276fdc6dc0b4aeb30ad1e609dd646d14ea))
- Capture language pack review constraints ([7d9a5dc](https://github.com/codersauce/red/commit/7d9a5dcdacb2f75801c94d88246d457eaf7d21d1))
- Record language pack distribution decision ([a508437](https://github.com/codersauce/red/commit/a5084370e9031aa101dab13c86f72e50eba6ce2f))

## [0.3.0](https://github.com/codersauce/red/compare/v0.2.4...v0.3.0)

### Features

- **languages:** Add curated pack catalog, verified release installs, and manager UI
- **picker:** Highlight file search matches ([96ffa91](https://github.com/codersauce/red/commit/96ffa91082b70ccbddc8c38e3866506fb35c5eec))
- **picker:** Improve file result ranking ([c4856c1](https://github.com/codersauce/red/commit/c4856c15534bb6ced6194d5369cd16b7d16aff43))
- **plugins:** Add external package platform ([cf32ae1](https://github.com/codersauce/red/commit/cf32ae18904a09205ba0f4c4d2a4a1463f1b13fd))
- **window:** Add interactive pane and split resizing ([#161](https://github.com/codersauce/red/issues/161)) ([cf86175](https://github.com/codersauce/red/commit/cf8617503277963ae115b4553e3a5d2916f7b7d3))
- **release:** Announce releases on Discord ([#154](https://github.com/codersauce/red/issues/154)) ([471e930](https://github.com/codersauce/red/commit/471e93005337c4b9d5efd2c054235532e41695cc))
- **window:** Add vim-style directional window moves ([#155](https://github.com/codersauce/red/issues/155)) ([9e6fe19](https://github.com/codersauce/red/commit/9e6fe195b7e13c63b6a5995e057b15888cd7a33d))

### Bug Fixes

- **barbecue:** Retain breadcrumbs during symbol refresh ([fd32186](https://github.com/codersauce/red/commit/fd321864c98f2ee471e98bc35447d1e2c908cc40))
- **editor:** Synchronize cursor transitions ([d5bb6ed](https://github.com/codersauce/red/commit/d5bb6ed277ec3381ece7cf1d7683d89bb4a75fb7))
- **editor:** Sync shift-i cursor position ([95cde5d](https://github.com/codersauce/red/commit/95cde5dbf7ab15313b51724f6a21f79fe3a88327))
- **ci:** Resolve clippy and wasmtime security failures ([9bf6de2](https://github.com/codersauce/red/commit/9bf6de2148de370c8b2a63ec6fbe855e1d85de5c))
- **plugins:** Validate default keymap prefixes ([12ff80b](https://github.com/codersauce/red/commit/12ff80b940dc46f4c209509c08faf5ed381510e0))
- **plugins:** Activate nested Husk packages ([904b293](https://github.com/codersauce/red/commit/904b2930c8a9fe9a917e1deffb1f28a90fe092d8))
- **editor:** Keep insert cursor aligned while typing ([0c36732](https://github.com/codersauce/red/commit/0c36732173052d746b16eea2f24a56a1126d49df))
- **cli:** Open nonexistent files on startup ([523ceda](https://github.com/codersauce/red/commit/523ceda35b0e083b6ab8f1762bbcaadae855e55e))
- **ci:** Support CodeAlmanac link routes ([fd38d07](https://github.com/codersauce/red/commit/fd38d07295fea2a60843edf010eb84bd530ebff7))
- **editor:** Match neovim motions and operators ([#157](https://github.com/codersauce/red/issues/157)) ([781da82](https://github.com/codersauce/red/commit/781da82347df6e12a41e20a96dcb20008963e9c5))
- **release:** Remap generated source paths ([f6077da](https://github.com/codersauce/red/commit/f6077dac0e3670f5e6bc0c960192897eb4093692))

### Refactoring

- **ui:** Unify terminal rendering and component primitives ([#159](https://github.com/codersauce/red/issues/159)) ([b7de490](https://github.com/codersauce/red/commit/b7de490269299e1eb008614a3d48ec96ca93d81c))

### Other

- Document panel-focused key dispatch ([8932431](https://github.com/codersauce/red/commit/8932431423dc848a47b089df96d3e24a79f20b27))
- Improve wiki graph navigation ([2c4b371](https://github.com/codersauce/red/commit/2c4b37108b774a392a368fcdd50b5751c147bf94))
- Build first wiki ([0a7e5fb](https://github.com/codersauce/red/commit/0a7e5fb7890005c58bf0d68210e31073745eca8d))

## [0.2.4](https://github.com/codersauce/red/compare/v0.2.3...v0.2.4)

### Features

- **neotree:** Add native-style tree scrolling ([fe39cc0](https://github.com/codersauce/red/commit/fe39cc08822ea4f65511045f29faec0922cc7d37))
- **neotree:** Add theme-aware tree colors ([612df4a](https://github.com/codersauce/red/commit/612df4a33842c2dd79c50cc1b5acff0808a5d7bb))
- **editor:** Add buffer-local syntax command ([#151](https://github.com/codersauce/red/issues/151)) ([42ad7fa](https://github.com/codersauce/red/commit/42ad7fa2929bb459943b0bb5d56a1c7b8e6de1f9))
- **husk:** Add native standard library ([#149](https://github.com/codersauce/red/issues/149)) ([4a79bc4](https://github.com/codersauce/red/commit/4a79bc49c7f91ad2314e3144f9ec779bf4f55cdd))
- **git:** Extract native husk core ([7d20a52](https://github.com/codersauce/red/commit/7d20a5202cdcaf26f6e56648976c40876b595314))
- **husk:** Add full language server ([de6a77d](https://github.com/codersauce/red/commit/de6a77de942d87d0bdd051644736a0cb0f90885f))
- **editor:** Highlight matching brackets in insert mode ([27cb966](https://github.com/codersauce/red/commit/27cb966c39d8e9472f8d354d902cdae7e2f40a4d))
- **editor:** Highlight matching bracket pairs ([ed72513](https://github.com/codersauce/red/commit/ed725130c981d70c16f9e0e083d1a4de4d7a1f2d))
- **editor:** Add neovim-style comment operators ([#138](https://github.com/codersauce/red/issues/138)) ([d5a953f](https://github.com/codersauce/red/commit/d5a953f324ab99f4d1d7e9dea0dd797d595c6e72))
- **husk:** Add generic specialization and grouped imports ([50ac178](https://github.com/codersauce/red/commit/50ac178a7347065c1c8443714efa71ea1a1c3790))
- **husk:** Add crate adapter workflow ([#136](https://github.com/codersauce/red/issues/136)) ([a5e4e39](https://github.com/codersauce/red/commit/a5e4e3993cabc2553fb2aca4f8907941879d08bb))
- **neotree:** Add file management actions ([fdfe517](https://github.com/codersauce/red/commit/fdfe51731f84b4bbd42fedb47d6b0b6d468d6913))
- **git:** Make the dashboard interactive and responsive ([#130](https://github.com/codersauce/red/issues/130)) ([dadde46](https://github.com/codersauce/red/commit/dadde46ea61f605a107d091cf9570ef7e80ae781))
- **husk:** Extract standalone language runtime ([#118](https://github.com/codersauce/red/issues/118)) ([b208bdd](https://github.com/codersauce/red/commit/b208bddcb185f8aceb9f5432882f3f9569736c4e))

### Bug Fixes

- **tui:** Inset right-aligned panel badges ([c0ef016](https://github.com/codersauce/red/commit/c0ef0166d12df75abc7a7fc7a171d748f5f6f905))
- **tui:** Match neotree directory status behavior ([9e85e9d](https://github.com/codersauce/red/commit/9e85e9d5d32ed52873376e61c7e320aa17ca3e23))
- **lsp:** Prevent pathological batching hangs ([7b126be](https://github.com/codersauce/red/commit/7b126be0d96c5cdb75768161a895b0c69a88db22))
- **editor:** Preserve YAML highlighting context ([c9b3f42](https://github.com/codersauce/red/commit/c9b3f4263f041bb5f7af0e156acd2e38f183807a))
- **lsp:** Surface initialization failures ([b6ae2ce](https://github.com/codersauce/red/commit/b6ae2ced1415840a5de53f2766ddfbbfe006cfd0))
- **editor:** Report failed searches ([781649e](https://github.com/codersauce/red/commit/781649e46eca703e9206d951a7ff4a7f7dbf3584))
- **editor:** Recover from missed terminal resizes ([#143](https://github.com/codersauce/red/issues/143)) ([f4d48a0](https://github.com/codersauce/red/commit/f4d48a076043a60388872eba6d1916ca585e4130))
- **husk:** Replace deprecated toml document alias ([ddc2fd7](https://github.com/codersauce/red/commit/ddc2fd7ce1e072803bde5616f6cc0acf4cc7e9a4))
- **editor:** Keep append cursor rendering in sync ([#139](https://github.com/codersauce/red/issues/139)) ([a036b73](https://github.com/codersauce/red/commit/a036b7385f886c2e53c8b141ec1264e99df86478))
- **lsp:** Avoid waiting after kill failure ([01bfd7a](https://github.com/codersauce/red/commit/01bfd7a584427494a0a22b442dd33cf1313fe06e))
- **lsp:** Bound editor shutdown latency ([1a5bf5f](https://github.com/codersauce/red/commit/1a5bf5fdef583a7301725b7220102ffd2ab813b9))
- **logging:** Use a cross-platform default path ([#133](https://github.com/codersauce/red/issues/133)) ([4380f51](https://github.com/codersauce/red/commit/4380f51d61e0161adae2235a3e31b0de71b2f306))
- **agent:** Honor required codex hooks ([dde7842](https://github.com/codersauce/red/commit/dde784206beb04c8f43147a120113bed9c1b356d))
- **theme:** Render consistent backgrounds ([#129](https://github.com/codersauce/red/issues/129)) ([b17f85e](https://github.com/codersauce/red/commit/b17f85ef9d6318582c388c31645001c61ca5d71e))
- **plugins:** Bound pathological callback workloads ([#128](https://github.com/codersauce/red/issues/128)) ([2dbf1b6](https://github.com/codersauce/red/commit/2dbf1b6a9f876d35ffcfa575e7414daf4e636622))

### Documentation

- Screenshots ([70dc651](https://github.com/codersauce/red/commit/70dc6518dd97744342f8f7ec677ac9ed6cca5755))

### Refactoring

- **neotree:** Reuse native husk standard library ([#152](https://github.com/codersauce/red/issues/152)) ([73db294](https://github.com/codersauce/red/commit/73db2945703d0bcd123604066b7b0066e92fb242))
- **git:** Reuse native husk standard library ([#150](https://github.com/codersauce/red/issues/150)) ([1d9e093](https://github.com/codersauce/red/commit/1d9e09345868d6a5c11eca844161f1271123657a))
- **editor:** Extract state controllers and tighten hot paths ([#132](https://github.com/codersauce/red/issues/132)) ([6ed3b86](https://github.com/codersauce/red/commit/6ed3b86d9610c6d41a816447dfb67fd5cf804f36))

### Continuous Integration

- **actions:** Accelerate trusted test matrix with warp ([#141](https://github.com/codersauce/red/issues/141)) ([567d660](https://github.com/codersauce/red/commit/567d660d9760b452182616375fe6d2001b16f318))

## [0.2.3](https://github.com/codersauce/red/compare/v0.2.2...v0.2.3)

### Bug Fixes

- **lsp:** Handle large symbol results ([#126](https://github.com/codersauce/red/issues/126)) ([f15626b](https://github.com/codersauce/red/commit/f15626bff60a3e8e6218cde47c4aa21308f514f0))

## [0.2.2](https://github.com/codersauce/red/compare/v0.2.1...v0.2.2)

### Features

- **picker:** Add semantic icons and colors ([#121](https://github.com/codersauce/red/issues/121)) ([833529b](https://github.com/codersauce/red/commit/833529b64b2a5b6d25b8f0923152d0f94b2e001f))
- **lsp:** Render rich hover documentation ([#122](https://github.com/codersauce/red/issues/122)) ([326b10d](https://github.com/codersauce/red/commit/326b10db66186b16814b91084e76ecd4c44e49de))

### Bug Fixes

- **lsp:** Size and position hover dialogs ([#124](https://github.com/codersauce/red/issues/124)) ([39ade74](https://github.com/codersauce/red/commit/39ade74ffc373e3555878a1492551764f34aeac7))

### Documentation

- Document architecture and safety contracts ([#123](https://github.com/codersauce/red/issues/123)) ([0d4d649](https://github.com/codersauce/red/commit/0d4d649fae4ecbf6d53c9a38bd1952656aaf1926))

## [0.2.1](https://github.com/codersauce/red/compare/v0.2.0...v0.2.1)

### Features

- **install:** Add verified cross-platform installers ([#117](https://github.com/codersauce/red/issues/117)) ([8994b4b](https://github.com/codersauce/red/commit/8994b4bf0c232ede7bc49729fa6327d55c4f2192))

### Documentation

- **readme:** Refresh v0.2 product guide ([#119](https://github.com/codersauce/red/issues/119)) ([6e8ccc8](https://github.com/codersauce/red/commit/6e8ccc8afbe2dec74ee6f7bd850f54b7c80293b0))

### Other

- Fix Windows warning cleanup ([71031b0](https://github.com/codersauce/red/commit/71031b07ed51d529f9251c9b93d7ee29297473cb))
- Fix Windows terminal colors and key input ([55a90a7](https://github.com/codersauce/red/commit/55a90a749bc112da0cef0bacf2afc7c514eb39cf))

## [0.2.0](https://github.com/codersauce/red/compare/v0.1.1...v0.2.0)

### Features

- **tui:** Complete command names with tab ([#108](https://github.com/codersauce/red/issues/108)) ([597be02](https://github.com/codersauce/red/commit/597be02dd2b371f6beccb55e52647e472ce84d26))
- **editor:** Add branded startup splash and red theme ([#113](https://github.com/codersauce/red/issues/113)) ([12754a0](https://github.com/codersauce/red/commit/12754a09deb41cbe12b57d69936acc7c6f7edc10))
- **core:** Recover from invalid user configuration ([#109](https://github.com/codersauce/red/issues/109)) ([e7adfd7](https://github.com/codersauce/red/commit/e7adfd7e4013d2d22373efae6072b66d238ee286))
- **agent:** Show live progress in conversation pane ([#111](https://github.com/codersauce/red/issues/111)) ([b6eb4a9](https://github.com/codersauce/red/commit/b6eb4a93f8294d139f5ee76c00467f428214435d))
- **core:** Integrate Codex app-server directly ([#110](https://github.com/codersauce/red/issues/110)) ([1214f35](https://github.com/codersauce/red/commit/1214f3536b294a921cff655f19c246d243fff863))
- **agent:** Improve conversation and editor interaction ([#106](https://github.com/codersauce/red/issues/106)) ([7fb8abc](https://github.com/codersauce/red/commit/7fb8abc6023eaffedb5c1d77eae3f669321f33b2))
- **picker:** Add command and keymap discovery ([#103](https://github.com/codersauce/red/issues/103)) ([fc8bee5](https://github.com/codersauce/red/commit/fc8bee5f27e709a8859ea22e67607b976d57d947))
- **vim:** Add editing and motion parity ([#102](https://github.com/codersauce/red/issues/102)) ([4db7541](https://github.com/codersauce/red/commit/4db75418adaff44c067e1580be028622d2936937))
- **agent:** Add the native agent foundation ([#100](https://github.com/codersauce/red/issues/100)) ([539c9e4](https://github.com/codersauce/red/commit/539c9e4c14fc1a4336175bef7aeb55f020f646cd))
- **tui:** Support visual selection changes ([8715102](https://github.com/codersauce/red/commit/8715102ce0b75775d6e63fd1583f673d4b20b972))
- **editor:** Add request callbacks and character motions ([9f4f7b4](https://github.com/codersauce/red/commit/9f4f7b4041ce30ecdeffefc00253fdd496301c88))
- **husk:** Report runtime errors with source spans ([90052d9](https://github.com/codersauce/red/commit/90052d96bf674ea481e5cc0502a21eaafa4a88ae))
- **husk:** Add source-aware diagnostics ([7699175](https://github.com/codersauce/red/commit/76991751b052709b30c97ed142565e2e70549bb1))
- **husk:** Restore inlay hint parity ([d30b286](https://github.com/codersauce/red/commit/d30b2868511e437a410972e7c529b7f68abb5e3c))
- **husk:** Restore fidget parity ([62b7ea9](https://github.com/codersauce/red/commit/62b7ea983f810e2036a7d51202209796d278c927))
- **husk:** Restore barbecue parity ([fcad7f1](https://github.com/codersauce/red/commit/fcad7f14cc45ecf73d3a6be0a7d5414d623bae55))
- **husk:** Restore git plugin parity ([a100dc3](https://github.com/codersauce/red/commit/a100dc392bc3dbb9767252064cbc371af06a4462))
- **husk:** Port barbecue breadcrumbs ([6304a64](https://github.com/codersauce/red/commit/6304a64106ce50561a7851ed8e50fbc076bd27c4))
- **husk:** Port fidget progress ([2501499](https://github.com/codersauce/red/commit/2501499edb3a3031a51c0442ab2a6941a07ae2f9))
- **husk:** Port inlay hints ([f165aaf](https://github.com/codersauce/red/commit/f165aafa0007244a2e4ba423fce988c93da3f32d))
- **husk:** Port session restore ([2696428](https://github.com/codersauce/red/commit/2696428acfa22a81ac8534cad01f41da96e26415))
- **husk:** Restore project search parity ([17b1f75](https://github.com/codersauce/red/commit/17b1f75d689078ebd3a396c844610ecba00ace72))
- **husk:** Restore theme browser parity ([3488f06](https://github.com/codersauce/red/commit/3488f0603f45905d2d30b06dbab0b474b78a8f5e))
- **husk:** Port buffer picker ([a08b1da](https://github.com/codersauce/red/commit/a08b1da4e8631513226723812ad49a55ce16e134))
- **husk:** Restore neotree sidebar ([575a1fb](https://github.com/codersauce/red/commit/575a1fb8b153bea42c01d4971553e7885a7d6636))
- **husk:** Port core plugins to runtime ([23746f8](https://github.com/codersauce/red/commit/23746f89cda63caea5388160026486ad27ffea46))
- **highlight:** Add husk syntax support ([44c3dbc](https://github.com/codersauce/red/commit/44c3dbc5e76108a1ba240b049e9d8d00e35cba3f))
- **plugin:** Replace deno runtime with husk ([9fa163b](https://github.com/codersauce/red/commit/9fa163b3b8c0de3c4086e536bfbf90c2fe10619b))
- **lua:** Add syntax and lsp support ([032f8d3](https://github.com/codersauce/red/commit/032f8d32f019da05cede75217a90b17fce2cdebf))
- **themes:** Port nvim color schemes ([#95](https://github.com/codersauce/red/issues/95)) ([a90cd73](https://github.com/codersauce/red/commit/a90cd73b77ff5ff8d81c1af125f2e5e94a4f265e))
- **highlighter:** Add powershell syntax support ([3a4007c](https://github.com/codersauce/red/commit/3a4007c2619a0b596e86a64f3d2d95a774654b1c))
- **keymap:** Add select-all leader binding ([147838f](https://github.com/codersauce/red/commit/147838f356ae8ff7bc04d592e6fb21c934c50fae))
- **editor:** Replace visual selections on paste ([620ecfa](https://github.com/codersauce/red/commit/620ecfa074f0c033688249f5311b57fe88519796))
- **editor:** Expand vim motion support ([549caa9](https://github.com/codersauce/red/commit/549caa93d9e3d8cd35d8ff68bc3964f92c570f53))
- **git:** Add native git integration ([aa4624a](https://github.com/codersauce/red/commit/aa4624ad63ead1b174c4b7eb1aaaf4e51f8aa709))
- **picker:** Toggle hidden and ignored files ([f9541e5](https://github.com/codersauce/red/commit/f9541e58c07542ef593967ee9874e2cec0fd74e2))

### Bug Fixes

- **picker:** Prioritize command actions on narrow screens ([#114](https://github.com/codersauce/red/issues/114)) ([c71c383](https://github.com/codersauce/red/commit/c71c383ef27d6088288d82d1b8e9a00d9a421c4d))
- **neotree:** Prevent instruction budget exhaustion ([#112](https://github.com/codersauce/red/issues/112)) ([82dd6ae](https://github.com/codersauce/red/commit/82dd6ae80a07693c6dfd387eaf89f7847ee1ddcb))
- **tui:** Report no-op action boundaries ([798643a](https://github.com/codersauce/red/commit/798643a2a2e747a855b97520f143a1346045a787))
- **git:** Render hunk navigation immediately ([#104](https://github.com/codersauce/red/issues/104)) ([696b76d](https://github.com/codersauce/red/commit/696b76dd86699a1dc81dfcf057b9ddecc71853f4))
- **tui:** Keep wrapped motion bottom anchored ([6ec6978](https://github.com/codersauce/red/commit/6ec697860f6611a2341d9a94e38af7e8b6ccb761))
- **editor:** Render command feedback after execution ([#101](https://github.com/codersauce/red/issues/101)) ([601b815](https://github.com/codersauce/red/commit/601b8157b9fe4ff6a35946ab075f1f0723ff58a0))
- **core:** Harden crash-prone editor paths ([#96](https://github.com/codersauce/red/issues/96)) ([26502b9](https://github.com/codersauce/red/commit/26502b9c8b8e704e14c71c96acfdaefe5a8b16db))
- **core:** Use production snapshots in self-check ([#97](https://github.com/codersauce/red/issues/97)) ([33de146](https://github.com/codersauce/red/commit/33de146b5b3b8b1ed7c702a8c9fa4bb472bea7f5))
- **editor:** Handle bracketed paste during resize ([cff7b92](https://github.com/codersauce/red/commit/cff7b9260442e7398781b745f8cecf7b003deed8))
- **husk:** Preserve integer division semantics ([a09b1c4](https://github.com/codersauce/red/commit/a09b1c490d78cb55654f26fe7ce4e1ee3ab52dac))
- **editor:** Repair theme and focus cursor behavior ([abf3709](https://github.com/codersauce/red/commit/abf3709968b55b7c65029ca8a09b172dd5dd6d74))
- **plugins:** Use serialized theme field names ([ccb36db](https://github.com/codersauce/red/commit/ccb36db56d88ab9946c23b86bb7dc41404a1f1e2))
- **husk:** Print diagnostics without rust prefix ([a34be6e](https://github.com/codersauce/red/commit/a34be6e83fb48267ed57e71f7efb2ab2985e759b))
- **husk:** Preserve project search history ([e7903de](https://github.com/codersauce/red/commit/e7903dea8b4b2fc9d0f5fbc45e6aa80d5be83ef2))
- **husk:** Sort inlay hints by position ([dd8a5a3](https://github.com/codersauce/red/commit/dd8a5a37271d76d6e53210eb8c23e2c1426e7465))
- **theme:** Enforce synthetic cursor contrast ([cbe586a](https://github.com/codersauce/red/commit/cbe586aca3d816381c5f198297d7c65c32d3b9b3))
- **editor:** Render tabs at configured stops ([4a170ad](https://github.com/codersauce/red/commit/4a170ad31d0b13062cece0dbfa50a7c8a6ae25a5))
- **theme:** Enforce accessible selection contrast ([45aac5d](https://github.com/codersauce/red/commit/45aac5dedb92702758da94df773bd31dcb0f7579))
- **neotree:** Keep selection and backgrounds visible ([1438125](https://github.com/codersauce/red/commit/143812578ee376dee03928d00d6a6610844b9ae2))
- **editor:** Prevent visual delete cursor underflow ([b73f0e6](https://github.com/codersauce/red/commit/b73f0e6c5be634ec04661f4f8adc9ba81f8f068e))
- **highlight:** Compose javascript family queries ([8ef42aa](https://github.com/codersauce/red/commit/8ef42aa8d4c8b1f050980497850efd32f24dec26))

### Performance

- **editor:** Optimize rendering and interactive hot paths ([#115](https://github.com/codersauce/red/issues/115)) ([5744fb2](https://github.com/codersauce/red/commit/5744fb2161e2bb97912b3dc80fdb16bbfc45f154))
- **tui:** Retain previous render frame ([#99](https://github.com/codersauce/red/issues/99)) ([9dab057](https://github.com/codersauce/red/commit/9dab05752a50970593eb1be1900c2fe0fa3a4c8b))
- **husk:** Optimize cursor plugin execution ([5a35292](https://github.com/codersauce/red/commit/5a352929542c28f51ee310705487a1fe4f5aee50))

### Documentation

- **plugin:** Document husk runtime accurately ([#98](https://github.com/codersauce/red/issues/98)) ([985c743](https://github.com/codersauce/red/commit/985c743666ca5c8ba9ec9632ac3a1fd23ef91d38))

### Refactoring

- **husk:** Use snake case plugin APIs ([42986d5](https://github.com/codersauce/red/commit/42986d583f2a2122b2bbf2f6305afe04cbd5b8e0))

### Testing

- **editor:** Cover focused panel cursor repaint ([47497b7](https://github.com/codersauce/red/commit/47497b7237eeedeea6915bb2562c7df0ab280088))
- **editor:** Cover visual paste size changes ([d7c8730](https://github.com/codersauce/red/commit/d7c8730038554ddfb42362362b0f7db8261c28f7))

### Maintenance

- **github:** Highlight husk files as rust ([ae5c43b](https://github.com/codersauce/red/commit/ae5c43bb315d275cbc1a63fb86c42c182d3d7393))

## [0.1.1](https://github.com/codersauce/red/compare/v0.1.0...v0.1.1)

### Bug Fixes

- **ci:** Normalize release checks across platforms ([571cf5c](https://github.com/codersauce/red/commit/571cf5c9b7cf02a48c97b0251b3b1f37af404f85))
- **release:** Make packaged runtime self-contained ([3c2c5e3](https://github.com/codersauce/red/commit/3c2c5e38810ba98dc0de43e5e62df81892455ffa))

## [0.1.0](https://github.com/codersauce/red/releases/tag/v0.1.0)

- Initial release.

<!-- generated by git-cliff -->
