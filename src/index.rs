use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};

use crate::parse::{ImportBinding, ImportDraft, ParsedFile, SymbolDraft, parse_file};

const EXTENSIONS: [&str; 4] = ["ts", "tsx", "js", "jsx"];
const SKIP_DIRECTORIES: [&str; 7] = [
    ".git",
    ".blastray",
    "node_modules",
    "dist",
    "build",
    "coverage",
    ".next",
];
const CACHE_DIRECTORY: &str = ".blastray";
const CACHE_FILE: &str = "index.bin";
const CACHE_SCHEMA: u32 = 3;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
    pub end_line: usize,
    pub column: usize,
    pub kind: SymbolKind,
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

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct CallSite {
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RelationshipIssue {
    pub status: RelationshipStatus,
    pub source: String,
    pub line: usize,
    pub column: usize,
    pub name: String,
    pub detail: String,
}

#[derive(Clone, Debug)]
pub struct Graph {
    pub files: Vec<File>,
    pub symbols: Vec<Symbol>,
    pub defines: Vec<DefineEdge>,
    pub imports: Vec<ImportEdge>,
    pub calls: Vec<CallEdge>,
    pub issues: Vec<RelationshipIssue>,
    imports_from: Vec<Vec<usize>>,
    callers_of: Vec<Vec<usize>>,
    callees_of: Vec<Vec<usize>>,
    call_sites: BTreeMap<(usize, usize), Vec<CallSite>>,
    canonical_symbols: BTreeMap<String, Vec<usize>>,
    named_symbols: BTreeMap<String, Vec<usize>>,
}

impl Graph {
    pub fn defining_file(&self, symbol: usize) -> Option<usize> {
        self.symbols.get(symbol).map(|item| item.file)
    }

    pub fn imported_files(&self, file: usize) -> &[usize] {
        self.imports_from
            .get(file)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn callers(&self, symbol: usize) -> &[usize] {
        self.callers_of
            .get(symbol)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn callees(&self, symbol: usize) -> &[usize] {
        self.callees_of
            .get(symbol)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn call_sites(&self, from: usize, to: usize) -> &[CallSite] {
        self.call_sites
            .get(&(from, to))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn symbol_candidates(&self, selector: &str) -> Vec<usize> {
        self.canonical_symbols
            .get(selector)
            .filter(|ids| !ids.is_empty())
            .cloned()
            .or_else(|| self.named_symbols.get(selector).cloned())
            .unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefreshKind {
    Incremental,
    FullRebuild,
}

pub struct Index {
    root: PathBuf,
    hashes: BTreeMap<String, [u8; 32]>,
    parsed: BTreeMap<String, ParsedFile>,
    resolved: BTreeMap<String, FileResolution>,
    graph: Graph,
}

impl Index {
    pub fn build(root: &Path) -> Result<Self, String> {
        let root = root
            .canonicalize()
            .map_err(|error| format!("cannot read repository root {}: {error}", root.display()))?;
        let hashes = source_hashes(&root)?;
        Self::build_with_hashes(root, hashes)
    }

    pub fn open(root: &Path) -> Result<Self, String> {
        let root = root
            .canonicalize()
            .map_err(|error| format!("cannot read repository root {}: {error}", root.display()))?;
        add_git_exclude(&root);
        let mut index = match Self::load(&root) {
            Some(index) => index,
            None => return Self::build_and_persist(root.clone(), source_hashes(&root)?),
        };
        index.root = root;
        index.sync()?;
        Ok(index)
    }

    pub fn sync(&mut self) -> Result<(), String> {
        let current_hashes = source_hashes(&self.root)?;
        if !self.hashes.keys().eq(current_hashes.keys()) {
            *self = Self::build_with_hashes(self.root.clone(), current_hashes)?;
            self.persist()?;
            return Ok(());
        }
        let modified: Vec<String> = current_hashes
            .iter()
            .filter(|(path, hash)| self.hashes.get(*path) != Some(*hash))
            .map(|(path, _)| path.clone())
            .collect();
        if modified.is_empty() {
            return Ok(());
        }
        for path in modified {
            if self.refresh(Path::new(&path))? == RefreshKind::FullRebuild {
                *self = Self::build_with_hashes(self.root.clone(), current_hashes)?;
                self.persist()?;
                return Ok(());
            }
        }
        self.hashes = current_hashes;
        self.persist()
    }

    fn build_and_persist(
        root: PathBuf,
        hashes: BTreeMap<String, [u8; 32]>,
    ) -> Result<Self, String> {
        let index = Self::build_with_hashes(root, hashes)?;
        index.persist()?;
        Ok(index)
    }

    fn build_with_hashes(
        root: PathBuf,
        hashes: BTreeMap<String, [u8; 32]>,
    ) -> Result<Self, String> {
        let mut parsed = BTreeMap::new();
        for relative in hashes.keys() {
            let path = root.join(relative);
            let source = std::fs::read_to_string(&path)
                .map_err(|error| format!("cannot read {relative}: {error}"))?;
            parsed.insert(relative.clone(), parse_file(relative, &source)?);
        }
        let context = ResolveContext::new(&parsed);
        let resolved = parsed
            .values()
            .map(|file| (file.path.clone(), resolve_file(file, &context)))
            .collect();
        let graph = materialize_graph(&parsed, &resolved);
        Ok(Self {
            root,
            hashes,
            parsed,
            resolved,
            graph,
        })
    }

    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    pub fn refresh(&mut self, modified_path: &Path) -> Result<RefreshKind, String> {
        let relative = match self.relative_refresh_path(modified_path) {
            Some(path) => path,
            None => return self.full_rebuild(),
        };
        let source_path = self.root.join(&relative);
        if !is_source_file(Path::new(&relative))
            || !self.parsed.contains_key(&relative)
            || !source_path.is_file()
        {
            return self.full_rebuild();
        }

        let importers = self.direct_importers(&relative);
        let source = std::fs::read_to_string(&source_path)
            .map_err(|error| format!("cannot read {relative}: {error}"))?;
        self.parsed
            .insert(relative.clone(), parse_file(&relative, &source)?);
        self.hashes.insert(
            relative.clone(),
            *blake3::hash(source.as_bytes()).as_bytes(),
        );

        let context = ResolveContext::new(&self.parsed);
        let mut affected = importers;
        affected.insert(relative);
        for path in affected {
            let file = self
                .parsed
                .get(&path)
                .expect("affected files remain in the parsed index");
            self.resolved.insert(path, resolve_file(file, &context));
        }
        self.graph = materialize_graph(&self.parsed, &self.resolved);
        Ok(RefreshKind::Incremental)
    }

    fn relative_refresh_path(&self, path: &Path) -> Option<String> {
        if path.is_absolute() {
            return relative_path(&self.root, path).ok();
        }
        normalize_relative(path)
    }

    fn direct_importers(&self, target: &str) -> BTreeSet<String> {
        self.resolved
            .iter()
            .filter(|(_, facts)| facts.imports.iter().any(|import| import == target))
            .map(|(path, _)| path.clone())
            .collect()
    }

    fn full_rebuild(&mut self) -> Result<RefreshKind, String> {
        *self = Self::build(&self.root)?;
        Ok(RefreshKind::FullRebuild)
    }

    fn persist(&self) -> Result<(), String> {
        let cached = CachedIndex {
            hashes: self.hashes.clone(),
            parsed: self.parsed.clone(),
            resolved: self.resolved.clone(),
        };
        let payload = bincode::serialize(&cached)
            .map_err(|error| format!("cannot encode BlastRay cache: {error}"))?;
        let envelope = CacheEnvelope {
            schema: CACHE_SCHEMA,
            checksum: *blake3::hash(&payload).as_bytes(),
            payload,
        };
        let bytes = bincode::serialize(&envelope)
            .map_err(|error| format!("cannot encode BlastRay cache envelope: {error}"))?;
        let directory = self.root.join(CACHE_DIRECTORY);
        fs::create_dir_all(&directory)
            .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
        atomic_write(&directory.join(CACHE_FILE), &bytes)
    }

    fn load(root: &Path) -> Option<Self> {
        let bytes = fs::read(root.join(CACHE_DIRECTORY).join(CACHE_FILE)).ok()?;
        let envelope: CacheEnvelope = bincode::deserialize(&bytes).ok()?;
        if envelope.schema != CACHE_SCHEMA
            || blake3::hash(&envelope.payload).as_bytes() != &envelope.checksum
        {
            return None;
        }
        let cached: CachedIndex = bincode::deserialize(&envelope.payload).ok()?;
        if !cached.valid() {
            return None;
        }
        let graph = materialize_graph(&cached.parsed, &cached.resolved);
        Some(Self {
            root: root.to_path_buf(),
            hashes: cached.hashes,
            parsed: cached.parsed,
            resolved: cached.resolved,
            graph,
        })
    }
}

pub fn build(root: &Path) -> Result<Graph, String> {
    Ok(Index::build(root)?.graph)
}

#[derive(Deserialize, Serialize)]
struct CacheEnvelope {
    schema: u32,
    checksum: [u8; 32],
    payload: Vec<u8>,
}

#[derive(Deserialize, Serialize)]
struct CachedIndex {
    hashes: BTreeMap<String, [u8; 32]>,
    parsed: BTreeMap<String, ParsedFile>,
    resolved: BTreeMap<String, FileResolution>,
}

impl CachedIndex {
    fn valid(&self) -> bool {
        if !self.hashes.keys().eq(self.parsed.keys())
            || !self.parsed.keys().eq(self.resolved.keys())
        {
            return false;
        }
        self.parsed.iter().all(|(path, file)| {
            file.path == *path
                && file.symbols.iter().all(|symbol| {
                    symbol.file == *path && symbol.canonical.starts_with(&format!("{path}::"))
                })
        })
    }
}

fn source_hashes(root: &Path) -> Result<BTreeMap<String, [u8; 32]>, String> {
    let mut hashes = BTreeMap::new();
    for path in source_files(root)? {
        let relative = relative_path(root, &path)?;
        let bytes = fs::read(&path).map_err(|error| format!("cannot read {relative}: {error}"))?;
        hashes.insert(relative, *blake3::hash(&bytes).as_bytes());
    }
    Ok(hashes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let directory = path.parent().expect("cache path has a parent");
    let name = path
        .file_name()
        .expect("cache path has a filename")
        .to_string_lossy();
    for attempt in 0..100 {
        let temporary = directory.join(format!("{name}.tmp-{}-{attempt}", std::process::id()));
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("cannot create {}: {error}", temporary.display())),
        };
        let result = file.write_all(bytes).and_then(|_| file.sync_all());
        drop(file);
        if let Err(error) = result {
            let _ = fs::remove_file(&temporary);
            return Err(format!("cannot write {}: {error}", temporary.display()));
        }
        let result = fs::rename(&temporary, path)
            .map_err(|error| format!("cannot publish {}: {error}", path.display()));
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        return result;
    }
    Err(format!(
        "cannot create a unique temporary cache beside {}",
        path.display()
    ))
}

fn add_git_exclude(root: &Path) {
    let git = root.join(".git");
    if !git.is_dir() {
        return;
    }
    let info = git.join("info");
    let exclude = info.join("exclude");
    let existing = match fs::read_to_string(&exclude) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => return,
    };
    if existing.lines().any(|line| line.trim() == ".blastray/") {
        return;
    }
    if fs::create_dir_all(&info).is_err() {
        return;
    }
    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(".blastray/\n");
    let _ = atomic_write(&exclude, updated.as_bytes());
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

pub(crate) fn is_source_file(path: &Path) -> bool {
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

#[derive(Clone, Default, Deserialize, Serialize)]
struct FileResolution {
    imports: Vec<String>,
    calls: Vec<ResolvedCall>,
    issues: Vec<RelationshipIssue>,
}

#[derive(Clone, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct ResolvedCall {
    from: String,
    to: String,
    line: usize,
    column: usize,
}

struct ResolveContext {
    files: BTreeSet<String>,
    canonical_symbols: BTreeMap<String, Vec<String>>,
    local_functions: BTreeMap<(String, String), Vec<String>>,
    exports: BTreeMap<(String, String, bool), Vec<String>>,
}

impl ResolveContext {
    fn new(parsed: &BTreeMap<String, ParsedFile>) -> Self {
        let mut context = Self {
            files: parsed.keys().cloned().collect(),
            canonical_symbols: BTreeMap::new(),
            local_functions: BTreeMap::new(),
            exports: BTreeMap::new(),
        };
        for file in parsed.values() {
            for symbol in &file.symbols {
                context
                    .canonical_symbols
                    .entry(symbol.canonical.clone())
                    .or_default()
                    .push(symbol.canonical.clone());
                if symbol.kind == SymbolKind::Function {
                    context
                        .local_functions
                        .entry((file.path.clone(), symbol.name.clone()))
                        .or_default()
                        .push(symbol.canonical.clone());
                    if symbol.exported {
                        context
                            .exports
                            .entry((file.path.clone(), symbol.name.clone(), false))
                            .or_default()
                            .push(symbol.canonical.clone());
                    }
                }
                if symbol.default_export {
                    context
                        .exports
                        .entry((file.path.clone(), String::new(), true))
                        .or_default()
                        .push(symbol.canonical.clone());
                }
            }
        }
        context
    }
}

#[derive(Clone)]
enum BindingTarget {
    Resolved(String),
    Unresolved,
    Ambiguous,
}

fn resolve_file(file: &ParsedFile, context: &ResolveContext) -> FileResolution {
    let mut result = FileResolution::default();
    let mut bindings = BTreeMap::new();
    for import in &file.imports {
        resolve_import(import, file, context, &mut bindings, &mut result);
    }
    for draft in &file.symbols {
        if context
            .canonical_symbols
            .get(&draft.canonical)
            .is_none_or(|symbols| symbols.len() != 1)
        {
            continue;
        }
        for call in &draft.calls {
            if !call.direct {
                result.issues.push(issue(
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
                result.issues.push(issue(
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
            let mut candidates = context
                .local_functions
                .get(&key)
                .cloned()
                .unwrap_or_default();
            candidates.extend(binding_targets(
                bindings.get(&key),
                &mut result.issues,
                &draft.canonical,
                call,
            ));
            candidates.sort();
            candidates.dedup();
            match candidates.as_slice() {
                [target] => result.calls.push(ResolvedCall {
                    from: draft.canonical.clone(),
                    to: target.clone(),
                    line: call.line,
                    column: call.column,
                }),
                [] if !bindings.contains_key(&key) => result.issues.push(issue(
                    RelationshipStatus::Unresolved,
                    &draft.canonical,
                    call.line,
                    call.column,
                    &call.name,
                    "no matching local function or resolved import",
                )),
                [] => {}
                _ => result.issues.push(issue(
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
    result.imports.sort();
    result.imports.dedup();
    result.calls.sort();
    result.calls.dedup();
    result.issues.sort_by(issue_order);
    result
}

fn resolve_import(
    import: &ImportDraft,
    file: &ParsedFile,
    context: &ResolveContext,
    bindings: &mut BTreeMap<(String, String), Vec<BindingTarget>>,
    result: &mut FileResolution,
) {
    let candidates = module_candidates(&file.path, &import.module, &context.files);
    let target = match candidates.as_slice() {
        [target] => {
            result.imports.push(target.clone());
            Some(target.clone())
        }
        [] => {
            let detail = if import.module.starts_with("./") || import.module.starts_with("../") {
                "relative module was not found"
            } else {
                "non-relative module imports are unsupported"
            };
            result.issues.push(issue(
                RelationshipStatus::Unresolved,
                &file.path,
                import.line,
                import.column,
                &import.module,
                detail,
            ));
            None
        }
        _ => {
            result.issues.push(issue(
                RelationshipStatus::Ambiguous,
                &file.path,
                import.line,
                import.column,
                &import.module,
                "more than one source file matches this relative module",
            ));
            None
        }
    };
    if let Some(detail) = &import.unsupported {
        result.issues.push(issue(
            RelationshipStatus::Unresolved,
            &file.path,
            import.line,
            import.column,
            &import.module,
            detail,
        ));
    }
    for binding in &import.bindings {
        let key = (file.path.clone(), binding.local().to_string());
        let value = if import.type_only {
            BindingTarget::Unresolved
        } else if let Some(target) = &target {
            let export_key = match binding {
                ImportBinding::Named { imported, .. } => (target.clone(), imported.clone(), false),
                ImportBinding::Default { .. } => (target.clone(), String::new(), true),
            };
            match context
                .exports
                .get(&export_key)
                .map(Vec::as_slice)
                .unwrap_or(&[])
            {
                [symbol] => BindingTarget::Resolved(symbol.clone()),
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
            result.issues.push(issue(
                match value {
                    BindingTarget::Ambiguous => RelationshipStatus::Ambiguous,
                    _ => RelationshipStatus::Unresolved,
                },
                &file.path,
                import.line,
                import.column,
                binding.local(),
                detail,
            ));
        }
        bindings.entry(key).or_default().push(value);
    }
}

fn binding_targets(
    entries: Option<&Vec<BindingTarget>>,
    issues: &mut Vec<RelationshipIssue>,
    source: &str,
    call: &crate::parse::CallDraft,
) -> Vec<String> {
    let Some(entries) = entries else {
        return Vec::new();
    };
    let mut targets = Vec::new();
    let mut unresolved = false;
    let mut ambiguous = false;
    for entry in entries {
        match entry {
            BindingTarget::Resolved(target) => targets.push(target.clone()),
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

fn materialize_graph(
    parsed: &BTreeMap<String, ParsedFile>,
    resolved: &BTreeMap<String, FileResolution>,
) -> Graph {
    let files: Vec<File> = parsed.keys().cloned().map(|path| File { path }).collect();
    let file_ids: BTreeMap<String, usize> = files
        .iter()
        .enumerate()
        .map(|(id, file)| (file.path.clone(), id))
        .collect();
    let mut drafts: Vec<SymbolDraft> = parsed
        .values()
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
            end_line: draft.end_line,
            column: draft.column,
            kind: draft.kind,
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
    let canonical_symbols = ids_by(&symbols, |symbol| symbol.canonical.clone());
    let named_symbols = ids_by(&symbols, |symbol| symbol.name.clone());
    let mut imports = Vec::new();
    let mut call_sites: BTreeMap<(usize, usize), Vec<CallSite>> = BTreeMap::new();
    let mut issues = Vec::new();
    for (path, facts) in resolved {
        let from = file_ids[path];
        imports.extend(
            facts
                .imports
                .iter()
                .filter_map(|target| file_ids.get(target).map(|to| ImportEdge { from, to: *to })),
        );
        for call in &facts.calls {
            if let (Some([from]), Some([to])) = (
                canonical_symbols.get(&call.from).map(Vec::as_slice),
                canonical_symbols.get(&call.to).map(Vec::as_slice),
            ) {
                call_sites.entry((*from, *to)).or_default().push(CallSite {
                    line: call.line,
                    column: call.column,
                });
            }
        }
        issues.extend(facts.issues.iter().cloned());
    }
    imports.sort_by_key(|edge| (edge.from, edge.to));
    imports.dedup_by_key(|edge| (edge.from, edge.to));
    for sites in call_sites.values_mut() {
        sites.sort();
        sites.dedup();
    }
    let mut calls: Vec<CallEdge> = call_sites
        .keys()
        .map(|(from, to)| CallEdge {
            from: *from,
            to: *to,
        })
        .collect();
    calls.sort_by(|left, right| {
        symbols[left.from]
            .canonical
            .cmp(&symbols[right.from].canonical)
            .then_with(|| symbols[left.to].canonical.cmp(&symbols[right.to].canonical))
    });
    issues.sort_by(issue_order);

    let mut imports_from = vec![Vec::new(); files.len()];
    for edge in &imports {
        imports_from[edge.from].push(edge.to);
    }
    let mut callers_of = vec![Vec::new(); symbols.len()];
    let mut callees_of = vec![Vec::new(); symbols.len()];
    for edge in &calls {
        callees_of[edge.from].push(edge.to);
        callers_of[edge.to].push(edge.from);
    }
    for adjacency in callers_of.iter_mut().chain(callees_of.iter_mut()) {
        adjacency.sort_by(|left, right| symbols[*left].canonical.cmp(&symbols[*right].canonical));
        adjacency.dedup();
    }
    for imports in &mut imports_from {
        imports.sort_by(|left, right| files[*left].path.cmp(&files[*right].path));
        imports.dedup();
    }
    Graph {
        files,
        symbols,
        defines,
        imports,
        calls,
        issues,
        imports_from,
        callers_of,
        callees_of,
        call_sites,
        canonical_symbols,
        named_symbols,
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

fn module_candidates(from: &str, request: &str, files: &BTreeSet<String>) -> Vec<String> {
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
        if !candidates.is_empty() {
            return candidates.into_iter().collect();
        }
        let extensions = match base.extension().and_then(|extension| extension.to_str()) {
            Some("js") => Some(["ts", "tsx", "js", "jsx"].as_slice()),
            Some("jsx") => Some(["tsx", "jsx"].as_slice()),
            _ => None,
        };
        if let Some(extensions) = extensions {
            for extension in extensions {
                insert_candidate(&base.with_extension(extension), files, &mut candidates);
            }
            if !candidates.is_empty() {
                return candidates.into_iter().collect();
            }
            let stem = base.with_extension("");
            for extension in extensions {
                insert_candidate(
                    &stem.join("index").with_extension(extension),
                    files,
                    &mut candidates,
                );
            }
        }
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

fn insert_candidate(path: &Path, files: &BTreeSet<String>, candidates: &mut BTreeSet<String>) {
    let key = path.to_string_lossy().replace('\\', "/");
    if files.contains(&key) {
        candidates.insert(key);
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

fn issue_order(left: &RelationshipIssue, right: &RelationshipIssue) -> std::cmp::Ordering {
    left.source
        .cmp(&right.source)
        .then(left.line.cmp(&right.line))
        .then(left.column.cmp(&right.column))
        .then(left.name.cmp(&right.name))
        .then(left.detail.cmp(&right.detail))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{CACHE_DIRECTORY, CACHE_FILE, Graph, Index, RefreshKind};
    use crate::query;

    static NEXT_REPO: AtomicUsize = AtomicUsize::new(0);

    struct Repo(PathBuf);

    impl Repo {
        fn new(files: &[(&str, &str)]) -> Self {
            let path = std::env::temp_dir().join(format!(
                "blastray-index-test-{}-{}",
                std::process::id(),
                NEXT_REPO.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            let repo = Self(path);
            for (file, source) in files {
                repo.write(file, source);
            }
            repo
        }

        fn write(&self, file: &str, source: &str) {
            let path = self.0.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, source).unwrap();
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn repo() -> Repo {
        Repo::new(&[
            (
                "src/api.ts",
                "export function save() {}\nexport function other() {}\n",
            ),
            (
                "src/use.ts",
                "import { save } from './api';\nexport function use() { save(); }\n",
            ),
            (
                "src/local.ts",
                "export function leaf() {}\nexport function entry() { leaf(); }\n",
            ),
            (
                "src/cycle.ts",
                "export function a() { b(); }\nexport function b() { a(); }\n",
            ),
        ])
    }

    fn meaning(graph: &Graph) -> String {
        let mut result = String::new();
        for file in &graph.files {
            result.push_str(&format!("F:{}\n", file.path));
        }
        for symbol in &graph.symbols {
            result.push_str(&format!(
                "S:{}:{:?}:{}:{}\n",
                symbol.canonical, symbol.kind, symbol.line, symbol.column
            ));
        }
        for edge in &graph.defines {
            result.push_str(&format!(
                "D:{}:{}\n",
                graph.files[edge.file].path, graph.symbols[edge.symbol].canonical
            ));
        }
        for edge in &graph.imports {
            result.push_str(&format!(
                "I:{}:{}\n",
                graph.files[edge.from].path, graph.files[edge.to].path
            ));
        }
        for edge in &graph.calls {
            result.push_str(&format!(
                "C:{}:{}\n",
                graph.symbols[edge.from].canonical, graph.symbols[edge.to].canonical
            ));
            for site in graph.call_sites(edge.from, edge.to) {
                result.push_str(&format!("E:{}:{}\n", site.line, site.column));
            }
        }
        for issue in &graph.issues {
            result.push_str(&format!(
                "X:{:?}:{}:{}:{}:{}:{}\n",
                issue.status, issue.source, issue.line, issue.column, issue.name, issue.detail
            ));
        }
        for (id, symbol) in graph.symbols.iter().enumerate() {
            let callers: Vec<_> = graph
                .callers(id)
                .iter()
                .map(|id| graph.symbols[*id].canonical.as_str())
                .collect();
            let callees: Vec<_> = graph
                .callees(id)
                .iter()
                .map(|id| graph.symbols[*id].canonical.as_str())
                .collect();
            result.push_str(&format!("A:{}:{callers:?}:{callees:?}\n", symbol.canonical));
        }
        result
    }

    fn assert_equivalent(incremental: &Index, full: &Index) {
        assert_eq!(meaning(incremental.graph()), meaning(full.graph()));
        for target in ["save", "use", "entry", "leaf", "a", "b", "src/api.ts::save"] {
            assert_eq!(
                query::find(incremental.graph(), target),
                query::find(full.graph(), target)
            );
            assert_eq!(
                query::inspect(incremental.graph(), target),
                query::inspect(full.graph(), target)
            );
            assert_eq!(
                query::impact(incremental.graph(), target),
                query::impact(full.graph(), target)
            );
        }
        for (from, to) in [
            ("src/use.ts::use", "src/api.ts::save"),
            ("src/cycle.ts::a", "src/cycle.ts::b"),
        ] {
            assert_eq!(
                query::trace(incremental.graph(), from, to),
                query::trace(full.graph(), from, to)
            );
        }
    }

    fn refresh(repo: &Repo, file: &str, source: &str) -> Index {
        let mut index = Index::build(&repo.0).unwrap();
        repo.write(file, source);
        assert_eq!(
            index.refresh(Path::new(file)).unwrap(),
            RefreshKind::Incremental
        );
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&index, &full);
        index
    }

    #[test]
    fn body_edits_and_direct_call_addition_and_removal_match_full_build() {
        let repo = repo();
        let mut index = refresh(
            &repo,
            "src/local.ts",
            "export function leaf() { return; }\nexport function entry() { leaf(); }\n",
        );
        assert!(
            query::inspect(index.graph(), "src/local.ts::entry")
                .unwrap()
                .contains("src/local.ts::leaf")
        );
        repo.write(
            "src/local.ts",
            "export function leaf() { return; }\nexport function extra() {}\nexport function entry() { leaf(); extra(); }\n",
        );
        assert_eq!(
            index.refresh(Path::new("src/local.ts")).unwrap(),
            RefreshKind::Incremental
        );
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&index, &full);
        assert!(
            query::inspect(index.graph(), "src/local.ts::entry")
                .unwrap()
                .contains("src/local.ts::extra")
        );
        repo.write(
            "src/local.ts",
            "export function leaf() { return; }\nexport function extra() {}\nexport function entry() { extra(); }\n",
        );
        assert_eq!(
            index.refresh(Path::new("src/local.ts")).unwrap(),
            RefreshKind::Incremental
        );
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&index, &full);
        assert!(
            !query::inspect(index.graph(), "src/local.ts::entry")
                .unwrap()
                .contains("src/local.ts::leaf")
        );
    }

    #[test]
    fn rename_export_and_import_binding_changes_re_resolve_importers() {
        let repo = repo();
        let mut index = refresh(
            &repo,
            "src/api.ts",
            "export function renamed() {}\nexport function other() {}\n",
        );
        let missing = query::inspect(index.graph(), "src/use.ts::use").unwrap();
        assert!(missing.contains("Direct callees: none"));
        assert!(missing.contains("imported binding is not uniquely resolved"));
        repo.write(
            "src/use.ts",
            "import { other as save } from './api';\nexport function use() { save(); }\n",
        );
        assert_eq!(
            index.refresh(Path::new("src/use.ts")).unwrap(),
            RefreshKind::Incremental
        );
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&index, &full);
        assert!(
            query::inspect(index.graph(), "src/use.ts::use")
                .unwrap()
                .contains("src/api.ts::other")
        );
    }

    #[test]
    fn ambiguity_and_unresolved_calls_appear_and_disappear() {
        let repo = repo();
        let mut index = refresh(
            &repo,
            "src/local.ts",
            "export function leaf() {}\nexport function entry() { missing(); }\n",
        );
        assert!(
            query::inspect(index.graph(), "src/local.ts::entry")
                .unwrap()
                .contains("no matching local function")
        );
        repo.write(
            "src/local.ts",
            "export function leaf() {}\nexport function missing() {}\nexport function entry() { missing(); }\n",
        );
        assert_eq!(
            index.refresh(Path::new("src/local.ts")).unwrap(),
            RefreshKind::Incremental
        );
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&index, &full);
        repo.write(
            "src/api.ts",
            "export function save() {}\nexport function save() {}\nexport function other() {}\n",
        );
        assert_eq!(
            index.refresh(Path::new("src/api.ts")).unwrap(),
            RefreshKind::Incremental
        );
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&index, &full);
        assert!(
            query::inspect(index.graph(), "src/use.ts::use")
                .unwrap()
                .contains("AMBIGUOUS")
        );
        repo.write(
            "src/api.ts",
            "export function save() {}\nexport function other() {}\n",
        );
        assert_eq!(
            index.refresh(Path::new("src/api.ts")).unwrap(),
            RefreshKind::Incremental
        );
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&index, &full);
        assert!(
            !query::inspect(index.graph(), "src/use.ts::use")
                .unwrap()
                .contains("AMBIGUOUS")
        );
    }

    #[test]
    fn cycles_and_output_remain_deterministic_after_refresh() {
        let repo = repo();
        let index = refresh(
            &repo,
            "src/cycle.ts",
            "export function a() { b(); }\nexport function b() { a(); }\n",
        );
        let first = query::impact(index.graph(), "src/cycle.ts::a").unwrap();
        assert_eq!(
            first,
            query::impact(index.graph(), "src/cycle.ts::a").unwrap()
        );
        assert!(first.contains("Total confirmed affected symbols: 1"));
    }

    #[test]
    fn added_deleted_and_renamed_files_fall_back_to_full_rebuild() {
        let repo = repo();
        let mut index = Index::build(&repo.0).unwrap();
        repo.write("src/new.ts", "export function fresh() {}\n");
        assert_eq!(
            index.refresh(Path::new("src/new.ts")).unwrap(),
            RefreshKind::FullRebuild
        );
        repo.write("src/new.ts", "");
        assert_eq!(
            index.refresh(Path::new("src/new.ts")).unwrap(),
            RefreshKind::Incremental
        );
        fs::remove_file(repo.0.join("src/new.ts")).unwrap();
        assert_eq!(
            index.refresh(Path::new("src/new.ts")).unwrap(),
            RefreshKind::FullRebuild
        );
        fs::rename(repo.0.join("src/local.ts"), repo.0.join("src/renamed.ts")).unwrap();
        assert_eq!(
            index.refresh(Path::new("src/local.ts")).unwrap(),
            RefreshKind::FullRebuild
        );
        assert!(query::find(index.graph(), "entry").contains("src/renamed.ts::entry"));
    }

    fn cache_path(repo: &Repo) -> PathBuf {
        repo.0.join(CACHE_DIRECTORY).join(CACHE_FILE)
    }

    fn assert_persistent_equivalent(repo: &Repo) -> Index {
        let persistent = Index::open(&repo.0).unwrap();
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&persistent, &full);
        persistent
    }

    #[test]
    fn persistent_open_creates_reloads_and_recreates_the_cache() {
        let repo = repo();
        assert!(!cache_path(&repo).exists());
        let first = assert_persistent_equivalent(&repo);
        assert!(cache_path(&repo).is_file());
        let second = assert_persistent_equivalent(&repo);
        assert_eq!(meaning(first.graph()), meaning(second.graph()));
        fs::remove_dir_all(repo.0.join(CACHE_DIRECTORY)).unwrap();
        assert_persistent_equivalent(&repo);
        assert!(cache_path(&repo).is_file());
    }

    #[test]
    fn persistent_modified_files_match_a_fresh_build() {
        let repo = repo();
        assert_persistent_equivalent(&repo);
        repo.write(
            "src/api.ts",
            "export function saved() {}\nexport function other() {}\n",
        );
        let one_modified = assert_persistent_equivalent(&repo);
        assert!(
            query::inspect(one_modified.graph(), "src/use.ts::use")
                .unwrap()
                .contains("Direct callees: none")
        );
        repo.write(
            "src/use.ts",
            "import { other } from './api';\nexport function use() { other(); }\n",
        );
        repo.write(
            "src/local.ts",
            "export function leaf() {}\nexport function entry() { leaf(); leaf(); }\n",
        );
        assert_persistent_equivalent(&repo);
    }

    #[test]
    fn esm_js_specifiers_resolve_typescript_targets_conservatively() {
        let repo = Repo::new(&[
            (
                "src/api.ts",
                "export const callable = async () => {};\nexport const expression = function () {};\n",
            ),
            (
                "src/use.ts",
                "import { callable, expression } from './api.js';\nexport const entry = () => { callable(); expression(); };\n",
            ),
        ]);
        let index = Index::build(&repo.0).unwrap();
        let graph = index.graph();
        assert!(
            query::trace(graph, "src/use.ts::entry", "src/api.ts::callable")
                .unwrap()
                .contains("Known CALLS path")
        );
        assert!(
            query::inspect(graph, "src/use.ts::entry")
                .unwrap()
                .contains("src/api.ts::expression")
        );
        assert!(query::find(graph, "callable").contains("exact name"));
        assert!(!query::find(graph, "notCallable").contains("src/api.ts"));
    }

    #[test]
    fn esm_js_specifier_prefers_exact_target_and_marks_multiple_substitutions_ambiguous() {
        let exact = Repo::new(&[
            ("src/api.js", "export const selected = () => {};\n"),
            ("src/api.ts", "export const selected = () => {};\n"),
            (
                "src/use.ts",
                "import { selected } from './api.js';\nexport function entry() { selected(); }\n",
            ),
        ]);
        let index = Index::build(&exact.0).unwrap();
        assert!(
            query::inspect(index.graph(), "src/use.ts::entry")
                .unwrap()
                .contains("src/api.js::selected")
        );

        let ambiguous = Repo::new(&[
            ("src/api.ts", "export const selected = () => {};\n"),
            ("src/api.tsx", "export const selected = () => {};\n"),
            (
                "src/use.ts",
                "import { selected } from './api.js';\nexport function entry() { selected(); }\n",
            ),
        ]);
        let index = Index::build(&ambiguous.0).unwrap();
        let output = query::inspect(index.graph(), "src/use.ts::entry").unwrap();
        assert!(output.contains("AMBIGUOUS"));
    }

    #[test]
    fn esm_js_specifier_can_resolve_a_typescript_index_file() {
        let repo = Repo::new(&[
            ("src/api/index.ts", "export const selected = () => {};\n"),
            (
                "src/use.ts",
                "import { selected } from './api.js';\nexport function entry() { selected(); }\n",
            ),
        ]);
        let index = Index::build(&repo.0).unwrap();
        assert!(
            query::trace(
                index.graph(),
                "src/use.ts::entry",
                "src/api/index.ts::selected"
            )
            .unwrap()
            .contains("Known CALLS path")
        );
    }

    #[test]
    fn callable_variables_refresh_and_persist_like_a_full_build() {
        let repo = Repo::new(&[
            ("src/a.ts", "export const leaf = () => {};\n"),
            (
                "src/b.ts",
                "import { leaf } from './a.js';\nexport const entry = () => { leaf(); leaf(); };\n",
            ),
        ]);
        let mut index = Index::build(&repo.0).unwrap();
        assert_eq!(index.graph().calls.len(), 1);
        assert_eq!(
            index
                .graph()
                .call_sites(
                    index.graph().symbol_candidates("src/b.ts::entry")[0],
                    index.graph().symbol_candidates("src/a.ts::leaf")[0]
                )
                .len(),
            2
        );
        repo.write(
            "src/b.ts",
            "import { leaf } from './a.js';\nexport const entry = () => { leaf(); };\n",
        );
        assert_eq!(
            index.refresh(Path::new("src/b.ts")).unwrap(),
            RefreshKind::Incremental
        );
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&index, &full);
        let persistent = Index::open(&repo.0).unwrap();
        assert_equivalent(&persistent, &full);
    }

    #[test]
    fn variable_bindings_and_nested_callable_scopes_remain_conservative() {
        let repo = Repo::new(&[(
            "src/a.ts",
            "const value = 1;\nexport const leaf = () => {};\nexport const entry = () => { const leaf = () => {}; leaf(); };\n",
        )]);
        let index = Index::build(&repo.0).unwrap();
        assert!(!query::find(index.graph(), "value").contains("src/a.ts::value"));
        let output = query::inspect(index.graph(), "src/a.ts::entry").unwrap();
        assert!(output.contains("unmodeled local binding could shadow this name"));
    }

    #[test]
    fn ranked_find_is_deterministic_multi_word_and_capped() {
        let mut files = vec![
            ("src/exact.ts", "export function Analyze() {}\n"),
            ("src/lock.ts", "export function acquireIndexLock() {}\n"),
            (
                "src/watch-lock.ts",
                "export function acquireWatchLock() {}\n",
            ),
            (
                "src/incremental-index.ts",
                "export function rebuildIncrementalIndex() {}\n",
            ),
        ];
        let generated: Vec<(String, String)> = (0..24)
            .map(|number| {
                (
                    format!("src/analyze-{number}.ts"),
                    format!("export function analyze{number}() {{}}\n"),
                )
            })
            .collect();
        for (path, source) in &generated {
            files.push((path, source));
        }
        let repo = Repo::new(&files);
        let index = Index::build(&repo.0).unwrap();
        let exact = query::find(index.graph(), "Analyze");
        assert!(
            exact.find("src/exact.ts::Analyze").unwrap()
                < exact.find("src/analyze-0.ts::analyze0").unwrap()
        );
        let exact_multi_token = query::find(index.graph(), "acquireIndexLock");
        assert!(
            exact_multi_token
                .find("src/lock.ts::acquireIndexLock")
                .unwrap()
                < exact_multi_token
                    .find("src/watch-lock.ts::acquireWatchLock")
                    .unwrap()
        );
        let multi_word = query::find(index.graph(), "incremental index");
        assert!(multi_word.contains("src/incremental-index.ts::rebuildIncrementalIndex"));
        let capped = query::find(index.graph(), "analyze");
        assert!(capped.starts_with("Showing 20 of 25 matches; refine the query."));
        assert_eq!(capped, query::find(index.graph(), "analyze"));
    }

    #[test]
    fn resolved_calls_expose_deterministic_call_site_evidence() {
        let repo = Repo::new(&[(
            "src/a.ts",
            "export function leaf() {}\nexport function middle() { leaf(); leaf(); }\nexport function entry() { middle(); }\n",
        )]);
        let index = Index::build(&repo.0).unwrap();
        let inspect = query::inspect(index.graph(), "src/a.ts::middle").unwrap();
        assert!(inspect.contains("calls at src/a.ts:2:28, src/a.ts:2:36"));
        let trace = query::trace(index.graph(), "entry", "leaf").unwrap();
        assert!(trace.contains("Call-site evidence:"));
        assert!(trace.contains("src/a.ts::entry -> src/a.ts::middle [call at src/a.ts:3:"));
        let impact = query::impact(index.graph(), "leaf").unwrap();
        assert!(impact.contains("Direct caller evidence:"));
        assert!(impact.contains("src/a.ts::middle [calls at src/a.ts:2:28, src/a.ts:2:36]"));
    }

    #[test]
    fn persistent_file_set_changes_use_safe_rebuilds() {
        let repo = repo();
        assert_persistent_equivalent(&repo);
        repo.write("src/new.ts", "export function fresh() {}\n");
        assert_persistent_equivalent(&repo);
        fs::remove_file(repo.0.join("src/new.ts")).unwrap();
        assert_persistent_equivalent(&repo);
        fs::rename(repo.0.join("src/local.ts"), repo.0.join("src/renamed.ts")).unwrap();
        let index = assert_persistent_equivalent(&repo);
        assert!(query::find(index.graph(), "entry").contains("src/renamed.ts::entry"));
    }

    #[test]
    fn corrupt_truncated_and_old_schema_caches_rebuild_safely() {
        let repo = repo();
        assert_persistent_equivalent(&repo);
        fs::write(cache_path(&repo), b"not a cache").unwrap();
        assert_persistent_equivalent(&repo);
        fs::write(cache_path(&repo), [1, 2, 3]).unwrap();
        assert_persistent_equivalent(&repo);
        let bytes = fs::read(cache_path(&repo)).unwrap();
        let mut envelope: super::CacheEnvelope = bincode::deserialize(&bytes).unwrap();
        envelope.schema -= 1;
        fs::write(cache_path(&repo), bincode::serialize(&envelope).unwrap()).unwrap();
        assert_persistent_equivalent(&repo);
        let bytes = fs::read(cache_path(&repo)).unwrap();
        let mut envelope: super::CacheEnvelope = bincode::deserialize(&bytes).unwrap();
        let mut cached: super::CachedIndex = bincode::deserialize(&envelope.payload).unwrap();
        cached.resolved.clear();
        envelope.payload = bincode::serialize(&cached).unwrap();
        envelope.checksum = *blake3::hash(&envelope.payload).as_bytes();
        fs::write(cache_path(&repo), bincode::serialize(&envelope).unwrap()).unwrap();
        assert_persistent_equivalent(&repo);
    }

    #[test]
    fn cache_is_self_excluded_and_git_exclusion_is_idempotent() {
        let repo = repo();
        repo.write(".blastray/ignored.ts", "export function hidden() {}\n");
        repo.write(".gitignore", "keep-this-tracked\n");
        fs::create_dir_all(repo.0.join(".git/info")).unwrap();
        fs::write(repo.0.join(".git/info/exclude"), "existing-rule\n").unwrap();
        let index = assert_persistent_equivalent(&repo);
        assert_eq!(
            query::find(index.graph(), "hidden"),
            "No symbols found for 'hidden'."
        );
        assert_eq!(
            fs::read_to_string(repo.0.join(".gitignore")).unwrap(),
            "keep-this-tracked\n"
        );
        assert_persistent_equivalent(&repo);
        let exclude = fs::read_to_string(repo.0.join(".git/info/exclude")).unwrap();
        assert!(exclude.contains("existing-rule"));
        assert_eq!(
            exclude.lines().filter(|line| *line == ".blastray/").count(),
            1
        );
    }

    #[test]
    fn non_git_repositories_persist_normally() {
        let repo = repo();
        assert!(!repo.0.join(".git").exists());
        assert_persistent_equivalent(&repo);
        assert!(cache_path(&repo).is_file());
    }

    fn benchmark_root(label: &str, source_root: &Path) {
        let repo = Repo::new(&[]);
        for source in super::source_files(source_root).unwrap() {
            let relative = super::relative_path(source_root, &source).unwrap();
            let destination = repo.0.join(&relative);
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::copy(source, destination).unwrap();
        }
        let start = std::time::Instant::now();
        let mut index = Index::build(&repo.0).unwrap();
        let full = start.elapsed();
        let changed = index.parsed.keys().next().unwrap().clone();
        let path = repo.0.join(&changed);
        let mut source = fs::read_to_string(&path).unwrap();
        source.push('\n');
        fs::write(&path, source).unwrap();
        let start = std::time::Instant::now();
        assert_eq!(
            index.refresh(Path::new(&changed)).unwrap(),
            RefreshKind::Incremental
        );
        let refresh = start.elapsed();
        println!(
            "{label}: files={} symbols={} imports={} calls={} issues={} full_us={} refresh_us={} changed={changed}",
            index.graph().files.len(),
            index.graph().symbols.len(),
            index.graph().imports.len(),
            index.graph().calls.len(),
            index.graph().issues.len(),
            full.as_micros(),
            refresh.as_micros(),
        );
    }

    #[test]
    #[ignore = "manual release benchmark"]
    fn release_build_and_refresh_benchmarks() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        benchmark_root("basic", &root.join("tests/fixtures/basic"));
        benchmark_root("gitnexus", &root.join("misc/references/gitnexus"));
    }
}
