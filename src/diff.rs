use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use crate::index::{Graph, RelationshipIssue, is_source_file};
use crate::language::{SymbolFact, parse};
use crate::query::reverse_impact;

#[derive(Clone, Copy, Eq, PartialEq)]
enum ChangeKind {
    Modified,
    Added,
    Deleted,
    Renamed,
    Other,
}

#[derive(Clone)]
struct FileChange {
    kind: ChangeKind,
    path: String,
}

#[derive(Clone, Copy)]
struct LineRange {
    start: usize,
    count: usize,
}

#[derive(Clone, Copy)]
struct Hunk {
    old: LineRange,
    new: LineRange,
}

struct Mapping<'a> {
    exact_roots: &'a mut BTreeSet<usize>,
    conservative_files: &'a mut BTreeMap<String, String>,
    deleted: &'a mut Vec<String>,
    incomplete: &'a mut Vec<String>,
}

pub fn impact(graph: &Graph, root: &Path) -> Result<String, String> {
    require_git_head(root)?;
    let changes = changed_files(root)?;
    let untracked = untracked_supported_files(root)?;
    let diff = git(
        root,
        &[
            "diff",
            "HEAD",
            "--no-ext-diff",
            "--no-color",
            "--unified=0",
            "--",
        ],
    )?;
    let diff = String::from_utf8(diff).map_err(|_| {
        "Git diff output was not valid UTF-8; cannot analyze diff impact.".to_string()
    })?;

    let mut exact_roots = BTreeSet::new();
    let mut conservative_files = BTreeMap::new();
    let mut incomplete = Vec::new();
    let mut unsupported = Vec::new();
    let mut deleted = Vec::new();
    let mut modified = BTreeSet::new();

    for change in &changes {
        if is_blastray_path(&change.path) {
            continue;
        }
        if !is_source_file(Path::new(&change.path)) {
            unsupported.push(change.path.clone());
            incomplete.push(format!(
                "unsupported changed file {} was not structurally analyzed",
                change.path
            ));
            continue;
        }
        match change.kind {
            ChangeKind::Modified => {
                modified.insert(change.path.clone());
            }
            ChangeKind::Added => incomplete.push(format!(
                "added source file {} is not structurally mapped",
                change.path
            )),
            ChangeKind::Deleted => incomplete.push(format!(
                "deleted source file {} is not structurally mapped",
                change.path
            )),
            ChangeKind::Renamed => incomplete.push(format!(
                "renamed source file {} is not structurally mapped",
                change.path
            )),
            ChangeKind::Other => {
                incomplete.push(format!("unsupported Git file change for {}", change.path))
            }
        }
    }

    let hunks = parse_hunks(&diff, &modified);
    if !diff.trim().is_empty() && changes.is_empty() {
        incomplete.push("Git diff output could not be matched to changed files".to_string());
    }
    for path in &modified {
        let Some(file_hunks) = hunks.get(path) else {
            incomplete.push(format!("could not parse changed line ranges for {path}"));
            continue;
        };
        let mut mapping = Mapping {
            exact_roots: &mut exact_roots,
            conservative_files: &mut conservative_files,
            deleted: &mut deleted,
            incomplete: &mut incomplete,
        };
        map_modified_file(graph, root, path, file_hunks, &mut mapping);
    }

    for path in &untracked {
        incomplete.push(format!(
            "untracked supported source file {path} is not included in diff impact"
        ));
    }

    let mut conservative_roots = BTreeSet::new();
    for path in conservative_files.keys() {
        let symbols = symbols_in_file(graph, path);
        if symbols.is_empty() {
            incomplete.push(format!(
                "unmapped changed region in {path}; file has no indexed symbols"
            ));
        } else {
            conservative_roots.extend(symbols);
        }
    }
    conservative_roots.retain(|root| !exact_roots.contains(root));
    let mut roots = exact_roots.clone();
    roots.extend(&conservative_roots);
    let traversal = reverse_impact(graph, &roots);
    let affected_count: usize = traversal.by_depth.values().map(Vec::len).sum();

    let changed_paths: BTreeSet<String> = roots
        .iter()
        .map(|root| graph.files[graph.symbols[*root].file].path.clone())
        .chain(conservative_files.keys().cloned())
        .collect();
    let relevant_issues: Vec<&RelationshipIssue> = graph
        .issues
        .iter()
        .filter(|issue| issue_is_in_files(issue, &changed_paths))
        .collect();

    let mut output = String::from("Diff impact: HEAD -> working tree");
    if changes.is_empty() {
        output.push_str("\nNo tracked changes relative to HEAD.");
    }
    append_root_section(&mut output, "Changed symbols", graph, &exact_roots);
    if conservative_roots.is_empty() {
        if !conservative_files.is_empty() {
            output.push_str("\nConservative file-level roots: none (no indexed symbols)");
        }
    } else {
        output.push_str("\nConservative file-level roots:");
        for symbol in &conservative_roots {
            let path = &graph.files[graph.symbols[*symbol].file].path;
            output.push_str(&format!(
                "\n- {} ({})",
                graph.symbols[*symbol].canonical, conservative_files[path]
            ));
        }
    }
    append_strings(&mut output, "Deleted or unmappable regions", &deleted);
    append_strings(&mut output, "Unsupported changed files", &unsupported);
    append_strings(&mut output, "Untracked supported source files", &untracked);

    output.push_str("\nConfirmed downstream impact:");
    if traversal.by_depth.is_empty() {
        output.push_str(" none");
    } else {
        for (depth, symbols) in &traversal.by_depth {
            output.push_str(&format!("\nDepth {depth}:"));
            for symbol in symbols {
                output.push_str(&format!("\n- {}", graph.symbols[*symbol].canonical));
            }
        }
    }
    output.push_str(&format!(
        "\nTotal changed symbols: {}\nTotal confirmed affected symbols: {affected_count}",
        exact_roots.len()
    ));
    if traversal.truncated {
        incomplete.push("reverse traversal was truncated at 500 symbols".to_string());
    }
    if !relevant_issues.is_empty() {
        incomplete.push(
            "unresolved or ambiguous relationships in changed files may hide impact".to_string(),
        );
        output.push_str("\nPotentially hidden relationships:");
        for issue in relevant_issues {
            output.push_str(&format!(
                "\n- {} {}:{}:{} '{}' — {}",
                issue.status.label(),
                issue.source,
                issue.line,
                issue.column,
                issue.name,
                issue.detail
            ));
        }
    }
    incomplete.sort();
    incomplete.dedup();
    if incomplete.is_empty() {
        output.push_str("\nCompleteness: exact within BlastRay's supported structural subset.");
    } else {
        output.push_str("\nCompleteness: conservative/incomplete:");
        for reason in incomplete {
            output.push_str(&format!("\n- {reason}"));
        }
    }
    Ok(output)
}

