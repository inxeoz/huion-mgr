mod actions;
mod config;
mod daemon;
mod tablet;

use clap::{Parser, Subcommand};
use config::{Action, AppConfig, KeyBinding};
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "huion-mgr",
    version,
    about = "Huion tablet key mapping CLI for Linux"
)]
struct Cli {
    /// Use a specific TOML configuration file.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the express key daemon.
    Daemon,
    /// Detect connected Huion tablet input devices.
    Detect,
    /// Listen to all tablet input (evdev + hidraw) and print signatures.
    ReadRaw {
        /// Comma-separated filters to hide: movement,pen,hidraw,evdev,button (e.g. --exclude movement)
        #[arg(long, value_name = "FILTER")]
        exclude: Option<String>,
    },
    /// Show or edit the TOML configuration.
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },
    /// Manage express key bindings.
    Keys {
        #[command(subcommand)]
        action: Option<KeyAction>,
    },
    /// Execute an action without changing the configuration.
    Test {
        /// Action such as `combo:ctrl+z`, `key:Return`, or `cmd:echo hello`.
        action: String,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Print the active configuration.
    Show,
    /// Generate `$HOME/.config/huion-mgr/config.toml`.
    #[command(alias = "init")]
    Generate {
        /// Replace an existing generated configuration.
        #[arg(long)]
        force: bool,
    },
    /// Open the configuration in `$EDITOR`.
    Edit,
    /// Set a supported scalar configuration value.
    Set { key: String, value: String },
}

