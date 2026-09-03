use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use ignore::WalkBuilder;

use crate::parse::{ImportBinding, ImportDraft, ParsedFile, SymbolDraft, parse_file};

const EXTENSIONS: [&str; 4] = ["ts", "tsx", "js", "jsx"];
const SKIP_DIRECTORIES: [&str; 6] = [".git", "node_modules", "dist", "build", "coverage", ".next"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SymbolKind {
    Function,
    Class,
    Method,
}

impl SymbolKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Class => "class",
            Self::Method => "method",
        }
    }
}

#[derive(Clone, Debug)]
pub struct File {
    pub path: String,
}

#[derive(Clone, Debug)]
pub struct Symbol {
    pub canonical: String,
    pub name: String,
    pub file: usize,
    pub line: usize,
    pub column: usize,
    pub kind: SymbolKind,
    exported: bool,
    default_export: bool,
}

#[derive(Clone, Debug)]
pub struct DefineEdge {
    pub file: usize,
    pub symbol: usize,
}

#[derive(Clone, Debug)]
pub struct ImportEdge {
    pub from: usize,
    pub to: usize,
}

#[derive(Clone, Debug)]
pub struct CallEdge {
    pub from: usize,
    pub to: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelationshipStatus {
    Unresolved,
    Ambiguous,
}

impl RelationshipStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unresolved => "UNRESOLVED",
            Self::Ambiguous => "AMBIGUOUS",
        }
    }
}

#[derive(Clone, Debug)]
pub struct RelationshipIssue {
    pub status: RelationshipStatus,
    pub source: String,
    pub line: usize,
    pub column: usize,
    pub name: String,
    pub detail: String,
}

#[derive(Debug)]
pub struct Graph {
    pub files: Vec<File>,
    pub symbols: Vec<Symbol>,
    pub defines: Vec<DefineEdge>,
    pub imports: Vec<ImportEdge>,
    pub calls: Vec<CallEdge>,
    pub issues: Vec<RelationshipIssue>,
}

impl Graph {
    pub fn defining_file(&self, symbol: usize) -> Option<usize> {
        self.defines
            .iter()
            .find_map(|edge| (edge.symbol == symbol).then_some(edge.file))
    }

    pub fn imported_files(&self, file: usize) -> Vec<usize> {
        self.imports
            .iter()
            .filter_map(|edge| (edge.from == file).then_some(edge.to))
            .collect()
    }

    pub fn symbol_candidates(&self, selector: &str) -> Vec<usize> {
        let exact: Vec<usize> = self
            .symbols
            .iter()
            .enumerate()
            .filter_map(|(index, symbol)| (symbol.canonical == selector).then_some(index))
            .collect();
        if !exact.is_empty() {
            return exact;
        }

        self.symbols
            .iter()
            .enumerate()
            .filter_map(|(index, symbol)| (symbol.name == selector).then_some(index))
            .collect()
    }
}

pub fn build(root: &Path) -> Result<Graph, String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("cannot read repository root {}: {error}", root.display()))?;
    let paths = source_files(&root)?;
    let mut parsed = Vec::with_capacity(paths.len());

    for path in paths {
        let relative = relative_path(&root, &path)?;
        let source = std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {relative}: {error}"))?;
        parsed.push(parse_file(&relative, &source)?);
    }

    Ok(resolve(parsed))
}

fn source_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .filter_entry(|entry| {
            !entry.file_type().is_some_and(|kind| {
                kind.is_dir()
                    && SKIP_DIRECTORIES.contains(&entry.file_name().to_string_lossy().as_ref())
            })
        })
        .build();

    for entry in walker {
        let entry = entry.map_err(|error| format!("cannot walk {}: {error}", root.display()))?;
        if entry.file_type().is_some_and(|kind| kind.is_file()) && is_source_file(entry.path()) {
            paths.push(entry.into_path());
        }
    }

    paths.sort();
    Ok(paths)
}

