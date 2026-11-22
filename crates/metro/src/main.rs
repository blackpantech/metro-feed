//! Metro dispatcher binary.
//!
//! Routes empty argv to TUI, all else to CLI. The CLI returns its exit code
//! (see `metro_cli::run`), which becomes the process exit status.

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() == 1 {
        // No arguments: invoke TUI
        metro_tui::run();
    } else {
        // Has arguments: invoke CLI and propagate its exit code
        std::process::exit(metro_cli::run(&args));
    }
}
