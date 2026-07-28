use clap::{Parser, Subcommand};
use nkdhr_ipc::{
    AudioProxyBlocking, BrightnessProxyBlocking, DaemonProxyBlocking, NetworkProxyBlocking,
    PowerProxyBlocking, SessionProxyBlocking,
};
use zbus::blocking::Connection;

#[derive(Parser)]
#[command(name = "nkdhrctl", about = "Command-line front end to nkdhrd")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check that nkdhrd is running and responding.
    Ping,
    /// Print nkdhrd's version, uptime and loaded modules.
    Status {
        /// Print machine-readable JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Print the current login session's id, seat, idle and locked state.
    Session {
        /// Print machine-readable JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Print the primary network connection's status, or connect to one.
    Network {
        #[command(subcommand)]
        action: Option<NetworkAction>,
        /// Print machine-readable JSON instead of text. Only applies when
        /// printing the current status.
        #[arg(long)]
        json: bool,
    },
    /// Print the current screen brightness, or change it.
    Brightness {
        #[command(subcommand)]
        action: Option<BrightnessAction>,
        /// Print machine-readable JSON instead of text. Only applies when
        /// printing the current brightness.
        #[arg(long)]
        json: bool,
    },
    /// Print battery charge, charging state and time remaining.
    Battery {
        /// Print machine-readable JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Print the default sink's volume and mute state, or change them.
    Audio {
        #[command(subcommand)]
        action: Option<AudioAction>,
        /// Print machine-readable JSON instead of text. Only applies when
        /// printing the current status.
        #[arg(long)]
        json: bool,
    },
    /// Power off, reboot or suspend the system.
    Power {
        #[command(subcommand)]
        action: PowerAction,
    },
    /// Stream one JSON line per real change to a module's status. Never
    /// polls: this blocks on nkdhrd's own change signal for the module.
    Watch { module: WatchModule },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum WatchModule {
    Battery,
    Network,
    Audio,
    Brightness,
    Session,
}

#[derive(Subcommand)]
enum PowerAction {
    /// Power off the system.
    Off,
    /// Reboot the system.
    Reboot,
    /// Suspend the system.
    Suspend,
}

#[derive(Subcommand)]
enum BrightnessAction {
    /// Set the screen brightness.
    Set {
        /// 0-100.
        percent: u8,
    },
}

#[derive(Subcommand)]
enum NetworkAction {
    /// Connect to a Wi-Fi network by SSID.
    Connect {
        ssid: String,
        /// WPA/WPA2 personal passphrase. Omit for an open network.
        #[arg(long)]
        password: Option<String>,
    },
}

#[derive(Subcommand)]
enum AudioAction {
    /// Set the default sink's volume.
    SetVolume {
        /// 0-100.
        percent: u8,
    },
    /// Mute the default sink.
    Mute,
    /// Unmute the default sink.
    Unmute,
}

fn run(command: Command) -> zbus::Result<()> {
    let connection = Connection::session()?;

    match command {
        Command::Ping => {
            let daemon = DaemonProxyBlocking::new(&connection)?;
            println!("{}", daemon.ping()?);
        }
        Command::Status { json } => {
            let daemon = DaemonProxyBlocking::new(&connection)?;
            let status = daemon.get_status()?;
            if json {
                print_json(&status);
            } else {
                let modules = if status.modules.is_empty() {
                    "(none)".to_owned()
                } else {
                    status.modules.join(", ")
                };
                println!("version: {}", status.version);
                println!("uptime: {}s", status.uptime_secs);
                println!("modules: {modules}");
            }
        }
        Command::Session { json } => {
            let session = SessionProxyBlocking::new(&connection)?;
            let status = session.get_status()?;
            if json {
                print_json(&status);
            } else {
                println!("id: {}", status.id);
                print_optional("seat", non_empty(status.seat));
                println!("active: {}", status.active);
                println!("idle: {}", status.idle);
                println!("locked: {}", status.locked);
            }
        }
        Command::Network { action, json } => {
            let network = NetworkProxyBlocking::new(&connection)?;
            match action {
                None => {
                    let status = network.get_status()?;
                    if json {
                        print_json(&status);
                    } else {
                        println!("connected: {}", status.connected);
                        println!("kind: {}", status.kind);
                        print_optional("interface", Option::<String>::from(status.interface));
                        print_optional("ssid", Option::<String>::from(status.ssid));
                        print_optional(
                            "signal",
                            Option::<u8>::from(status.signal_percent)
                                .map(|percent| format!("{percent}%")),
                        );
                        print_optional("ip4-address", Option::<String>::from(status.ip4_address));
                    }
                }
                Some(NetworkAction::Connect { ssid, password }) => {
                    network.connect(&ssid, password.as_deref().unwrap_or(""))?;
                }
            }
        }
        Command::Brightness { action, json } => {
            let brightness = BrightnessProxyBlocking::new(&connection)?;
            match action {
                None => {
                    let status = brightness.get_status()?;
                    if json {
                        print_json(&status);
                    } else {
                        println!("brightness: {}%", status.percent);
                    }
                }
                Some(BrightnessAction::Set { percent }) => brightness.set(percent)?,
            }
        }
        Command::Battery { json } => {
            let power = PowerProxyBlocking::new(&connection)?;
            let status = power.get_status()?;
            if json {
                print_json(&status);
            } else {
                println!("present: {}", status.is_present);
                println!("charge: {}%", status.percentage);
                println!("state: {}", status.state);
                print_optional(
                    "time-to-empty",
                    Option::<u32>::from(status.time_to_empty_secs).map(|secs| format!("{secs}s")),
                );
                print_optional(
                    "time-to-full",
                    Option::<u32>::from(status.time_to_full_secs).map(|secs| format!("{secs}s")),
                );
            }
        }
        Command::Audio { action, json } => {
            let audio = AudioProxyBlocking::new(&connection)?;
            match action {
                None => {
                    let status = audio.get_status()?;
                    if json {
                        print_json(&status);
                    } else {
                        print_optional("sink", Option::<String>::from(status.sink_name));
                        print_optional(
                            "sink-volume",
                            Option::<u8>::from(status.sink_volume_percent)
                                .map(|percent| format!("{percent}%")),
                        );
                        println!("sink-muted: {}", status.sink_muted);
                        print_optional("source", Option::<String>::from(status.source_name));
                        print_optional(
                            "source-volume",
                            Option::<u8>::from(status.source_volume_percent)
                                .map(|percent| format!("{percent}%")),
                        );
                        println!("source-muted: {}", status.source_muted);
                    }
                }
                Some(AudioAction::SetVolume { percent }) => audio.set_volume(percent)?,
                Some(AudioAction::Mute) => audio.set_mute(true)?,
                Some(AudioAction::Unmute) => audio.set_mute(false)?,
            }
        }
        Command::Power { action } => {
            let power = PowerProxyBlocking::new(&connection)?;
            match action {
                PowerAction::Off => power.power_off()?,
                PowerAction::Reboot => power.reboot()?,
                PowerAction::Suspend => power.suspend()?,
            }
        }
        Command::Watch { module } => watch(&connection, module)?,
    }

    Ok(())
}

/// Loops over the matching module's `Changed` signal, printing one JSON
/// line per event as it arrives. Never polls: each iteration blocks on the
/// D-Bus connection until `nkdhrd` emits the next signal.
fn watch(connection: &Connection, module: WatchModule) -> zbus::Result<()> {
    match module {
        WatchModule::Battery => {
            let power = PowerProxyBlocking::new(connection)?;
            for signal in power.receive_changed()? {
                print_json(signal.args()?.status());
            }
        }
        WatchModule::Network => {
            let network = NetworkProxyBlocking::new(connection)?;
            for signal in network.receive_changed()? {
                print_json(signal.args()?.status());
            }
        }
        WatchModule::Audio => {
            let audio = AudioProxyBlocking::new(connection)?;
            for signal in audio.receive_changed()? {
                print_json(signal.args()?.status());
            }
        }
        WatchModule::Brightness => {
            let brightness = BrightnessProxyBlocking::new(connection)?;
            for signal in brightness.receive_changed()? {
                print_json(signal.args()?.status());
            }
        }
        WatchModule::Session => {
            let session = SessionProxyBlocking::new(connection)?;
            for signal in session.receive_changed()? {
                print_json(signal.args()?.status());
            }
        }
    }
    Ok(())
}

fn print_json<T: serde::Serialize>(value: &T) {
    let text = serde_json::to_string(value).expect("nkdhr-ipc status types always serialize");
    println!("{text}");
}

fn print_optional<T: std::fmt::Display>(label: &str, value: Option<T>) {
    match value {
        Some(value) => println!("{label}: {value}"),
        None => println!("{label}: (none)"),
    }
}

/// `logind`'s `Seat` property is `""` for a session not bound to a seat
/// (e.g. an SSH session) rather than using D-Bus's own optional-value
/// convention, so this normalizes it the same way as the `Optional<T>`
/// fields already coming out of the Network module.
fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn main() {
    let cli = Cli::parse();
    if let Err(err) = run(cli.command) {
        eprintln!("nkdhrctl: {err}");
        std::process::exit(1);
    }
}
