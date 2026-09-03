use std::path::Path;
use std::process::ExitCode;

use blastray::{index, query};

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<String, String> {
    match args.as_slice() {
        [] => Ok(help()),
        [command] if command == "--help" || command == "-h" => Ok(help()),
        [command] if command == "--version" || command == "-V" => {
            Ok(format!("blastray {}", env!("CARGO_PKG_VERSION")))
        }
        [command, target] if command == "find" => Ok(query::find(open_index()?.graph(), target)),
        [command, target] if command == "inspect" => query::inspect(open_index()?.graph(), target),
        [command, from, to] if command == "trace" => query::trace(open_index()?.graph(), from, to),
        [command, target] if command == "impact" => query::impact(open_index()?.graph(), target),
        _ => Err(help()),
    }
}

fn open_index() -> Result<index::Index, String> {
    index::Index::open(Path::new("."))
}

fn help() -> String {
    "BlastRay — code intelligence for coding agents\n\nUsage:\n  blastray find <query>\n  blastray inspect <target>\n  blastray trace <from> <to>\n  blastray impact <target>\n\nTargets accept a canonical symbol identity such as src/auth/session.ts::refreshSession.".to_string()
}
