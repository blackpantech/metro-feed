//! Metro dispatcher binary.
//!
//! Routes empty argv to TUI, all else to CLI.

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() == 1 {
        // No arguments: invoke TUI
        metro_tui::run();
    } else {
        // Has arguments: invoke CLI
        metro_cli::run(&args);
    }
}