fn map_modified_file(
    graph: &Graph,
    root: &Path,
    path: &str,
    hunks: &[Hunk],
    mapping: &mut Mapping<'_>,
) {
    let current_symbols = symbols_in_file(graph, path);
    let old_symbols = match git(root, &["show", &format!("HEAD:{path}")]) {
        Ok(source) => match String::from_utf8(source)
            .map_err(|_| "old source was not valid UTF-8".to_string())
            .and_then(|source| parse(path, &source))
        {
            Ok(parsed) => Some(parsed.symbols()),
            Err(error) => {
                mapping
                    .incomplete
                    .push(format!("cannot parse HEAD version of {path}: {error}"));
                None
            }
        },
        Err(error) => {
            mapping
                .incomplete
                .push(format!("cannot read HEAD version of {path}: {error}"));
            None
        }
    };

    for hunk in hunks {
        let mut mapped = false;
        for line in lines(hunk.new) {
            if let Some(symbol) = narrowest_current_symbol(graph, &current_symbols, line) {
                mapping.exact_roots.insert(symbol);
                mapped = true;
            }
        }
        if hunk.old.count > 0
            && let Some(old_symbols) = &old_symbols
        {
            for line in lines(hunk.old) {
                if let Some(old) = narrowest_old_symbol(old_symbols, line) {
                    let current = graph.symbol_candidates(&old.canonical);
                    if current.len() == 1 {
                        mapping.exact_roots.insert(current[0]);
                        mapped = true;
                    } else {
                        mapping
                            .deleted
                            .push(format!("{} (deleted or renamed)", old.canonical));
                        mapping.incomplete.push(format!(
                            "deleted or renamed symbol {} has no current graph node",
                            old.canonical
                        ));
                    }
                }
            }
        }
        if !mapped {
            mapping
                .conservative_files
                .entry(path.to_string())
                .or_insert_with(|| {
                    "changed lines were outside indexed symbols; all file symbols are possible roots"
                        .to_string()
                });
        }
    }
}

fn lines(range: LineRange) -> impl Iterator<Item = usize> {
    range.start..range.start.saturating_add(range.count)
}

fn narrowest_current_symbol(graph: &Graph, symbols: &[usize], line: usize) -> Option<usize> {
    symbols
        .iter()
        .copied()
        .filter(|symbol| {
            let symbol = &graph.symbols[*symbol];
            symbol.line <= line && line <= symbol.end_line
        })
        .min_by_key(|symbol| {
            let symbol = &graph.symbols[*symbol];
            (symbol.end_line - symbol.line, symbol.canonical.clone())
        })
}

fn narrowest_old_symbol(symbols: &[SymbolFact], line: usize) -> Option<&SymbolFact> {
    symbols
        .iter()
        .filter(|symbol| symbol.line <= line && line <= symbol.end_line)
        .min_by_key(|symbol| (symbol.end_line - symbol.line, symbol.canonical.clone()))
}

fn symbols_in_file(graph: &Graph, path: &str) -> Vec<usize> {
    graph
        .symbols
        .iter()
        .enumerate()
        .filter_map(|(index, symbol)| (graph.files[symbol.file].path == path).then_some(index))
        .collect()
}