fn is_source_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| EXTENSIONS.contains(&extension))
}

fn relative_path(root: &Path, path: &Path) -> Result<String, String> {
    path.strip_prefix(root)
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .map_err(|error| {
            format!(
                "cannot make {} relative to {}: {error}",
                path.display(),
                root.display()
            )
        })
}

fn resolve(parsed: Vec<ParsedFile>) -> Graph {
    let files: Vec<File> = parsed
        .iter()
        .map(|file| File {
            path: file.path.clone(),
        })
        .collect();
    let file_ids: BTreeMap<String, usize> = files
        .iter()
        .enumerate()
        .map(|(index, file)| (file.path.clone(), index))
        .collect();
    let mut drafts: Vec<SymbolDraft> = parsed
        .iter()
        .flat_map(|file| file.symbols.clone())
        .collect();
    drafts.sort_by(|left, right| left.canonical.cmp(&right.canonical));

    let symbols: Vec<Symbol> = drafts
        .iter()
        .map(|draft| Symbol {
            canonical: draft.canonical.clone(),
            name: draft.name.clone(),
            file: file_ids[&draft.file],
            line: draft.line,
            column: draft.column,
            kind: draft.kind,
            exported: draft.exported,
            default_export: draft.default_export,
        })
        .collect();
    let defines: Vec<DefineEdge> = symbols
        .iter()
        .enumerate()
        .map(|(symbol, item)| DefineEdge {
            file: item.file,
            symbol,
        })
        .collect();
    let canonical_ids = ids_by(&symbols, |symbol| symbol.canonical.clone());
    let local_function_ids = ids_by_function(&symbols, &files);
    let exports = export_ids(&symbols);
    let mut resolution = Resolution::default();

    for file in &parsed {
        let file_id = file_ids[&file.path];
        for import in &file.imports {
            resolve_import(
                import,
                file_id,
                &file.path,
                &file_ids,
                &exports,
                &mut resolution,
            );
        }
    }

    let mut calls = Vec::new();
    for file in &parsed {
        for draft in &file.symbols {
            let Some(source_ids) = canonical_ids.get(&draft.canonical) else {
                continue;
            };
            if source_ids.len() != 1 {
                continue;
            }
            let source = source_ids[0];
            for call in &draft.calls {
                if !call.direct {
                    resolution.issues.push(issue(
                        RelationshipStatus::Unresolved,
                        &draft.canonical,
                        call.line,
                        call.column,
                        &call.name,
                        "receiver or dynamic call syntax is outside the Mission 1 subset",
                    ));
                    continue;
                }
                if draft.shadowed.contains(&call.name) {
                    resolution.issues.push(issue(
                        RelationshipStatus::Unresolved,
                        &draft.canonical,
                        call.line,
                        call.column,
                        &call.name,
                        "an unmodeled local binding could shadow this name",
                    ));
                    continue;
                }

                let key = (file.path.clone(), call.name.clone());
                let mut candidates = local_function_ids.get(&key).cloned().unwrap_or_default();
                candidates.extend(binding_targets(
                    resolution.bindings.get(&key),
                    &mut resolution.issues,
                    &draft.canonical,
                    call,
                ));
                candidates.sort_unstable();
                candidates.dedup();

                match candidates.as_slice() {
                    [target] => calls.push(CallEdge {
                        from: source,
                        to: *target,
                    }),
                    [] if !resolution.bindings.contains_key(&key) => resolution.issues.push(issue(
                        RelationshipStatus::Unresolved,
                        &draft.canonical,
                        call.line,
                        call.column,
                        &call.name,
                        "no matching local function or resolved import",
                    )),
                    [] => {}
                    _ => resolution.issues.push(issue(
                        RelationshipStatus::Ambiguous,
                        &draft.canonical,
                        call.line,
                        call.column,
                        &call.name,
                        "multiple callable definitions match this name",
                    )),
                }
            }
        }
    }

    resolution.imports.sort_by_key(|edge| (edge.from, edge.to));
    resolution.imports.dedup_by_key(|edge| (edge.from, edge.to));
    calls.sort_by(|left, right| {
        symbols[left.from]
            .canonical
            .cmp(&symbols[right.from].canonical)
            .then_with(|| symbols[left.to].canonical.cmp(&symbols[right.to].canonical))
    });
    calls.dedup_by_key(|edge| (edge.from, edge.to));
    resolution.issues.sort_by(|left, right| {
        left.source
            .cmp(&right.source)
            .then(left.line.cmp(&right.line))
            .then(left.column.cmp(&right.column))
            .then(left.name.cmp(&right.name))
            .then(left.detail.cmp(&right.detail))
    });

    Graph {
        files,
        symbols,
        defines,
        imports: resolution.imports,
        calls,
        issues: resolution.issues,
    }
}

