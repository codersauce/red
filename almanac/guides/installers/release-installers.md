---
title: "Release Installers"
summary: "This guide explains how to verify Red's Unix and Windows release installers, including platform selection, checksum verification, self-check execution, fixture tests, and latest-release smoke tests."
topics: [guides, installers, release, validation, runtime-assets]
sources:
  - id: install-sh
    type: file
    path: install/install.sh
  - id: install-ps1
    type: file
    path: install/install.ps1
  - id: installer-workflow
    type: file
    path: .github/workflows/installers.yml
  - id: install-sh-test
    type: file
    path: tests/installers/install-sh.sh
  - id: install-ps1-test
    type: file
    path: tests/installers/install-ps1.ps1
---

Use this guide when installer scripts, release assets, checksums, supported platforms, or release instructions change. Red has two installers: a POSIX shell installer for macOS and glibc Linux tarballs, and a PowerShell installer for 64-bit Windows zip archives [@install-sh] [@install-ps1]. Both download the release archive and `SHA256SUMS.txt`, verify the archive checksum, install the binary, run `red --version`, and run `red --self-check`; an installer release is not ready until those behaviors are covered locally or by the installer workflow [@install-sh] [@install-ps1] [@installer-workflow].

## Check The Unix Installer

`install/install.sh` supports macOS ARM64, macOS x86_64, and Linux x86_64 or amd64 with glibc [@install-sh]. It rejects musl Linux, Linux ARM64, and unsupported OS or architecture pairs with explicit failure messages [@install-sh]. The installer reads `RED_VERSION`, `RED_INSTALL_DIR`, `RED_RELEASES_URL`, `RED_INSTALLER_OS`, and `RED_INSTALLER_ARCH`, defaulting to `latest`, `$HOME/.local/bin`, GitHub releases, `uname -s`, and `uname -m` respectively [@install-sh].

For a pinned release smoke test:

```shell
RED_VERSION=0.2.0 RED_INSTALL_DIR="$(mktemp -d)/bin" sh install/install.sh
```

The script maps `latest` to `/latest/download`, accepts versions with or without a leading `v`, requires `curl` and `tar`, downloads the target tarball and `SHA256SUMS.txt`, accepts either plain or `*`-prefixed checksum filenames, verifies with `sha256sum` or `shasum -a 256`, extracts `red`, stages the binary as `.red.install.$$`, moves it into place, then runs `--version` and `NO_COLOR=1 --self-check` [@install-sh]. It prints a PATH hint when the install directory is not already on `PATH` [@install-sh].

## Check The Windows Installer

`install/install.ps1` supports only 64-bit Windows and defaults to the latest release in `%LOCALAPPDATA%\Programs\Red\bin` unless `-Version`, `-InstallDir`, or environment variables override it [@install-ps1]. It downloads `red-x86_64-pc-windows-msvc.zip` and `SHA256SUMS.txt`, extracts the expected 64-character checksum entry, verifies with `Get-FileHash -Algorithm SHA256`, extracts `red.exe`, stages a replacement executable, and moves it into place [@install-ps1].

For a pinned release smoke test:

```powershell
./install/install.ps1 -Version 0.2.0 `
  -InstallDir (Join-Path $env:TEMP "red-release-check") -NoModifyPath
```

Without `-NoModifyPath`, the PowerShell installer appends the install directory to the user PATH if it is missing and updates the current process PATH as well [@install-ps1]. It runs `red.exe --version`, temporarily sets `NO_COLOR=1`, runs `red.exe --self-check`, restores the prior `NO_COLOR` state, and then prints the installed path and optional agent-support note [@install-ps1]. If it cannot replace an existing binary, it removes the staged file and tells the user to close running Red processes before retrying [@install-ps1].

## Run The Installer Workflow Equivalents

The installer workflow runs on installer, installer-test, workflow, README, release-doc, and release-workflow changes, and it can also be started manually [@installer-workflow]. It has three jobs:

| Job | What it proves |
| --- | --- |
| `lint` | Runs ShellCheck on the shell installer and fixture, installs PSScriptAnalyzer, and parses/analyzes the PowerShell installer through `tests/installers/install-ps1.ps1` [@installer-workflow]. |
| `unix-fixture` | Runs `sh tests/installers/install-sh.sh` on Ubuntu and macOS [@installer-workflow]. |
| `release-smoke` | Installs and verifies the latest release on Ubuntu, macOS Intel, macOS Apple Silicon, and Windows using temporary install directories [@installer-workflow]. |

The shell fixture builds a local fake release with a stub `red` binary, writes a matching checksum, installs from a `file://` release URL, verifies `red self-check ok`, reinstalls over the existing binary with a `v`-prefixed version, then proves checksum mismatch and unsupported architecture cases fail [@install-sh-test]. The PowerShell fixture parses the installer with the PowerShell parser and fails on PSScriptAnalyzer warnings or errors [@install-ps1-test].

## Verify Release Assets Before Publishing

Installer verification depends on the release workflow uploading both installer scripts and `SHA256SUMS.txt` beside the archives. The release runbook for the whole publication flow is [Release Red](../releases/release-red). After the draft release is built but before it is published, confirm each installer can find the exact archive name it expects for the target platform and that the checksum file contains matching entries.

After installation, both scripts rely on the binary's built-in self-check as the final runtime validation. Use [Self Check](../../reference/runtime/self-check) when a script downloads and installs successfully but fails at `red --self-check`, and use [Runtime Assets](../../architecture/runtime/runtime-assets) if the failure points at bundled plugins, themes, or embedded runtime data.
