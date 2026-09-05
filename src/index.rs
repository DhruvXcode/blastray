use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};

use crate::language::{self, ParsedFile, ResolvedFile, SymbolFact};

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
const CACHE_SCHEMA: u32 = 14;

pub fn no_supported_source_files_message() -> String {
    language::no_supported_source_files_message()
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SymbolKind {
    Function,
    Class,
    Method,
    Type,
}

impl SymbolKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Class => "class",
            Self::Method => "method",
            Self::Type => "type",
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

/// Compact, source-derived discovery evidence. This is deliberately separate
/// from graph facts: terms can make a symbol easier to find, but never create
/// a relationship.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct SearchFact {
    pub canonical: String,
    pub identifier: Vec<String>,
    pub path: Vec<String>,
    pub declaration: Vec<String>,
    pub comments: Vec<String>,
    pub strings: Vec<String>,
    pub body: Vec<String>,
    pub test: Vec<String>,
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
    pub(crate) search: Vec<SearchFact>,
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
    context_hashes: BTreeMap<String, [u8; 32]>,
    context: language::ProviderContext,
    parsed: BTreeMap<String, ParsedFile>,
    search: BTreeMap<String, Vec<SearchFact>>,
    resolved: BTreeMap<String, ResolvedFile>,
    graph: Graph,
}

impl Index {
    pub fn build(root: &Path) -> Result<Self, String> {
        let root = root
            .canonicalize()
            .map_err(|error| format!("cannot read repository root {}: {error}", root.display()))?;
        let hashes = source_hashes(&root)?;
        let (context_hashes, context) = provider_context(&root)?;
        Self::build_with_hashes(root, hashes, context_hashes, context)
    }

    pub fn open(root: &Path) -> Result<Self, String> {
        let root = root
            .canonicalize()
            .map_err(|error| format!("cannot read repository root {}: {error}", root.display()))?;
        add_git_exclude(&root);
        let mut index = match Self::load(&root) {
            Some(index) => index,
            None => {
                let hashes = source_hashes(&root)?;
                let (context_hashes, context) = provider_context(&root)?;
                return Self::build_and_persist(root.clone(), hashes, context_hashes, context);
            }
        };
        index.root = root;
        index.sync()?;
        Ok(index)
    }

