use crate::config::Action;
use std::process::Command;

/// Modifier names accepted by `hold` actions, mapped to evdev key codes.
const MODIFIER_CODES: [(&str, u16); 8] = [
    ("shift", 42),
    ("leftshift", 42),
    ("ctrl", 29),
    ("control", 29),
    ("alt", 56),
    ("super", 125),
    ("meta", 125),
    ("capslock", 58),
];

/// Resolve a `hold` value (modifier name or evdev code) to a key code.
fn hold_key_code(value: &str) -> Option<u16> {
    let lower = value.to_ascii_lowercase();
    MODIFIER_CODES
        .iter()
        .find(|(name, _)| *name == lower)
        .map(|(_, code)| *code)
        .or_else(|| value.trim().parse::<u16>().ok())
}

/// Parse `combo:value` strings like `ctrl+z`, `ctrl+shift+equal`, or
/// `ctrl+plus` into wtype modifier names and the xkb key name.
/// Shifted symbols (plus, underscore) add the shift modifier automatically.
///
/// wtype 0.4 has no '+' combo syntax — its TEXT argument is typed literally —
/// so combos are emitted as `wtype -M <mod>... -k <key>`. Modifiers only
/// compose with keys from the same wtype instance; separate processes do not
/// share modifier state.
fn parse_combo(combo: &str) -> Option<(Vec<&'static str>, String)> {
    let mut mods = Vec::new();
    let mut extra_shift = false;
    let mut key = None;
    for part in combo.split('+') {
        let p = part.trim().to_ascii_lowercase();
        let mod_name = match p.as_str() {
            "ctrl" | "control" => Some("ctrl"),
            "shift" => Some("shift"),
            "alt" => Some("alt"),
            "super" | "meta" | "logo" | "win" => Some("logo"),
            "caps" | "capslock" => Some("capslock"),
            _ => None,
        };
        if let Some(name) = mod_name {
            if !mods.contains(&name) {
                mods.push(name);
            }
            continue;
        }
        let (name, shifted) = match p.as_str() {
            "plus" => ("equal", true),
            "underscore" => ("minus", true),
            "equal" => ("equal", false),
            "minus" => ("minus", false),
            "tab" => ("Tab", false),
            "return" | "enter" => ("Return", false),
            "space" => ("space", false),
            "esc" | "escape" => ("Escape", false),
            "backspace" => ("BackSpace", false),
            "left" => ("Left", false),
            "right" => ("Right", false),
            "up" => ("Up", false),
            "down" => ("Down", false),
            "home" => ("Home", false),
            "end" => ("End", false),
            "pagedown" | "page_down" => ("Page_Down", false),
            "pageup" | "page_up" => ("Page_Up", false),
            name if name.len() == 1 && name.as_bytes()[0].is_ascii_lowercase() => (name, false),
            name if name.len() == 1 && name.as_bytes()[0].is_ascii_digit() => (name, false),
            _ => return None,
        };
        if shifted {
            extra_shift = true;
        }
        key = Some(name.to_string());
    }
    if extra_shift && !mods.contains(&"shift") {
        mods.push("shift");
    }
    Some((mods, key?))
}

/// Press or release a held key through ydotool (`ydotool key <code>:<0|1>`).
///
/// wtype cannot hold a key down, so modifiers use the ydotool virtual
/// keyboard instead.
pub fn hold(action: &Action, pressed: bool) {
    let Action::Hold(value) = action else {
        return;
    };
    let Some(code) = hold_key_code(value) else {
        log::warn!("hold: unknown key name or code '{value}'; use shift, ctrl, alt, super, meta, or an evdev code");
        return;
    };
    let state = if pressed { 1 } else { 0 };
    let status = Command::new("ydotool")
        .args(["key", &format!("{code}:{state}")])
        .status();
    match status {
        Ok(s) if !s.success() => log::warn!("ydotool key {code}:{state} failed: exit {}", s),
        Err(e) => log::error!("ydotool key error: {e}"),
        _ => {}
    }
}

pub fn execute(action: &Action) {
    match action {
        Action::KeyCombo(combo) => {
            log::debug!("Key combo: {}", combo);
            let Some((mods, key)) = parse_combo(combo) else {
                log::warn!(
                    "combo '{}': unknown key name; use names like ctrl+z, ctrl+equal",
                    combo
                );
                return;
            };
            let mut cmd = Command::new("wtype");
            cmd.arg("-d").arg("5").arg("-s").arg("20");
            for m in &mods {
                cmd.arg("-M").arg(m);
            }
            cmd.arg("-k").arg(key);
            let status = cmd.status();
            match status {
                Ok(s) if !s.success() => log::warn!("wtype combo '{}' failed: exit {}", combo, s),
                Err(e) => log::error!("wtype combo '{}' failed: {}", combo, e),
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
        Action::Hold(value) => {
            // `keys test` runs the action once; a momentary tap exercises the
            // same ydotool path the daemon uses for press/release.
            let _ = hold_key_code(value);
            hold(action, true);
            hold(action, false);
        }
        Action::None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::parse_combo;

    #[test]
    fn parses_combos() {
        assert_eq!(parse_combo("ctrl+z"), Some((vec!["ctrl"], "z".to_string())));
        assert_eq!(
            parse_combo("ctrl+shift+equal"),
            Some((vec!["ctrl", "shift"], "equal".to_string()))
        );
        // plus needs shift on a US layout
        assert_eq!(
            parse_combo("ctrl+plus"),
            Some((vec!["ctrl", "shift"], "equal".to_string()))
        );
        assert_eq!(
            parse_combo("shift+underscore"),
            Some((vec!["shift"], "minus".to_string()))
        );
        assert_eq!(
            parse_combo("ctrl+tab"),
            Some((vec!["ctrl"], "Tab".to_string()))
        );
        assert_eq!(parse_combo("ctrl+2"), Some((vec!["ctrl"], "2".to_string())));
        assert_eq!(
            parse_combo("super+Return"),
            Some((vec!["logo"], "Return".to_string()))
        );
        assert_eq!(parse_combo("bogus+key"), None);
    }
}
