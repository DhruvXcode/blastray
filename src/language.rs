use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::index::{RelationshipIssue, SymbolKind};
use crate::languages::js_ts;
use crate::languages::python;
use crate::languages::rust;

#[derive(Clone, Deserialize, Serialize)]
pub(crate) enum ParsedFile {
    JsTs(js_ts::ParsedFile),
    Python(python::ParsedFile),
    Rust(rust::ParsedFile),
}

struct Provider {
    supports_path: fn(&Path) -> bool,
    parse: fn(&str, &str) -> Result<ParsedFile, String>,
    resolve: ResolveProvider,
    extensions: &'static [&'static str],
}

type ResolveProvider =
    fn(&BTreeMap<String, ParsedFile>, &BTreeSet<String>) -> BTreeMap<String, ResolvedFile>;

const PROVIDERS: [Provider; 3] = [
    Provider {
        supports_path: js_ts::supports_path,
        parse: js_ts::parse,
        resolve: js_ts::resolve,
        extensions: js_ts::EXTENSIONS,
    },
    Provider {
        supports_path: python::supports_path,
        parse: python::parse,
        resolve: python::resolve,
        extensions: python::EXTENSIONS,
    },
    Provider {
        supports_path: rust::supports_path,
        parse: rust::parse,
        resolve: rust::resolve,
        extensions: rust::EXTENSIONS,
    },
];

#[derive(Clone)]
pub(crate) struct SymbolFact {
    pub canonical: String,
    pub name: String,
    pub file: String,
    pub line: usize,
    pub end_line: usize,
    pub column: usize,
    pub kind: SymbolKind,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub(crate) struct ResolvedFile {
    pub imports: Vec<String>,
    pub calls: Vec<ResolvedCall>,
    pub issues: Vec<RelationshipIssue>,
}

#[derive(Clone, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) struct ResolvedCall {
    pub from: String,
    pub to: String,
    pub line: usize,
    pub column: usize,
}

impl ParsedFile {
    pub(crate) fn path(&self) -> &str {
        match self {
            Self::JsTs(file) => &file.path,
            Self::Python(file) => &file.path,
            Self::Rust(file) => &file.path,
        }
    }

    pub(crate) fn symbols(&self) -> Vec<SymbolFact> {
        match self {
            Self::JsTs(file) => file.symbols.iter().map(SymbolFact::from).collect(),
            Self::Python(file) => file.symbols.iter().map(SymbolFact::from).collect(),
            Self::Rust(file) => file.symbols.iter().map(SymbolFact::from).collect(),
        }
    }
}

impl From<&js_ts::SymbolDraft> for SymbolFact {
    fn from(symbol: &js_ts::SymbolDraft) -> Self {
        Self {
            canonical: symbol.canonical.clone(),
            name: symbol.name.clone(),
            file: symbol.file.clone(),
            line: symbol.line,
            end_line: symbol.end_line,
            column: symbol.column,
            kind: symbol.kind,
        }
    }
}

impl From<&python::SymbolDraft> for SymbolFact {
    fn from(symbol: &python::SymbolDraft) -> Self {
        Self {
            canonical: symbol.canonical.clone(),
            name: symbol.name.clone(),
            file: symbol.file.clone(),
            line: symbol.line,
            end_line: symbol.end_line,
            column: symbol.column,
            kind: symbol.kind,
        }
    }
}

impl From<&rust::SymbolDraft> for SymbolFact {
    fn from(symbol: &rust::SymbolDraft) -> Self {
        Self {
            canonical: symbol.canonical.clone(),
            name: symbol.name.clone(),
            file: symbol.file.clone(),
            line: symbol.line,
            end_line: symbol.end_line,
            column: symbol.column,
            kind: symbol.kind,
        }
    }
}

fn provider_for_path(path: &Path) -> Option<&'static Provider> {
    PROVIDERS
        .iter()
        .find(|provider| (provider.supports_path)(path))
}

pub(crate) fn is_supported_path(path: &Path) -> bool {
    provider_for_path(path).is_some()
}

pub(crate) fn parse(path: &str, source: &str) -> Result<ParsedFile, String> {
    provider_for_path(Path::new(path))
        .map(|provider| (provider.parse)(path, source))
        .unwrap_or_else(|| Err(format!("unsupported source file {path}")))
}

pub(crate) fn resolve_all(parsed: &BTreeMap<String, ParsedFile>) -> BTreeMap<String, ResolvedFile> {
    resolve_files(parsed, &parsed.keys().cloned().collect())
}

pub(crate) fn resolve_files(
    parsed: &BTreeMap<String, ParsedFile>,
    paths: &BTreeSet<String>,
) -> BTreeMap<String, ResolvedFile> {
    let mut resolved = BTreeMap::new();
    for provider in &PROVIDERS {
        resolved.extend((provider.resolve)(parsed, paths));
    }
    resolved
}

pub(crate) fn supported_extensions() -> Vec<&'static str> {
    PROVIDERS
        .iter()
        .flat_map(|provider| provider.extensions)
        .copied()
        .collect()
}

pub(crate) fn no_supported_source_files_message() -> String {
    let extensions: Vec<String> = supported_extensions()
        .into_iter()
        .map(|extension| format!(".{extension}"))
        .collect();
    format!(
        "No supported source files found.\nBlastRay currently indexes {}.",
        list_extensions(&extensions)
    )
}

fn list_extensions(extensions: &[String]) -> String {
    match extensions {
        [] => "no source extensions".to_string(),
        [extension] => extension.clone(),
        [first, second] => format!("{first} and {second}"),
        _ => format!(
            "{}, and {}",
            extensions[..extensions.len() - 1].join(", "),
            extensions.last().expect("the extension list is non-empty")
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{is_supported_path, no_supported_source_files_message, provider_for_path};

    #[test]
    fn js_ts_provider_owns_the_registered_extensions() {
        for path in ["a.ts", "a.tsx", "a.js", "a.jsx"] {
            assert!(provider_for_path(Path::new(path)).is_some());
        }
        assert!(provider_for_path(Path::new("a.py")).is_some());
        assert!(provider_for_path(Path::new("a.rs")).is_some());
        assert!(!is_supported_path(Path::new("a.dart")));
        assert_eq!(
            no_supported_source_files_message(),
            "No supported source files found.\nBlastRay currently indexes .ts, .tsx, .js, .jsx, .py, and .rs."
        );
    }
}