fn issue_is_in_files(issue: &RelationshipIssue, paths: &BTreeSet<String>) -> bool {
    paths
        .iter()
        .any(|path| issue.source == *path || issue.source.starts_with(&format!("{path}::")))
}

fn append_root_section(output: &mut String, heading: &str, graph: &Graph, roots: &BTreeSet<usize>) {
    output.push_str(&format!("\n{heading}:"));
    if roots.is_empty() {
        output.push_str(" none");
        return;
    }
    for root in roots {
        output.push_str(&format!("\n- {}", graph.symbols[*root].canonical));
    }
}

fn append_strings(output: &mut String, heading: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    output.push_str(&format!("\n{heading}:"));
    for item in items {
        output.push_str(&format!("\n- {item}"));
    }
}

fn require_git_head(root: &Path) -> Result<(), String> {
    let inside = git(root, &["rev-parse", "--is-inside-work-tree"])
        .ok()
        .is_some_and(|value| value == b"true\n");
    if !inside || git(root, &["rev-parse", "--verify", "HEAD"]).is_err() {
        return Err(
            "impact --diff requires a Git repository with a resolvable HEAD. Commit once, then retry."
                .to_string(),
        );
    }
    Ok(())
}

fn changed_files(root: &Path) -> Result<Vec<FileChange>, String> {
    let output = git(
        root,
        &[
            "diff",
            "--name-status",
            "-z",
            "--find-renames",
            "HEAD",
            "--",
        ],
    )?;
    let values: Vec<&[u8]> = output
        .split(|byte| *byte == 0)
        .filter(|item| !item.is_empty())
        .collect();
    let mut index = 0;
    let mut changes = Vec::new();
    while index < values.len() {
        let status = std::str::from_utf8(values[index])
            .map_err(|_| "Git reported a non-UTF-8 status entry.".to_string())?;
        index += 1;
        let kind = match status.chars().next() {
            Some('M') => ChangeKind::Modified,
            Some('A') => ChangeKind::Added,
            Some('D') => ChangeKind::Deleted,
            Some('R') => ChangeKind::Renamed,
            _ => ChangeKind::Other,
        };
        let needs_two_paths = matches!(kind, ChangeKind::Renamed);
        let path_index = if needs_two_paths { index + 1 } else { index };
        if path_index >= values.len() {
            return Err("Git reported a truncated file-status entry.".to_string());
        }
        let path = std::str::from_utf8(values[path_index])
            .map_err(|_| "Git reported a non-UTF-8 path.".to_string())?
            .to_string();
        index += if needs_two_paths { 2 } else { 1 };
        changes.push(FileChange { kind, path });
    }
    changes.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(changes)
}

fn untracked_supported_files(root: &Path) -> Result<Vec<String>, String> {
    let output = git(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let mut paths = BTreeSet::new();
    for entry in output
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        if entry.starts_with(b"?? ") {
            let path = std::str::from_utf8(&entry[3..])
                .map_err(|_| "Git reported a non-UTF-8 untracked path.".to_string())?;
            if !is_blastray_path(path) && is_source_file(Path::new(path)) {
                paths.insert(path.to_string());
            }
        }
    }
    Ok(paths.into_iter().collect())
}

fn parse_hunks(diff: &str, modified: &BTreeSet<String>) -> BTreeMap<String, Vec<Hunk>> {
    let mut result = BTreeMap::new();
    let mut current = None;
    let mut saw_hunk = false;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            current = None;
            saw_hunk = false;
        } else if line.starts_with("+++ b/") && !saw_hunk {
            let path = &line[6..];
            current = modified.contains(path).then(|| path.to_string());
        } else if let Some((old, new)) = parse_hunk_header(line) {
            saw_hunk = true;
            if let Some(path) = &current {
                result
                    .entry(path.clone())
                    .or_insert_with(Vec::new)
                    .push(Hunk { old, new });
            }
        }
    }
    result
}

fn parse_hunk_header(line: &str) -> Option<(LineRange, LineRange)> {
    let body = line.strip_prefix("@@ -")?.split_once(" @@")?.0;
    let (old, new) = body.split_once(" +")?;
    Some((parse_range(old)?, parse_range(new)?))
}

fn parse_range(value: &str) -> Option<LineRange> {
    let (start, count) = match value.split_once(',') {
        Some((start, count)) => (start.parse().ok()?, count.parse().ok()?),
        None => (value.parse().ok()?, 1),
    };
    Some(LineRange { start, count })
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|error| format!("cannot run Git: {error}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if detail.is_empty() {
            "Git command failed".to_string()
        } else {
            format!("Git command failed: {detail}")
        })
    }
}

fn is_blastray_path(path: &str) -> bool {
    path == ".blastray" || path.starts_with(".blastray/")
}

#[cfg(test)]
mod tests {
    use super::{LineRange, parse_hunk_header};

    #[test]
    fn parses_zero_context_hunks_and_deletions() {
        assert!(matches!(
            parse_hunk_header("@@ -4,2 +3,0 @@"),
            Some((
                LineRange { start: 4, count: 2 },
                LineRange { start: 3, count: 0 }
            ))
        ));
    }
}
