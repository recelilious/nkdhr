use clap::{Parser, Subcommand};
use nkdhr_ipc::DaemonProxyBlocking;
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
}

fn run(command: Command) -> zbus::Result<()> {
    let connection = Connection::session()?;
    let daemon = DaemonProxyBlocking::new(&connection)?;

    match command {
        Command::Ping => println!("{}", daemon.ping()?),
        Command::Status { json } => {
            let status = daemon.get_status()?;
            if json {
                let text = serde_json::to_string(&status).expect("DaemonStatus always serializes");
                println!("{text}");
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
    }

    Ok(())
}

fn main() {
    let cli = Cli::parse();
    if let Err(err) = run(cli.command) {
        eprintln!("nkdhrctl: {err}");
        std::process::exit(1);
    }
}
