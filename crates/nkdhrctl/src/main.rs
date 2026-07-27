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
    /// Print the primary network connection's status.
    Network {
        /// Print machine-readable JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Print the current screen brightness.
    Brightness {
        /// Print machine-readable JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Print battery charge, charging state and time remaining.
    Battery {
        /// Print machine-readable JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Print the default sink's volume and mute state.
    Audio {
        /// Print machine-readable JSON instead of text.
        #[arg(long)]
        json: bool,
    },
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
        Command::Network { json } => {
            let network = NetworkProxyBlocking::new(&connection)?;
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
                    Option::<u8>::from(status.signal_percent).map(|percent| format!("{percent}%")),
                );
                print_optional("ip4-address", Option::<String>::from(status.ip4_address));
            }
        }
        Command::Brightness { json } => {
            let brightness = BrightnessProxyBlocking::new(&connection)?;
            let status = brightness.get_status()?;
            if json {
                print_json(&status);
            } else {
                println!("brightness: {}%", status.percent);
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
        Command::Audio { json } => {
            let audio = AudioProxyBlocking::new(&connection)?;
            let status = audio.get_status()?;
            if json {
                print_json(&status);
            } else {
                print_optional("sink", Option::<String>::from(status.sink_name));
                print_optional(
                    "volume",
                    Option::<u8>::from(status.volume_percent).map(|percent| format!("{percent}%")),
                );
                println!("muted: {}", status.muted);
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
