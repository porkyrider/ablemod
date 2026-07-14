use clap::Parser;

fn main() {
    let cli = ablemod::cli::Cli::parse();
    if let Err(e) = ablemod::cli::run(cli) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
