# Crossterm compatibility patch

`crossterm/` is the crates.io source of Crossterm 0.27.0 (MIT), whose archive
SHA-256 is `f476fe445d41c9e991fd07515a6f463074b782242ccf4a5b7b1d1012e70824df`.
The upstream license is retained in `crossterm/LICENSE`.
Git attributes preserve upstream line endings and Markdown whitespace while
marking the crate as vendored for review.

The local change teaches the existing Unix input parser to decode xterm
`modifyOtherKeys` sequences (`CSI 27 ; modifier ; codepoint ~`) using its
existing CSI-u decoder. This keeps one owner of the terminal byte stream and
preserves all ordinary Crossterm events. The corresponding parser tests cover
modified Enter, ordinary characters, and incomplete sequences. Three small
compiler-hygiene changes remove obsolete feature checks, make a test-only event
filter test-only, and remove redundant parentheses; these avoid warnings that
registry dependencies normally hide through Cargo's lint cap.
An empty workspace table and the retained upstream lockfile keep its test suite
independently runnable and reproducible.
Windows-only tests also exercise modified Enter through native console records.
The keyboard-capability query now bounds its optional device-attributes drain
and propagates reader errors, so an incomplete terminal reply cannot hang startup.

Remove the Cargo override when a registry release provides equivalent decoding
and passes Red's terminal-key compatibility tests. Crossterm 0.29.0 was checked
and still lacked this decoding case when this patch was introduced.
