# fad

[![CI](https://github.com/ntsk/fad/actions/workflows/ci.yml/badge.svg)](https://github.com/ntsk/fad/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

A CLI tool to upload, download, and install APK / AAB releases on Firebase App Distribution.

Unlike the Firebase App Tester app, `fad` installs a release straight onto a connected device from the command line (via `adb`) — no tester app to set up, no email invite, and no tapping through a UI. Because every step is a plain command, it slots neatly into CI pipelines and AI/agent workflows that need to grab a build and install it without a human in the loop.

## Requirements

- `adb` on PATH
- `bundletool` on PATH (used when installing AAB releases)
- A Google account with access to the target Firebase project

## Installation

### Homebrew

```bash
brew install ntsk/tap/fad
```

### Debian / Ubuntu

Download the `.deb` for your architecture (amd64 or arm64) from the [latest release](https://github.com/ntsk/fad/releases/latest), then:

```bash
sudo apt install ./fad-*.deb
```

### Arch Linux (AUR)

Available on the AUR as [`fad-bin`](https://aur.archlinux.org/packages/fad-bin):

```bash
yay -S fad-bin   # or: paru -S fad-bin
```

Without an AUR helper, build and install it with `makepkg`:

```bash
git clone https://aur.archlinux.org/fad-bin.git
cd fad-bin
makepkg -si
```

### Nix

```bash
nix run github:ntsk/fad
```

### From source

```bash
cargo install --path .
```

## Get Started

```bash
fad login          # Sign in with Google in the browser and pick the target app
fad releases       # List the app's releases
fad install <ID>   # Download and install one
```

`fad login` opens your browser to sign in, then lets you interactively pick a Firebase project and Android app. The choice is saved to `~/.config/fad/config.toml`, so you only do this once — no manual setup required.

## Usage

```
fad login                 # Sign in with your Google account in the browser and pick the target app
fad logout                # Revoke the token and delete the stored credentials
fad projects              # List accessible Firebase projects (* marks the current target)
fad use                   # Interactively switch the target project and app (no re-login needed)
fad use <PROJECT_ID>      # Pick an app from the given project
fad releases              # List releases of the target app
fad upload <FILE>         # Upload an APK/AAB as a new release
fad upload <FILE> -n MSG  # Upload and attach release notes (-n / --notes)
fad install <ID>          # Download and install a release
fad install <ID> --ks KS --ks-pass P --ks-key-alias A --key-pass P  # Sign the AAB with your own keystore
fad download <ID>         # Save a release binary to the current directory
fad download <ID> -o DIR  # Save into the given directory (-o / --output)
```

`download` saves the APK / AAB as is, named `{displayVersion}-{buildVersion}-{releaseId}.{apk,aab}`.

`releases`, `upload`, `install`, and `download` accept `--app <APP_ID>` to target a specific Firebase App ID, overriding the `app_id` in `config.toml`. This is handy in CI, where you can pass the target app without an interactive `fad use` or a committed config file.

To switch the target app, run `fad use` or edit `app_id` in `config.toml`.

## Configuration (optional)

fad works out of the box after `fad login`, so this section is only needed if you want to bypass the interactive picker or use your own OAuth client.

Set the target app manually by creating `~/.config/fad/config.toml`:

```toml
app_id = "1:1234567890:android:0a1b2c3d4e5f"
```

You can find the `app_id` in the Firebase console under Project settings > Your apps. The project number is derived from the `app_id` automatically.

By default, fad authenticates with the same public OAuth client as the Firebase CLI. To use your own OAuth client (desktop app type), add:

```toml
[oauth]
client_id = "..."
client_secret = "..."
```

## Service account (CI)

For non-interactive use such as CI, authenticate with a service account instead of `fad login`. Point `GOOGLE_APPLICATION_CREDENTIALS` at a service account JSON key, the same way the Firebase CLI does:

```bash
export GOOGLE_APPLICATION_CREDENTIALS=/absolute/path/to/service-account.json
fad upload app.apk -n "CI build"
```

The service account needs the **Firebase App Distribution Admin** role. When `GOOGLE_APPLICATION_CREDENTIALS` is set and you are not logged in, fad mints an access token from the key via the JWT bearer flow. A stored `fad login` session takes precedence, so this only kicks in on machines that have not logged in.

## How it works

- `fad login` performs OpenID Connect (OAuth 2.0) authentication in the browser and stores tokens in `~/.config/fad/credentials.json`
- With `GOOGLE_APPLICATION_CREDENTIALS` set (and no stored login), fad signs a JWT with the service account key and exchanges it for an access token
- APK releases are installed directly with `adb install -r`
- AAB releases are converted to a universal APK with `bundletool build-apks --mode=universal` before installing. By default bundletool signs it with the debug keystore at `~/.android/debug.keystore`; pass `--ks` / `--ks-pass` / `--ks-key-alias` / `--key-pass` to `fad install` to sign with your own keystore (e.g. the release key) instead. If no keystore is given and no debug keystore exists, `fad install` stops early rather than producing an unsigned, uninstallable APK

## Disclaimer

This is not an official Google or Firebase product. It is an unofficial tool that uses the Firebase App Distribution and Firebase Management REST APIs.

By default it authenticates with the public OAuth client that the Firebase CLI ships with, so signing in looks like signing in to the Firebase CLI. If you prefer, register your own OAuth client (desktop app type) and set it in the `[oauth]` section of the config file.

## License

Licensed under the [MIT License](LICENSE).
