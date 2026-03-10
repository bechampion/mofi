# mofi

```
 ███╗   ███╗ ██████╗ ███████╗██╗
 ████╗ ████║██╔═══██╗██╔════╝██║
 ██╔████╔██║██║   ██║█████╗  ██║
 ██║╚██╔╝██║██║   ██║██╔══╝  ██║
 ██║ ╚═╝ ██║╚██████╔╝██║     ██║
 ╚═╝     ╚═╝ ╚═════╝ ╚═╝     ╚═╝
```

> A fast, keyboard-driven launcher built with Rust + egui.
> App launcher · Clipboard history · Password Store integration · Shell commands · Theme picker · Pipe-select mode.
>
> Runs on **macOS** (eframe) and **Linux/Wayland** (zwlr-layer-shell).

---

## Features

- **App launcher** — fuzzy-search installed applications with frecency-based sorting
  - macOS: scans `.app` bundles across `/Applications`, `/System/Applications`, and `~/Applications`
  - Linux: scans `.desktop` files from XDG data dirs, launched via `gtk-launch`
- **Shell command execution** — prefix any query with `!` in the Apps tab to run it as a shell command (e.g. `!killall waybar`, `!htop`). The command runs detached via `sh -c` with `setsid`
- **Clipboard history** — persistent, searchable history of everything you've copied (up to 100 entries)
  - macOS: polls the system pasteboard
  - Linux: polls `wl-paste`, pastes back via `wl-copy`. Supports **image clipboard** with thumbnail previews (80px-tall thumbnails generated at capture time)
  - Image entries are deduplicated by comparing actual PNG bytes
