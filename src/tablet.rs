use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct InputDevice {
    pub name: String,
    pub event_path: PathBuf,
    pub device_type: DeviceType,
    pub phys: String,
}

#[derive(Debug, Clone)]
pub struct RawHidDevice {
    pub name: String,
    pub hidraw_path: PathBuf,
    pub phys: String,
    pub vendor_specific: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeviceType {
    Pen,
    ExpressKeys,
    Mouse,
    Unknown,
}

/// Detect Huion tablet input devices from /proc/bus/input/devices
pub fn detect_devices(pattern: &str) -> Vec<InputDevice> {
    let content = match std::fs::read_to_string("/proc/bus/input/devices") {
        Ok(c) => c,
        Err(e) => {
            log::error!("Cannot read /proc/bus/input/devices: {}", e);
            return Vec::new();
        }
    };

    let mut devices = Vec::new();
    let mut current_name = String::new();
    let mut current_phys = String::new();

    for line in content.lines() {
        if line.starts_with("N: Name=") {
            current_name = line
                .trim_start_matches("N: Name=\"")
                .trim_end_matches('"')
                .to_string();
        } else if line.starts_with("P: Phys=") {
            current_phys = line.trim_start_matches("P: Phys=").to_string();
        } else if line.starts_with("H: Handlers=") {
            let handlers = line.trim_start_matches("H: Handlers=");

            // Process this device entry
            if current_name
                .to_lowercase()
                .contains(&pattern.to_lowercase())
            {
                let device_type = classify_device(&current_name, handlers);
                let event_path = extract_event_path(handlers);

                if let Some(path) = event_path {
                    devices.push(InputDevice {
                        name: current_name.clone(),
                        event_path: path,
                        device_type,
                        phys: current_phys.clone(),
                    });
                }
            }
        }
    }

    devices
}

fn classify_device(name: &str, handlers: &str) -> DeviceType {
    let name_lower = name.to_lowercase();
    if name_lower.contains("pen") {
        DeviceType::Pen
    } else if name_lower.contains("keyboard") {
        DeviceType::ExpressKeys
    } else if name_lower.contains("mouse") || name_lower.contains("pad") {
        DeviceType::Mouse
    } else if handlers.contains("event") && handlers.contains("kbd") {
        DeviceType::ExpressKeys
    } else {
        DeviceType::Unknown
    }
}

fn extract_event_path(handlers: &str) -> Option<PathBuf> {
    for part in handlers.split_whitespace() {
        if part.starts_with("event") {
            return Some(PathBuf::from(format!("/dev/input/{}", part)));
        }
    }
    None
}

/// Return the H951P button bitmap from a vendor report.
///
/// Report byte 4 contains the low eight button bits and byte 5 contains the
/// high four button bits. The first byte is the HID report ID. The report-state
/// byte at index 1 is also part of a button's identity: `0xf1:0x0100` is
/// scroll-up while `0xe0:0x0100` is bottom-button1.
pub fn h951p_button_bitmap(report: &[u8]) -> Option<u16> {
    if report.first().copied() != Some(0x08) || report.len() < 6 {
        return None;
    }
    let bitmap = u16::from(report[4]) | (u16::from(report[5]) << 8);
    (bitmap != 0).then_some(bitmap)
}

/// Identify a logical H951P button as `(report-state, bitmap)`.
pub fn h951p_button_key(name: &str) -> Option<(u8, u16)> {
    let name = name.to_ascii_lowercase().replace('_', "-");
    match name.as_str() {
        "mode1" => Some((0xe3, 0x0001)),
        "mode2" => Some((0xe3, 0x0002)),
        "mode3" => Some((0xe3, 0x0004)),
        "top-button1" | "key-prog1" => Some((0xe0, 0x0008)),
        "top-button2" | "key-prog2" => Some((0xe0, 0x0010)),
        "top-button3" | "key-prog3" => Some((0xe0, 0x0020)),
        "top-button4" | "key-prog4" => Some((0xe0, 0x0040)),
        "scroll-up" => Some((0xf1, 0x0100)),
        "scroll-down" => Some((0xf1, 0x0200)),
        "bottom-button1" | "key-f13" => Some((0xe0, 0x0080)),
        "bottom-button2" | "key-f14" => Some((0xe0, 0x0100)),
        "bottom-button3" | "key-f15" => Some((0xe0, 0x0200)),
        "bottom-button4" | "key-f16" => Some((0xe0, 0x0400)),
        _ => None,
    }
}

fn h951p_button_name(state: u8, bit: u16) -> Option<&'static str> {
    match (state, bit) {
        (0xe3, 0x0001) => Some("mode1"),
        (0xe3, 0x0002) => Some("mode2"),
        (0xe3, 0x0004) => Some("mode3"),
        (0xe0, 0x0008) => Some("top-button1"),
        (0xe0, 0x0010) => Some("top-button2"),
        (0xe0, 0x0020) => Some("top-button3"),
        (0xe0, 0x0040) => Some("top-button4"),
        (0xf1, 0x0100) => Some("scroll-up"),
        (0xf1, 0x0200) => Some("scroll-down"),
        (0xe0, 0x0080) => Some("bottom-button1"),
        (0xe0, 0x0100) => Some("bottom-button2"),
        (0xe0, 0x0200) => Some("bottom-button3"),
        (0xe0, 0x0400) => Some("bottom-button4"),
        _ => None,
    }
}

