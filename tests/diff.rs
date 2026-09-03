use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use blastray::{diff, index::Index, query};

static NEXT_REPO: AtomicUsize = AtomicUsize::new(0);

struct Repo(PathBuf);

impl Repo {
    fn new(files: &[(&str, &str)]) -> Self {
        let path = std::env::temp_dir().join(format!(
            "blastray-diff-test-{}-{}",
            std::process::id(),
            NEXT_REPO.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        let repo = Self(path);
        repo.git(&["init", "-q"]);
        repo.git(&["config", "user.name", "BlastRay test"]);
        repo.git(&["config", "user.email", "test@example.invalid"]);
        for (path, source) in files {
            repo.write(path, source);
        }
        repo.git(&["add", "."]);
        repo.git(&["commit", "-qm", "initial"]);
        repo
    }

    fn write(&self, path: &str, source: &str) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }

    fn remove(&self, path: &str) {
        fs::remove_file(self.0.join(path)).unwrap();
    }

    fn git(&self, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.0)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn impact(&self) -> String {
        let index = Index::open(&self.0).unwrap();
        diff::impact(index.graph(), &self.0).unwrap()
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn non_git_repo() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "blastray-diff-non-git-test-{}-{}",
        std::process::id(),
        NEXT_REPO.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn requires_git_head_and_reports_clean_worktrees() {
    let path = non_git_repo();
    let graph = Index::build(&path).unwrap();
    assert!(
        diff::impact(graph.graph(), &path)
            .unwrap_err()
            .contains("requires a Git repository")
    );
    fs::remove_dir_all(path).unwrap();

    let repo = Repo::new(&[("src/a.ts", "export function a() {}\n")]);
    let output = repo.impact();
    assert!(output.contains("No tracked changes relative to HEAD."));
    assert!(output.contains("Completeness: exact"));
}

#[test]
fn maps_unstaged_staged_and_combined_function_edits() {
    let repo = Repo::new(&[
        ("src/a.ts", "export function leaf() {}\n"),
        (
            "src/b.ts",
            "import { leaf } from './a';\nexport function entry() { leaf(); }\n",
        ),
    ]);
    repo.write("src/a.ts", "export function leaf() { return; }\n");
    let unstaged = repo.impact();
    assert!(unstaged.contains("src/a.ts::leaf"));
    assert!(unstaged.contains("src/b.ts::entry"));
    repo.git(&["add", "src/a.ts"]);
    let staged = repo.impact();
    assert_eq!(unstaged, staged);

    repo.write(
        "src/b.ts",
        "import { leaf } from './a';\nexport function entry() { leaf(); return; }\n",
    );
    let combined = repo.impact();
    assert!(combined.contains("src/a.ts::leaf"));
    assert!(combined.contains("src/b.ts::entry"));
    assert!(combined.contains("Total changed symbols: 2"));
}

#[test]
fn prefers_methods_and_merges_converging_roots() {
    let repo = Repo::new(&[
        (
            "src/service.ts",
            "export class Service {\n  save() { return 1; }\n}\nexport function one() {}\nexport function two() {}\n",
        ),
        (
            "src/use.ts",
            "import { one, two } from './service';\nexport function caller() { one(); two(); }\n",
        ),
    ]);
    repo.write(
        "src/service.ts",
        "export class Service {\n  save() { return 2; }\n}\nexport function one() { return 1; }\nexport function two() { return 2; }\n",
    );
    let output = repo.impact();
    assert!(output.contains("src/service.ts::Service.save"));
    assert!(!output.contains("Changed symbols:\n- src/service.ts::Service\n"));
    assert_eq!(output.matches("src/use.ts::caller").count(), 1);
}

#[test]
fn maps_top_level_changes_conservatively_and_deletions_from_head() {
    let repo = Repo::new(&[
        (
            "src/a.ts",
            "export function keep() { return 1; }\nexport function dead() {}\n",
        ),
        (
            "src/b.ts",
            "import { keep } from './a';\nexport function use() { keep(); }\n",
        ),
    ]);
    repo.write(
        "src/a.ts",
        "import { other } from './other';\nexport function keep() { return 1; }\nexport function dead() {}\n",
    );
    let import_change = repo.impact();
    assert!(import_change.contains("Conservative file-level roots:"));
    assert!(import_change.contains("changed lines were outside indexed symbols"));

    repo.write("src/a.ts", "export function keep() {\n}\n");
    let deletion = repo.impact();
    assert!(deletion.contains("src/a.ts::keep"));
    assert!(deletion.contains("src/a.ts::dead (deleted or renamed)"));
    assert!(deletion.contains("deleted or renamed symbol src/a.ts::dead"));
}

#[test]
fn reports_unsupported_untracked_and_uncertain_relationships() {
    let repo = Repo::new(&[
        (
            "src/a.ts",
            "export function entry() { entry(); thing.entry(); }\n",
        ),
        ("notes.txt", "original\n"),
    ]);
    repo.write(
        "src/a.ts",
        "export function entry() { entry(); thing.entry(); return; }\n",
    );
    repo.write("notes.txt", "changed\n");
    repo.write("src/untracked.ts", "export function newFile() {}\n");
    let output = repo.impact();
    assert!(output.contains("Potentially hidden relationships:"));
    assert!(output.contains("UNRESOLVED"));
    assert!(output.contains("Unsupported changed files:\n- notes.txt"));
    assert!(output.contains("Untracked supported source files:\n- src/untracked.ts"));
    assert!(output.contains("Completeness: conservative/incomplete"));
}

#[test]
fn refreshes_persistent_cache_deterministically_and_rejects_old_cache() {
    let repo = Repo::new(&[("src/a.ts", "export function a() {}\n")]);
    let first = repo.impact();
    assert!(repo.0.join(".blastray/index.bin").is_file());
    let second = repo.impact();
    assert_eq!(first, second);

    let cache = repo.0.join(".blastray/index.bin");
    let mut bytes = fs::read(&cache).unwrap();
    bytes[..4].copy_from_slice(&1u32.to_le_bytes());
    fs::write(&cache, bytes).unwrap();
    let rebuilt = repo.impact();
    assert_eq!(second, rebuilt);
}

#[test]
fn regular_impact_remains_available_after_diff_index_refresh() {
    let repo = Repo::new(&[
        ("src/a.ts", "export function leaf() {}\n"),
        (
            "src/b.ts",
            "import { leaf } from './a';\nexport function entry() { leaf(); }\n",
        ),
    ]);
    repo.write("src/a.ts", "export function leaf() { return; }\n");
    let index = Index::open(&repo.0).unwrap();
    assert!(
        query::impact(index.graph(), "src/a.ts::leaf")
            .unwrap()
            .contains("src/b.ts::entry")
    );
}

#[test]
fn deleted_source_files_and_renames_are_incomplete_without_blocking_other_files() {
    let repo = Repo::new(&[
        ("src/a.ts", "export function a() {}\n"),
        ("src/b.ts", "export function b() {}\n"),
    ]);
    repo.remove("src/a.ts");
    repo.git(&["mv", "src/b.ts", "src/c.ts"]);
    let output = repo.impact();
    assert!(output.contains("deleted source file src/a.ts"));
    assert!(output.contains("renamed source file src/c.ts"));
    assert!(output.contains("Completeness: conservative/incomplete"));
}

#[test]
fn direct_line_mapping_is_stable_for_repeated_runs() {
    let repo = Repo::new(&[("src/a.ts", "export function a() {}\n")]);
    repo.write("src/a.ts", "export function a() { return; }\n");
    assert_eq!(repo.impact(), repo.impact());
}
