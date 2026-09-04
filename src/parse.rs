use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use tree_sitter::{Language, Node, Parser};

use crate::index::SymbolKind;

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
