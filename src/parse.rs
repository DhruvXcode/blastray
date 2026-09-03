use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use tree_sitter::{Language, Node, Parser};

use crate::index::SymbolKind;

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct ParsedFile {
    pub path: String,
    pub symbols: Vec<SymbolDraft>,
    pub imports: Vec<ImportDraft>,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct SymbolDraft {
    pub canonical: String,
    pub name: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub kind: SymbolKind,
    pub exported: bool,
    pub default_export: bool,
    pub calls: Vec<CallDraft>,
    pub shadowed: BTreeSet<String>,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct CallDraft {
    pub name: String,
    pub line: usize,
    pub column: usize,
    pub direct: bool,
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
    };
    let mut cursor = tree.root_node().walk();

    for child in tree.root_node().named_children(&mut cursor) {
        match child.kind() {
            "import_statement" => parsed.imports.push(import_draft(child, source)),
            "function_declaration" => add_function(&mut parsed, child, source, false, false),
            "class_declaration" => add_class(&mut parsed, child, source, false, false),
            "export_statement" => add_export(&mut parsed, child, source),
            _ => {}
        }
    }
    Ok(parsed)
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
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "function_declaration" => add_function(parsed, child, source, true, default_export),
            "class_declaration" => add_class(parsed, child, source, true, default_export),
            _ => {}
        }
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
    let (calls, shadowed) = callable_body(node, source);
    let position = node.start_position();
    parsed.symbols.push(SymbolDraft {
        canonical,
        name,
        file: parsed.path.clone(),
        line: position.row + 1,
        column: position.column + 1,
        kind: SymbolKind::Function,
        exported,
        default_export,
        calls,
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
    parsed.symbols.push(SymbolDraft {
        canonical: format!("{}::{name}", parsed.path),
        name: name.clone(),
        file: parsed.path.clone(),
        line: position.row + 1,
        column: position.column + 1,
        kind: SymbolKind::Class,
        exported,
        default_export,
        calls: Vec::new(),
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
    let (calls, shadowed) = callable_body(node, source);
    let position = node.start_position();
    parsed.symbols.push(SymbolDraft {
        canonical: format!("{}::{class}.{name}", parsed.path),
        name,
        file: parsed.path.clone(),
        line: position.row + 1,
        column: position.column + 1,
        kind: SymbolKind::Method,
        exported: false,
        default_export: false,
        calls,
        shadowed,
    });
}

fn callable_body(node: Node<'_>, source: &str) -> (Vec<CallDraft>, BTreeSet<String>) {
    let mut shadowed = BTreeSet::new();
    if let Some(parameters) = node.child_by_field_name("parameters") {
        binding_names(parameters, source, &mut shadowed);
    }
    let Some(body) = node.child_by_field_name("body") else {
        return (Vec::new(), shadowed);
    };
    let mut calls = Vec::new();
    walk_body(body, source, &mut calls, &mut shadowed);
    (calls, shadowed)
}

fn walk_body(
    node: Node<'_>,
    source: &str,
    calls: &mut Vec<CallDraft>,
    shadowed: &mut BTreeSet<String>,
) {
    match node.kind() {
        "function_declaration" | "class_declaration" => {
            if let Some(name) = field_text(node, "name", source) {
                shadowed.insert(name);
            }
            return;
        }
        "function" | "arrow_function" | "method_definition" => return,
        "variable_declarator" => {
            if let Some(name) = node.child_by_field_name("name") {
                binding_names(name, source, shadowed);
            }
        }
        "call_expression" => calls.push(call_draft(node, source)),
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk_body(child, source, calls, shadowed);
    }
}

fn call_draft(node: Node<'_>, source: &str) -> CallDraft {
    let function = node.child_by_field_name("function");
    let (name, direct) = match function {
        Some(function) if function.kind() == "identifier" => {
            (text(function, source).to_string(), true)
        }
        Some(function) if function.kind() == "member_expression" => (
            field_text(function, "property", source)
                .unwrap_or_else(|| text(function, source).to_string()),
            false,
        ),
        Some(function) => (text(function, source).to_string(), false),
        None => ("<unknown>".to_string(), false),
    };
    let position = node.start_position();
    CallDraft {
        name,
        line: position.row + 1,
        column: position.column + 1,
        direct,
    }
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
