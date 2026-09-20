# Packaging

## Release archive

Tags matching `v*` run the release workflow. For `v0.1.0` it builds the
CLI and GTK binaries on Linux x86_64 and publishes:

```text
niri-workbench-v0.1.0-x86_64-unknown-linux-gnu.tar.gz
SHA256SUMS
```

The archive contains both `niri-workbench` and `niri-workbench-ui`, the
desktop entry, README, changelog, license, install/uninstall scripts, and the
example PKGBUILD.

Verify an archive before installing:

```bash
sha256sum -c SHA256SUMS
tar -xzf niri-workbench-v0.1.0-x86_64-unknown-linux-gnu.tar.gz
cd niri-workbench-v0.1.0-x86_64-unknown-linux-gnu
./install.sh
```

The install script defaults to `~/.local/bin`. Set `PREFIX` to choose a
different prefix:

```bash
PREFIX="$HOME/.local" ./install.sh
```

It does not require root and does not edit Niri configuration.

## Arch Linux

`packaging/PKGBUILD` is an example source package for a tagged release. Review
the source URL and checksum before using it for an AUR submission. The project
does not publish to AUR automatically.

For a local check:

```bash
cp packaging/PKGBUILD /tmp/niri-workbench-pkgbuild
cd /tmp
makepkg -p niri-workbench-pkgbuild
```

A full desktop package should install the CLI binary, `niri-workbench-ui`,
and `dev.t1ktak.NiriWorkbench.desktop`. Headless/CLI-only packages may omit the GUI.
The default configuration remains user-owned at
`$XDG_CONFIG_HOME/niri-workbench/config.toml`.
