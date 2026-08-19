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
            Ok(())
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