/// Return all known logical buttons represented by a report.
pub fn h951p_button_keys(report: &[u8]) -> Option<Vec<(u8, u16)>> {
    let state = *report.get(1)?;
    let bitmap = h951p_button_bitmap(report)?;
    let keys = (0..16)
        .map(|shift| 1u16 << shift)
        .filter(|bit| bitmap & bit != 0)
        .filter(|bit| h951p_button_name(state, *bit).is_some())
        .map(|bit| (state, bit))
        .collect::<Vec<_>>();
    (!keys.is_empty()).then_some(keys)
}

pub fn h951p_button_description(report: &[u8]) -> Option<String> {
    let state = *report.get(1)?;
    let bitmap = h951p_button_bitmap(report)?;
    let mut names = Vec::new();
    for bit in (0..16).map(|shift| 1u16 << shift) {
        if bitmap & bit != 0 {
            names.push(h951p_button_name(state, bit).unwrap_or("unknown"));
        }
    }
    if names.is_empty() {
        names.push("unknown");
    }
    Some(format!(
        "state=0x{state:02x} button=0x{bitmap:04x} ({})",
        names.join(", ")
    ))
}

/// Detect HID raw interfaces belonging to a Huion tablet.
///
/// The official driver reads the vendor-specific interface directly instead of
/// relying only on the decoded `/dev/input/event*` keyboard device.
pub fn detect_raw_hid(pattern: &str) -> Vec<RawHidDevice> {
    let entries = match std::fs::read_dir("/sys/class/hidraw") {
        Ok(entries) => entries,
        Err(error) => {
            log::error!("Cannot read /sys/class/hidraw: {}", error);
            return Vec::new();
        }
    };
    let pattern = pattern.to_lowercase();
    let mut devices = Vec::new();

    for entry in entries.flatten() {
        let hidraw_name = entry.file_name().to_string_lossy().into_owned();
        let uevent_path = entry.path().join("device/uevent");
        let content = match std::fs::read_to_string(&uevent_path) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let fields = parse_uevent(&content);
        let name = fields.get("HID_NAME").cloned().unwrap_or_default();
        if !name.to_lowercase().contains(&pattern) {
            continue;
        }

        let phys = fields.get("HID_PHYS").cloned().unwrap_or_default();
        let descriptor =
            std::fs::read(entry.path().join("device/report_descriptor")).unwrap_or_default();
        let vendor_specific = descriptor
            .windows(3)
            .any(|window| window == [0x06, 0x00, 0xff]);

        devices.push(RawHidDevice {
            name,
            hidraw_path: PathBuf::from(format!("/dev/{hidraw_name}")),
            phys,
            vendor_specific,
        });
    }

    devices.sort_by_key(|device| (!device.vendor_specific, device.phys.clone()));
    devices
}

