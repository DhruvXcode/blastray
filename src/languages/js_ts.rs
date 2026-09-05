use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use tree_sitter::{Language, Node, Parser};

use crate::index::{RelationshipIssue, RelationshipStatus, SymbolKind};
use crate::language::{
    ParsedFile as ProviderParsedFile, ProviderContext, ResolvedCall, ResolvedFile,
};

pub(crate) const EXTENSIONS: &[&str] = &["ts", "tsx", "js", "jsx"];

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct ParsedFile {
    pub path: String,
    pub symbols: Vec<SymbolDraft>,
    pub imports: Vec<ImportDraft>,
    pub local_exports: Vec<LocalExportDraft>,
    pub reexports: Vec<ReexportDraft>,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct SymbolDraft {
    pub canonical: String,
    pub name: String,
    pub file: String,
    pub line: usize,
    pub end_line: usize,
    pub column: usize,
    pub kind: SymbolKind,
    pub exported: bool,
    pub default_export: bool,
    pub is_static: bool,
    pub calls: Vec<CallDraft>,
    pub receiver_bindings: Vec<ReceiverBindingDraft>,
    pub shadowed: BTreeSet<String>,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct CallDraft {
    pub name: String,
    pub receiver: Option<String>,
    pub line: usize,
    pub column: usize,
    pub direct: bool,
    pub this_member: bool,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct ReceiverBindingDraft {
    pub name: String,
    pub class: String,
    pub line: usize,
    pub column: usize,
    pub reassigned: bool,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct ImportDraft {
    pub module: String,
    pub line: usize,
    pub column: usize,
    pub bindings: Vec<ImportBinding>,
    pub type_only: bool,
    pub unsupported: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct ReexportDraft {
    pub module: String,
    pub line: usize,
    pub column: usize,
    pub bindings: Vec<ReexportBinding>,
    pub type_only: bool,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct LocalExportDraft {
    pub local: String,
    pub exported: String,
    pub type_only: bool,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct ReexportBinding {
    pub local: String,
    pub exported: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) enum ImportBinding {
    Named { local: String, imported: String },
    Default { local: String },
}

impl ImportBinding {
    pub fn local(&self) -> &str {
        match self {
            Self::Named { local, .. } | Self::Default { local } => local,
        }
    }
}

pub(crate) fn supports_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| EXTENSIONS.contains(&extension))
}

pub(crate) fn parse_file(path: &str, source: &str) -> Result<ParsedFile, String> {
    let language = language(path).ok_or_else(|| format!("unsupported source file {path}"))?;
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .map_err(|error| format!("cannot configure parser for {path}: {error}"))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| format!("cannot parse {path}"))?;
    let mut parsed = ParsedFile {
        path: path.to_string(),
        symbols: Vec::new(),
        imports: Vec::new(),
        local_exports: Vec::new(),
        reexports: Vec::new(),
    };
    let mut cursor = tree.root_node().walk();

    for child in tree.root_node().named_children(&mut cursor) {
        match child.kind() {
            "import_statement" => parsed.imports.push(import_draft(child, source)),
            "function_declaration" => add_function(&mut parsed, child, source, false, false),
            "class_declaration" => add_class(&mut parsed, child, source, false, false),
            "lexical_declaration" | "variable_declaration" => {
                add_variable_callables(&mut parsed, child, source, false)
            }
            "export_statement" => add_export(&mut parsed, child, source),
            _ => {}
        }
    }
    Ok(parsed)
}

pub(crate) fn parse(path: &str, source: &str) -> Result<ProviderParsedFile, String> {
    parse_file(path, source).map(ProviderParsedFile::JsTs)
}

fn language(path: &str) -> Option<Language> {
    match path.rsplit('.').next()? {
        "js" | "jsx" => Some(tree_sitter_javascript::LANGUAGE.into()),
        "ts" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        _ => None,
    }
}

fn add_export(parsed: &mut ParsedFile, node: Node<'_>, source: &str) {
    let text = text(node, source).trim_start();
    let default_export = text.starts_with("export default");
    if let Some(reexport) = reexport_draft(node, source) {
        parsed.reexports.push(reexport);
    } else if let Some(local_export) = local_export_draft(node, source) {
        parsed.local_exports.extend(local_export);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "function_declaration" => add_function(parsed, child, source, true, default_export),
            "class_declaration" => add_class(parsed, child, source, true, default_export),
            "lexical_declaration" | "variable_declaration" => {
                add_variable_callables(parsed, child, source, true)
            }
            _ => {}
        }
    }
}

fn local_export_draft(node: Node<'_>, source: &str) -> Option<Vec<LocalExportDraft>> {
    if node.child_by_field_name("source").is_some() {
        return None;
    }
    let export_text = text(node, source).trim_start();
    let open = export_text.find('{')?;
    let close = export_text[open + 1..].find('}')? + open + 1;
    let mut bindings = Vec::new();
    for specifier in export_text[open + 1..close].split(',') {
        let words: Vec<_> = specifier.split_whitespace().collect();
        let Some(local) = words.first() else {
            continue;
        };
        if *local == "type" {
            continue;
        }
        let exported = words
            .windows(2)
            .find(|pair| pair[0] == "as")
            .map(|pair| pair[1])
            .unwrap_or(local);
        if is_identifier(local) && is_identifier(exported) {
            bindings.push(LocalExportDraft {
                local: (*local).to_string(),
                exported: exported.to_string(),
                type_only: export_text.starts_with("export type "),
            });
        }
    }
    (!bindings.is_empty()).then_some(bindings)
}

fn reexport_draft(node: Node<'_>, source: &str) -> Option<ReexportDraft> {
    let module = node
        .child_by_field_name("source")
        .map(|source_node| string_text(source_node, source))?;
    let export_text = text(node, source).trim_start();
    let open = export_text.find('{')?;
    let close = export_text[open + 1..].find('}')? + open + 1;
    let mut bindings = Vec::new();
    for specifier in export_text[open + 1..close].split(',') {
        let words: Vec<_> = specifier.split_whitespace().collect();
        let Some(local) = words.first() else {
            continue;
        };
        if *local == "type" {
            continue;
        }
        let exported = words
            .windows(2)
            .find(|pair| pair[0] == "as")
            .map(|pair| pair[1])
            .unwrap_or(local);
        if is_identifier(local) && is_identifier(exported) {
            bindings.push(ReexportBinding {
                local: (*local).to_string(),
                exported: exported.to_string(),
            });
        }
    }
    (!bindings.is_empty()).then(|| ReexportDraft {
        module,
        line: node.start_position().row + 1,
        column: node.start_position().column + 1,
        bindings,
        type_only: export_text.starts_with("export type "),
    })
}

fn is_identifier(value: &str) -> bool {
    value
        .chars()
        .all(|character| character == '_' || character == '$' || character.is_alphanumeric())
}

fn add_variable_callables(parsed: &mut ParsedFile, node: Node<'_>, source: &str, exported: bool) {
    let mut cursor = node.walk();
    for declarator in node.named_children(&mut cursor) {
        if declarator.kind() != "variable_declarator" {
            continue;
        }
        let Some(name_node) = declarator.child_by_field_name("name") else {
            continue;
        };
        if name_node.kind() != "identifier" {
            continue;
        }
        let Some(value) = declarator.child_by_field_name("value") else {
            continue;
        };
        if !matches!(
            value.kind(),
            "arrow_function" | "function" | "function_expression"
        ) {
            continue;
        }
        let name = text(name_node, source).to_string();
        let (calls, receiver_bindings, shadowed) = callable_body(value, source);
        let position = declarator.start_position();
        let end_position = value.end_position();
        parsed.symbols.push(SymbolDraft {
            canonical: format!("{}::{name}", parsed.path),
            name,
            file: parsed.path.clone(),
            line: position.row + 1,
            end_line: end_position.row + 1,
            column: position.column + 1,
            kind: SymbolKind::Function,
            exported,
            default_export: false,
            is_static: false,
            calls,
            receiver_bindings,
            shadowed,
        });
    }
}

fn add_function(
    parsed: &mut ParsedFile,
    node: Node<'_>,
    source: &str,
    exported: bool,
    default_export: bool,
) {
    let Some(name) = field_text(node, "name", source) else {
        return;
    };
    let canonical = format!("{}::{name}", parsed.path);
    let (calls, receiver_bindings, shadowed) = callable_body(node, source);
    let position = node.start_position();
    let end_position = node.end_position();
    parsed.symbols.push(SymbolDraft {
        canonical,
        name,
        file: parsed.path.clone(),
        line: position.row + 1,
        end_line: end_position.row + 1,
        column: position.column + 1,
        kind: SymbolKind::Function,
        exported,
        default_export,
        is_static: false,
        calls,
        receiver_bindings,
        shadowed,
    });
}

fn add_class(
    parsed: &mut ParsedFile,
    node: Node<'_>,
    source: &str,
    exported: bool,
    default_export: bool,
) {
    let Some(name) = field_text(node, "name", source) else {
        return;
    };
    let position = node.start_position();
    let end_position = node.end_position();
    parsed.symbols.push(SymbolDraft {
        canonical: format!("{}::{name}", parsed.path),
        name: name.clone(),
        file: parsed.path.clone(),
        line: position.row + 1,
        end_line: end_position.row + 1,
        column: position.column + 1,
        kind: SymbolKind::Class,
        exported,
        default_export,
        is_static: false,
        calls: Vec::new(),
        receiver_bindings: Vec::new(),
        shadowed: BTreeSet::new(),
    });

    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        if child.kind() == "method_definition" {
            add_method(parsed, &name, child, source);
        }
    }
}

fn add_method(parsed: &mut ParsedFile, class: &str, node: Node<'_>, source: &str) {
    let Some(name) = field_text(node, "name", source) else {
        return;
    };
    let (calls, receiver_bindings, shadowed) = callable_body(node, source);
    let position = node.start_position();
    let end_position = node.end_position();
    parsed.symbols.push(SymbolDraft {
        canonical: format!("{}::{class}.{name}", parsed.path),
        name,
        file: parsed.path.clone(),
        line: position.row + 1,
        end_line: end_position.row + 1,
        column: position.column + 1,
        kind: SymbolKind::Method,
        exported: false,
        default_export: false,
        is_static: is_static_method(node, source),
        calls,
        receiver_bindings,
        shadowed,
    });
}

fn callable_body(
    node: Node<'_>,
    source: &str,
) -> (Vec<CallDraft>, Vec<ReceiverBindingDraft>, BTreeSet<String>) {
    let mut shadowed = BTreeSet::new();
    if let Some(parameters) = node.child_by_field_name("parameters") {
        binding_names(parameters, source, &mut shadowed);
    }
    let Some(body) = node.child_by_field_name("body") else {
        return (Vec::new(), Vec::new(), shadowed);
    };
    let mut calls = Vec::new();
    let mut receiver_bindings = Vec::new();
    let mut reassigned = BTreeSet::new();
    walk_body(
        body,
        source,
        &mut calls,
        &mut receiver_bindings,
        &mut reassigned,
        &mut shadowed,
    );
    for binding in &mut receiver_bindings {
        binding.reassigned = reassigned.contains(&binding.name);
    }
    (calls, receiver_bindings, shadowed)
}

fn walk_body(
    node: Node<'_>,
    source: &str,
    calls: &mut Vec<CallDraft>,
    receiver_bindings: &mut Vec<ReceiverBindingDraft>,
    reassigned: &mut BTreeSet<String>,
    shadowed: &mut BTreeSet<String>,
) {
    match node.kind() {
        "function_declaration" | "class_declaration" => {
            if let Some(name) = field_text(node, "name", source) {
                shadowed.insert(name);
            }
            return;
        }
        "function" | "function_expression" | "arrow_function" | "method_definition" => return,
        "variable_declarator" => {
            if let Some(name) = node.child_by_field_name("name") {
                binding_names(name, source, shadowed);
            }
            if let Some(binding) = receiver_binding(node, source) {
                receiver_bindings.push(binding);
            }
        }
        "call_expression" => calls.push(call_draft(node, source)),
        "assignment_expression" => assignment_name(node, source, reassigned),
        "update_expression" => update_name(node, source, reassigned),
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk_body(
            child,
            source,
            calls,
            receiver_bindings,
            reassigned,
            shadowed,
        );
    }
}

fn call_draft(node: Node<'_>, source: &str) -> CallDraft {
    let function = node.child_by_field_name("function");
    let (name, receiver, direct, this_member) = match function {
        Some(function) if function.kind() == "identifier" => {
            (text(function, source).to_string(), None, true, false)
        }
        Some(function) if function.kind() == "member_expression" => {
            let this_member = function
                .child_by_field_name("object")
                .is_some_and(|object| object.kind() == "this")
                && function
                    .child_by_field_name("property")
                    .is_some_and(|property| {
                        matches!(property.kind(), "identifier" | "property_identifier")
                    });
            let receiver = function
                .child_by_field_name("property")
                .filter(|property| matches!(property.kind(), "identifier" | "property_identifier"))
                .and_then(|_| function.child_by_field_name("object"))
                .filter(|object| object.kind() == "identifier")
                .map(|object| text(object, source).to_string());
            (
                field_text(function, "property", source)
                    .unwrap_or_else(|| text(function, source).to_string()),
                receiver,
                false,
                this_member,
            )
        }
        Some(function) => (text(function, source).to_string(), None, false, false),
        None => ("<unknown>".to_string(), None, false, false),
    };
    let position = node.start_position();
    CallDraft {
        name,
        receiver,
        line: position.row + 1,
        column: position.column + 1,
        direct,
        this_member,
    }
}

fn receiver_binding(node: Node<'_>, source: &str) -> Option<ReceiverBindingDraft> {
    let name = node.child_by_field_name("name")?;
    let value = node.child_by_field_name("value")?;
    if name.kind() != "identifier" || value.kind() != "new_expression" {
        return None;
    }
    let constructor = value.child_by_field_name("constructor")?;
    if constructor.kind() != "identifier" {
        return None;
    }
    let declaration = node.parent()?;
    if !text(declaration, source).trim_start().starts_with("const ") {
        return None;
    }
    let position = node.start_position();
    Some(ReceiverBindingDraft {
        name: text(name, source).to_string(),
        class: text(constructor, source).to_string(),
        line: position.row + 1,
        column: position.column + 1,
        reassigned: false,
    })
}

fn assignment_name(node: Node<'_>, source: &str, reassigned: &mut BTreeSet<String>) {
    if let Some(left) = node.child_by_field_name("left")
        && left.kind() == "identifier"
    {
        reassigned.insert(text(left, source).to_string());
    }
}

fn update_name(node: Node<'_>, source: &str, reassigned: &mut BTreeSet<String>) {
    if let Some(argument) = node.child_by_field_name("argument")
        && argument.kind() == "identifier"
    {
        reassigned.insert(text(argument, source).to_string());
    }
}

fn is_static_method(node: Node<'_>, source: &str) -> bool {
    let Some(name) = node.child_by_field_name("name") else {
        return false;
    };
    let prefix = &source[node.start_byte()..name.start_byte()];
    prefix.split_whitespace().any(|word| word == "static")
}

fn binding_names(node: Node<'_>, source: &str, names: &mut BTreeSet<String>) {
    if node.kind() == "identifier" {
        names.insert(text(node, source).to_string());
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        binding_names(child, source, names);
    }
}

fn import_draft(node: Node<'_>, source: &str) -> ImportDraft {
    let position = node.start_position();
    let module = node
        .child_by_field_name("source")
        .map(|source_node| string_text(source_node, source))
        .unwrap_or_else(|| "<unknown>".to_string());
    let import_text = text(node, source).trim_start();
    let type_only = import_text.starts_with("import type ");
    let mut bindings = Vec::new();
    let mut unsupported = None;

    if let Some(clause) = named_child(node, "import_clause") {
        let mut cursor = clause.walk();
        for child in clause.named_children(&mut cursor) {
            match child.kind() {
                "identifier" => bindings.push(ImportBinding::Default {
                    local: text(child, source).to_string(),
                }),
                "named_imports" => named_imports(child, source, &mut bindings),
                "namespace_import" => {
                    unsupported =
                        Some("namespace imports are outside the Mission 1 subset".to_string())
                }
                _ => {}
            }
        }
    }

    ImportDraft {
        module,
        line: position.row + 1,
        column: position.column + 1,
        bindings,
        type_only,
        unsupported,
    }
}

fn named_child<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind() == kind)
}