fn ids_by<T>(symbols: &[Symbol], key: impl Fn(&Symbol) -> T) -> BTreeMap<T, Vec<usize>>
where
    T: Ord,
{
    let mut ids = BTreeMap::new();
    for (index, symbol) in symbols.iter().enumerate() {
        ids.entry(key(symbol)).or_insert_with(Vec::new).push(index);
    }
    ids
}

fn ids_by_function(symbols: &[Symbol], files: &[File]) -> BTreeMap<(String, String), Vec<usize>> {
    let mut ids = BTreeMap::new();
    for (index, symbol) in symbols.iter().enumerate() {
        if symbol.kind == SymbolKind::Function {
            ids.entry((files[symbol.file].path.clone(), symbol.name.clone()))
                .or_insert_with(Vec::new)
                .push(index);
        }
    }
    ids
}

fn export_ids(symbols: &[Symbol]) -> BTreeMap<(usize, String, bool), Vec<usize>> {
    let mut ids = BTreeMap::new();
    for (index, symbol) in symbols.iter().enumerate() {
        if symbol.exported && symbol.kind == SymbolKind::Function {
            ids.entry((symbol.file, symbol.name.clone(), false))
                .or_insert_with(Vec::new)
                .push(index);
        }
        if symbol.default_export {
            ids.entry((symbol.file, String::new(), true))
                .or_insert_with(Vec::new)
                .push(index);
        }
    }
    ids
}

#[derive(Clone, Copy)]
enum BindingTarget {
    Resolved(usize),
    Unresolved,
    Ambiguous,
}

#[derive(Default)]
struct Resolution {
    imports: Vec<ImportEdge>,
    bindings: BTreeMap<(String, String), Vec<BindingTarget>>,
    issues: Vec<RelationshipIssue>,
}

