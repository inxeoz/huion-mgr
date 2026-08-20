use crate::actions;
use crate::config::{AppConfig, KeyBinding};
use crate::tablet;
use evdev::{AbsoluteAxisType, Device, EventType, InputEvent, InputEventKind, Key, RelativeAxisType};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::os::unix::io::AsRawFd;

type HuionKey = (u8, u16);

/// Run the express-key daemon.
///
/// H951P exposes its express keys through a vendor-specific HID report. Use
/// that path when available, and retain the evdev path as a fallback for other
/// tablets or systems without hidraw permissions.
pub fn run(config: &AppConfig) -> Result<(), String> {
    let raw_devices = tablet::detect_raw_hid(&config.tablet_name);
    let mut tried = false;
    for raw_device in raw_devices.iter().filter(|d| d.vendor_specific) {
        tried = true;
        match run_raw(config, raw_device) {
            Ok(()) => return Ok(()),
            Err(error) => log::warn!("Raw HID {} unavailable: {error}", raw_device.hidraw_path.display()),
        }
    }
    if tried {
        log::warn!("All vendor hidraw failed; trying evdev fallback");
    } else {
        // no vendor hidraw found, try any hidraw?
        for raw_device in raw_devices.iter() {
            tried = true;
            match run_raw(config, raw_device) {
                Ok(()) => return Ok(()),
                Err(error) => log::warn!("Raw HID {} unavailable: {error}", raw_device.hidraw_path.display()),
            }
        }
    }
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
    let pen_keymap = build_pen_keymap(config);
    let mut device = std::fs::File::open(&device_info.hidraw_path)
        .map_err(|error| format!("cannot open {}: {error}", device_info.hidraw_path.display()))?;
    // ponytail: non-blocking hidraw + pen polling, 5ms loop; blocking read would stall pen tracking
    set_nonblocking(&device)?;

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

    // pen device for huion drag (two-finger pan emulation)
    let mut pen = open_pen_device(&config.tablet_name);
    let mut virt = None::<evdev::uinput::VirtualDevice>;
    let mut tablet_scale = pen.as_ref().map(pen_scale).unwrap_or((0.04, 0.04));

    let mut active_mode = "mode1";
    log::info!("Active Huion mode: {active_mode}");
    let mut previous_keys: HashSet<HuionKey> = HashSet::new();
    let mut huion_active: Option<(HuionKey, Key, f32, bool, bool)> = None; // (key, btn, sens, hold, is_drag)
    let mut last_pen: Option<(i32, i32)> = None;
    let mut rem_x: f32 = 0.0;
    let mut rem_y: f32 = 0.0;
    let mut last_toggle = std::time::Instant::now() - std::time::Duration::from_secs(1);
    let mut buffer = [0u8; 256];

    loop {
        // --- read hidraw (non-blocking) ---
        let read_res = device.read(&mut buffer);
        let current_keys: HashSet<HuionKey> = match read_res {
            Ok(len) if len > 0 => {
                let report = &buffer[..len];
                tablet::h951p_button_keys(report)
                    .unwrap_or_default()
                    .into_iter()
                    .collect()
            }
            Ok(_) => previous_keys.clone(),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => previous_keys.clone(),
            Err(e) => return Err(format!("raw HID read error: {e}")),
        };
        let new_report = !matches!(read_res, Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock);

        if new_report {
            // detect new presses and releases
            for key in &current_keys {
                let is_new_press = !previous_keys.contains(key);
                if !is_new_press && !is_scroll_key(*key) {
                    continue;
                }
                if let Some(mode) = mode_for_key(*key) {
                    active_mode = mode;
                    log::info!("Active Huion mode: {active_mode}");
                    continue;
                }
                if let Some(binding) = find_huion_binding(&keymap, *key, active_mode) {
                    match &binding.action {
                        crate::config::Action::Huion(val) => {
                            // toggle: press once to enter pan, press again to exit (H951P buttons are momentary pulse)
                            if huion_active.is_none() {
                                let (btn, sens, hold) = actions::parse_huion(val);
                                let is_drag = val.to_ascii_lowercase().contains("drag");
                                if last_toggle.elapsed() < std::time::Duration::from_millis(400) {
                                    log::debug!("Huion pan start debounce, ignoring");
                                    continue;
                                }
                                last_toggle = std::time::Instant::now();
                                log::info!(
                                    "Huion pan start: {} [{}] btn={:?} sens={} (state=0x{:02x}, bitmap=0x{:04x}) [press again to stop]",
                                    binding.name, active_mode, btn, sens, key.0, key.1
                                );
                                // ensure pen + virt ready
                                if pen.is_none() {
                                    pen = open_pen_device(&config.tablet_name);
                                    tablet_scale = pen.as_ref().map(pen_scale).unwrap_or(tablet_scale);
                                }
                                if pen.is_none() {
                                    log::warn!("Huion pan needs pen device but none found; fix: `sudo usermod -aG input $USER` + re-login, then `huion-mgr read-raw` to verify");
                                    huion_active = Some((*key, btn, sens, hold, is_drag));
                                    last_pen = None;
                                } else {
                                    virt = virt.or_else(|| create_virtual_device().ok());
                                    if virt.is_none() {
                                        log::warn!("cannot create virtual mouse (/dev/uinput); huion pan disabled (need /dev/uinput rw, `sudo usermod -aG input $USER`)");
                                        huion_active = Some((*key, btn, sens, hold, is_drag));
                                        last_pen = None;
                                        rem_x = 0.0; rem_y = 0.0;
                                    } else {
                                        if is_drag {
                                            let _ = emit_key(virt.as_mut().unwrap(), btn, 1);
                                        }
                                        huion_active = Some((*key, btn, sens, hold, is_drag));
                                        last_pen = None;
                                        rem_x = 0.0; rem_y = 0.0;
                                    }
                                }
                            } else {
                                // hold vs toggle: hold ignores second press, toggle stops
                                if last_toggle.elapsed() < std::time::Duration::from_millis(400) {
                                    log::debug!("Huion pan toggle debounce, ignoring");
                                } else if let Some((active_key, btn, _, active_hold, active_is_drag)) = huion_active {
                                    if active_hold {
                                        log::debug!("Huion hold active, ignoring second press");
                                    } else if active_key == *key {
                                        log::info!("Huion pan stop (toggle): btn={:?} [press again to start] drag={}", btn, active_is_drag);
                                        if active_is_drag {
                                            if let Some(v) = virt.as_mut() {
                                                let _ = emit_key(v, btn, 0);
                                            }
                                        }
                                        huion_active = None;
                                        last_pen = None;
                                        rem_x = 0.0; rem_y = 0.0;
                                        last_toggle = std::time::Instant::now();
                                    } else {
                                        log::info!("Huion pan switch: already active btn={:?}, ignoring {:?}", btn, key);
                                    }
                                }
                            }
                        }
                        _ => {
                            // normal actions still edge-triggered; scroll repeats every report
                            log::info!(
                                "Huion button: {} [{}] (state=0x{:02x}, bitmap=0x{:04x})",
                                binding.name, active_mode, key.0, key.1
                            );
                            actions::execute(&binding.action);
                        }
                    }
                } else {
                    log::info!(
                        "Unmapped Huion button: state=0x{:02x}, bitmap=0x{:04x}, mode={active_mode} -> use `huion-mgr read-raw --exclude movement` to see id",
                        key.0, key.1
                    );
                }
            }
            previous_keys = current_keys.clone();
            // hold mode: release stops (for --hold)
            if let Some((active_key, btn, _, hold_active, is_drag_hold)) = huion_active {
                if hold_active && !current_keys.contains(&active_key) {
                    log::info!("Huion pan stop (hold release): btn={:?} drag={}", btn, is_drag_hold);
                    if is_drag_hold {
                        if let Some(v) = virt.as_mut() {
                            let _ = emit_key(v, btn, 0);
                        }
                    }
                    huion_active = None;
                    last_pen = None;
                    rem_x = 0.0; rem_y = 0.0;
                    last_toggle = std::time::Instant::now();
                }
            }
        }

                // --- pen tracking + pen buttons (always poll pen, pan only when huion_active) ---
        if let Some(pen_dev) = pen.as_mut() {
            // collect pen moves for pan
            let mut dx_acc: i32 = 0;
            let mut dy_acc: i32 = 0;
            let mut have_move = false;
            // need huion_active details for pan
            let pan_info = huion_active;
            match pen_dev.fetch_events() {
                Ok(events) => {
                    for ev in events {
                        match ev.kind() {
                            InputEventKind::AbsAxis(AbsoluteAxisType::ABS_X) => {
                                if let Some((_, _, sens, _, is_drag)) = pan_info {
                                    let scale = tablet_scale;
                                    let x = ev.value();
                                    if let Some((lx, _)) = last_pen {
                                        let raw_dx = (x - lx) as f32 * scale.0 * sens + rem_x;
                                        let dx = raw_dx as i32;
                                        rem_x = raw_dx - dx as f32;
                                        if dx != 0 {
                                            dx_acc += dx;
                                            have_move = true;
                                        }
                                        last_pen = Some((x, last_pen.unwrap().1));
                                    } else {
                                        let y = last_pen.map(|p| p.1).unwrap_or(0);
                                        last_pen = Some((x, y));
                                    }
                                    // store is_drag for later emit
                                    let _ = is_drag;
                                } else {
                                    // not in pan, still update last_pen for next pan start
                                    let x = ev.value();
                                    if let Some((_, ly)) = last_pen {
                                        last_pen = Some((x, ly));
                                    } else {
                                        last_pen = Some((x, 0));
                                    }
                                }
                            }
                            InputEventKind::AbsAxis(AbsoluteAxisType::ABS_Y) => {
                                if let Some((_, _, sens, _, _)) = pan_info {
                                    let scale = tablet_scale;
                                    let y = ev.value();
                                    if let Some((_, ly)) = last_pen {
                                        let raw_dy = (y - ly) as f32 * scale.1 * sens + rem_y;
                                        let dy = raw_dy as i32;
                                        rem_y = raw_dy - dy as f32;
                                        if dy != 0 && (last_pen.unwrap().1 != 0 || ly != 0) {
                                            dy_acc += dy;
                                            have_move = true;
                                        }
                                        last_pen = Some((last_pen.unwrap().0, y));
                                    } else {
                                        last_pen = Some((0, y));
                                    }
                                } else {
                                    let y = ev.value();
                                    if let Some((lx, _)) = last_pen {
                                        last_pen = Some((lx, y));
                                    } else {
                                        last_pen = Some((0, y));
                                    }
                                }
                            }
                            InputEventKind::Key(k) => {
                                // pen buttons: BTN_STYLUS / BTN_STYLUS2
                                if ev.value() == 1 {
                                    if let Some(bindings) = pen_keymap.get(&k) {
                                        if let Some(b) = find_pen_binding(bindings, active_mode) {
                                            log::info!("Pen button: {} ({:?})", b.name, k);
                                            actions::execute(&b.action);
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => {
                    log::debug!("pen fetch error: {e}");
                }
            }
            if let Some((_, btn, _, _, is_drag)) = pan_info {
                if have_move && (dx_acc != 0 || dy_acc != 0) {
                    if is_drag {
                        if let Some(v) = virt.as_mut() {
                            let _ = emit_relative(v, dx_acc, dy_acc);
                        } else {
                            let _ = std::process::Command::new("ydotool")
                                .args(["mousemove", "-x", &dx_acc.to_string(), "-y", &dy_acc.to_string()])
                                .status();
                        }
                    } else {
                        if let Some(v) = virt.as_mut() {
                            let wx = dx_acc / 2;
                            let wy = dy_acc / 2;
                            if wx != 0 || wy != 0 {
                                let _ = emit_scroll(v, wx, wy);
                            }
                        } else {
                            if dy_acc != 0 {
                                let _ = std::process::Command::new("ydotool")
                                    .args(["mousemove", "--wheel", "--", "0", &dy_acc.to_string()])
                                    .status();
                            }
                            if dx_acc != 0 {
                                let _ = std::process::Command::new("ydotool")
                                    .args(["mousemove", "--wheel", "--", &dx_acc.to_string(), "0"])
                                    .status();
                            }
                        }
                    }
                    log::debug!("huion pan move dx={} dy={} btn={:?} drag={}", dx_acc, dy_acc, btn, is_drag);
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[allow(dead_code)]
fn cached_abs(_dev: &Device, _axis: AbsoluteAxisType) -> Option<i32> {
    // ponytail: keep minimal; pen tracking uses per-axis last value, no need for cached query
    None
}

fn pen_scale(_dev: &Device) -> (f32, f32) {
    // ponytail: fixed 1920x1080 target; real screen width varies, tune via huion sensitivity
    // Estimate tablet range via ABS info if we can read it; otherwise 0.04.
    // Try to query via evdev supported info; if range large, scale down.
    // We attempt to read via cached; if fails use 0.04.
    // Use raw ioctl via evdev internals is heavy, so just heuristic.
    // Take tablet ~ 40000 range => 1920/40000 = 0.048
    (0.05, 0.05)
}

fn create_virtual_device() -> std::io::Result<evdev::uinput::VirtualDevice> {
    use evdev::uinput::VirtualDeviceBuilder;
    use evdev::{AttributeSet, InputId, BusType};
    let mut keys = AttributeSet::<Key>::new();
    keys.insert(Key::BTN_LEFT);
    keys.insert(Key::BTN_MIDDLE);
    keys.insert(Key::BTN_RIGHT);
    let mut rel = AttributeSet::<RelativeAxisType>::new();
    rel.insert(RelativeAxisType::REL_X);
    rel.insert(RelativeAxisType::REL_Y);
    rel.insert(RelativeAxisType::REL_WHEEL);
    rel.insert(RelativeAxisType::REL_HWHEEL);
    VirtualDeviceBuilder::new()?
        .name("huion-mgr huion pan")
        .input_id(InputId::new(BusType::BUS_USB, 0x256c, 0x0067, 1))
        .with_keys(&keys)?
        .with_relative_axes(&rel)?
        .build()
}

fn emit_key(dev: &mut evdev::uinput::VirtualDevice, key: Key, value: i32) -> std::io::Result<()> {
    let ev = InputEvent::new(EventType::KEY, key.code(), value);
    dev.emit(&[ev])
}

fn emit_relative(dev: &mut evdev::uinput::VirtualDevice, dx: i32, dy: i32) -> std::io::Result<()> {
    let mut evs = Vec::new();
    if dx != 0 {
        evs.push(InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_X.0, dx));
    }
    if dy != 0 {
        evs.push(InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_Y.0, dy));
    }
    if !evs.is_empty() {
        dev.emit(&evs)?;
    }
    Ok(())
}

fn emit_scroll(dev: &mut evdev::uinput::VirtualDevice, dx: i32, dy: i32) -> std::io::Result<()> {
    // two-finger scrolling: REL_WHEEL (vertical) and REL_HWHEEL (horizontal)
    // ponytail: scale is small, 1 scroll step per ~10 device units; wheel is discrete
    let mut evs = Vec::new();
    if dy != 0 {
        // invert dy for natural scroll: pen up (y decreasing) -> scroll up (wheel positive) ?
        // Keep natural: pen up -> dy negative -> wheel negative? Tune via sens sign if needed.
        evs.push(InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_WHEEL.0, dy));
    }
    if dx != 0 {
        evs.push(InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_HWHEEL.0, dx));
    }
    if !evs.is_empty() {
        dev.emit(&evs)?;
    }
    Ok(())
}

fn open_pen_device(pattern: &str) -> Option<Device> {
    // prefer OTD virtual when OTD is running (physical is grabbed and yields no events)
    for (path, dev) in evdev::enumerate() {
        let name = dev.name().unwrap_or("").to_string();
        let lower = name.to_ascii_lowercase();
        if (lower.contains("virtual") && lower.contains("tablet")) || lower.contains("opentabletdriver") {
            if let Ok(d) = Device::open(&path) {
                let _ = set_nonblocking_dev(&d);
                // check if it has ABS_X/Y (pen)
                if let Some(axes) = d.supported_absolute_axes() {
                    if axes.contains(AbsoluteAxisType::ABS_X) {
                        log::info!("Huion pen device (OTD virtual): {} @ {}", name, path.display());
                        return Some(d);
                    }
                }
            }
        }
    }
    // try physical pen
    if let Some(info) = tablet::find_pen_device(pattern) {
        match Device::open(&info.event_path) {
            Ok(dev) => {
                let _ = set_nonblocking_dev(&dev);
                log::info!("Huion pen device: {} @ {}", info.name, info.event_path.display());
                return Some(dev);
            }
            Err(e) => {
                log::warn!("cannot open pen {} @ {}: {e}; try `sudo usermod -aG input {}` and re-login, or `sudo chmod 666 {}`", info.name, info.event_path.display(), std::env::var("USER").unwrap_or("user".into()), info.event_path.display());
            }
        }
    }
    // fallback: any virtual tablet
    for (path, dev) in evdev::enumerate() {
        let name = dev.name().unwrap_or("").to_string();
        if name.to_ascii_lowercase().contains("virtual") && name.to_ascii_lowercase().contains("tablet") {
            if let Ok(d) = Device::open(&path) {
                let _ = set_nonblocking_dev(&d);
                log::info!("Huion pen fallback (OTD virtual): {} @ {}", name, path.display());
                return Some(d);
            }
        }
    }
    // fallback: any device with ABS_X/Y that looks like tablet
    for (path, dev) in evdev::enumerate() {
        if let Some(axes) = dev.supported_absolute_axes() {
            if axes.contains(AbsoluteAxisType::ABS_X) && axes.contains(AbsoluteAxisType::ABS_Y) {
                let name = dev.name().unwrap_or("").to_ascii_lowercase();
                if name.contains("huion") || name.contains("tablet") || name.contains("pen") {
                    if let Ok(d) = Device::open(&path) {
                        let _ = set_nonblocking_dev(&d);
                        log::info!("Huion pen fallback (abs): {} @ {}", dev.name().unwrap_or("unknown"), path.display());
                        return Some(d);
                    }
                }
            }
        }
    }
    log::warn!("Huion pan needs pen device but none found; check `huion-mgr detect` and `huion-mgr read-raw`");
    None
}

fn set_nonblocking<F: AsRawFd>(f: &F) -> Result<(), String> {
    // ponytail: use libc directly to avoid extra nix dep
    let ret = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
    if ret == -1 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}
fn set_nonblocking_dev(dev: &Device) -> Result<(), String> {
    set_nonblocking(dev)
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

    let mut device = Device::open(&device_info.event_path)
        .map_err(|e| format!("Cannot open {}: {}", device_info.event_path.display(), e))?;
    let _ = set_nonblocking_dev(&device);

    log::info!("Device: {}", device.name().unwrap_or("unknown"));
    let keymap = build_evdev_keymap(config);
    let huion_evdev_map = build_evdev_huion_map(config);
    let pen_keymap = build_pen_keymap(config);
    log::info!("Mapped {} decoded keys ({} huion)", keymap.len(), huion_evdev_map.len());

    // huion state for evdev fallback
    let mut pen = open_pen_device(pattern);
    let mut virt = None::<evdev::uinput::VirtualDevice>;
    let mut huion_active: Option<(Key, Key, f32, bool, bool)> = None; // (trigger_key, mouse_btn, sens, hold, is_drag)
    let mut last_pen: Option<(i32, i32)> = None;
    let mut rem_x: f32 = 0.0;
    let mut rem_y: f32 = 0.0;
    let mut last_toggle = std::time::Instant::now() - std::time::Duration::from_secs(1);
    let tablet_scale = pen.as_ref().map(pen_scale).unwrap_or((0.05, 0.05));

    loop {
        let mut had_key_event = false;
        match device.fetch_events() {
            Ok(events) => {
                for event in events {
                    if event.event_type() != EventType::KEY {
                        continue;
                    }
                    let is_press = event.value() == 1;
                    let is_release = event.value() == 0;
                    if !is_press && !is_release {
                        continue;
                    }
                    if let InputEventKind::Key(code) = event.kind() {
                        // check huion bindings first
                        if let Some(binding) = huion_evdev_map.get(&code).and_then(|v| {
                            // mode not tracked in evdev fallback -> just take first
                            v.first().copied()
                        }) {
                            match &binding.action {
                                crate::config::Action::Huion(val) => {
                                    let (btn, sens, hold) = actions::parse_huion(val);
                                let is_drag = val.to_ascii_lowercase().contains("drag");
                                    if hold {
                                        // hold mode: press to start, release to stop
                                        if is_press && huion_active.is_none() {
                                            if last_toggle.elapsed() < std::time::Duration::from_millis(400) {
                                                had_key_event = true;
                                                continue;
                                            }
                                            last_toggle = std::time::Instant::now();
                                            log::info!("Huion pan start (evdev hold): {} btn={:?} drag={}", binding.name, btn, is_drag);
                                            virt = virt.or_else(|| create_virtual_device().ok());
                                            if is_drag {
                                                if let Some(v) = virt.as_mut() {
                                                    let _ = emit_key(v, btn, 1);
                                                }
                                            }
                                            huion_active = Some((code, btn, sens, hold, is_drag));
                                            last_pen = None;
                                            rem_x = 0.0; rem_y = 0.0;
                                        } else if is_release {
                                            if let Some((k, b, _, _, _)) = huion_active {
                                                if k == code {
                                                    log::info!("Huion pan stop (evdev hold release)");
                                                    if let Some(v) = virt.as_mut() {
                                                        let _ = emit_key(v, b, 0);
                                                    }
                                                    huion_active = None;
                                                    last_pen = None;
                                                    rem_x = 0.0; rem_y = 0.0;
                                                }
                                            }
                                        }
                                    } else {
                                        // toggle mode: press to toggle
                                        if !is_press {
                                            had_key_event = true;
                                            continue;
                                        }
                                        if last_toggle.elapsed() < std::time::Duration::from_millis(400) {
                                            had_key_event = true;
                                            continue;
                                        }
                                        if huion_active.is_none() {
                                            last_toggle = std::time::Instant::now();
                                            log::info!("Huion pan start (evdev): {} btn={:?} [press again to stop]", binding.name, btn);
                                            virt = virt.or_else(|| create_virtual_device().ok());
                                            if let Some(v) = virt.as_mut() {
                                                let _ = emit_key(v, btn, 1);
                                            }
                                            huion_active = Some((code, btn, sens, hold, is_drag));
                                            last_pen = None;
                                            rem_x = 0.0; rem_y = 0.0;
                                        } else {
                                            if let Some((k, b, _, _, _)) = huion_active {
                                                if k == code {
                                                    last_toggle = std::time::Instant::now();
                                                    log::info!("Huion pan stop (evdev) (toggle)");
                                                    if let Some(v) = virt.as_mut() {
                                                        let _ = emit_key(v, b, 0);
                                                    }
                                                    huion_active = None;
                                                    last_pen = None;
                                                    rem_x = 0.0; rem_y = 0.0;
                                                }
                                            }
                                        }
                                    }
                                    had_key_event = true;
                                    continue;
                                }
                                _ => {}
                            }
                        }
                        if is_press {
                            if let Some(binding) = keymap.get(&code) {
                                log::info!("Express key: {} ({}) -> {:?}", binding.name, binding.key, binding.action);
                                actions::execute(&binding.action);
                            } else {
                                log::info!("Unmapped key: {:?} (code {}) -> add with `huion-mgr keys set <name> <action>`", code, code.code());
                            }
                        }
                        had_key_event = true;
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => {
                log::error!("Event read error: {}", error);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
                // pen tracking + pen buttons (always poll, pan only when huion_active)
        if let Some(pen_dev) = pen.as_mut() {
            let pan_info = huion_active;
            let mut dx_acc: i32 = 0;
            let mut dy_acc: i32 = 0;
            let mut have_move = false;
            match pen_dev.fetch_events() {
                Ok(evts) => {
                    for ev in evts {
                        match ev.kind() {
                            InputEventKind::AbsAxis(AbsoluteAxisType::ABS_X) => {
                                if let Some((_, _, sens, _, _)) = pan_info {
                                    let x = ev.value();
                                    if let Some((lx,_)) = last_pen {
                                        let raw_dx = (x - lx) as f32 * tablet_scale.0 * sens + rem_x;
                                        let dx = raw_dx as i32;
                                        rem_x = raw_dx - dx as f32;
                                        if dx != 0 {
                                            dx_acc += dx;
                                            have_move = true;
                                        }
                                    }
                                    if let Some((_, ly)) = last_pen { last_pen = Some((x, ly)); } else { last_pen = Some((x, 0)); }
                                } else {
                                    let x = ev.value();
                                    if let Some((_, ly)) = last_pen { last_pen = Some((x, ly)); } else { last_pen = Some((x, 0)); }
                                }
                            }
                            InputEventKind::AbsAxis(AbsoluteAxisType::ABS_Y) => {
                                if let Some((_, _, sens, _, _)) = pan_info {
                                    let y = ev.value();
                                    if let Some((_, ly)) = last_pen {
                                        let raw_dy = (y - ly) as f32 * tablet_scale.1 * sens + rem_y;
                                        let dy = raw_dy as i32;
                                        rem_y = raw_dy - dy as f32;
                                        if dy != 0 {
                                            dy_acc += dy;
                                            have_move = true;
                                        }
                                    }
                                    if let Some((lx,_)) = last_pen { last_pen = Some((lx, y)); } else { last_pen = Some((0, y)); }
                                } else {
                                    let y = ev.value();
                                    if let Some((lx,_)) = last_pen { last_pen = Some((lx, y)); } else { last_pen = Some((0, y)); }
                                }
                            }
                            InputEventKind::Key(k) => {
                                if ev.value() == 1 {
                                    if let Some(bindings) = pen_keymap.get(&k) {
                                        if let Some(b) = find_pen_binding(bindings, "") {
                                            log::info!("Pen button: {} ({:?})", b.name, k);
                                            actions::execute(&b.action);
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => { log::debug!("pen fetch err {e}"); }
            }
            if let Some((_, btn, _, _, is_drag)) = pan_info {
                if have_move && (dx_acc!=0 || dy_acc!=0) {
                    if is_drag {
                        if let Some(v) = virt.as_mut() { let _ = emit_relative(v, dx_acc, dy_acc); }
                    } else {
                        if let Some(v) = virt.as_mut() {
                            let wx = dx_acc / 2;
                            let wy = dy_acc / 2;
                            if wx != 0 || wy != 0 { let _ = emit_scroll(v, wx, wy); }
                        }
                    }
                    log::debug!("huion pan evdev dx={} dy={} btn={:?} drag={}", dx_acc, dy_acc, btn, is_drag);
                }
            }
        }
        if !had_key_event {
            // avoid busy loop when idle but still need pen polling if active
            std::thread::sleep(std::time::Duration::from_millis(5));
        } else {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
}

fn build_evdev_keymap(config: &AppConfig) -> HashMap<Key, &KeyBinding> {
    let mut map = HashMap::new();
    for binding in &config.express_keys {
        // skip huion-only bindings from normal map to avoid double dispatch; keep fallback?
        if matches!(binding.action, crate::config::Action::Huion(_)) {
            continue;
        }
        let key = binding.key.trim();
        let code = if key.starts_with("KEY_") || key.starts_with("BTN_") {
            match key_name_to_code(key) {
                Some(code) => Key::new(code),
                None => {
                    log::warn!("Unknown key identifier: {}", binding.key);
                    continue;
                }
            }
        } else if let Ok(n) = key.parse::<u16>() {
            Key::new(n)
        } else if let Some((_, bit)) = tablet::h951p_button_key(key).or_else(|| parse_number(key).map(|b| (0xe0, b))) {
            let ev_code = match bit {
                0x0001 => 149, // mode1 as prog? not used but map
                0x0002 => 150,
                0x0004 => 151,
                0x0008 => 149, // top-button1 -> KEY_PROG1
                0x0010 => 150,
                0x0020 => 151,
                0x0040 => 152,
                0x0080 => 183, // bottom-button1 -> F13
                0x0100 => 184,
                0x0200 => 185,
                0x0400 => 186,
                _ => {
                    log::warn!("Unknown H951P key for evdev: {} bit 0x{:04x}", binding.key, bit);
                    continue;
                }
            };
            Key::new(ev_code)
        } else if let Some(c) = key_name_to_code(&key.to_ascii_uppercase().replace('-', "_")) {
            Key::new(c)
        } else {
            log::debug!("Ignoring non-evdev H951P key identifier: {}", key);
            continue;
        };
        map.insert(code, binding);
    }
    map
}

fn build_evdev_huion_map(config: &AppConfig) -> HashMap<Key, Vec<&KeyBinding>> {
    let mut map: HashMap<Key, Vec<&KeyBinding>> = HashMap::new();
    for binding in &config.express_keys {
        if !matches!(binding.action, crate::config::Action::Huion(_)) {
            continue;
        }
        let key = binding.key.trim();
        // try evdev name first, then H951P name mapping
        let code = if let Some(c) = key_name_to_code(&key.to_ascii_uppercase()) {
            Key::new(c)
        } else if let Some((_, bit)) = tablet::h951p_button_key(key).or_else(|| parse_number(key).map(|b| (0xe0, b))) {
            // Map H951P button to a synthetic evdev code for fallback matching: use bit low byte as code
            // This is approximate; raw path is preferred for H951P. For evdev fallback we try KEY_PROG1 etc
            // If button was H951P name but we are in evdev mode, map to corresponding KEY_F*
            let ev_code = match bit {
                0x0008 => 149, // KEY_PROG1
                0x0010 => 150,
                0x0020 => 151,
                0x0040 => 152,
                0x0080 => 183, // F13
                0x0100 => 184,
                0x0200 => 185,
                0x0400 => 186,
                _ => continue,
            };
            Key::new(ev_code)
        } else { continue; };
        map.entry(code).or_default().push(binding);
    }
    map
}

fn build_pen_keymap(config: &AppConfig) -> HashMap<Key, Vec<&KeyBinding>> {
    let mut map: HashMap<Key, Vec<&KeyBinding>> = HashMap::new();
    for binding in &config.express_keys {
        let key = binding.key.trim();
        let lower = key.to_ascii_lowercase().replace('_', "-");
        let code = if matches!(lower.as_str(), "pen-button1" | "pen1" | "stylus" | "btn-stylus") {
            Key::new(331)
        } else if matches!(lower.as_str(), "pen-button2" | "pen2" | "stylus2" | "btn-stylus2") {
            Key::new(332)
        } else if let Some(c) = key_name_to_code(&key.to_ascii_uppercase().replace('-', "_")) {
            // only treat as pen if it's stylus
            let k = Key::new(c);
            if k == Key::new(331) || k == Key::new(332) {
                k
            } else {
                continue;
            }
        } else {
            continue;
        };
        map.entry(code).or_default().push(binding);
    }
    map
}

fn find_pen_binding<'a>(bindings: &[&'a KeyBinding], mode: &str) -> Option<&'a KeyBinding> {
    bindings.iter().find(|b| b.mode.as_deref() == Some(mode)).copied()
        .or_else(|| bindings.iter().find(|b| b.mode.is_none()).copied())
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
        "BTN_STYLUS" => Some(331),
        "BTN_STYLUS2" => Some(332),
        "BTN_TOOL_PEN" => Some(320),
        "PEN_BUTTON1" => Some(331),
        "PEN-BUTTON1" => Some(331),
        "PEN_BUTTON2" => Some(332),
        "PEN-BUTTON2" => Some(332),
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
            crate::config::Action::Huion(value) => format!("huion: {value}"),
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