    pub fn sync(&mut self) -> Result<(), String> {
        let current_hashes = source_hashes(&self.root)?;
        let (context_hashes, context) = provider_context(&self.root)?;
        if self.context_hashes != context_hashes {
            *self = Self::build_with_hashes(
                self.root.clone(),
                current_hashes,
                context_hashes,
                context,
            )?;
            self.persist()?;
            return Ok(());
        }
        self.context = context;
        if !self.hashes.keys().eq(current_hashes.keys()) {
            *self = Self::build_with_hashes(
                self.root.clone(),
                current_hashes,
                context_hashes,
                self.context.clone(),
            )?;
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
                let (context_hashes, context) = provider_context(&self.root)?;
                *self = Self::build_with_hashes(
                    self.root.clone(),
                    current_hashes,
                    context_hashes,
                    context,
                )?;
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
        context_hashes: BTreeMap<String, [u8; 32]>,
        context: language::ProviderContext,
    ) -> Result<Self, String> {
        let index = Self::build_with_hashes(root, hashes, context_hashes, context)?;
        index.persist()?;
        Ok(index)
    }

    fn build_with_hashes(
        root: PathBuf,
        hashes: BTreeMap<String, [u8; 32]>,
        context_hashes: BTreeMap<String, [u8; 32]>,
        context: language::ProviderContext,
    ) -> Result<Self, String> {
        let mut parsed = BTreeMap::new();
        let mut search = BTreeMap::new();
        for relative in hashes.keys() {
            let path = root.join(relative);
            let source = std::fs::read_to_string(&path)
                .map_err(|error| format!("cannot read {relative}: {error}"))?;
            let file = language::parse(relative, &source)?;
            search.insert(
                relative.clone(),
                search_facts(relative, &source, &file.symbols()),
            );
            parsed.insert(relative.clone(), file);
        }
        let resolved = language::resolve_all(&parsed, &context);
        let graph = materialize_graph(&parsed, &resolved, &search);
        Ok(Self {
            root,
            hashes,
            context_hashes,
            context,
            parsed,
            search,
            resolved,
            graph,
        })
    }

    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// Source is deliberately read at inspection time instead of duplicated in
    /// the cache. `sync` has already made the graph fresh for CLI/MCP callers.
    pub fn inspect(&self, target: &str) -> Result<String, String> {
        let source = self
            .graph
            .symbol_candidates(target)
            .as_slice()
            .first()
            .and_then(|symbol| self.graph.defining_file(*symbol))
            .and_then(|file| fs::read_to_string(self.root.join(&self.graph.files[file].path)).ok());
        crate::query::inspect_with_source(&self.graph, target, source.as_deref())
    }

    pub fn has_supported_source_files(&self) -> bool {
        !self.graph.files.is_empty()
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

        let importers = self.importer_closure(&relative);
        let source = std::fs::read_to_string(&source_path)
            .map_err(|error| format!("cannot read {relative}: {error}"))?;
        let file = language::parse(&relative, &source)?;
        self.search.insert(
            relative.clone(),
            search_facts(&relative, &source, &file.symbols()),
        );
        self.parsed.insert(relative.clone(), file);
        self.hashes.insert(
            relative.clone(),
            *blake3::hash(source.as_bytes()).as_bytes(),
        );

        let mut affected = importers;
        affected.insert(relative);
        let resolved = language::resolve_files(&self.parsed, &affected, &self.context);
        for path in affected {
            let facts = resolved
                .get(&path)
                .expect("affected files remain in the parsed index")
                .clone();
            self.resolved.insert(path, facts);
        }
        self.graph = materialize_graph(&self.parsed, &self.resolved, &self.search);
        Ok(RefreshKind::Incremental)
    }

    fn relative_refresh_path(&self, path: &Path) -> Option<String> {
        if path.is_absolute() {
            return relative_path(&self.root, path).ok();
        }
        normalize_relative(path)
    }

    fn importer_closure(&self, target: &str) -> BTreeSet<String> {
        let mut affected = BTreeSet::from([target.to_string()]);
        loop {
            let before = affected.len();
            for (path, facts) in &self.resolved {
                if facts
                    .imports
                    .iter()
                    .chain(&facts.dependencies)
                    .any(|import| affected.contains(import))
                {
                    affected.insert(path.clone());
                }
            }
            if affected.len() == before {
                return affected;
            }
        }
    }

    fn full_rebuild(&mut self) -> Result<RefreshKind, String> {
        *self = Self::build(&self.root)?;
        Ok(RefreshKind::FullRebuild)
    }

    fn persist(&self) -> Result<(), String> {
        let cached = CachedIndex {
            hashes: self.hashes.clone(),
            context_hashes: self.context_hashes.clone(),
            parsed: self.parsed.clone(),
            search: self.search.clone(),
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
        let (_context_hashes, context) = provider_context(root).ok()?;
        let graph = materialize_graph(&cached.parsed, &cached.resolved, &cached.search);
        Some(Self {
            root: root.to_path_buf(),
            hashes: cached.hashes,
            context_hashes: cached.context_hashes,
            context,
            parsed: cached.parsed,
            search: cached.search,
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
    context_hashes: BTreeMap<String, [u8; 32]>,
    parsed: BTreeMap<String, ParsedFile>,
    search: BTreeMap<String, Vec<SearchFact>>,
    resolved: BTreeMap<String, ResolvedFile>,
}

impl CachedIndex {
    fn valid(&self) -> bool {
        if !self.hashes.keys().eq(self.parsed.keys())
            || !self.parsed.keys().eq(self.resolved.keys())
            || !self.parsed.keys().eq(self.search.keys())
        {
            return false;
        }
        self.parsed.iter().all(|(path, file)| {
            let symbols: BTreeSet<_> = file
                .symbols()
                .into_iter()
                .map(|symbol| symbol.canonical)
                .collect();
            let facts: BTreeSet<_> = self.search[path]
                .iter()
                .map(|fact| fact.canonical.clone())
                .collect();
            file.path() == path
                && symbols
                    .iter()
                    .all(|canonical| canonical.starts_with(&format!("{path}::")))
                && symbols == facts
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

fn provider_context(
    root: &Path,
) -> Result<(BTreeMap<String, [u8; 32]>, language::ProviderContext), String> {
    let mut hashes = BTreeMap::new();
    let mut files = BTreeMap::new();
    let mut walker = WalkBuilder::new(root);
    walker
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true);
    for entry in walker.build() {
        let entry = entry.map_err(|error| format!("cannot walk {}: {error}", root.display()))?;
        let path = entry.path();
        if !path.is_file() || !language::is_context_path(path) {
            continue;
        }
        let relative = relative_path(root, path)?;
        let bytes = fs::read(path).map_err(|error| format!("cannot read {relative}: {error}"))?;
        let text = String::from_utf8(bytes.clone())
            .map_err(|error| format!("cannot read {relative} as UTF-8: {error}"))?;
        hashes.insert(relative.clone(), *blake3::hash(&bytes).as_bytes());
        files.insert(relative, text);
    }
    Ok((hashes, language::ProviderContext { files }))
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
    language::is_supported_path(path)
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

fn materialize_graph(
    parsed: &BTreeMap<String, ParsedFile>,
    resolved: &BTreeMap<String, ResolvedFile>,
    search_files: &BTreeMap<String, Vec<SearchFact>>,
) -> Graph {
    let files: Vec<File> = parsed.keys().cloned().map(|path| File { path }).collect();
    let file_ids: BTreeMap<String, usize> = files
        .iter()
        .enumerate()
        .map(|(id, file)| (file.path.clone(), id))
        .collect();
    let mut drafts: Vec<SymbolFact> = parsed.values().flat_map(ParsedFile::symbols).collect();
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
    let search_by_canonical: BTreeMap<_, _> = search_files
        .values()
        .flatten()
        .map(|fact| (fact.canonical.clone(), fact.clone()))
        .collect();
    let search = symbols
        .iter()
        .map(|symbol| {
            search_by_canonical
                .get(&symbol.canonical)
                .cloned()
                .unwrap_or_default()
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
        search,
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

fn search_facts(path: &str, source: &str, symbols: &[SymbolFact]) -> Vec<SearchFact> {
    let line_starts = line_starts(source);
    symbols
        .iter()
        .map(|symbol| {
            let start_line = symbol.line.saturating_sub(1);
            let end_line = symbol.end_line.saturating_sub(1);
            let context_start = start_line.saturating_sub(4);
            let start = *line_starts.get(context_start).unwrap_or(&0);
            let end = line_starts
                .get(end_line.saturating_add(1))
                .copied()
                .unwrap_or(source.len())
                .min(start.saturating_add(12_000));
            let declaration_end = line_starts
                .get(start_line.saturating_add(3))
                .copied()
                .unwrap_or(end)
                .min(end);
            let evidence = lexical_evidence(&source[start..end]);
            let declaration = lexical_evidence(&source[start..declaration_end]);
            let before = &source[start..line_starts.get(start_line).copied().unwrap_or(end)];
            let is_test = path_tokens(path)
                .iter()
                .any(|term| matches!(term.as_str(), "test" | "tests" | "spec" | "specs"))
                || before.contains("#[test]")
                || before.contains("describe(")
                || before.contains("it(")
                || before.contains("test_");
            SearchFact {
                canonical: symbol.canonical.clone(),
                identifier: search_terms(&format!("{} {}", symbol.name, symbol.canonical)),
                path: path_tokens(path),
                declaration: search_terms(&declaration.code),
                comments: search_terms(&evidence.comments),
                strings: search_terms(&evidence.strings),
                body: search_terms(&evidence.code),
                test: if is_test {
                    search_terms(&format!("{} {}", symbol.name, declaration.code))
                } else {
                    Vec::new()
                },
            }
        })
        .collect()
}

fn line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(
        source
            .bytes()
            .enumerate()
            .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
    );
    starts
}

struct LexicalEvidence {
    code: String,
    comments: String,
    strings: String,
}

/// A deliberately tiny lexer is enough to retain useful source words without
/// storing whole files per symbol. Providers remain responsible for syntax and
/// structural facts; this only classifies common comment and string spellings.
fn lexical_evidence(source: &str) -> LexicalEvidence {
    let bytes = source.as_bytes();
    let mut evidence = LexicalEvidence {
        code: String::new(),
        comments: String::new(),
        strings: String::new(),
    };
    let mut index = 0;
    while index < bytes.len() {
        // The scanner is ASCII-delimiter based, but source comments can be
        // UTF-8. Never form a string slice from a continuation-byte offset.
        if !source.is_char_boundary(index) {
            index += 1;
            continue;
        }
        if bytes[index..].starts_with(b"//") {
            let end = source[index..]
                .find('\n')
                .map(|offset| index + offset)
                .unwrap_or(bytes.len());
            evidence.comments.push_str(&source[index + 2..end]);
            evidence.comments.push(' ');
            index = end;
        } else if bytes[index..].starts_with(b"/*") {
            let end = source[index + 2..]
                .find("*/")
                .map(|offset| index + 2 + offset)
                .unwrap_or(bytes.len());
            evidence.comments.push_str(&source[index + 2..end]);
            evidence.comments.push(' ');
            index = end.saturating_add(2).min(bytes.len());
        } else if bytes[index] == b'#' {
            let end = source[index..]
                .find('\n')
                .map(|offset| index + offset)
                .unwrap_or(bytes.len());
            evidence.comments.push_str(&source[index + 1..end]);
            evidence.comments.push(' ');
            index = end;
        } else if bytes[index..].starts_with(b"\"\"\"") || bytes[index..].starts_with(b"'''") {
            let end = source[index + 3..]
                .find(&source[index..index + 3])
                .map(|offset| index + 3 + offset)
                .unwrap_or(bytes.len());
            evidence.comments.push_str(&source[index + 3..end]);
            evidence.comments.push(' ');
            index = end.saturating_add(3).min(bytes.len());
        } else if matches!(bytes[index], b'\'' | b'\"' | b'`') {
            let quote = bytes[index];
            let mut end = index + 1;
            while end < bytes.len() {
                if bytes[end] == b'\\' {
                    end = end.saturating_add(2);
                } else if bytes[end] == quote {
                    break;
                } else {
                    end += 1;
                }
            }
            evidence
                .strings
                .push_str(&source[index + 1..end.min(bytes.len())]);
            evidence.strings.push(' ');
            index = end.saturating_add(1);
        } else {
            evidence.code.push(bytes[index] as char);
            index += 1;
        }
    }
    evidence
}

fn path_tokens(path: &str) -> Vec<String> {
    search_terms(path)
}

fn search_terms(value: &str) -> Vec<String> {
    let mut terms = BTreeSet::new();
    let mut current = String::new();
    let mut previous_lowercase = false;
    for character in value.chars() {
        if !character.is_alphanumeric() {
            insert_search_term(&mut terms, &mut current);
            previous_lowercase = false;
            continue;
        }
        if character.is_uppercase() && previous_lowercase && !current.is_empty() {
            insert_search_term(&mut terms, &mut current);
        }
        previous_lowercase = character.is_lowercase();
        current.extend(character.to_lowercase());
    }
    insert_search_term(&mut terms, &mut current);
    terms.into_iter().collect()
}

fn insert_search_term(terms: &mut BTreeSet<String>, current: &mut String) {
    if current.is_empty() {
        return;
    }
    let word = std::mem::take(current);
    if word.len() < 2 || is_search_stop_word(&word) {
        return;
    }
    terms.insert(stem(&word));
}

fn stem(word: &str) -> String {
    if word.len() > 5 && word.ends_with("ies") {
        format!("{}y", &word[..word.len() - 3])
    } else if word.len() > 6 && word.ends_with("ation") {
        word[..word.len() - 5].to_owned()
    } else if word.len() > 5 && word.ends_with("ing") {
        word[..word.len() - 3].to_owned()
    } else if word.len() > 4 && word.ends_with("ed") {
        word[..word.len() - 2].to_owned()
    } else if word.len() > 3 && word.ends_with('s') {
        word[..word.len() - 1].to_owned()
    } else {
        word.to_owned()
    }
}

fn is_search_stop_word(word: &str) -> bool {
    matches!(
        word,
        "a" | "an"
            | "and"
            | "are"
            | "at"
            | "be"
            | "by"
            | "do"
            | "does"
            | "for"
            | "from"
            | "get"
            | "gets"
            | "how"
            | "in"
            | "is"
            | "it"
            | "of"
            | "on"
            | "or"
            | "the"
            | "to"
            | "what"
            | "when"
            | "where"
            | "with"
    )
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
    fn discovery_find_uses_source_evidence_across_providers() {
        let repo = Repo::new(&[
            (
                "web/session.ts",
                "/** Keep browser sessions alive after a refresh. */\nexport function renewSession() { return 'session renewed'; }\n",
            ),
            (
                "net/retry.py",
                "def retry_failed_request():\n    \"\"\"Retry failed HTTP network requests.\"\"\"\n    return 'retry request'\n",
            ),
            (
                "auth/tokens.rs",
                "// Validates incoming authentication tokens before requests run.\npub fn verify_bearer_token() {}\n",
            ),
            (
                "cmd/options.go",
                "// Register command line options for the server.\nfunc RegisterFlags() {}\n",
            ),
            (
                "src/Cleanup.java",
                "class Cleanup { // Remove test resources after every test.\n  void tearDownTest() {}\n}\n",
            ),
        ]);
        let index = Index::build(&repo.0).unwrap();
        for (task, target, reason) in [
            (
                "users get logged out after browser refresh",
                "web/session.ts::renewSession",
                "comment/doc match",
            ),
            (
                "where are failed HTTP requests retried",
                "net/retry.py::retry_failed_request",
                "comment/doc match",
            ),
            (
                "what validates incoming auth tokens",
                "auth/tokens.rs::verify_bearer_token",
                "comment/doc match",
            ),
            (
                "where are command-line options registered",
                "cmd/options.go::RegisterFlags",
                "comment/doc match",
            ),
            (
                "where is test cleanup performed",
                "src/Cleanup.java::Cleanup.tearDownTest",
                "comment/doc match",
            ),
        ] {
            let found = query::find(index.graph(), task);
            assert!(found.contains(target), "{task}: {found}");
            assert!(found.contains(reason), "{task}: {found}");
        }
        assert!(query::find(index.graph(), "renewSession").contains("exact name"));
        assert_eq!(
            query::find(index.graph(), "where are command-line options registered"),
            query::find(index.graph(), "where are command-line options registered")
        );
    }

    #[test]
    fn discovery_evidence_refreshes_and_persists_with_source_edits() {
        let repo = Repo::new(&[("src/session.ts", "export function rotate() {}\n")]);
        let mut index = Index::build(&repo.0).unwrap();
        assert!(query::find(index.graph(), "browser cookie renewal").starts_with("No symbols"));
        repo.write(
            "src/session.ts",
            "/** Renew the browser cookie after page refresh. */\nexport function rotate() {}\n",
        );
        assert_eq!(
            index.refresh(Path::new("src/session.ts")).unwrap(),
            RefreshKind::Incremental
        );
        let refreshed = query::find(index.graph(), "browser cookie renewal");
        assert!(refreshed.contains("src/session.ts::rotate"), "{refreshed}");
        let persistent = Index::open(&repo.0).unwrap();
        assert_eq!(
            refreshed,
            query::find(persistent.graph(), "browser cookie renewal")
        );
    }

    #[test]
    fn inspection_reads_a_bounded_fresh_source_packet_and_ranks_tests_as_context() {
        let repo = Repo::new(&[
            (
                "src/auth.ts",
                "/** Validate incoming bearer tokens before a request reaches a handler. */\nexport function verifyCredentials(token: string) {\n  return token.length > 0;\n}\n\nexport function longOperation() {\n  const one = '01';\n  const two = '02';\n  const three = '03';\n  const four = '04';\n  const five = '05';\n  const six = '06';\n  const seven = '07';\n  const eight = '08';\n  const nine = '09';\n  const ten = '10';\n  const eleven = '11';\n  const twelve = '12';\n  const thirteen = '13';\n  const fourteen = '14';\n  const fifteen = '15';\n  const sixteen = '16';\n  const seventeen = '17';\n  const eighteen = '18';\n  const nineteen = '19';\n  const twenty = '20';\n  const twentyOne = '21';\n  const twentyTwo = '22';\n  const twentyThree = '23';\n  const twentyFour = '24';\n  return one + two + three + four + five + six + seven + eight + nine + ten + eleven + twelve + thirteen + fourteen + fifteen + sixteen + seventeen + eighteen + nineteen + twenty + twentyOne + twentyTwo + twentyThree + twentyFour;\n}\n",
            ),
            (
                "src/use.ts",
                "import { verifyCredentials } from './auth';\nexport function handleRequest() { return verifyCredentials('token'); }\n",
            ),
            (
                "tests/auth.test.ts",
                "import { verifyCredentials } from '../src/auth';\nexport function test_rejects_empty_token() { return verifyCredentials(''); }\n",
            ),
        ]);
        let mut index = Index::build(&repo.0).unwrap();
        let packet = index.inspect("src/auth.ts::verifyCredentials").unwrap();
        assert!(packet.contains("Source context (1-4):"), "{packet}");
        assert!(
            packet.contains("Validate incoming bearer tokens"),
            "{packet}"
        );
        assert!(packet.contains("src/use.ts::handleRequest"), "{packet}");
        assert!(packet.contains("call at src/use.ts:2:"), "{packet}");
        assert!(packet.contains("Likely relevant tests"), "{packet}");
        assert!(packet.contains("test_rejects_empty_token"), "{packet}");

        let long = index.inspect("src/auth.ts::longOperation").unwrap();
        assert!(long.contains("source lines omitted"), "{long}");

        repo.write(
            "src/auth.ts",
            "/** Validate freshly rotated bearer tokens before a request reaches a handler. */\nexport function verifyCredentials(token: string) {\n  return token.length > 0;\n}\n",
        );
        assert_eq!(
            index.refresh(Path::new("src/auth.ts")).unwrap(),
            RefreshKind::Incremental
        );
        assert!(
            index
                .inspect("src/auth.ts::verifyCredentials")
                .unwrap()
                .contains("freshly rotated bearer tokens")
        );
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
    fn same_class_this_calls_resolve_without_receiver_inference() {
        let repo = Repo::new(&[(
            "src/worker.ts",
            "export class Worker {\n  leaf() {}\n  entry() { this.leaf(); this.leaf(); }\n  static staticLeaf() {}\n  static staticEntry() { this.staticLeaf(); }\n}\n",
        )]);
        let index = Index::build(&repo.0).unwrap();
        let graph = index.graph();
        assert_eq!(graph.calls.len(), 2);
        let entry = graph.symbol_candidates("src/worker.ts::Worker.entry")[0];
        let leaf = graph.symbol_candidates("src/worker.ts::Worker.leaf")[0];
        assert_eq!(graph.call_sites(entry, leaf).len(), 2);
        assert!(
            query::trace(
                graph,
                "src/worker.ts::Worker.entry",
                "src/worker.ts::Worker.leaf"
            )
            .unwrap()
            .contains("calls at src/worker.ts:3:13, src/worker.ts:3:26")
        );
        assert!(
            query::trace(
                graph,
                "src/worker.ts::Worker.staticEntry",
                "src/worker.ts::Worker.staticLeaf"
            )
            .unwrap()
            .contains("Known CALLS path")
        );
    }

    #[test]
    fn this_calls_remain_conservative_for_missing_ambiguous_and_similar_syntax() {
        let repo = Repo::new(&[(
            "src/worker.ts",
            "export class Worker {\n  leaf() {}\n  static staticLeaf() {}\n  value = 1;\n  entry() { const leaf = () => {}; const self = this; this.leaf(); this.missing(); this.value(); this.staticLeaf(); this['leaf'](); self.leaf(); service.leaf(); }\n  static staticEntry() { this.leaf(); }\n}\nexport class Duplicate { foo() {} foo() {} entry() { this.foo(); } }\n",
        )]);
        let index = Index::build(&repo.0).unwrap();
        let entry = query::inspect(index.graph(), "src/worker.ts::Worker.entry").unwrap();
        assert!(entry.contains("src/worker.ts::Worker.leaf"));
        assert!(entry.contains("no matching same-class method"));
        assert!(entry.contains("receiver or dynamic call syntax"));
        let duplicate = query::inspect(index.graph(), "src/worker.ts::Duplicate.entry").unwrap();
        assert!(duplicate.contains("AMBIGUOUS"));
        assert!(duplicate.contains("multiple same-class methods"));
    }

    #[test]
    fn this_calls_refresh_and_persist_like_a_full_build() {
        let repo = Repo::new(&[(
            "src/worker.ts",
            "export class Worker { leaf() {} entry() { this.leaf(); } }\n",
        )]);
        let mut index = Index::build(&repo.0).unwrap();
        repo.write(
            "src/worker.ts",
            "export class Worker { leaf() {} next() {} entry() { this.leaf(); this.next(); } }\n",
        );
        assert_eq!(
            index.refresh(Path::new("src/worker.ts")).unwrap(),
            RefreshKind::Incremental
        );
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&index, &full);
        let persistent = Index::open(&repo.0).unwrap();
        assert_equivalent(&persistent, &full);
    }

    #[test]
    fn one_hop_named_reexports_preserve_canonical_targets_and_evidence() {
        let repo = Repo::new(&[
            (
                "src/leaf.ts",
                "export function leaf() {}\nexport const value = 1;\n",
            ),
            (
                "src/barrel.ts",
                "export { leaf, leaf as publicLeaf, value } from './leaf.js';\n",
            ),
            (
                "src/use.ts",
                "import { publicLeaf } from './barrel.js';\nexport function entry() { publicLeaf(); publicLeaf(); }\n",
            ),
        ]);
        let index = Index::build(&repo.0).unwrap();
        let graph = index.graph();
        let entry = graph.symbol_candidates("src/use.ts::entry")[0];
        let leaf = graph.symbol_candidates("src/leaf.ts::leaf")[0];
        assert_eq!(graph.calls.len(), 1);
        assert_eq!(graph.call_sites(entry, leaf).len(), 2);
        assert!(
            query::trace(graph, "src/use.ts::entry", "src/leaf.ts::leaf")
                .unwrap()
                .contains("Known CALLS path")
        );
        assert!(
            query::inspect(graph, "src/use.ts::entry")
                .unwrap()
                .contains("calls at src/use.ts:2:")
        );
        assert!(graph.imports.iter().any(|edge| {
            graph.files[edge.from].path == "src/barrel.ts"
                && graph.files[edge.to].path == "src/leaf.ts"
        }));
        assert!(
            query::inspect(graph, "src/use.ts::entry")
                .unwrap()
                .contains("src/leaf.ts::leaf")
        );
    }

    #[test]
    fn named_reexports_remain_conservative_for_missing_values_types_and_ambiguous_modules() {
        let missing = Repo::new(&[
            (
                "src/leaf.ts",
                "export function leaf() {}\nexport const value = 1;\n",
            ),
            (
                "src/barrel.ts",
                "export { missing, value } from './leaf.js';\nexport type { leaf } from './leaf.js';\n",
            ),
            (
                "src/use.ts",
                "import { missing, value, leaf } from './barrel.js';\nexport function entry() { missing(); value(); leaf(); }\n",
            ),
        ]);
        let output = query::inspect(
            Index::build(&missing.0).unwrap().graph(),
            "src/use.ts::entry",
        )
        .unwrap();
        assert!(output.contains("imported binding is not uniquely resolved"));
        assert!(!output.contains("src/leaf.ts::leaf"));

        let ambiguous = Repo::new(&[
            ("src/api.ts", "export function leaf() {}\n"),
            ("src/api.tsx", "export function leaf() {}\n"),
            ("src/barrel.ts", "export { leaf } from './api.js';\n"),
            (
                "src/use.ts",
                "import { leaf } from './barrel.js';\nexport function entry() { leaf(); }\n",
            ),
        ]);
        let output = query::inspect(
            Index::build(&ambiguous.0).unwrap().graph(),
            "src/use.ts::entry",
        )
        .unwrap();
        assert!(output.contains("AMBIGUOUS"));
        assert!(output.contains("imported binding is not uniquely resolved"));

        let conflicting = Repo::new(&[
            ("src/one.ts", "export function leaf() {}\n"),
            ("src/two.ts", "export function leaf() {}\n"),
            (
                "src/barrel.ts",
                "export { leaf as publicLeaf } from './one.js';\nexport { leaf as publicLeaf } from './two.js';\n",
            ),
            (
                "src/use.ts",
                "import { publicLeaf } from './barrel.js';\nexport function entry() { publicLeaf(); }\n",
            ),
        ]);
        let output = query::inspect(
            Index::build(&conflicting.0).unwrap().graph(),
            "src/use.ts::entry",
        )
        .unwrap();
        assert!(output.contains("AMBIGUOUS"));
    }

    #[test]
    fn named_reexport_refresh_and_persistence_re_resolve_transitive_importers() {
        let repo = Repo::new(&[
            ("src/leaf.ts", "export function leaf() {}\n"),
            ("src/barrel.ts", "export { leaf } from './leaf.js';\n"),
            (
                "src/use.ts",
                "import { leaf } from './barrel.js';\nexport function entry() { leaf(); }\n",
            ),
        ]);
        let mut index = Index::build(&repo.0).unwrap();
        repo.write("src/leaf.ts", "export function renamed() {}\n");
        assert_eq!(
            index.refresh(Path::new("src/leaf.ts")).unwrap(),
            RefreshKind::Incremental
        );
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&index, &full);
        assert!(
            query::inspect(index.graph(), "src/use.ts::entry")
                .unwrap()
                .contains("Direct callees: none")
        );
        let persistent = Index::open(&repo.0).unwrap();
        assert_equivalent(&persistent, &full);
    }

    #[test]
    fn local_export_lists_forward_callables_without_changing_canonical_identity() {
        let repo = Repo::new(&[
            (
                "src/leaf.ts",
                "export function leaf() {}\nexport const value = 1;\n",
            ),
            (
                "src/barrel.ts",
                "import { leaf as localLeaf, value } from './leaf.js';\nexport { localLeaf as publicLeaf, value };\n",
            ),
            (
                "src/use.ts",
                "import { publicLeaf, value } from './barrel.js';\nexport function entry() { publicLeaf(); publicLeaf(); value(); }\n",
            ),
        ]);
        let index = Index::build(&repo.0).unwrap();
        let graph = index.graph();
        let entry = graph.symbol_candidates("src/use.ts::entry")[0];
        let leaf = graph.symbol_candidates("src/leaf.ts::leaf")[0];
        assert_eq!(graph.calls.len(), 1);
        assert_eq!(graph.call_sites(entry, leaf).len(), 2);
        assert!(
            query::trace(graph, "entry", "src/leaf.ts::leaf")
                .unwrap()
                .contains("Known CALLS path")
        );
        let output = query::inspect(graph, "src/use.ts::entry").unwrap();
        assert!(output.contains("src/leaf.ts::leaf"));
        assert!(output.contains("imported binding is not uniquely resolved"));
    }

    #[test]
    fn local_callable_export_lists_and_failures_remain_conservative() {
        let direct = Repo::new(&[
            ("src/api.ts", "const leaf = () => {};\nexport { leaf };\n"),
            (
                "src/use.ts",
                "import { leaf } from './api.js';\nexport function entry() { leaf(); }\n",
            ),
        ]);
        assert!(
            query::trace(
                Index::build(&direct.0).unwrap().graph(),
                "src/use.ts::entry",
                "src/api.ts::leaf"
            )
            .unwrap()
            .contains("Known CALLS path")
        );

        let local = Repo::new(&[
            (
                "src/api.ts",
                "const hidden = () => {};\nexport { hidden as publicLeaf };\n",
            ),
            (
                "src/use.ts",
                "import { publicLeaf } from './api.js';\nexport function entry() { const publicLeaf = () => {}; publicLeaf(); }\n",
            ),
        ]);
        let output =
            query::inspect(Index::build(&local.0).unwrap().graph(), "src/use.ts::entry").unwrap();
        assert!(output.contains("unmodeled local binding could shadow this name"));
        assert!(!output.contains("src/api.ts::hidden"));

        let missing = Repo::new(&[
            (
                "src/api.ts",
                "export { missing };\nexport type { TypeOnly };\n",
            ),
            (
                "src/use.ts",
                "import { missing, TypeOnly } from './api.js';\nexport function entry() { missing(); TypeOnly(); }\n",
            ),
        ]);
        let output = query::inspect(
            Index::build(&missing.0).unwrap().graph(),
            "src/use.ts::entry",
        )
        .unwrap();
        assert!(output.contains("imported binding is not uniquely resolved"));

        let ambiguous = Repo::new(&[
            (
                "src/leaf.ts",
                "export function leaf() {}\nexport function leaf() {}\n",
            ),
            (
                "src/barrel.ts",
                "import { leaf } from './leaf.js';\nexport { leaf as publicLeaf };\n",
            ),
            (
                "src/use.ts",
                "import { publicLeaf } from './barrel.js';\nexport function entry() { publicLeaf(); }\n",
            ),
        ]);
        let output = query::inspect(
            Index::build(&ambiguous.0).unwrap().graph(),
            "src/use.ts::entry",
        )
        .unwrap();
        assert!(output.contains("AMBIGUOUS"));
    }

    #[test]
    fn local_export_forwarding_refreshes_and_persists_like_a_full_build() {
        let repo = Repo::new(&[
            ("src/leaf.ts", "export function leaf() {}\n"),
            (
                "src/barrel.ts",
                "import { leaf } from './leaf.js';\nexport { leaf as publicLeaf };\n",
            ),
            (
                "src/use.ts",
                "import { publicLeaf } from './barrel.js';\nexport function entry() { publicLeaf(); publicLeaf(); }\n",
            ),
        ]);
        let mut index = Index::build(&repo.0).unwrap();
        repo.write("src/leaf.ts", "export function renamed() {}\n");
        assert_eq!(
            index.refresh(Path::new("src/leaf.ts")).unwrap(),
            RefreshKind::Incremental
        );
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&index, &full);
        assert!(
            query::inspect(index.graph(), "src/use.ts::entry")
                .unwrap()
                .contains("Direct callees: none")
        );
        let persistent = Index::open(&repo.0).unwrap();
        assert_equivalent(&persistent, &full);
    }

    #[test]
    fn immutable_constructor_receivers_resolve_unique_instance_methods() {
        let same_file = Repo::new(&[(
            "src/worker.ts",
            "export class Worker { run() {} static staticRun() {} entry() { const worker = new Worker(); worker.run(); worker.run(); } staticEntry() { const worker = new Worker(); worker.staticRun(); } }\n",
        )]);
        let index = Index::build(&same_file.0).unwrap();
        let graph = index.graph();
        let entry = graph.symbol_candidates("src/worker.ts::Worker.entry")[0];
        let run = graph.symbol_candidates("src/worker.ts::Worker.run")[0];
        assert_eq!(graph.call_sites(entry, run).len(), 2);
        assert!(
            query::trace(
                graph,
                "src/worker.ts::Worker.entry",
                "src/worker.ts::Worker.run"
            )
            .unwrap()
            .contains("Known CALLS path")
        );
        assert!(
            query::inspect(graph, "src/worker.ts::Worker.staticEntry")
                .unwrap()
                .contains("no matching non-static method")
        );

        let imported = Repo::new(&[
            ("src/service.ts", "export class Service { run() {} }\n"),
            (
                "src/use.ts",
                "import { Service } from './service.js';\nexport function entry() { const service = new Service(); service.run(); }\n",
            ),
        ]);
        assert!(
            query::trace(
                Index::build(&imported.0).unwrap().graph(),
                "src/use.ts::entry",
                "src/service.ts::Service.run"
            )
            .unwrap()
            .contains("Known CALLS path")
        );
    }

    #[test]
    fn constructor_receiver_limits_remain_explicit() {
        let repo = Repo::new(&[(
            "src/worker.ts",
            "class Worker { run() {} duplicate() {} duplicate() {} entry(Worker: unknown) { const service = new Worker(); service.run(); } shadow() { const service = new Worker(); { const service = new Worker(); service.run(); } } reassigned() { const service = new Worker(); service = other; service.run(); } mutable() { let service = new Worker(); service = other; service.run(); } conditional() { const service = ok ? new Worker() : new Worker(); service.run(); } computed() { const service = new Worker(); service['run'](); } inline() { new Worker().run(); } duplicateEntry() { const service = new Worker(); service.duplicate(); } missingCall() { const service = new Worker(); service.missing(); } }\n",
        )]);
        let graph = Index::build(&repo.0).unwrap();
        let shadow = query::inspect(graph.graph(), "src/worker.ts::Worker.entry").unwrap();
        assert!(shadow.contains("constructor class name could be shadowed"));
        let nested = query::inspect(graph.graph(), "src/worker.ts::Worker.shadow").unwrap();
        assert!(nested.contains("receiver ownership is not uniquely proven"));
        let reassigned = query::inspect(graph.graph(), "src/worker.ts::Worker.reassigned").unwrap();
        assert!(reassigned.contains("receiver binding was reassigned"));
        for target in [
            "src/worker.ts::Worker.mutable",
            "src/worker.ts::Worker.conditional",
            "src/worker.ts::Worker.computed",
            "src/worker.ts::Worker.inline",
        ] {
            assert!(
                query::inspect(graph.graph(), target)
                    .unwrap()
                    .contains("receiver or dynamic call syntax")
            );
        }
        assert!(
            query::inspect(graph.graph(), "src/worker.ts::Worker.duplicateEntry")
                .unwrap()
                .contains("multiple non-static methods")
        );
        let missing = query::inspect(graph.graph(), "src/worker.ts::Worker.missingCall").unwrap();
        assert!(
            missing.contains("no matching non-static method"),
            "{missing}"
        );
    }

    #[test]
    fn constructor_receivers_refresh_and_persist_like_a_full_build() {
        let repo = Repo::new(&[
            ("src/service.ts", "export class Service { run() {} }\n"),
            (
                "src/use.ts",
                "import { Service } from './service.js';\nexport function entry() { const service = new Service(); service.run(); }\n",
            ),
        ]);
        let mut index = Index::build(&repo.0).unwrap();
        repo.write("src/service.ts", "export class Service { renamed() {} }\n");
        assert_eq!(
            index.refresh(Path::new("src/service.ts")).unwrap(),
            RefreshKind::Incremental
        );
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&index, &full);
        assert!(
            query::inspect(index.graph(), "src/use.ts::entry")
                .unwrap()
                .contains("no matching non-static method")
        );
        let persistent = Index::open(&repo.0).unwrap();
        assert_equivalent(&persistent, &full);
    }

    #[test]
    fn python_refresh_imports_and_persistence_match_a_full_build() {
        let repo = Repo::new(&[
            ("pkg/util.py", "def leaf():\n    pass\n"),
            (
                "pkg/main.py",
                "from .util import leaf\n\ndef entry():\n    leaf()\n",
            ),
        ]);
        let mut index = Index::build(&repo.0).unwrap();
        assert!(
            query::trace(index.graph(), "pkg/main.py::entry", "pkg/util.py::leaf")
                .unwrap()
                .contains("Known CALLS path")
        );
        repo.write(
            "pkg/main.py",
            "from .util import leaf\n\ndef entry():\n    leaf()\n    leaf()\n",
        );
        assert_eq!(
            index.refresh(Path::new("pkg/main.py")).unwrap(),
            RefreshKind::Incremental
        );
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&index, &full);
        let entry = index.graph().symbol_candidates("pkg/main.py::entry")[0];
        let leaf = index.graph().symbol_candidates("pkg/util.py::leaf")[0];
        assert_eq!(index.graph().call_sites(entry, leaf).len(), 2);

        repo.write("pkg/util.py", "def renamed():\n    pass\n");
        assert_eq!(
            index.refresh(Path::new("pkg/util.py")).unwrap(),
            RefreshKind::Incremental
        );
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&index, &full);
        assert!(
            query::inspect(index.graph(), "pkg/main.py::entry")
                .unwrap()
                .contains("imported Python binding is not uniquely resolved")
        );
        let persistent = Index::open(&repo.0).unwrap();
        assert_equivalent(&persistent, &full);
    }

    #[test]
    fn mixed_js_ts_and_python_files_share_one_graph_and_refresh_independently() {
        let repo = Repo::new(&[
            (
                "frontend.ts",
                "export function uiLeaf() {}\nexport function uiEntry() { uiLeaf(); }\n",
            ),
            (
                "backend.py",
                "def api_leaf():\n    pass\n\ndef api_entry():\n    api_leaf()\n",
            ),
        ]);
        let mut index = Index::build(&repo.0).unwrap();
        assert_eq!(index.graph().files.len(), 2);
        assert!(query::find(index.graph(), "uiEntry").contains("frontend.ts::uiEntry"));
        assert!(query::find(index.graph(), "api_entry").contains("backend.py::api_entry"));

        repo.write(
            "backend.py",
            "def api_leaf():\n    pass\n\ndef api_entry():\n    api_leaf()\n    api_leaf()\n",
        );
        assert_eq!(
            index.refresh(Path::new("backend.py")).unwrap(),
            RefreshKind::Incremental
        );
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&index, &full);
        assert!(
            query::inspect(index.graph(), "frontend.ts::uiEntry")
                .unwrap()
                .contains("frontend.ts::uiLeaf")
        );

        repo.write(
            "frontend.ts",
            "export function uiLeaf() {}\nexport function uiNext() { uiLeaf(); }\n",
        );
        assert_eq!(
            index.refresh(Path::new("frontend.ts")).unwrap(),
            RefreshKind::Incremental
        );
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&index, &full);
        assert!(
            query::inspect(index.graph(), "backend.py::api_entry")
                .unwrap()
                .contains("backend.py::api_leaf")
        );
    }

    #[test]
    fn rust_refresh_modules_and_persistence_match_a_full_build() {
        let repo = Repo::new(&[
            (
                "src/lib.rs",
                "mod util;\nuse crate::util::leaf;\nfn entry() { leaf(); }\nstruct Worker;\nimpl Worker { fn leaf(&self) {} fn entry(&self) { self.leaf(); } }\n",
            ),
            ("src/util.rs", "pub fn leaf() {}\n"),
        ]);
        let mut index = Index::build(&repo.0).unwrap();
        assert!(
            query::trace(index.graph(), "src/lib.rs::entry", "src/util.rs::leaf")
                .unwrap()
                .contains("Known CALLS path")
        );
        repo.write(
            "src/lib.rs",
            "mod util;\nuse crate::util::leaf;\nfn entry() { leaf(); leaf(); }\nstruct Worker;\nimpl Worker { fn leaf(&self) {} fn next(&self) {} fn entry(&self) { self.leaf(); self.next(); } }\n",
        );
        assert_eq!(
            index.refresh(Path::new("src/lib.rs")).unwrap(),
            RefreshKind::Incremental
        );
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&index, &full);
        let entry = index.graph().symbol_candidates("src/lib.rs::entry")[0];
        let leaf = index.graph().symbol_candidates("src/util.rs::leaf")[0];
        assert_eq!(index.graph().call_sites(entry, leaf).len(), 2);

        repo.write("src/util.rs", "pub fn renamed() {}\n");
        assert_eq!(
            index.refresh(Path::new("src/util.rs")).unwrap(),
            RefreshKind::Incremental
        );
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&index, &full);
        assert!(
            query::inspect(index.graph(), "src/lib.rs::entry")
                .unwrap()
                .contains("imported Rust binding is not uniquely resolved")
        );
        let persistent = Index::open(&repo.0).unwrap();
        assert_equivalent(&persistent, &full);
    }

    #[test]
    fn mixed_js_ts_python_rust_go_and_java_files_share_one_graph_and_refresh_independently() {
        let repo = Repo::new(&[
            (
                "frontend.ts",
                "export function uiLeaf() {}\nexport function uiEntry() { uiLeaf(); }\n",
            ),
            (
                "backend.py",
                "def api_leaf():\n    pass\n\ndef api_entry():\n    api_leaf()\n",
            ),
            (
                "engine.rs",
                "fn engine_leaf() {}\nfn engine_entry() { engine_leaf(); }\n",
            ),
            (
                "worker.go",
                "package worker\nfunc goLeaf() {}\nfunc goEntry() { goLeaf() }\n",
            ),
            (
                "Worker.java",
                "package app; class Worker { void javaLeaf() {} void javaEntry() { this.javaLeaf(); } }\n",
            ),
        ]);
        let mut index = Index::build(&repo.0).unwrap();
        assert_eq!(index.graph().files.len(), 5);
        for target in [
            "uiEntry",
            "api_entry",
            "engine_entry",
            "goEntry",
            "javaEntry",
        ] {
            assert!(!query::find(index.graph(), target).starts_with("No symbols found"));
        }
        repo.write(
            "engine.rs",
            "fn engine_leaf() {}\nfn engine_entry() { engine_leaf(); engine_leaf(); }\n",
        );
        assert_eq!(
            index.refresh(Path::new("engine.rs")).unwrap(),
            RefreshKind::Incremental
        );
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&index, &full);
        assert!(
            query::inspect(index.graph(), "frontend.ts::uiEntry")
                .unwrap()
                .contains("frontend.ts::uiLeaf")
        );
        repo.write(
            "backend.py",
            "def api_leaf():\n    pass\n\ndef api_entry():\n    api_leaf()\n    api_leaf()\n",
        );
        assert_eq!(
            index.refresh(Path::new("backend.py")).unwrap(),
            RefreshKind::Incremental
        );
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&index, &full);
        assert!(
            query::inspect(index.graph(), "engine.rs::engine_entry")
                .unwrap()
                .contains("engine.rs::engine_leaf")
        );
        repo.write(
            "worker.go",
            "package worker\nfunc goLeaf() {}\nfunc goEntry() { goLeaf(); goLeaf() }\n",
        );
        assert_eq!(
            index.refresh(Path::new("worker.go")).unwrap(),
            RefreshKind::Incremental
        );
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&index, &full);
        assert_eq!(
            index
                .graph()
                .call_sites(
                    index.graph().symbol_candidates("worker.go::goEntry")[0],
                    index.graph().symbol_candidates("worker.go::goLeaf")[0]
                )
                .len(),
            2
        );
        repo.write(
            "Worker.java",
            "package app; class Worker { void javaLeaf() {} void javaEntry() { this.javaLeaf(); this.javaLeaf(); } }\n",
        );
        assert_eq!(
            index.refresh(Path::new("Worker.java")).unwrap(),
            RefreshKind::Incremental
        );
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&index, &full);
        assert_eq!(
            index
                .graph()
                .call_sites(
                    index
                        .graph()
                        .symbol_candidates("Worker.java::Worker.javaEntry")[0],
                    index
                        .graph()
                        .symbol_candidates("Worker.java::Worker.javaLeaf")[0]
                )
                .len(),
            2
        );
    }

    #[test]
    fn java_overload_signatures_are_individually_queryable() {
        let repo = Repo::new(&[(
            "Worker.java",
            "class Worker { void f() {} void f(int value) {} void g() { f(); f(1); } }\n",
        )]);
        let index = Index::build(&repo.0).unwrap();
        let graph = index.graph();
        let empty = "Worker.java::Worker.f()";
        let integer = "Worker.java::Worker.f(int)";
        assert_eq!(graph.symbol_candidates(empty).len(), 1);
        assert_eq!(graph.symbol_candidates(integer).len(), 1);
        assert!(query::inspect(graph, empty).unwrap().contains(empty));
        assert!(query::inspect(graph, integer).unwrap().contains(integer));
        assert!(
            query::trace(graph, "Worker.java::Worker.g", empty)
                .unwrap()
                .contains("Known CALLS path")
        );
        assert!(
            query::trace(graph, "Worker.java::Worker.g", integer)
                .unwrap()
                .contains("Known CALLS path")
        );
    }

    #[test]
    fn go_module_context_rebuilds_and_re_resolves_importers() {
        let repo = Repo::new(&[
            ("go.mod", "module example.com/one\n"),
            (
                "main.go",
                "package main\nimport \"example.com/one/util\"\nfunc entry() { util.Helper() }\n",
            ),
            ("util/util.go", "package util\nfunc Helper() {}\n"),
        ]);
        let mut index = Index::open(&repo.0).unwrap();
        assert!(query::trace(index.graph(), "main.go::entry", "util/util.go::Helper").is_ok());
        repo.write("go.mod", "module example.com/two\n");
        index.sync().unwrap();
        let full = Index::build(&repo.0).unwrap();
        assert_equivalent(&index, &full);
        assert!(
            query::trace(index.graph(), "main.go::entry", "util/util.go::Helper")
                .unwrap()
                .starts_with("No known CALLS path")
        );
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