fn named_imports(node: Node<'_>, source: &str, bindings: &mut Vec<ImportBinding>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() != "import_specifier" {
            continue;
        }
        let Some(imported) = field_text(child, "name", source) else {
            continue;
        };
        let local = field_text(child, "alias", source).unwrap_or_else(|| imported.clone());
        bindings.push(ImportBinding::Named { local, imported });
    }
}

fn field_text(node: Node<'_>, field: &str, source: &str) -> Option<String> {
    node.child_by_field_name(field)
        .map(|child| text(child, source).to_string())
}

fn string_text(node: Node<'_>, source: &str) -> String {
    let raw = text(node, source);
    raw.strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
        .or_else(|| {
            raw.strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
        })
        .unwrap_or(raw)
        .to_string()
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}
pub(crate) fn resolve(
    parsed: &BTreeMap<String, ProviderParsedFile>,
    paths: &BTreeSet<String>,
    _: &ProviderContext,
) -> BTreeMap<String, ResolvedFile> {
    let parsed: BTreeMap<String, &ParsedFile> = parsed
        .iter()
        .filter_map(|(path, file)| match file {
            ProviderParsedFile::JsTs(file) => Some((path.clone(), file)),
            ProviderParsedFile::Python(_)
            | ProviderParsedFile::Rust(_)
            | ProviderParsedFile::Go(_) => None,
        })
        .collect();
    let context = ResolveContext::new(&parsed);
    parsed
        .values()
        .filter(|file| paths.contains(&file.path))
        .map(|file| (file.path.clone(), resolve_file(file, &context)))
        .collect()
}