#[derive(Subcommand)]
enum KeyAction {
    /// Print all configured bindings.
    List,
    /// Set a binding, for example `keys set top-button1 combo ctrl+z --mode mode1`.
    Set {
        key: String,
        action_type: String,
        value: String,
        /// Optional mode layer: mode1, mode2, or mode3.
        #[arg(long)]
        mode: Option<String>,
    },
    /// Remove a binding from the configuration.
    Unset {
        key: String,
        /// Remove only this mode-specific binding.
        #[arg(long)]
        mode: Option<String>,
    },
    /// Print decoded key events from the tablet's express-key device.
    Scan,
    /// Print unique raw HID reports from the vendor-specific tablet interface.
    RawScan,
    /// Listen to all tablet input (evdev + hidraw) and print signatures.
    ReadRaw {
        /// Comma-separated filters to hide: movement,pen,hidraw,evdev,button (e.g. --exclude movement)
        #[arg(long, value_name = "FILTER")]
        exclude: Option<String>,
    },
    /// Execute one configured binding.
    Test {
        key: String,
        /// Select the mode-specific binding to test.
        #[arg(long)]
        mode: Option<String>,
    },
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    if let Err(error) = run(Cli::parse()) {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), String> {
    let config_override = cli.config.is_some();
    let config_path = cli.config.unwrap_or_else(AppConfig::config_path);

    match cli.command {
        Commands::Daemon => {
            let config = load_config(&config_path)?;
            log::info!("Starting huion-mgr daemon...");
            daemon::run(&config)
        }
        Commands::Detect => {
            let config = load_config(&config_path)?;
            tablet::list_all(&config.tablet_name);
            // also list hidraw for debugging
            let raw = tablet::detect_raw_hid(&config.tablet_name);
            if raw.is_empty() {
                println!("No hidraw devices matching '{}'", config.tablet_name);
            } else {
                println!("Raw HID devices:");
                for d in raw {
                    println!(
                        "  {} @ {} ({}) vendor_specific={}",
                        d.name,
                        d.hidraw_path.display(),
                        d.phys,
                        d.vendor_specific
                    );
                }
            }
            Ok(())
        }
        Commands::ReadRaw { exclude } => {
            read_raw_all(&load_config(&config_path)?, exclude.as_deref())
        }
        Commands::Config { action } => match action.unwrap_or(ConfigAction::Show) {
            ConfigAction::Show => {
                let config = load_config(&config_path)?;
                println!(
                    "{}",
                    toml::to_string_pretty(&config).map_err(|e| e.to_string())?
                );
                Ok(())
            }
            ConfigAction::Generate { force } => {
                let path = if config_override {
                    config_path.clone()
                } else {
                    AppConfig::home_config_path()
                };
                if path.exists() && !force {
                    return Err(format!(
                        "config already exists at {}; use --force to replace it",
                        path.display()
                    ));
                }
                AppConfig::default().save_to(&path)?;
                println!("Config generated at {}", path.display());
                Ok(())
            }
            ConfigAction::Edit => {
                if !config_path.exists() {
                    AppConfig::default().save_to(&config_path)?;
                }
                let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
                let status = std::process::Command::new(&editor)
                    .arg(&config_path)
                    .status()
                    .map_err(|e| format!("failed to start editor '{editor}': {e}"))?;
                if status.success() {
                    Ok(())
                } else {
                    Err(format!("editor exited with {status}"))
                }
            }
            ConfigAction::Set { key, value } => {
                let mut config = load_config(&config_path)?;
                set_config_value(&mut config, &key, &value)?;
                config.save_to(&config_path)?;
                println!("Set {key} = {value}");
                Ok(())
            }
        },
        Commands::Keys { action } => match action.unwrap_or(KeyAction::List) {
            KeyAction::List => {
                let config = load_config(&config_path)?;
                daemon::show_bindings(&config);
                Ok(())
            }
            KeyAction::Set {
                key,
                action_type,
                value,
                mode,
            } => {
                let mode = normalize_mode(mode.as_deref())?;
                let mut config = load_config(&config_path)?;
                set_key_binding(
                    &mut config,
                    &key,
                    &format!("{action_type}:{value}"),
                    mode.as_deref(),
                )?;
                config.save_to(&config_path)?;
                let mode_text = mode.as_deref().unwrap_or("fallback");
                println!("Bound {key} [{mode_text}] -> {action_type}:{value}");
                Ok(())
            }
            KeyAction::Unset { key, mode } => {
                let mode = normalize_mode(mode.as_deref())?;
                let mut config = load_config(&config_path)?;
                let old_len = config.express_keys.len();
                config.express_keys.retain(|binding| {
                    if binding.key != key {
                        return true;
                    }
                    match mode.as_deref() {
                        Some(mode) => binding.mode.as_deref() != Some(mode),
                        None => false,
                    }
                });
                if config.express_keys.len() == old_len {
                    return Err(format!(
                        "key '{key}' is not configured for the requested mode"
                    ));
                }
                config.save_to(&config_path)?;
                println!("Unbound {key}");
                Ok(())
            }
            KeyAction::Scan => scan_keys(&load_config(&config_path)?),
            KeyAction::RawScan => raw_scan_keys(&load_config(&config_path)?),
            KeyAction::ReadRaw { exclude } => {
                read_raw_all(&load_config(&config_path)?, exclude.as_deref())
            }
            KeyAction::Test { key, mode } => {
                let mode = normalize_mode(mode.as_deref())?;
                let config = load_config(&config_path)?;
                let binding = find_binding(&config, &key, mode.as_deref())
                    .ok_or_else(|| format!("key '{key}' is not configured"))?;
                println!("Testing: {} -> {:?}", binding.name, binding.action);
                actions::execute(&binding.action);
                Ok(())
            }
        },
        Commands::Test { action } => {
            let parsed = Action::parse(&action)?;
            println!("Testing: {parsed:?}");
            actions::execute(&parsed);
            Ok(())
        }
    }
}

fn load_config(path: &Path) -> Result<AppConfig, String> {
    AppConfig::load_from(path)
}

fn set_config_value(config: &mut AppConfig, key: &str, value: &str) -> Result<(), String> {
    match key {
        "tablet_name" => config.tablet_name = value.to_string(),
        "pen.output" => config.pen.output = value.to_string(),
        "hyprland.monitor" => {
            let hyprland = config.hyprland.get_or_insert(config::HyprlandConfig {
                monitor: None,
                region: None,
            });
            hyprland.monitor = Some(value.to_string());
        }
        _ => {
            return Err(format!(
                "unknown config key '{key}'; available keys: tablet_name, pen.output, hyprland.monitor"
            ));
        }
    }
    Ok(())
}

fn normalize_mode(mode: Option<&str>) -> Result<Option<String>, String> {
    let Some(mode) = mode else {
        return Ok(None);
    };
    let mode = mode.to_ascii_lowercase();
    match mode.as_str() {
        "mode1" | "mode2" | "mode3" => Ok(Some(mode)),
        _ => Err(format!("unknown mode '{mode}'; use mode1, mode2, or mode3")),
    }
}

fn set_key_binding(
    config: &mut AppConfig,
    key: &str,
    spec: &str,
    mode: Option<&str>,
) -> Result<(), String> {
    let action = Action::parse(spec)?;
    if let Some(binding) = config
        .express_keys
        .iter_mut()
        .find(|binding| binding.key == key && binding.mode.as_deref() == mode)
    {
        binding.action = action;
    } else {
        config.express_keys.push(KeyBinding {
            key: key.to_string(),
            name: key.to_string(),
            mode: mode.map(str::to_string),
            action,
        });
    }
    Ok(())
}

fn find_binding<'a>(
    config: &'a AppConfig,
    key: &str,
    mode: Option<&str>,
) -> Option<&'a KeyBinding> {
    if let Some(mode) = mode {
        config
            .express_keys
            .iter()
            .find(|binding| binding.key == key && binding.mode.as_deref() == Some(mode))
            .or_else(|| {
                config
                    .express_keys
                    .iter()
                    .find(|binding| binding.key == key && binding.mode.is_none())
            })
    } else {
        config
            .express_keys
            .iter()
            .find(|binding| binding.key == key)
    }
}

