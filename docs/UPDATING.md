# Updating Red

Use the update method that matches how you installed Red. The standalone updater
supports macOS Intel and Apple Silicon, x86_64 glibc Linux, and x86_64 Windows.

## Official standalone installations

Check for a newer stable release without changing your executable:

```shell
red update --check
```

Install the latest stable release:

```shell
red update
```

Inside the editor, run `:update` or choose **Update Red** in the command palette.
You can continue editing while the update worker runs. The updater verifies the
archive's published SHA-256 checksum, candidate version, and bundled runtime
self-check before replacing the executable. It uses an exclusive installation
lock to prevent concurrent updates.

Restart your editing sessions after a successful update. Running sessions keep
their current version. If a detached session is active, save and stop it before
retrying the update.

## Migrate an older standalone installation

If you installed Red before v0.8.2 or extracted an archive manually, rerun the
v0.8.2 installer once. It writes the installation receipt and lock file that
enable managed updates. Save and close Red first, including detached sessions.

On macOS or Linux, use your existing install directory. This example uses the
default `~/.local/bin`:

```shell
curl --proto '=https' --tlsv1.2 -fsSL \
  https://github.com/codersauce/red/releases/download/v0.8.2/install.sh \
  -o /tmp/red-install-v0.8.2.sh
RED_VERSION=0.8.2 RED_INSTALL_DIR="$HOME/.local/bin" \
  sh /tmp/red-install-v0.8.2.sh
```

On Windows, run the release's PowerShell installer. Replace `-InstallDir` if you
use a directory other than the default:

```powershell
$installer = Join-Path $env:TEMP "red-install-v0.8.2.ps1"
Invoke-WebRequest https://github.com/codersauce/red/releases/download/v0.8.2/install.ps1 -OutFile $installer
& $installer -Version 0.8.2 -InstallDir (Join-Path $env:LOCALAPPDATA "Programs\Red\bin")
```

Run `red --version` and `red update --check` to confirm the installed version and
check for subsequent releases.

## Homebrew and source builds

For Homebrew, run:

```shell
brew update
brew upgrade codersauce/tap/red
```

For a source build, rebuild and reinstall using your existing build process.
The standalone updater refuses to replace executables it doesn't own.

## Disable automatic checks

Red checks at most once per day by default and only notifies you; it never
installs a release unattended. Add this to `~/.config/red/config.toml` and restart
Red to disable those checks:

```toml
[updates]
mode = "off"
```

`red update --check`, `red update`, and `:update` remain available.

## Next steps

- Follow [Getting started](GETTING_STARTED.md) for day-to-day editor commands.
- See the [installation guide](../README.md#install) for fresh installations or
  source-build instructions.
