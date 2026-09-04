use std::path::Path;
use std::process::ExitCode;

use blastray::{diff, index, mcp, query};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.as_slice() == ["mcp"] {
        return match mcp::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        };
    }
    match run(args) {
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
        [command, target] if command == "find" => {
            answer_with_index(|index| Ok(query::find(index.graph(), target)))
        }
        [command, target] if command == "inspect" => {
            answer_with_index(|index| query::inspect(index.graph(), target))
        }
        [command, from, to] if command == "trace" => {
            answer_with_index(|index| query::trace(index.graph(), from, to))
        }
        [command, flag] if command == "impact" && flag == "--diff" => {
            answer_with_index(|index| diff::impact(index.graph(), Path::new(".")))
        }
        [command, target] if command == "impact" => {
            answer_with_index(|index| query::impact(index.graph(), target))
        }
        _ => Err(help()),
    }
}

fn answer_with_index(
    query_fn: impl FnOnce(&index::Index) -> Result<String, String>,
) -> Result<String, String> {
    let index = index::Index::open(Path::new("."))?;
    if !index.has_supported_source_files() {
        return Ok(index::NO_SUPPORTED_SOURCE_FILES.to_string());
    }
    query_fn(&index)
}

fn help() -> String {
    "BlastRay — code intelligence for coding agents\n\nUsage:\n  blastray find <query>\n  blastray inspect <target>\n  blastray trace <from> <to>\n  blastray impact <target>\n  blastray impact --diff\n  blastray mcp\n\nTargets accept a canonical symbol identity such as src/auth/session.ts::refreshSession.".to_string()
}
