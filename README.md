# mofi

A keyboard-driven app launcher and productivity daemon for macOS, built with Rust + egui. Inspired by rofi.

## Features

- **App launcher** — fuzzy-search and launch any installed application
- **Pass integration** — browse and copy passwords from a `pass` store
- **Clipboard manager** — searchable clipboard history
- **Input selector** — pipe arbitrary lists into mofi and get a selection back (`mofi --input`)
- **Theme picker** — live-preview switching between Kanagawa colour themes (`mofi --themes`)
- **Menu bar icon** — persistent `NSStatusItem` with a custom rendered icon; click to open the About tab

## Menu bar icon

A 22×22 pt (44×44 px @2× retina) square rendered entirely in Rust at startup using [`ab_glyph`](https://crates.io/crates/ab_glyph):

| Element | Kanagawa colour | Value |
|---------|----------------|-------|
| Background | `oniViolet` | `#9360DC` |
| Glyph | `fujiWhite` | `#DCD7BA` |

Bold **M** in **Maple Mono NF Bold** (`~/Library/Fonts/MapleMono-NF-Bold.ttf`). Clicking the icon sends `show:about` to the daemon socket and opens mofi on the About tab.

## Installation

```sh
cargo build --release
./target/release/mofi --install   # writes launchd plist + skhd hotkeys
```

`--install` registers a launchd agent (`com.user.mofi`) that keeps the daemon running at login.

## Usage

| Command | Action |
|---------|--------|
| `Cmd+Space` | Toggle mofi launcher |
| `Cmd+Shift+P` | Open pass tab, copy selected password |
| `Cmd+Shift+Y` | Open clipboard tab, paste selected entry |
| `mofi --themes` | Live-preview theme switcher |
| `mofi --input` | Read lines from stdin, return selected line to stdout |
| `mofi --restart` | Reload the launchd daemon after a binary update |

## Dependencies

- [eframe](https://crates.io/crates/eframe) / [egui](https://crates.io/crates/egui) — UI
- [ab_glyph](https://crates.io/crates/ab_glyph) — icon glyph rasterisation
- [objc2](https://crates.io/crates/objc2) + [objc2-app-kit](https://crates.io/crates/objc2-app-kit) — `NSStatusItem` menu bar integration
- [signal-hook](https://crates.io/crates/signal-hook) — `SIGUSR1` wakeup signal
- [fuzzy-matcher](https://crates.io/crates/fuzzy-matcher) — fuzzy search

## Font

Maple Mono NF — place the `.ttf` files in `~/Library/Fonts/`. The bold variant (`MapleMono-NF-Bold.ttf`) is required for the menu bar icon; the regular variant is used in the UI.