- **Pass integration** — browse and copy passwords from your `~/.password-store` via [`pass`](https://www.passwordstore.org/). Copied passwords are **auto-cleared from the clipboard after 45 seconds**
- **Theme picker** — 10 built-in themes with live preview; switch instantly from the Themes tab or `mofi --themes`
- **Pipe-select mode** — `mofi --input` reads lines from stdin, presents them as a fuzzy-searchable list, and prints the selected line to stdout (exit 0) or exits 1 on cancel
- **Maple Mono NF** — Nerd Font glyphs for every icon, no image loading
- **Frecency sorting** — frequently and recently used items rise to the top (stored in `~/.local/share/mofi/frecency.json`)
- **Daemon architecture** — persistent background process for instant window open times
  - macOS: managed via `launchd`, toggled via `skhd`; no Dock icon (`NSApplicationActivationPolicyAccessory`)
  - Linux: managed via systemd user service, renders via `zwlr_layer_shell_v1` (Wayland layer shell). Single-instance guard via `flock`. System tray icon via `ksni` (StatusNotifierItem) with Nerd Font glyphs
- **Focus restore** (macOS) — returns focus to the previously active app on dismiss

---

## Screenshots

![Apps](screenshots/screenshot1.png)
![Clipboard](screenshots/screenshot2.png)
![Pass](screenshots/screenshot3.png)
![About](screenshots/screenshot4.png)

---

## Requirements

### macOS

| Tool | Purpose |
|------|---------|
| [Rust](https://rustup.rs) | Build toolchain |
| [skhd](https://github.com/koekeishiya/skhd) | Global hotkey daemon |
| [pass](https://www.passwordstore.org/) | Password store (optional) |
| [pinentry-mac](https://formulae.brew.sh/formula/pinentry-mac) | GPG PIN entry for pass (optional) |
| Maple Mono NF | Font — place `MapleMono-NF-Regular.ttf` and `MapleMono-NF-Medium.ttf` in `~/Library/Fonts/` |

```bash
brew install skhd pass pinentry-mac
```

### Linux (Wayland)

| Tool | Purpose |
|------|---------|
| [Rust](https://rustup.rs) | Build toolchain |
| A Wayland compositor with `zwlr_layer_shell_v1` support | e.g. Hyprland, Sway |
| `wl-clipboard` (`wl-copy`, `wl-paste`) | Clipboard read/write |
| `gtk-launch` | Launching `.desktop` applications |
| [pass](https://www.passwordstore.org/) | Password store (optional) |
| Maple Mono NF | Font — install system-wide or in `~/.local/share/fonts/` |

```bash
# Arch
pacman -S wl-clipboard pass

# Debian/Ubuntu
apt install wl-clipboard pass
```

**Environment variable:** set `MOFI_SCALE` to match your display scale factor (e.g. `MOFI_SCALE=1.5` for 150% HiDPI).

---

## Build

```bash
git clone https://github.com/bechampion/mofi
cd mofi
cargo build --release
# binary at: target/release/mofi
```

The same codebase compiles on both macOS and Linux — platform-specific code is gated with `#[cfg(target_os = "...")]`.

---

## Setup

### macOS

#### Quick install

```bash
cargo build --release
./target/release/mofi --install
```

`--install` does everything automatically:

1. Writes `~/Library/LaunchAgents/com.user.mofi.plist` and loads it via `launchctl`
2. Appends hotkeys to `~/.skhdrc` and reloads `skhd`:
   - `Cmd+Space` — Apps tab
   - `Cmd+Shift+P` — Pass tab
   - `Cmd+Shift+Y` — Clipboard tab
3. Creates `~/.config/mofi/config.toml` with `theme = "kanagawa"` if it doesn't exist

<details>
<summary>Manual macOS setup</summary>

#### launchd plist

Save to `~/Library/LaunchAgents/com.user.mofi.plist`:

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

```bash
launchctl load ~/Library/LaunchAgents/com.user.mofi.plist
```

#### skhd hotkeys

Add to `~/.skhdrc`:

```
# mofi
cmd - space       : /path/to/mofi/target/release/mofi --client
cmd + shift - p   : /path/to/mofi/target/release/mofi --pass
cmd + shift - y   : /path/to/mofi/target/release/mofi --clip
```

```bash
skhd --reload
```

</details>

### Linux (Wayland)

#### systemd user service

Create `~/.config/systemd/user/mofi.service`:

```ini
[Unit]
Description=mofi launcher daemon
After=graphical-session.target

[Service]
ExecStart=/path/to/mofi/target/release/mofi --daemon
Environment=MOFI_SCALE=1.5
Restart=on-failure
KillMode=process

[Install]
WantedBy=default.target
```

```bash
systemctl --user daemon-reload
systemctl --user enable --now mofi.service
```

`KillMode=process` ensures launched applications survive when the daemon restarts.

#### Hyprland keybindings

Add to `~/.config/hypr/hyprland.conf`:

```
bind = SUPER, SPACE, exec, /path/to/mofi --client
bind = SUPER_SHIFT, P, exec, /path/to/mofi --pass
bind = SUPER_SHIFT, Y, exec, /path/to/mofi --clip
```

For Sway, add equivalent `bindsym` entries to `~/.config/sway/config`.

---

## Keybindings

| Key | Action |
|-----|--------|
| `Cmd+Space` / `Super+Space` | Open mofi on Apps tab |
| `Cmd+Shift+P` / `Super+Shift+P` | Open mofi on Pass tab |
| `Cmd+Shift+Y` / `Super+Shift+Y` | Open mofi on Clipboard tab |
| `Escape` | Close / dismiss |
| `Tab` | Cycle tabs: Apps → Clipboard → Pass → Themes → About |
| `↓` / `Ctrl+J` | Move selection down |
| `↑` / `Ctrl+K` | Move selection up |
| `Enter` | Launch / copy / confirm selected item |

### Apps tab special syntax

| Prefix | Behaviour |
|--------|-----------|
| *(none)* | Fuzzy-search installed applications |
| `!` | Shell command — e.g. `!killall waybar` shows a "Run: killall waybar" row; Enter executes it |

---

## Themes

10 built-in themes: `kanagawa` (default), `gruvbox`, `nord`, `tokyonight`, `dracula`, `solarized`, `monokai`, `catppuccin`, `onedark`, `rosepine`.

**From the UI** — navigate to the Themes tab. Use `↑`/`↓` or `Ctrl+K`/`Ctrl+J` for a live preview. Press `Enter` to confirm, `Escape` to cancel and restore the previous theme.

**From the command line:**

```bash
mofi --themes
```

The active theme is saved to `~/.config/mofi/config.toml`:

```toml
theme = "kanagawa"
```

---

## Pipe-select mode (`--input`)

`mofi --input` turns mofi into a general-purpose fuzzy picker. Feed it lines on stdin; it presents them in the launcher UI and prints the selected line to stdout.

```bash
# Pick a git branch and check it out
git branch | mofi --input | xargs git checkout

# Pick a running process and kill it
ps aux | awk '{print $2, $11}' | mofi --input | awk '{print $1}' | xargs kill
```

Exit codes: `0` = item selected (selected text on stdout), `1` = cancelled.

---

## Architecture

```
mofi --daemon     persistent egui window (hidden by default)
                    macOS: started via launchd
                    Linux: started via systemd, renders with zwlr_layer_shell_v1

mofi --client     toggle show/hide on Apps tab
mofi --pass       show mofi on the Pass tab
mofi --clip       show mofi on the Clipboard tab
mofi --input      pipe-select: reads stdin, sends items to daemon, prints selection
mofi --themes     theme picker with live preview
mofi --install    (macOS only) install plist, skhd hotkeys, create config
mofi --restart    (macOS only) reload the launchd agent
```

Communication uses a Unix socket at `/tmp/mofi.sock`. The daemon PID is written to `/tmp/mofi.pid`. A lock file at `/tmp/mofi.lock` (flock-based) ensures only one daemon instance runs.

On Linux, the daemon also exposes a system tray icon via StatusNotifierItem (ksni) with Nerd Font glyphs for quick tab access.

### IPC protocol

| Message | Direction | Description |
|---------|-----------|-------------|
| `ready\n` | client → daemon | Toggle show/hide on Apps tab |
| `show:pass\n` | client → daemon | Show window on Pass tab |
| `show:clip\n` | client → daemon | Show window on Clipboard tab |
| `tab:<name>\n` | client → daemon | Switch to a specific tab (Linux) |
| `input\t<l1>\t<l2>\t...\n` | client → daemon | Pipe-select mode |
| `themes\t<l1>\t<l2>\t...\n` | client → daemon | Theme-picker mode |
| `ok:<selected>\n` | daemon → client | Item was selected |
| `cancel\n` | daemon → client | User dismissed without selecting |

---

## Colour palette

The default `kanagawa` theme uses named palette tokens from [Kanagawa](https://github.com/rebelot/kanagawa.nvim). All themes use the same `Theme` struct — background, foreground, accent, border, separator, and row-highlight colours are all configurable per theme in `src/config.rs`.

---

## Window switcher (`mofisw`) — macOS only

`mofisw` is a separate binary — an Option+Tab window switcher that replaces the default macOS switcher with a keyboard-driven overlay showing all open windows across every app.

### Usage

```
Hold Option + press Tab   → show switcher / advance to next window
Keep pressing Tab          → cycle through all open windows
Release Option             → activate selected window and dismiss
```

### Setup

```bash
cargo build --release
cp target/release/mofisw /usr/local/bin/mofisw
```

Example launchd plist (`~/Library/LaunchAgents/com.user.mofisw.plist`):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.user.mofisw</string>
    <key>ProgramArguments</key>
    <array>
        <string>/path/to/mofisw</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
</dict>
</plist>
```

> **Accessibility permission required** — macOS will prompt on first launch.

---

## License

MIT