fn scan_keys(config: &AppConfig) -> Result<(), String> {
    use evdev::{Device, EventType, InputEventKind};

    let device_info = tablet::find_express_keys(&config.tablet_name).ok_or_else(|| {
        format!(
            "no express-key device found matching '{}'; run 'huion-mgr detect' first",
            config.tablet_name
        )
    })?;

    println!("Scanning keys on {}...", device_info.name);
    println!("Press every key once. Each unique key code is printed once.");
    println!("Press Ctrl+C when finished.\n");

    let mut device = Device::open(&device_info.event_path).map_err(|e| {
        format!(
            "cannot open {}: {e}; check that your user can read /dev/input devices",
            device_info.event_path.display()
        )
    })?;

    let mut seen_codes = BTreeSet::new();
    loop {
        match device.fetch_events() {
            Ok(events) => {
                for event in events {
                    if event.event_type() == EventType::KEY && event.value() == 1 {
                        if let InputEventKind::Key(code) = event.kind() {
                            if seen_codes.insert(code.code()) {
                                println!("  New key: {:?} (code {})", code, code.code());
                                std::io::stdout()
                                    .flush()
                                    .map_err(|e| format!("failed to flush scan output: {e}"))?;
                            }
                        }
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(format!("error reading input device: {error}")),
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn raw_scan_keys(config: &AppConfig) -> Result<(), String> {
    let device = tablet::find_vendor_hid(&config.tablet_name).ok_or_else(|| {
        format!(
            "no vendor-specific HID interface found for '{}'; check /sys/class/hidraw and run as a user with hidraw access",
            config.tablet_name
        )
    })?;

    println!("Scanning raw HID reports on {}...", device.name);
    println!(
        "Interface: {} ({})",
        device.hidraw_path.display(),
        device.phys
    );
    println!("Press every tablet key once. Each unique report is printed once.");
    println!("Press Ctrl+C when finished.\n");
    std::io::stdout()
        .flush()
        .map_err(|error| format!("failed to flush scan output: {error}"))?;

    let mut file = std::fs::File::open(&device.hidraw_path).map_err(|error| {
        format!(
            "cannot open {}: {error}; add hidraw permissions or run with sudo",
            device.hidraw_path.display()
        )
    })?;
    let mut seen_reports = BTreeSet::new();
    let mut previous_by_id = std::collections::HashMap::<u8, Vec<u8>>::new();
    let mut buffer = [0u8; 256];

    loop {
        let length = file
            .read(&mut buffer)
            .map_err(|error| format!("error reading {}: {error}", device.hidraw_path.display()))?;
        if length == 0 {
            continue;
        }

        let report = buffer[..length].to_vec();
        let report_id = report[0];
        let changed = previous_by_id
            .get(&report_id)
            .map(|previous| format_changed_bytes(previous, &report))
            .unwrap_or_else(|| "first report".to_string());
        previous_by_id.insert(report_id, report.clone());

        if seen_reports.insert(report.clone()) {
            let button = tablet::h951p_button_description(&report)
                .map(|description| format!(" | {description}"))
                .unwrap_or_default();
            println!(
                "  Report id=0x{report_id:02x} len={length}: {} | changed: {changed}{button}",
                format_report(&report)
            );
            std::io::stdout()
                .flush()
                .map_err(|error| format!("failed to flush scan output: {error}"))?;
        }
    }
}

fn format_report(report: &[u8]) -> String {
    report
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_changed_bytes(previous: &[u8], current: &[u8]) -> String {
    let mut changes = Vec::new();
    for index in 0..previous.len().max(current.len()) {
        let before = previous.get(index).copied().unwrap_or(0);
        let after = current.get(index).copied().unwrap_or(0);
        if before != after {
            changes.push(format!("[{index}] {before:02x}->{after:02x}"));
        }
    }
    if changes.is_empty() {
        "none".to_string()
    } else {
        changes.join(", ")
    }
}

fn read_raw_all(config: &AppConfig, exclude: Option<&str>) -> Result<(), String> {
    use evdev::{Device, EventType};
    use std::os::unix::io::AsRawFd;

    fn set_nb<F: AsRawFd>(f: &F) {
        let _ = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
    }

    let exclude_lower = exclude.unwrap_or("").to_ascii_lowercase();
    let hide_movement = exclude_lower.contains("movement")
        || exclude_lower.contains("pen")
        || exclude_lower.contains("move");
    let hide_hidraw = exclude_lower.contains("hidraw");
    let hide_evdev = exclude_lower.contains("evdev");
    let hide_button = exclude_lower.contains("button");
    if hide_movement || hide_hidraw || hide_evdev || hide_button {
        println!("Excluding: {exclude:?}\n");
    }
    println!("Listening all tablet input. Signature per event/ report. Ctrl+C to stop.\n");

    let ev_devices = tablet::detect_devices(&config.tablet_name);
    let raw_devices = tablet::detect_raw_hid(&config.tablet_name);

    if ev_devices.is_empty() && raw_devices.is_empty() {
        return Err(format!(
            "no devices found matching '{}'; try `huion-mgr detect`",
            config.tablet_name
        ));
    }

    println!("EVDEV devices ({}):", ev_devices.len());
    for d in &ev_devices {
        println!(
            "  [{:?}] {} @ {} ({})",
            d.device_type,
            d.name,
            d.event_path.display(),
            d.phys
        );
    }
    println!("HIDRAW devices ({}):", raw_devices.len());
    for d in &raw_devices {
        println!(
            "  {} @ {} ({}) vendor_specific={}",
            d.name,
            d.hidraw_path.display(),
            d.phys,
            d.vendor_specific
        );
    }
    println!();

    // open evdev
    let mut ev_open: Vec<(String, Device)> = Vec::new();
    for info in ev_devices {
        match Device::open(&info.event_path) {
            Ok(dev) => {
                set_nb(&dev);
                println!("[open] evdev {} @ {}", info.name, info.event_path.display());
                ev_open.push((
                    format!("{} @ {}", info.name, info.event_path.display()),
                    dev,
                ));
            }
            Err(e) => {
                println!(
                    "[fail] evdev {} @ {}: {e} (try `sudo usermod -aG input $USER` and re-login)",
                    info.name,
                    info.event_path.display()
                );
            }
        }
    }
    // always also try OTD virtual tablet as pen source (physical is grabbed by OTD)
    for (path, dev) in evdev::enumerate() {
        let name = dev.name().unwrap_or("").to_string();
        let lower = name.to_ascii_lowercase();
        if (lower.contains("virtual") && lower.contains("tablet"))
            || lower.contains("opentabletdriver")
        {
            if ev_open.iter().any(|(n, _)| n.contains(&name)) {
                continue;
            }
            match Device::open(&path) {
                Ok(d) => {
                    set_nb(&d);
                    println!("[open] evdev OTD virtual {} @ {}", name, path.display());
                    ev_open.push((format!("{} @ {}", name, path.display()), d));
                }
                Err(e) => {
                    println!(
                        "[fail] evdev OTD virtual {} @ {}: {e}",
                        name,
                        path.display()
                    );
                }
            }
        }
    }

    // open hidraw
    let mut raw_open: Vec<(String, std::fs::File, String)> = Vec::new();
    for info in raw_devices {
        match std::fs::File::open(&info.hidraw_path) {
            Ok(f) => {
                set_nb(&f);
                println!(
                    "[open] hidraw {} @ {}",
                    info.name,
                    info.hidraw_path.display()
                );
                raw_open.push((info.name.clone(), f, info.hidraw_path.display().to_string()));
            }
            Err(e) => {
                println!(
                    "[fail] hidraw {} @ {}: {e}",
                    info.name,
                    info.hidraw_path.display()
                );
            }
        }
    }
    println!("\n--- listening (evdev shows KEY/ABS/REL, hidraw shows hex + button) ---\n");
    std::io::stdout()
        .flush()
        .map_err(|e| format!("flush failed: {e}"))?;

    let mut hid_buf = [0u8; 256];
    let mut hid_prev: std::collections::HashMap<String, Vec<u8>> = std::collections::HashMap::new();

    loop {
        // poll evdev
        for (label, dev) in ev_open.iter_mut() {
            match dev.fetch_events() {
                Ok(events) => {
                    for ev in events {
                        // format signature: type + kind + value
                        let kind = ev.kind();
                        let et = ev.event_type();
                        // skip SYN_REPORT alone? still show if needed
                        if et == EventType::SYNCHRONIZATION {
                            continue;
                        }
                        if hide_evdev {
                            continue;
                        }
                        if hide_movement && matches!(kind, evdev::InputEventKind::AbsAxis(_)) {
                            continue;
                        }
                        if hide_button && matches!(kind, evdev::InputEventKind::Key(_)) {
                            continue;
                        }
                        println!(
                            "[evdev {label}] {et:?} {kind:?} value={} raw_type={} code={}",
                            ev.value(),
                            ev.event_type().0,
                            ev.code()
                        );
                        // also for ABS show more: if pen ABS_X/Y
                        if let evdev::InputEventKind::AbsAxis(_axis) = kind {
                            // axis already in kind
                        }
                    }
                    let _ = std::io::stdout().flush();
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => {
                    eprintln!("[evdev {label} error] {e}");
                }
            }
        }
        // poll hidraw
        for (name, file, path) in raw_open.iter_mut() {
            use std::io::Read;
            match file.read(&mut hid_buf) {
                Ok(0) => {}
                Ok(len) => {
                    if hide_hidraw {
                        hid_prev.insert(path.clone(), hid_buf[..len].to_vec());
                        continue;
                    }
                    let report = &hid_buf[..len];
                    let is_pen_hid =
                        matches!(report.get(1), Some(0x80) | Some(0x81)) && report.len() >= 12;
                    let is_known_button = tablet::h951p_button_keys(report).is_some();
                    if hide_movement && !is_known_button {
                        hid_prev.insert(path.clone(), report.to_vec());
                        continue;
                    }
                    if hide_button && is_known_button {
                        hid_prev.insert(path.clone(), report.to_vec());
                        continue;
                    }
                    // also hide pen if specifically requested (redundant with above)
                    if hide_movement && is_pen_hid {
                        hid_prev.insert(path.clone(), report.to_vec());
                        continue;
                    }
                    let hex = report
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    let prev = hid_prev.get(path).cloned().unwrap_or_default();
                    let changed = if prev.is_empty() {
                        "first".to_string()
                    } else {
                        format_changed_bytes(&prev, report)
                    };
                    // state 0x80 hover, 0x81 tip down = pen, 0xe0/0xe3/0xf1 = buttons; don't show unknown button for pen
                    let is_pen =
                        matches!(report.get(1), Some(0x80) | Some(0x81)) && report.len() >= 12;
                    let button = if is_pen {
                        String::new()
                    } else {
                        tablet::h951p_button_description(report)
                            .map(|d| format!(" | {d}"))
                            .unwrap_or_default()
                    };
                    let pen_info = if is_pen {
                        // ponytail: heuristic H951P pen: [id, 0x80, X_L, X_H, Y_L, Y_H, P_L, P_H, tiltX, tiltY, ...]
                        let x = u16::from(report[2]) | (u16::from(report[3]) << 8);
                        let y = u16::from(report[4]) | (u16::from(report[5]) << 8);
                        let p = u16::from(report[6]) | (u16::from(report[7]) << 8);
                        format!(" | pen x={x} y={y} pressure={p}")
                    } else {
                        String::new()
                    };
                    let ev_hint = if report.first() == Some(&0x08) {
                        " (H951P)"
                    } else {
                        ""
                    };
                    let extra = if !button.is_empty() { button } else { pen_info };
                    println!("[hidraw {name}@{path} len={len} id=0x{:02x}] {hex} | changed: {changed}{extra}{ev_hint}", report[0]);
                    hid_prev.insert(path.clone(), report.to_vec());
                    let _ = std::io::stdout().flush();
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => {
                    eprintln!("[hidraw {name}@{path} error] {e}");
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[cfg(test)]
mod raw_scan_tests {
    use super::format_changed_bytes;

    #[test]
    fn reports_changed_and_added_bytes() {
        assert_eq!(
            format_changed_bytes(&[0x08, 0x00], &[0x08, 0x80, 0x01]),
            "[1] 00->80, [2] 00->01"
        );
    }

    #[test]
    fn reports_no_change() {
        assert_eq!(format_changed_bytes(&[1, 2], &[1, 2]), "none");
    }
}
