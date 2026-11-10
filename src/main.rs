mod anchor;
mod cli;
mod config;
mod store;
mod tui;

fn main() {
    if let Err(e) = cli::run() {
        eprintln!("redline: {e:#}");
        std::process::exit(1);
    }
}
