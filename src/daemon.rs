use crate::actions;
use crate::config::{AppConfig, KeyBinding};
use crate::tablet;
use evdev::{Device, EventType, InputEventKind, Key};
use std::collections::HashMap;
use std::io::Read;

type HuionKey = (u8, u16);

/// Run the express-key daemon.
///
/// H951P exposes its express keys through a vendor-specific HID report. Use
/// that path when available, and retain the evdev path as a fallback for other
/// tablets or systems without hidraw permissions.
pub fn run(config: &AppConfig) -> Result<(), String> {
    if let Some(raw_device) = tablet::find_vendor_hid(&config.tablet_name) {
        match run_raw(config, &raw_device) {
            Ok(()) => return Ok(()),
            Err(error) => log::warn!("Raw HID daemon unavailable: {error}; trying evdev"),
        }
    }
    run_evdev(config)
}

fn run_raw(config: &AppConfig, device_info: &tablet::RawHidDevice) -> Result<(), String> {
    let keymap = build_huion_keymap(config);
    let mut device = std::fs::File::open(&device_info.hidraw_path)
        .map_err(|error| format!("cannot open {}: {error}", device_info.hidraw_path.display()))?;

    log::info!(
        "Monitoring raw Huion keys: {} @ {}",
        device_info.name,
        device_info.hidraw_path.display()
    );
    log::info!(
        "Mapped {} Huion buttons across {} mode layers",
        keymap.values().map(Vec::len).sum::<usize>(),
        keymap.len()
    );

    let mut active_mode = "mode1";
    log::info!("Active Huion mode: {active_mode}");
    let mut previous_keys = std::collections::HashSet::new();
    let mut buffer = [0u8; 256];
    loop {
        let length = device
            .read(&mut buffer)
            .map_err(|error| format!("raw HID read error: {error}"))?;
        let report = &buffer[..length];
        let current_keys = tablet::h951p_button_keys(report)
            .unwrap_or_default()
            .into_iter()
            .collect::<std::collections::HashSet<_>>();

        for key in &current_keys {
            let is_new_press = !previous_keys.contains(key);
            // Wheel reports are pulses. The same raw state can be emitted for
            // multiple ticks, so dispatch every report instead of only the
            // first edge. Physical buttons remain edge-triggered.
            if !is_new_press && !is_scroll_key(*key) {
                continue;
            }

            if let Some(mode) = mode_for_key(*key) {
                active_mode = mode;
                log::info!("Active Huion mode: {active_mode}");
                continue;
            }

            if let Some(binding) = find_huion_binding(&keymap, *key, active_mode) {
                log::info!(
                    "Huion button: {} [{}] (state=0x{:02x}, bitmap=0x{:04x})",
                    binding.name,
                    active_mode,
                    key.0,
                    key.1
                );
                actions::execute(&binding.action);
            } else {
                log::debug!(
                    "Unmapped Huion button: state=0x{:02x}, bitmap=0x{:04x}, mode={active_mode}",
                    key.0,
                    key.1
                );
            }
        }
        previous_keys = current_keys;
    }
}

fn build_huion_keymap(config: &AppConfig) -> HashMap<HuionKey, Vec<&KeyBinding>> {
    let mut map: HashMap<HuionKey, Vec<&KeyBinding>> = HashMap::new();
    for binding in &config.express_keys {
        let key = binding.key.trim();
        let parsed =
            tablet::h951p_button_key(key).or_else(|| parse_number(key).map(|bit| (0xe0, bit)));
        match parsed {
            Some((state, bit)) if bit != 0 => {
                map.entry((state, bit)).or_default().push(binding);
            }
            _ => log::debug!("Ignoring non-H951P key identifier: {key}"),
        }
    }
    map
}

fn find_huion_binding<'a>(
    keymap: &'a HashMap<HuionKey, Vec<&'a KeyBinding>>,
    key: HuionKey,
    mode: &str,
) -> Option<&'a KeyBinding> {
    keymap.get(&key).and_then(|bindings| {
        bindings
            .iter()
            .find(|binding| binding.mode.as_deref() == Some(mode))
            .or_else(|| bindings.iter().find(|binding| binding.mode.is_none()))
            .copied()
    })
}

fn is_scroll_key(key: HuionKey) -> bool {
    matches!(key, (0xf1, 0x0100 | 0x0200))
}

fn mode_for_key(key: HuionKey) -> Option<&'static str> {
    match key {
        (0xe3, 0x0001) => Some("mode1"),
        (0xe3, 0x0002) => Some("mode2"),
        (0xe3, 0x0004) => Some("mode3"),
        _ => None,
    }
}

fn parse_number(value: &str) -> Option<u16> {
    value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .and_then(|hex| u16::from_str_radix(hex, 16).ok())
        .or_else(|| value.parse::<u16>().ok())
}

