# Changelog

All notable changes to Red are documented in this file.

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
