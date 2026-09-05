use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

fn run(args: &[&str]) -> std::process::Output {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic");
    let fixture = std::env::temp_dir().join(format!(
        "blastray-cli-test-{}-{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    ));
    copy_tree(&source, &fixture);
    let output = run_in(&fixture, args);
    fs::remove_dir_all(fixture).unwrap();
    output
}

fn run_in(directory: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_blastray"))
        .args(args)
        .current_dir(directory)
        .output()
        .unwrap()
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn stdout(args: &[&str]) -> String {
    let output = run(args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn help_is_available() {
    let output = Command::new(env!("CARGO_BIN_EXE_blastray"))
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("blastray find"));
}

#[test]
fn unsupported_repositories_explain_the_language_boundary() {
    let repository = std::env::temp_dir().join(format!(
        "blastray-cli-unsupported-test-{}-{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&repository).unwrap();

    let empty = run_in(&repository, &["find", "anything"]);
    assert!(empty.status.success());
    assert_eq!(
        String::from_utf8(empty.stdout).unwrap(),
        "No supported source files found.\nBlastRay currently indexes .ts, .tsx, .js, .jsx, .py, and .rs.\n"
    );

    fs::write(repository.join("main.dart"), "void main() {}\n").unwrap();
    let dart_only = run_in(&repository, &["inspect", "anything"]);
    assert!(dart_only.status.success());
    assert!(
        String::from_utf8(dart_only.stdout)
            .unwrap()
            .starts_with("No supported source files found.")
    );

    fs::write(
        repository.join("main.ts"),
        "export function supported() {}\n",
    )
    .unwrap();
    let mixed = run_in(&repository, &["find", "supported"]);
    assert!(mixed.status.success());
    let mixed = String::from_utf8(mixed.stdout).unwrap();
    assert!(mixed.contains("main.ts::supported"));
    assert!(!mixed.contains("No supported source files"));
    fs::remove_dir_all(repository).unwrap();
}

#[test]
fn find_returns_deterministic_matches() {
    let first = stdout(&["find", "duplicate"]);
    let second = stdout(&["find", "duplicate"]);
    assert_eq!(first, second);
    assert!(first.contains("src/duplicate-a.ts::duplicate"));
    assert!(first.contains("src/duplicate-b.ts::duplicate"));
}

#[test]
fn local_calls_resolve_without_global_name_guessing() {
    let output = stdout(&["inspect", "src/local.ts::middle"]);
    assert!(output.contains("src/local.ts::leaf"));

    let misleading = stdout(&["inspect", "src/misleading.ts::misleading"]);
    assert!(misleading.contains("Direct callees: none"));
    assert!(misleading.contains("UNRESOLVED"));
}

#[test]
fn named_default_and_aliased_relative_imports_resolve() {
    let output = stdout(&["inspect", "src/consumer.ts::useImported"]);
    assert!(output.contains("src/imported.ts::saveUser"));
    assert!(output.contains("src/imported.ts::createUser"));
}

#[test]
fn index_file_imports_resolve() {
    let output = stdout(&["inspect", "src/index-user.ts::usesIndex"]);
    assert!(output.contains("src/index-target/index.ts::fromIndex"));
}

#[test]
fn javascript_and_jsx_extensions_are_indexed() {
    let javascript = stdout(&["inspect", "src/javascript.js::jsEntry"]);
    assert!(javascript.contains("src/javascript.js::jsLeaf"));

    let views = stdout(&["find", "view"]);
    assert!(views.contains("src/view.tsx::TsxView"));
    assert!(views.contains("src/view.jsx::JsxView"));
}

#[test]
fn receiver_calls_remain_unresolved() {
    let output = stdout(&["inspect", "src/local.ts::methodCaller"]);
    assert!(output.contains("Direct callees: none"));
    assert!(output.contains("UNRESOLVED"));
    assert!(output.contains("receiver or dynamic call syntax"));
}

#[test]
fn class_methods_are_symbols_and_can_call_top_level_functions() {
    let output = stdout(&["inspect", "src/local.ts::Worker.run"]);
    assert!(output.contains("[method src/local.ts"));
    assert!(output.contains("src/local.ts::leaf"));
}

#[test]
fn impact_marks_potentially_hidden_callers() {
    let output = stdout(&["impact", "src/local.ts::Storage.save"]);
    assert!(output.contains("conservative/incomplete"));
    assert!(output.contains("receiver or dynamic call syntax"));
}

#[test]
fn ambiguous_selectors_are_rejected() {
    let output = run(&["inspect", "duplicate"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("is ambiguous"));
    assert!(stderr.contains("src/duplicate-a.ts::duplicate"));
    assert!(stderr.contains("src/duplicate-b.ts::duplicate"));
}

#[test]
fn trace_follows_resolved_calls() {
    let output = stdout(&["trace", "src/local.ts::entry", "src/local.ts::leaf"]);
    assert!(
        output.contains("src/local.ts::entry\n -> src/local.ts::middle\n -> src/local.ts::leaf")
    );
    assert!(output.contains("Only RESOLVED calls"));
}

#[test]
fn impact_walks_reverse_calls() {
    let output = stdout(&["impact", "src/local.ts::leaf"]);
    assert!(output.contains("Direct callers:"));
    assert!(output.contains("src/local.ts::middle"));
    assert!(output.contains("Depth 2:"));
    assert!(output.contains("src/local.ts::entry"));
    assert!(output.contains("src/cross.ts::cross"));
}

#[test]
fn cycles_do_not_loop() {
    let output = stdout(&["impact", "src/cycle.ts::cycleA"]);
    assert!(output.contains("Total confirmed affected symbols: 1"));
}
