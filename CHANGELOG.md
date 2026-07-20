# Changelog

All notable changes to Red are documented in this file.

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
