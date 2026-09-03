fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("--version") | Some("-V") => {
            println!("blastray {}", env!("CARGO_PKG_VERSION"));
        }
        _ => {
            println!("BlastRay — code intelligence for coding agents\n\nUsage: blastray [--help | --version]");
        }
    }
}
