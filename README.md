# mofi

```
 ███╗   ███╗ ██████╗ ███████╗██╗
 ████╗ ████║██╔═══██╗██╔════╝██║
 ██╔████╔██║██║   ██║█████╗  ██║
 ██║╚██╔╝██║██║   ██║██╔══╝  ██║
 ██║ ╚═╝ ██║╚██████╔╝██║     ██║
 ╚═╝     ╚═╝ ╚═════╝ ╚═╝     ╚═╝
```

> A fast, keyboard-driven launcher for macOS — built with Rust + egui.  
> App launcher · Clipboard history · Password Store (pass) integration.

---

## Features

- **App launcher** — fuzzy-search all installed `.app` bundles across `/Applications`, `/System/Applications`, and `~/Applications`
- **Clipboard history** — persistent, searchable history of everything you've copied (up to 100 entries)
- **Pass integration** — browse and copy passwords from your `~/.password-store` via [`pass`](https://www.passwordstore.org/) and `pinentry-mac`
- **Kanagawa colour scheme** — strictly themed with named palette tokens
- **Maple Mono NF** — Nerd Font glyphs for every app icon, no PNG loading
- **Daemon architecture** — persistent background process toggled via hotkeys through `skhd`; no Dock icon, no Cmd-Tab entry
- **Focus restore** — returns focus to the previously active app on dismiss

---

## Screenshots

![Apps](screenshots/screenshot1.png)
![Clipboard](screenshots/screenshot2.png)
![Pass](screenshots/screenshot3.png)
![About](screenshots/screenshot4.png)

---

## Requirements

| Tool | Purpose |
|------|---------|
| [Rust](https://rustup.rs) | Build toolchain |
| [skhd](https://github.com/koekeishiya/skhd) | Global hotkey daemon |
| [pass](https://www.passwordstore.org/) | Password store (optional) |
| [pinentry-mac](https://formulae.brew.sh/formula/pinentry-mac) | GPG PIN entry for pass (optional) |
| Maple Mono NF | Font — place `MapleMono-NF-Regular.ttf` and `MapleMono-NF-Medium.ttf` in `~/Library/Fonts/` |

```
brew install skhd pass pinentry-mac
```

---

## Build

```bash
git clone https://github.com/bechampion/mofi
cd mofi
cargo build --release
# binary at: target/release/mofi
```

---

## Setup

### 1. launchd — run mofi as a login daemon

Save the following to `~/Library/LaunchAgents/com.user.mofi.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.user.mofi</string>
    <key>ProgramArguments</key>
    <array>
        <string>/path/to/mofi/target/release/mofi</string>
        <string>--daemon</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/tmp/mofi-daemon.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/mofi-daemon.log</string>
</dict>
</plist>
```

Then load it:

```bash
launchctl load ~/Library/LaunchAgents/com.user.mofi.plist
```

### 2. skhd — global hotkeys

Add to `~/.skhdrc`:

```
cmd - space : /path/to/mofi/target/release/mofi --client
cmd - r     : /path/to/mofi/target/release/mofi --client
```

Reload skhd:

```bash
skhd --reload
```

### 3. pass + pinentry-mac (optional)

```bash
brew install pass pinentry-mac
```

Add to `~/.gnupg/gpg-agent.conf`:

```
pinentry-program /opt/homebrew/bin/pinentry-mac
```

Reload the agent:

```bash
gpgconf --kill gpg-agent
```

---

## Keybindings

| Key | Action |
|-----|--------|
| `Cmd+Space` / `Cmd+R` | Open mofi |
| `Escape` | Close / dismiss |
| `Tab` | Cycle through tabs (Apps → Clipboard → Pass → About) |
| `↓` / `Ctrl+J` | Move selection down |
| `↑` / `Ctrl+K` | Move selection up |
| `Enter` / double-click | Launch / copy selected item |

---

## Architecture

```
mofi --daemon     persistent egui window, hidden by default
mofi --client     sends SIGUSR1 to daemon to toggle visibility
```

Communication between client and daemon uses a Unix socket at `/tmp/mofi.sock`.  
The daemon PID is written to `/tmp/mofi.pid` on start.

When a Pass entry is selected, the daemon sends the entry name back to the client process, which runs `pass show <name>` (so that `pinentry-mac` can prompt for the GPG passphrase with a proper GUI dialog) and pipes the password to `pbcopy`.

---

## Colour Palette

All colours are from the [Kanagawa](https://github.com/rebelot/kanagawa.nvim) theme.

---

## License

MIT