fn run_evdev(config: &AppConfig) -> Result<(), String> {
    let pattern = &config.tablet_name;
    let device_info = tablet::find_express_keys(pattern)
        .ok_or_else(|| format!("No express key device found matching '{pattern}'"))?;

    log::info!(
        "Monitoring decoded express keys: {} @ {}",
        device_info.name,
        device_info.event_path.display()
    );

    let device = Device::open(&device_info.event_path)
        .map_err(|e| format!("Cannot open {}: {}", device_info.event_path.display(), e))?;

    log::info!("Device: {}", device.name().unwrap_or("unknown"));
    let keymap = build_evdev_keymap(config);
    log::info!("Mapped {} decoded keys", keymap.len());

    let mut device = device;
    loop {
        match device.fetch_events() {
            Ok(events) => {
                for event in events {
                    if event.event_type() != EventType::KEY || event.value() != 1 {
                        continue;
                    }
                    if let InputEventKind::Key(code) = event.kind() {
                        if let Some(binding) = keymap.get(&code) {
                            log::info!("Express key: {} ({})", binding.name, binding.key);
                            actions::execute(&binding.action);
                        } else {
                            log::debug!("Unmapped key: {:?}", code);
                        }
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => {
                log::error!("Event read error: {}", error);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn build_evdev_keymap(config: &AppConfig) -> HashMap<Key, &KeyBinding> {
    let mut map = HashMap::new();
    for binding in &config.express_keys {
        let code = if binding.key.starts_with("KEY_") || binding.key.starts_with("BTN_") {
            match key_name_to_code(&binding.key) {
                Some(code) => Key::new(code),
                None => {
                    log::warn!("Unknown key identifier: {}", binding.key);
                    continue;
                }
            }
        } else if let Ok(n) = binding.key.parse::<u16>() {
            Key::new(n)
        } else {
            continue;
        };
        map.insert(code, binding);
    }
    map
}

/// Map common key names to their evdev codes.
fn key_name_to_code(name: &str) -> Option<u16> {
    match name {
        "KEY_PROG1" => Some(149),
        "KEY_PROG2" => Some(150),
        "KEY_PROG3" => Some(151),
        "KEY_PROG4" => Some(152),
        "KEY_F13" => Some(183),
        "KEY_F14" => Some(184),
        "KEY_F15" => Some(185),
        "KEY_F16" => Some(186),
        "KEY_F17" => Some(187),
        "KEY_F18" => Some(188),
        "KEY_F19" => Some(189),
        "KEY_F20" => Some(190),
        "KEY_F21" => Some(191),
        "KEY_F22" => Some(192),
        "KEY_F23" => Some(193),
        "KEY_F24" => Some(194),
        "KEY_LEFT" => Some(105),
        "KEY_RIGHT" => Some(106),
        "KEY_UP" => Some(103),
        "KEY_DOWN" => Some(108),
        "KEY_ENTER" => Some(28),
        "KEY_SPACE" => Some(57),
        "KEY_TAB" => Some(15),
        "KEY_ESC" => Some(1),
        "KEY_BACKSPACE" => Some(14),
        "KEY_DELETE" => Some(111),
        "KEY_HOME" => Some(102),
        "KEY_END" => Some(107),
        "KEY_PAGEUP" => Some(104),
        "KEY_PAGEDOWN" => Some(109),
        "KEY_INSERT" => Some(110),
        "KEY_LEFTCTRL" => Some(29),
        "KEY_LEFTSHIFT" => Some(42),
        "KEY_LEFTALT" => Some(56),
        "KEY_LEFTMETA" => Some(125),
        "BTN_LEFT" => Some(0x110),
        "BTN_RIGHT" => Some(0x111),
        "BTN_MIDDLE" => Some(0x112),
        "BTN_SIDE" => Some(0x113),
        "BTN_EXTRA" => Some(0x114),
        _ => None,
    }
}

/// Print current express key bindings.
pub fn show_bindings(config: &AppConfig) {
    println!("Express Key Bindings:");
    println!("{:<20} {:<10} {:<15} Action", "Key", "Mode", "Name");
    println!("{}", "-".repeat(72));
    for binding in &config.express_keys {
        let action_str = match &binding.action {
            crate::config::Action::KeyCombo(combo) => format!("combo: {combo}"),
            crate::config::Action::KeyPress(key) => format!("key: {key}"),
            crate::config::Action::Command(command) => format!("cmd: {command}"),
            crate::config::Action::Hyprctl(dispatch) => format!("hyprctl: {dispatch}"),
            crate::config::Action::MouseClick(button) => format!("mouse: {button}"),
            crate::config::Action::MouseScroll(direction) => format!("scroll: {direction}"),
            crate::config::Action::None => "(unbound)".to_string(),
        };
        println!(
            "{:<20} {:<10} {:<15} {}",
            binding.key,
            binding.mode.as_deref().unwrap_or("fallback"),
            binding.name,
            action_str
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{build_huion_keymap, find_huion_binding, mode_for_key, parse_number};
    use crate::config::{Action, AppConfig, KeyBinding};

    #[test]
    fn parses_decimal_and_hex_huion_codes() {
        assert_eq!(parse_number("128"), Some(128));
        assert_eq!(parse_number("0x0080"), Some(128));
    }

    #[test]
    fn mode_key_selects_mode_binding_then_fallback() {
        let mut config = AppConfig::default();
        config.express_keys = vec![
            KeyBinding {
                key: "top-button1".into(),
                name: "fallback".into(),
                mode: None,
                action: Action::KeyPress("A".into()),
            },
            KeyBinding {
                key: "top-button1".into(),
                name: "mode2".into(),
                mode: Some("mode2".into()),
                action: Action::KeyPress("B".into()),
            },
        ];
        let map = build_huion_keymap(&config);
        assert_eq!(
            find_huion_binding(&map, (0xe0, 0x0008), "mode2")
                .unwrap()
                .name,
            "mode2"
        );
        assert_eq!(
            find_huion_binding(&map, (0xe0, 0x0008), "mode3")
                .unwrap()
                .name,
            "fallback"
        );
    }

    #[test]
    fn scroll_reports_repeat() {
        assert!(super::is_scroll_key((0xf1, 0x0100)));
        assert!(super::is_scroll_key((0xf1, 0x0200)));
        assert!(!super::is_scroll_key((0xe0, 0x0008)));
    }

    #[test]
    fn recognizes_mode_buttons() {
        assert_eq!(mode_for_key((0xe3, 0x0001)), Some("mode1"));
        assert_eq!(mode_for_key((0xe0, 0x0008)), None);
    }
}