struct ResolveContext {
    files: BTreeSet<String>,
    canonical_symbols: BTreeMap<String, Vec<String>>,
    local_functions: BTreeMap<(String, String), Vec<String>>,
    local_classes: BTreeMap<(String, String), Vec<String>>,
    exported_classes: BTreeMap<(String, String, bool), Vec<String>>,
    this_methods: BTreeMap<(String, String, String, bool), Vec<String>>,
    exports: BTreeMap<(String, String, bool), Vec<String>>,
    ambiguous_exports: BTreeSet<(String, String, bool)>,
}

impl ResolveContext {
    fn new(parsed: &BTreeMap<String, &ParsedFile>) -> Self {
        let mut context = Self {
            files: parsed.keys().cloned().collect(),
            canonical_symbols: BTreeMap::new(),
            local_functions: BTreeMap::new(),
            local_classes: BTreeMap::new(),
            exported_classes: BTreeMap::new(),
            this_methods: BTreeMap::new(),
            exports: BTreeMap::new(),
            ambiguous_exports: BTreeSet::new(),
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
                if symbol.kind == SymbolKind::Class {
                    context
                        .local_classes
                        .entry((file.path.clone(), symbol.name.clone()))
                        .or_default()
                        .push(symbol.canonical.clone());
                    if symbol.exported {
                        context
                            .exported_classes
                            .entry((file.path.clone(), symbol.name.clone(), false))
                            .or_default()
                            .push(symbol.canonical.clone());
                    }
                    if symbol.default_export {
                        context
                            .exported_classes
                            .entry((file.path.clone(), String::new(), true))
                            .or_default()
                            .push(symbol.canonical.clone());
                    }
                }
                if symbol.kind == SymbolKind::Method
                    && let Some((class, _)) = method_owner(&symbol.canonical)
                {
                    context
                        .this_methods
                        .entry((
                            file.path.clone(),
                            class.to_string(),
                            symbol.name.clone(),
                            symbol.is_static,
                        ))
                        .or_default()
                        .push(symbol.canonical.clone());
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
        let direct_exports = context.exports.clone();
        for file in parsed.values() {
            for local_export in &file.local_exports {
                resolve_local_export(file, local_export, &direct_exports, &mut context);
            }
        }
        for file in parsed.values() {
            for reexport in &file.reexports {
                if reexport.type_only {
                    continue;
                }
                let source_candidates =
                    module_candidates(&file.path, &reexport.module, &context.files);
                if source_candidates.len() > 1 {
                    for binding in &reexport.bindings {
                        context.ambiguous_exports.insert((
                            file.path.clone(),
                            binding.exported.clone(),
                            false,
                        ));
                    }
                    continue;
                }
                let [source] = source_candidates.as_slice() else {
                    continue;
                };
                for binding in &reexport.bindings {
                    if let Some(symbols) =
                        direct_exports.get(&(source.clone(), binding.local.clone(), false))
                    {
                        context
                            .exports
                            .entry((file.path.clone(), binding.exported.clone(), false))
                            .or_default()
                            .extend(symbols.clone());
                    }
                }
            }
        }
        context
    }
}

fn resolve_local_export(
    file: &ParsedFile,
    local_export: &LocalExportDraft,
    direct_exports: &BTreeMap<(String, String, bool), Vec<String>>,
    context: &mut ResolveContext,
) {
    if local_export.type_only {
        return;
    }
    let mut candidates = context
        .local_functions
        .get(&(file.path.clone(), local_export.local.clone()))
        .cloned()
        .unwrap_or_default();
    for import in &file.imports {
        if import.type_only {
            continue;
        }
        for binding in &import.bindings {
            if binding.local() != local_export.local {
                continue;
            }
            let source_candidates = module_candidates(&file.path, &import.module, &context.files);
            if source_candidates.len() > 1 {
                context.ambiguous_exports.insert((
                    file.path.clone(),
                    local_export.exported.clone(),
                    false,
                ));
                continue;
            }
            let [source] = source_candidates.as_slice() else {
                continue;
            };
            let source_key = match binding {
                ImportBinding::Named { imported, .. } => (source.clone(), imported.clone(), false),
                ImportBinding::Default { .. } => (source.clone(), String::new(), true),
            };
            if context.ambiguous_exports.contains(&source_key) {
                context.ambiguous_exports.insert((
                    file.path.clone(),
                    local_export.exported.clone(),
                    false,
                ));
            } else if let Some(symbols) = direct_exports.get(&source_key) {
                candidates.extend(symbols.clone());
            }
        }
    }
    match candidates.as_slice() {
        [symbol] => context
            .exports
            .entry((file.path.clone(), local_export.exported.clone(), false))
            .or_default()
            .push(symbol.clone()),
        [] => {}
        _ => {
            context.ambiguous_exports.insert((
                file.path.clone(),
                local_export.exported.clone(),
                false,
            ));
        }
    }
}

#[derive(Clone)]
enum BindingTarget {
    Resolved(String),
    Unresolved,
    Ambiguous,
}

fn resolve_file(file: &ParsedFile, context: &ResolveContext) -> ResolvedFile {
    let mut result = ResolvedFile::default();
    let mut bindings = BTreeMap::new();
    for import in &file.imports {
        resolve_import(import, file, context, &mut bindings, &mut result);
    }
    for reexport in &file.reexports {
        resolve_reexport_dependency(reexport, file, context, &mut result);
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
            if call.this_member {
                resolve_this_member_call(draft, file, call, context, &mut result);
                continue;
            }
            if let Some(receiver) = &call.receiver
                && draft.receiver_bindings.iter().any(|binding| {
                    binding.name == *receiver
                        && (binding.line, binding.column) <= (call.line, call.column)
                })
            {
                resolve_constructor_member_call(draft, file, call, context, &mut result);
                continue;
            }
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

fn resolve_constructor_member_call(
    draft: &SymbolDraft,
    file: &ParsedFile,
    call: &CallDraft,
    context: &ResolveContext,
    result: &mut ResolvedFile,
) {
    let receiver = call.receiver.as_ref().unwrap();
    let bindings = draft
        .receiver_bindings
        .iter()
        .filter(|binding| {
            binding.name == *receiver && (binding.line, binding.column) <= (call.line, call.column)
        })
        .collect::<Vec<_>>();
    let [binding] = bindings.as_slice() else {
        result.issues.push(issue(
            RelationshipStatus::Unresolved,
            &draft.canonical,
            call.line,
            call.column,
            &call.name,
            "receiver ownership is not uniquely proven by a direct immutable constructor binding",
        ));
        return;
    };
    if binding.reassigned {
        result.issues.push(issue(
            RelationshipStatus::Unresolved,
            &draft.canonical,
            call.line,
            call.column,
            &call.name,
            "receiver binding was reassigned in this callable scope",
        ));
        return;
    }
    if draft.shadowed.contains(&binding.class) {
        result.issues.push(issue(
            RelationshipStatus::Unresolved,
            &draft.canonical,
            call.line,
            call.column,
            &call.name,
            "constructor class name could be shadowed in this callable scope",
        ));
        return;
    }
    let (classes, ambiguous) = constructor_class_candidates(file, &binding.class, context);
    if ambiguous || classes.len() > 1 {
        result.issues.push(issue(
            RelationshipStatus::Ambiguous,
            &draft.canonical,
            call.line,
            call.column,
            &call.name,
            "constructor class does not resolve uniquely",
        ));
        return;
    }
    let [class] = classes.as_slice() else {
        result.issues.push(issue(
            RelationshipStatus::Unresolved,
            &draft.canonical,
            call.line,
            call.column,
            &call.name,
            "constructor class is not a uniquely indexed local or relative imported Class",
        ));
        return;
    };
    let Some((class_file, class_name)) = class.rsplit_once("::") else {
        return;
    };
    let methods = context
        .this_methods
        .get(&(
            class_file.to_string(),
            class_name.to_string(),
            call.name.clone(),
            false,
        ))
        .cloned()
        .unwrap_or_default();
    match methods.as_slice() {
        [target] => result.calls.push(ResolvedCall {
            from: draft.canonical.clone(),
            to: target.clone(),
            line: call.line,
            column: call.column,
        }),
        [] => result.issues.push(issue(
            RelationshipStatus::Unresolved,
            &draft.canonical,
            call.line,
            call.column,
            &call.name,
            "no matching non-static method exists on the constructor Class",
        )),
        _ => result.issues.push(issue(
            RelationshipStatus::Ambiguous,
            &draft.canonical,
            call.line,
            call.column,
            &call.name,
            "multiple non-static methods exist on the constructor Class",
        )),
    }
}

fn constructor_class_candidates(
    file: &ParsedFile,
    class: &str,
    context: &ResolveContext,
) -> (Vec<String>, bool) {
    let mut candidates = context
        .local_classes
        .get(&(file.path.clone(), class.to_string()))
        .cloned()
        .unwrap_or_default();
    let mut ambiguous = false;
    for import in &file.imports {
        if import.type_only {
            continue;
        }
        let source_candidates = module_candidates(&file.path, &import.module, &context.files);
        for binding in &import.bindings {
            if binding.local() != class {
                continue;
            }
            if source_candidates.len() > 1 {
                ambiguous = true;
                continue;
            }
            let [source] = source_candidates.as_slice() else {
                continue;
            };
            let key = match binding {
                ImportBinding::Named { imported, .. } => (source.clone(), imported.clone(), false),
                ImportBinding::Default { .. } => (source.clone(), String::new(), true),
            };
            candidates.extend(
                context
                    .exported_classes
                    .get(&key)
                    .cloned()
                    .unwrap_or_default(),
            );
        }
    }
    (candidates, ambiguous)
}

fn resolve_reexport_dependency(
    reexport: &ReexportDraft,
    file: &ParsedFile,
    context: &ResolveContext,
    result: &mut ResolvedFile,
) {
    if reexport.type_only {
        return;
    }
    if let [source] = module_candidates(&file.path, &reexport.module, &context.files).as_slice() {
        result.imports.push(source.clone());
    }
}

fn resolve_this_member_call(
    draft: &SymbolDraft,
    file: &ParsedFile,
    call: &CallDraft,
    context: &ResolveContext,
    result: &mut ResolvedFile,
) {
    let Some((class, _)) = method_owner(&draft.canonical) else {
        result.issues.push(issue(
            RelationshipStatus::Unresolved,
            &draft.canonical,
            call.line,
            call.column,
            &call.name,
            "this member call is outside an indexed direct class method",
        ));
        return;
    };
    let candidates = context
        .this_methods
        .get(&(
            file.path.clone(),
            class.to_string(),
            call.name.clone(),
            draft.is_static,
        ))
        .cloned()
        .unwrap_or_default();
    match candidates.as_slice() {
        [target] => result.calls.push(ResolvedCall {
            from: draft.canonical.clone(),
            to: target.clone(),
            line: call.line,
            column: call.column,
        }),
        [] => result.issues.push(issue(
            RelationshipStatus::Unresolved,
            &draft.canonical,
            call.line,
            call.column,
            &call.name,
            "no matching same-class method",
        )),
        _ => result.issues.push(issue(
            RelationshipStatus::Ambiguous,
            &draft.canonical,
            call.line,
            call.column,
            &call.name,
            "multiple same-class methods match this member call",
        )),
    }
}

fn method_owner(canonical: &str) -> Option<(&str, &str)> {
    let (_, member) = canonical.rsplit_once("::")?;
    member.rsplit_once('.')
}

fn resolve_import(
    import: &ImportDraft,
    file: &ParsedFile,
    context: &ResolveContext,
    bindings: &mut BTreeMap<(String, String), Vec<BindingTarget>>,
    result: &mut ResolvedFile,
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
            if context.ambiguous_exports.contains(&export_key) {
                BindingTarget::Ambiguous
            } else {
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
    call: &CallDraft,
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
    if supports_path(&base) {
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
