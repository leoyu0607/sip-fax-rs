use clap::{Parser, Subcommand};

/// SIP fax server (fax-in over T.38 / G.711).
#[derive(Parser)]
#[command(name = "sip-faxd", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the fax server daemon (not implemented yet).
    Serve,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Serve) => {
            eprintln!("sip-faxd serve: not implemented yet (see docs/ROADMAP.md)");
            std::process::exit(2);
        }
        None => println!("sip-faxd {}", env!("CARGO_PKG_VERSION")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
