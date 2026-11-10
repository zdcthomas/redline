fn main() {
    if let Err(e) = redline::cli::run() {
        eprintln!("redline: {e:#}");
        std::process::exit(1);
    }
}
