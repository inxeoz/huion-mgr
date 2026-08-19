#!/usr/bin/env bash
# Install and validate huion-mgr without hiding failures.
set -u -o pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
BINARY="$ROOT_DIR/target/release/huion-mgr"
CONFIG="$HOME/.config/huion-mgr/config.toml"
FAILURES=0

info() { printf '[INFO] %s\n' "$*"; }
ok() { printf '[ OK ] %s\n' "$*"; }
warn() { printf '[WARN] %s\n' "$*"; FAILURES=$((FAILURES + 1)); }
manual() {
    warn "$1"
    printf '       Manual step: %s\n' "$2"
}

info "Project: $ROOT_DIR"

if [[ "$(uname -s)" != "Linux" ]]; then
    manual "Linux is required" "Run setup.sh on a Linux system."
fi

if [[ "$EUID" -eq 0 ]]; then
    manual "Do not run setup.sh as root" "Run it as your desktop user so config and ydotool use the correct user session."
fi

if [[ ! -f "$ROOT_DIR/Cargo.toml" ]]; then
    manual "Cargo.toml was not found" "Run setup.sh from the huion-mgr project directory."
fi

check_command() {
    local command_name="$1"
    local package_hint="$2"
    if command -v "$command_name" >/dev/null 2>&1; then
        ok "$command_name: $(command -v "$command_name")"
        return 0
    fi

    if command -v pacman >/dev/null 2>&1; then
        manual "$command_name is missing" "sudo pacman -S $package_hint"
    elif command -v apt >/dev/null 2>&1; then
        manual "$command_name is missing" "sudo apt install $package_hint"
    else
        manual "$command_name is missing" "Install '$package_hint' with your distribution package manager."
    fi
    return 1
}

info "Checking required commands"
check_command cargo rust || true
check_command wtype wtype || true
check_command ydotool ydotool || true
check_command ydotoold ydotool || true
check_command systemctl systemd || true

if command -v cargo >/dev/null 2>&1; then
    info "Checking Rust formatting"
    if (cd "$ROOT_DIR" && cargo fmt -- --check); then
        ok "cargo fmt -- --check"
    else
        manual "Rust formatting check failed" "cd '$ROOT_DIR' && cargo fmt"
    fi

    info "Running tests"
    if (cd "$ROOT_DIR" && cargo test); then
        ok "cargo test"
    else
        manual "Rust tests failed" "cd '$ROOT_DIR' && cargo test"
    fi

    info "Running Clippy"
    if (cd "$ROOT_DIR" && cargo clippy -- -D warnings); then
        ok "cargo clippy -- -D warnings"
    else
        manual "Clippy failed" "cd '$ROOT_DIR' && cargo clippy -- -D warnings"
    fi

    info "Building release binary"
    if (cd "$ROOT_DIR" && cargo build --release); then
        ok "Release binary built: $BINARY"
    else
        manual "Release build failed" "cd '$ROOT_DIR' && cargo build --release"
    fi
else
    manual "Cargo is missing; the Rust build was skipped" "Install Rust, then run '$ROOT_DIR/setup.sh' again."
fi

if [[ -x "$BINARY" ]]; then
    if [[ "$EUID" -eq 0 ]]; then
        manual "Binary installation was skipped because setup is running as root" "sudo install -Dm755 '$BINARY' /usr/local/bin/huion-mgr"
    elif command -v sudo >/dev/null 2>&1 && sudo -n install -Dm755 "$BINARY" /usr/local/bin/huion-mgr 2>/dev/null; then
        ok "Installed /usr/local/bin/huion-mgr"
    else
        manual "Could not install /usr/local/bin/huion-mgr without a sudo prompt" "sudo install -Dm755 '$BINARY' /usr/local/bin/huion-mgr"
    fi
fi

if [[ "$EUID" -eq 0 ]]; then
    manual "User config generation was skipped because setup is running as root" "Run setup.sh as your desktop user, then run: $BINARY config generate"
elif [[ -x "$BINARY" ]]; then
    if [[ -f "$CONFIG" ]]; then
        ok "Config already exists: $CONFIG"
    elif "$BINARY" config generate; then
        ok "Generated config: $CONFIG"
    else
        manual "Config generation failed" "$BINARY config generate"
    fi
else
    manual "Config generation was skipped because the binary is unavailable" "Build '$BINARY', then run '$BINARY config generate'."
fi

if command -v systemctl >/dev/null 2>&1 && command -v ydotoold >/dev/null 2>&1 && [[ "$EUID" -ne 0 ]]; then
    if systemctl --user enable --now ydotool.service; then
        ok "ydotool.service is enabled and running"
    else
        manual "Could not start the user ydotool service" "systemctl --user enable --now ydotool.service"
    fi
fi

huion_hidraw_found=0
for sys_device in /sys/class/hidraw/hidraw*; do
    [[ -e "$sys_device" ]] || continue
    if grep -qi '^HID_NAME=.*HUION' "$sys_device/device/uevent" 2>/dev/null; then
        hidraw_path="/dev/$(basename "$sys_device")"
        huion_hidraw_found=1
        if [[ -r "$hidraw_path" ]]; then
            ok "Huion HID access: $hidraw_path"
        else
            manual "Huion HID device is not readable: $hidraw_path" "Add your user to the input group, reconnect the tablet, and run: getfacl '$hidraw_path'"
        fi
    fi
done

if [[ "$huion_hidraw_found" -eq 0 ]]; then
    manual "No Huion hidraw device was detected" "Connect the H951P, then run: huion-mgr detect"
fi

printf '\n'
if [[ "$FAILURES" -eq 0 ]]; then
    ok "Setup complete"
    printf '     Start the daemon with:\n'
    printf '     huion-mgr --config "$HOME/.config/huion-mgr/config.toml" daemon\n'
    exit 0
fi

printf '[WARN] Setup completed with %d manual step(s).\n' "$FAILURES"
printf '       Resolve the steps above, then run setup.sh again.\n'
exit 1