/// Prefer the vendor-specific HID interface used for Huion's proprietary reports.
pub fn find_vendor_hid(pattern: &str) -> Option<RawHidDevice> {
    detect_raw_hid(pattern)
        .into_iter()
        .find(|device| device.vendor_specific)
}

fn parse_uevent(content: &str) -> BTreeMap<String, String> {
    content
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

/// Find the express key device specifically
pub fn find_express_keys(pattern: &str) -> Option<InputDevice> {
    let devices = detect_devices(pattern);
    devices
        .into_iter()
        .find(|d| d.device_type == DeviceType::ExpressKeys)
}

/// Find the pen device (absolute tablet stylus)
pub fn find_pen_device(pattern: &str) -> Option<InputDevice> {
    let devices = detect_devices(pattern);
    devices
        .into_iter()
        .find(|d| d.device_type == DeviceType::Pen)
        .or_else(|| {
            // fallback: any device with pen in name already handled; try mouse that may carry ABS
            detect_devices(pattern)
                .into_iter()
                .find(|d| d.device_type == DeviceType::Mouse)
        })
}

/// List all detected tablet devices
pub fn list_all(pattern: &str) {
    let devices = detect_devices(pattern);
    if devices.is_empty() {
        println!("No devices found matching '{}'", pattern);
        return;
    }
    println!("Detected tablet devices:");
    for d in &devices {
        println!(
            "  [{:?}] {} @ {} ({})",
            d.device_type,
            d.name,
            d.event_path.display(),
            d.phys
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_h951p_button_names() {
        assert_eq!(h951p_button_key("scroll-up"), Some((0xf1, 0x0100)));
        assert_eq!(h951p_button_key("bottom_button1"), Some((0xe0, 0x0080)));
        assert_eq!(h951p_button_key("bottom_button4"), Some((0xe0, 0x0400)));
        assert_eq!(h951p_button_key("KEY_PROG1"), Some((0xe0, 0x0008)));
        assert_eq!(h951p_button_key("KEY_F13"), Some((0xe0, 0x0080)));
        assert_eq!(h951p_button_key("unknown"), None);
    }

    #[test]
    fn decodes_h951p_button_bitmap() {
        let report = [0x08, 0xe0, 0x01, 0x01, 0x80, 0x01];
        assert_eq!(h951p_button_bitmap(&report), Some(0x0180));
        assert_eq!(
            h951p_button_description(&report).as_deref(),
            Some("state=0xe0 button=0x0180 (bottom-button1, bottom-button2)")
        );

        let scroll_up = [0x08, 0xf1, 0x01, 0x01, 0x00, 0x01];
        assert_eq!(
            h951p_button_description(&scroll_up).as_deref(),
            Some("state=0xf1 button=0x0100 (scroll-up)")
        );
    }

    #[test]
    fn ignores_non_h951p_reports() {
        assert_eq!(h951p_button_bitmap(&[0x03, 0, 0, 0, 0, 1]), None);
        assert_eq!(h951p_button_bitmap(&[0x08, 0, 0]), None);
        assert_eq!(h951p_button_name(0xe0, 0x1000), None);
    }

    #[test]
    fn parses_hid_uevent_fields() {
        let fields = parse_uevent(
            "DRIVER=hid-generic\nHID_NAME=HUION Huion Tablet_H951P\nHID_PHYS=usb-1/input0\n",
        );
        assert_eq!(
            fields.get("HID_NAME"),
            Some(&"HUION Huion Tablet_H951P".to_string())
        );
        assert_eq!(fields.get("HID_PHYS"), Some(&"usb-1/input0".to_string()));
    }
}
