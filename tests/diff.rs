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
    assert!(output.matches("src/use.ts::caller").count() >= 1);
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
fn unsupported_changed_files_make_completeness_incomplete() {
    let repo = Repo::new(&[
        ("src/a.ts", "export function a() {}\n"),
        ("notes.txt", "original\n"),
    ]);
    repo.write("notes.txt", "changed\n");
    let output = repo.impact();
    assert!(output.contains("Unsupported changed files:\n- notes.txt"));
    assert!(output.contains("Completeness: conservative/incomplete"));
    assert!(output.contains("unsupported changed file notes.txt was not structurally analyzed"));
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

#[test]
fn maps_rust_function_and_impl_method_edits_to_narrowest_symbols() {
    let repo = Repo::new(&[(
        "src/lib.rs",
        "fn leaf() {}\nfn entry() { leaf(); }\nstruct Worker;\nimpl Worker {\n    fn leaf(&self) {}\n    fn entry(&self) { self.leaf(); }\n}\n",
    )]);
    repo.write(
        "src/lib.rs",
        "fn leaf() { let _ = 1; }\nfn entry() { leaf(); }\nstruct Worker;\nimpl Worker {\n    fn leaf(&self) { let _ = 2; }\n    fn entry(&self) { self.leaf(); }\n}\n",
    );
    let output = repo.impact();
    assert!(output.contains("src/lib.rs::leaf"));
    assert!(output.contains("src/lib.rs::Worker.leaf"));
    assert!(output.contains("src/lib.rs::entry"));
    assert!(output.contains("src/lib.rs::Worker.entry"));
}

#[test]
fn maps_java_method_body_edits_to_the_narrowest_symbol() {
    let repo = Repo::new(&[(
        "src/demo/Worker.java",
        "package demo;\nclass Worker {\n  void leaf() {}\n  void entry() { this.leaf(); }\n}\n",
    )]);
    repo.write(
        "src/demo/Worker.java",
        "package demo;\nclass Worker {\n  void leaf() { int value = 1; }\n  void entry() { this.leaf(); }\n}\n",
    );
    let output = repo.impact();
    assert!(output.contains("src/demo/Worker.java::Worker.leaf"));
    assert!(output.contains("src/demo/Worker.java::Worker.entry"));
    assert!(!output.contains("Changed symbols:\n- src/demo/Worker.java::Worker\n"));
}

#[test]
fn diff_impact_propagates_a_changed_typescript_contract_to_implementers() {
    let repo = Repo::new(&[(
        "src/store.ts",
        "export interface Store {\n  save(): void;\n}\nexport class ConcreteStore implements Store {\n  save(): void {}\n}\nexport class CachedStore extends ConcreteStore {}\n",
    )]);
    repo.write(
        "src/store.ts",
        "export interface Store {\n  save(): void;\n  load(): void;\n}\nexport class ConcreteStore implements Store {\n  save(): void {}\n}\nexport class CachedStore extends ConcreteStore {}\n",
    );
    let output = repo.impact();
    assert!(output.contains("src/store.ts::Store"));
    assert!(output.contains("src/store.ts::ConcreteStore"));
    assert!(output.contains("ConcreteStore -> IMPLEMENTS"));
    assert!(output.contains("src/store.ts::CachedStore"));
}

#[test]
fn maps_python_function_and_method_edits_to_common_symbol_spans() {
    let function = Repo::new(&[(
        "pkg/main.py",
        "def leaf():\n    pass\n\ndef entry():\n    leaf()\n",
    )]);
    function.write(
        "pkg/main.py",
        "def leaf():\n    return 1\n\ndef entry():\n    leaf()\n",
    );
    let function_output = function.impact();
    assert!(function_output.contains("pkg/main.py::leaf"));
    assert!(function_output.contains("pkg/main.py::entry"));

    let method = Repo::new(&[(
        "pkg/worker.py",
        "class Worker:\n    def leaf(self):\n        pass\n\n    def entry(self):\n        self.leaf()\n",
    )]);
    method.write(
        "pkg/worker.py",
        "class Worker:\n    def leaf(self):\n        return 1\n\n    def entry(self):\n        self.leaf()\n",
    );
    let method_output = method.impact();
    assert!(method_output.contains("pkg/worker.py::Worker.leaf"));
    assert!(method_output.contains("pkg/worker.py::Worker.entry"));
    assert!(!method_output.contains("Changed symbols:\n- pkg/worker.py::Worker\n"));
}
