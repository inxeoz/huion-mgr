# huion-mgr

A Linux CLI for configuring Huion tablet express keys with a TOML file. For
the H951P it reads the vendor HID reports used by the official driver, and it
also monitors the tablet's keyboard interface, where the express keys surface
as plain keyboard keys (`KEY_I`, `KEY_E`, ...). The current default device
pattern detects the Huion H951P.

## Requirements

- Linux with `/dev/input/event*` devices
- A Huion tablet, tested with `HUION Huion Tablet_H951P` (`256c:0067`)
- Rust 1.70 or newer to build
- `wtype` for keyboard actions
- `ydotool` for mouse click and scroll actions
- `hyprctl` only when using `hyprctl:` actions

The `keys scan` command prints each decoded Linux key code only on its first
press. The `keys raw-scan` command reads the vendor-specific HID interface used
by the official Huion driver and prints raw reports plus changed byte
positions. Stop either scan with `Ctrl+C`.

On some systems (including the one this was built on), the vendor HID
interface stays silent and the express keys report as ordinary keyboard keys
through the tablet's keyboard device — pressing top-button4 here emits
`KEY_I`. Find out what your buttons emit by stopping the daemon and running
`keys scan`, then bind those names (`KEY_I`, `KEY_E`, ...) in the config. The
daemon grabs the tablet keyboard device, so the native letters the buttons
would type are suppressed and only the configured actions run.

For the H951P, the report state is part of the button identity:

```text
state 0xe3: 0x0001 mode1             0x0002 mode2
           0x0004 mode3

state 0xe0: 0x0008 top-button1        0x0010 top-button2
           0x0020 top-button3         0x0040 top-button4
           0x0080 bottom-button1     0x0100 bottom-button2
           0x0200 bottom-button3     0x0400 bottom-button4

state 0xf1: 0x0100 scroll-up         0x0200 scroll-down
```

For the raw HID path, bindings accept these button names or numeric bitmap
values (`0x0080`, `128`, etc.). Use names for scroll actions because the same
bitmap values can have different meanings in different report states. The
legacy names `KEY_PROG1` through `KEY_PROG4` and `KEY_F13` through `KEY_F16`
are also accepted as aliases for the top and bottom buttons.

The daemon polls three sources in one non-blocking loop: the vendor HID
interface, the tablet keyboard device (synced with the raw path), and the pen
device. If the vendor interface delivers reports, those bindings fire;
otherwise the keyboard bindings (`KEY_*` names from `keys scan`) do. All
devices are opened non-blocking so a silent device cannot stall the loop.

Run the daemon as your normal desktop user. Do not use `sudo` on Wayland:
`wtype` needs the user session variables `XDG_RUNTIME_DIR` and
`WAYLAND_DISPLAY` to connect to the compositor.

```bash
huion-mgr --config "$HOME/.config/huion-mgr/config.toml" daemon
```

The daemon also needs permission to read the tablet HID device. Add your user
to the `input` group if required by your distribution, then log in again:

```bash
sudo usermod -aG input "$USER"
```

## Build

```bash
cargo build --release
sudo install -Dm755 target/release/huion-mgr /usr/local/bin/huion-mgr
```

For a guided installation and validation, run this as your normal desktop
user:

```bash
./setup.sh
```

The script builds and tests the release binary, installs it when passwordless
`sudo` is available, generates the user config if missing, enables the user
`ydotool` service, checks Huion HID access, and prints manual commands for any
step it cannot complete. It never installs packages automatically.

## Config file

By default the file is:

```text
$XDG_CONFIG_HOME/huion-mgr/config.toml
```

If `XDG_CONFIG_HOME` is not set, the path is normally
`~/.config/huion-mgr/config.toml`. Use `--config PATH` to select another file.

Create and inspect a default file:

```bash
huion-mgr config generate
huion-mgr config show
```

`config generate` writes `$HOME/.config/huion-mgr/config.toml` and refuses to
overwrite an existing file. Use `huion-mgr config generate --force` to replace
it. `config.toml.example` in this repository is a complete editable example.

The generated format is compact: actions are inline tables and the pen section
is one line, so each binding stays readable.

```toml
[[express_keys]]
key = "KEY_I"
name = "top-button4 -> P"
action = { type = "key", value = "p" }
```

Example mapping:

```toml
tablet_name = "HUION Huion Tablet_H951P"

[[express_keys]]
key = "top-button1"
name = "Undo"
action = { type = "combo", value = "ctrl+z" }

[[express_keys]]
key = "bottom-button1"
name = "Open terminal"
action = { type = "command", value = "alacritty" }

# The same physical key can have a different action per mode.
[[express_keys]]
key = "top-button2"
mode = "mode1"
name = "Mode 1 next tab"
action = { type = "combo", value = "ctrl+tab" }

[[express_keys]]
key = "top-button2"
mode = "mode2"
name = "Mode 2 next workspace"
action = { type = "hyprctl", value = "dispatch workspace e+1" }
```

A binding without `mode` is the fallback for all modes. Pressing `mode1`,
`mode2`, or `mode3` selects the active layer. The daemon starts in `mode1`.

`key` and `combo` actions use `wtype`. `mouse` and `scroll` actions use
`ydotool`. `scroll:up` and `scroll:down` generate mouse-wheel events, while
`key:Up` and `key:Down` generate keyboard arrow keys. `ydotoold` must be
running for mouse and scroll actions:

```bash
systemctl --user enable --now ydotool.service
```

`hyprctl` actions use
`hyprctl dispatch`, and `command` actions run through `sh -c`. Only use
commands you trust.

## Commands

```bash
# Find the tablet's pen, express-key, and mouse event devices
huion-mgr detect

# List, set, remove, and test mappings
huion-mgr keys list
huion-mgr keys set top-button1 combo ctrl+z
huion-mgr keys set bottom-button1 key Return
huion-mgr keys set top-button2 command 'alacritty'
huion-mgr keys set top-button3 hyprctl 'dispatch workspace e+1'
huion-mgr keys set bottom-button1 mouse left
huion-mgr keys set scroll-up scroll up
huion-mgr keys set scroll-down scroll down
huion-mgr keys set top-button2 combo ctrl+z --mode mode1
huion-mgr keys set top-button2 combo ctrl+y --mode mode2
huion-mgr keys unset top-button2 --mode mode1
huion-mgr keys test top-button2 --mode mode2

# Print each unique decoded Linux key code while pressing tablet buttons
huion-mgr keys scan

# Print unique raw Huion HID reports and changed bytes
huion-mgr keys raw-scan

# Start the mapping daemon (raw HID + tablet keyboard, grabbed)
huion-mgr daemon
```

Supported action forms are `combo:value`, `key:value`, `command:value`,
`hyprctl:value`, `mouse:value`, `scroll:value`, and `none`. Short aliases are
`c`, `k`, `cmd`, `h`, `m`, and `s`.

`config set` supports these scalar values:

```bash
huion-mgr config set tablet_name 'HUION Huion Tablet_H951P'
huion-mgr config set pen.output all
huion-mgr config set hyprland.monitor DP-1
```

## Development

```bash
cargo fmt -- --check
cargo check
cargo test
```

## Scope

This project is CLI-only. It does not include a graphical user interface.
