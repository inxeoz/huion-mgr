use crate::config::Action;
use std::process::Command;

pub fn execute(action: &Action) {
    match action {
        Action::KeyCombo(combo) => {
            log::debug!("Key combo: {}", combo);
            // wtype simulates keyboard on Wayland
            let status = Command::new("wtype").arg(combo).status();
            match status {
                Ok(s) if !s.success() => log::warn!("wtype failed for '{}': exit {}", combo, s),
                Err(e) => log::error!("wtype not found or failed: {}", e),
                _ => {}
            }
        }
        Action::KeyPress(key) => {
            log::debug!("Key press: {}", key);
            let status = Command::new("wtype").args(["-k", key]).status();
            match status {
                Ok(s) if !s.success() => log::warn!("wtype key '{}' failed: exit {}", key, s),
                Err(e) => log::error!("wtype key error: {}", e),
                _ => {}
            }
        }
        Action::Command(cmd) => {
            log::debug!("Shell command: {}", cmd);
            let status = Command::new("sh").args(["-c", cmd]).status();
            match status {
                Ok(s) if !s.success() => log::warn!("Command '{}' exit {}", cmd, s),
                Err(e) => log::error!("Command '{}' failed: {}", cmd, e),
                _ => {}
            }
        }
        Action::Hyprctl(dispatch) => {
            log::debug!("Hyprctl dispatch: {}", dispatch);
            let status = Command::new("hyprctl")
                .args(["dispatch", dispatch])
                .status();
            match status {
                Ok(s) if !s.success() => log::warn!("hyprctl '{}' failed: exit {}", dispatch, s),
                Err(e) => log::error!("hyprctl error: {}", e),
                _ => {}
            }
        }
        Action::MouseClick(button) => {
            log::debug!("Mouse click: {}", button);
            // ydotool sends mouse input through uinput.
            let btn = match button.as_str() {
                "left" | "l" => "0x110",   // BTN_LEFT
                "right" | "r" => "0x111",  // BTN_RIGHT
                "middle" | "m" => "0x112", // BTN_MIDDLE
                other => other,
            };
            let status = Command::new("ydotool").args(["click", btn]).status();
            if status.is_err() {
                log::warn!("ydotool not available for mouse click");
            }
        }
        Action::MouseScroll(direction) => {
            let amount = match direction.to_ascii_lowercase().as_str() {
                "up" | "u" => "1",
                "down" | "d" => "-1",
                other => {
                    log::warn!(
                        "unknown scroll direction '{}'; use scroll:up or scroll:down",
                        other
                    );
                    return;
                }
            };
            log::debug!("Mouse scroll: {}", direction);
            // ydotool 1.0.4 sends a relative vertical wheel event through
            // `mousemove --wheel`; it has no `wheel` subcommand.
            let status = Command::new("ydotool")
                .args(["mousemove", "--wheel", "--", "0", amount])
                .status();
            match status {
                Ok(s) if !s.success() => {
                    log::warn!("ydotool mouse wheel '{}' failed: exit {}", direction, s)
                }
                Err(error) => log::error!("ydotool not found or failed: {}", error),
                _ => {}
            }
        }
        Action::Huion(value) => {
            // Huion drag is handled inside the daemon's hold-loop (pen tracking).
            // Direct `huion-mgr test` or key without daemon falls back to a hint.
            log::info!("Huion action '{value}' needs daemon hold; use as express_keys binding while daemon runs");
        }
        Action::None => {}
    }
}

/// Parse huion value like `pan`, `pan:left`, `pan:1.5`, `pan:left:0.8`, `pan:hold`, `pan:left:hold:1.5`
pub fn parse_huion(value: &str) -> (evdev::Key, f32, bool) {
    let lower = value.to_ascii_lowercase();
    let button = if lower.contains("left") {
        evdev::Key::BTN_LEFT
    } else if lower.contains("right") {
        evdev::Key::BTN_RIGHT
    } else {
        evdev::Key::BTN_MIDDLE
    };
    let hold = lower.contains("hold");
    let mut sensitivity = 1.0f32;
    for part in lower.split([':', ',', ' ', ';']) {
        if let Ok(v) = part.parse::<f32>() {
            if v > 0.0 && v < 20.0 {
                sensitivity = v;
            }
        }
    }
    (button, sensitivity, hold)
}
