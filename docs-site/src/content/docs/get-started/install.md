---
title: Installation
description: Install the agentflare binary on Linux, macOS, or Windows, and verify the download.
---

## Linux and macOS

The install script downloads a prebuilt binary. It verifies the SHA-256 checksum when
`sha256sum` or `shasum` is available and warns otherwise. Run from inside a clone, it
builds from source instead.

```bash
curl -fsSL https://agentflare.dev/install.sh | sh
```

## Homebrew

```bash
brew tap getappz/agentflare
brew install agentflare
```

## Windows

Scoop installs a prebuilt binary. It is not Authenticode-signed, so Defender or
SmartScreen may flag it; verify the release with cosign/SLSA instead of trusting the
signature.

```powershell
scoop bucket add agentflare https://github.com/getappz/agentflare
scoop install agentflare
```

To build from source, clone the repository and run the PowerShell installer (requires
Rust and a native linker):

```powershell
git clone https://github.com/getappz/agentflare
cd agentflare
.\install.ps1
```

## Any platform with Rust

```bash
cargo install --git https://github.com/getappz/agentflare
```

## Verify the CLI

```bash
agentflare --version
```

## Verify a release binary

The Unix installer checks SHA-256 when a checksum tool is present. Checksums catch a
corrupted download, not a swapped artifact. Releases are also signed with cosign (keyless,
via GitHub OIDC) and carry SLSA L3 provenance. The verification commands and the
threat-model table are in the repository's
[release verification section](https://github.com/getappz/agentflare/blob/master/README.md#verifying-release-binaries).

## Uninstall

```bash
curl -fsSL https://agentflare.dev/install.sh | sh -s -- --uninstall
```

`agentflare uninstall` removes hooks, configuration, and installed files for the current
user. See the [CLI reference](/docs/reference/cli/) for its flags.
