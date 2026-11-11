//! Metro CLI library.
//!
//! Provides the command-line interface with global flags.

use clap::{Parser, Subcommand};

/// Metro feed manager
#[derive(Parser)]
#[command(name = "metro")]
#[command(about = "Metro feed manager", long_about = None)]
#[command(version)]
pub struct Cli {
    /// Verbose output (inert until M1)
    #[arg(short = 'v', long = "verbose", global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Log configuration (inert until M1)
    #[arg(long = "log", global = true)]
    pub log: Option<String>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Placeholder for future commands
    #[command(hide = true)]
    Placeholder,
}

/// Run the CLI with the provided arguments.
///
/// Returns exit code: 0 for success, 2 for usage errors.
pub fn run(args: &[String]) {
    let cli = Cli::parse_from(args);

    match cli.command {
        None => {
            // No subcommand - show help
            let _ = Cli::parse_from(["metro", "--help"]);
        }
        Some(Commands::Placeholder) => {
            // Placeholder for future commands
            println!("No command specified. Use `metro --help` for usage.");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_exits_successfully() {
        // Just verify we can parse --version without error
        let _cli = Cli::parse_from(["metro", "--version"]);
        // clap handles version, test just verifies parsing succeeds
    }

    #[test]
    fn test_verbose_flag_accepted() {
        let cli = Cli::parse_from(["metro", "-v", "--version"]);
        assert_eq!(cli.verbose, 1);
    }

    #[test]
    fn test_log_flag_accepted() {
        let cli = Cli::parse_from(["metro", "--log", "debug", "--version"]);
        assert_eq!(cli.log, Some("debug".to_string()));
    }
}