fn resolve_import(
    import: &ImportDraft,
    from: usize,
    from_path: &str,
    files: &BTreeMap<String, usize>,
    exports: &BTreeMap<(usize, String, bool), Vec<usize>>,
    resolution: &mut Resolution,
) {
    let candidates = module_candidates(from_path, &import.module, files);
    let target = match candidates.as_slice() {
        [target] => {
            resolution.imports.push(ImportEdge { from, to: *target });
            Some(*target)
        }
        [] => {
            let detail = if import.module.starts_with("./") || import.module.starts_with("../") {
                "relative module was not found"
            } else {
                "non-relative module imports are unsupported"
            };
            resolution.issues.push(issue(
                RelationshipStatus::Unresolved,
                from_path,
                import.line,
                import.column,
                &import.module,
                detail,
            ));
            None
        }
        _ => {
            resolution.issues.push(issue(
                RelationshipStatus::Ambiguous,
                from_path,
                import.line,
                import.column,
                &import.module,
                "more than one source file matches this relative module",
            ));
            None
        }
    };

    if let Some(detail) = &import.unsupported {
        resolution.issues.push(issue(
            RelationshipStatus::Unresolved,
            from_path,
            import.line,
            import.column,
            &import.module,
            detail,
        ));
    }

    for binding in &import.bindings {
        let key = (from_path.to_string(), binding.local().to_string());
        let value = if import.type_only {
            BindingTarget::Unresolved
        } else if let Some(target) = target {
            let export_key = match binding {
                ImportBinding::Named { imported, .. } => (target, imported.clone(), false),
                ImportBinding::Default { .. } => (target, String::new(), true),
            };
            match exports.get(&export_key).map(Vec::as_slice).unwrap_or(&[]) {
                [symbol] => BindingTarget::Resolved(*symbol),
                [] => BindingTarget::Unresolved,
                _ => BindingTarget::Ambiguous,
            }
        } else if candidates.len() > 1 {
            BindingTarget::Ambiguous
        } else {
            BindingTarget::Unresolved
        };

        if !matches!(value, BindingTarget::Resolved(_)) {
            let detail = if import.type_only {
                "type-only imports are not callable"
            } else if target.is_some() {
                "the imported symbol was not uniquely exported by the resolved module"
            } else {
                "the imported binding cannot be resolved until its module is resolved"
            };
            resolution.issues.push(issue(
                match value {
                    BindingTarget::Ambiguous => RelationshipStatus::Ambiguous,
                    _ => RelationshipStatus::Unresolved,
                },
                from_path,
                import.line,
                import.column,
                binding.local(),
                detail,
            ));
        }
        resolution.bindings.entry(key).or_default().push(value);
    }
}

fn binding_targets(
    entries: Option<&Vec<BindingTarget>>,
    issues: &mut Vec<RelationshipIssue>,
    source: &str,
    call: &crate::parse::CallDraft,
) -> Vec<usize> {
    let Some(entries) = entries else {
        return Vec::new();
    };
    let mut targets = Vec::new();
    let mut unresolved = false;
    let mut ambiguous = false;
    for entry in entries {
        match entry {
            BindingTarget::Resolved(target) => targets.push(*target),
            BindingTarget::Unresolved => unresolved = true,
            BindingTarget::Ambiguous => ambiguous = true,
        }
    }
    if unresolved || ambiguous {
        issues.push(issue(
            if ambiguous {
                RelationshipStatus::Ambiguous
            } else {
                RelationshipStatus::Unresolved
            },
            source,
            call.line,
            call.column,
            &call.name,
            "the imported binding is not uniquely resolved",
        ));
        return Vec::new();
    }
    targets
}

fn module_candidates(from: &str, request: &str, files: &BTreeMap<String, usize>) -> Vec<usize> {
    if !request.starts_with("./") && !request.starts_with("../") {
        return Vec::new();
    }
    let base = Path::new(from)
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(request);
    let Some(base) = normalize_relative(&base) else {
        return Vec::new();
    };
    let base = PathBuf::from(base);
    let mut candidates = BTreeSet::new();
    if is_source_file(&base) {
        insert_candidate(&base, files, &mut candidates);
    } else {
        for extension in EXTENSIONS {
            insert_candidate(&base.with_extension(extension), files, &mut candidates);
            insert_candidate(
                &base.join("index").with_extension(extension),
                files,
                &mut candidates,
            );
        }
    }
    candidates.into_iter().collect()
}

fn insert_candidate(
    path: &Path,
    files: &BTreeMap<String, usize>,
    candidates: &mut BTreeSet<usize>,
) {
    let key = path.to_string_lossy().replace('\\', "/");
    if let Some(file) = files.get(&key) {
        candidates.insert(*file);
    }
}

fn normalize_relative(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn issue(
    status: RelationshipStatus,
    source: &str,
    line: usize,
    column: usize,
    name: &str,
    detail: &str,
) -> RelationshipIssue {
    RelationshipIssue {
        status,
        source: source.to_string(),
        line,
        column,
        name: name.to_string(),
        detail: detail.to_string(),
    }
}
