use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use tree_sitter::{Node, Parser};

use crate::index::{RelationshipIssue, RelationshipStatus, SymbolKind};
use crate::language::{
    ParsedFile as ProviderParsedFile, ProviderContext, ResolvedCall, ResolvedFile,
};

pub(crate) const EXTENSIONS: &[&str] = &["java"];

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct ParsedFile {
    pub path: String,
    pub symbols: Vec<SymbolDraft>,
    package: String,
    imports: Vec<ImportDraft>,
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
    calls: Vec<CallDraft>,
    shadowed: BTreeSet<String>,
    owner: Option<String>,
    is_static: bool,
}

#[derive(Clone, Deserialize, Serialize)]
struct ImportDraft {
    path: String,
    is_static: bool,
    line: usize,
    column: usize,
}

#[derive(Clone, Deserialize, Serialize)]
struct CallDraft {
    kind: CallKind,
    name: String,
    receiver: Option<String>,
    line: usize,
    column: usize,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
enum CallKind {
    Direct,
    This,
    Class,
    Other,
}

#[derive(Clone)]
struct TypeTarget {
    canonical: String,
    file: String,
    is_class: bool,
}

pub(crate) fn supports_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| EXTENSIONS.contains(&extension))
}

pub(crate) fn parse(path: &str, source: &str) -> Result<ProviderParsedFile, String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_java::LANGUAGE.into())
        .map_err(|error| format!("cannot configure parser for {path}: {error}"))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| format!("cannot parse {path}"))?;
    let root = tree.root_node();
    let package = root
        .named_children(&mut root.walk())
        .find(|node| node.kind() == "package_declaration")
        .map(|node| package_name(node, source))
        .unwrap_or_default();
    let mut parsed = ParsedFile {
        path: path.to_owned(),
        symbols: Vec::new(),
        package,
        imports: Vec::new(),
    };
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        match child.kind() {
            "class_declaration" => add_type(&mut parsed, child, source, SymbolKind::Class),
            "interface_declaration" | "enum_declaration" => {
                // Interfaces and enums are type declarations, but not classes.
                add_type(&mut parsed, child, source, SymbolKind::Type);
            }
            "import_declaration" => {
                if let Some(import) = import_draft(child, source) {
                    parsed.imports.push(import);
                }
            }
            _ => {}
        }
    }
    Ok(ProviderParsedFile::Java(parsed))
}

fn package_name(node: Node<'_>, source: &str) -> String {
    text(node, source)
        .trim()
        .strip_prefix("package")
        .unwrap_or_default()
        .trim()
        .trim_end_matches(';')
        .trim()
        .to_owned()
}

fn import_draft(node: Node<'_>, source: &str) -> Option<ImportDraft> {
    let value = text(node, source).trim().trim_end_matches(';').trim();
    let value = value.strip_prefix("import")?.trim_start();
    let (is_static, value) = match value.strip_prefix("static") {
        Some(rest) if rest.starts_with(char::is_whitespace) => (true, rest.trim_start()),
        _ => (false, value),
    };
    (!value.is_empty() && !value.ends_with(".*")).then(|| ImportDraft {
        path: value.to_owned(),
        is_static,
        line: node.start_position().row + 1,
        column: node.start_position().column + 1,
    })
}

fn add_type(parsed: &mut ParsedFile, node: Node<'_>, source: &str, kind: SymbolKind) {
    let Some(name) = field_text(node, "name", source) else {
        return;
    };
    let position = node.start_position();
    let canonical = format!("{}::{name}", parsed.path);
    parsed.symbols.push(SymbolDraft {
        canonical: canonical.clone(),
        name,
        file: parsed.path.clone(),
        line: position.row + 1,
        end_line: node.end_position().row + 1,
        column: position.column + 1,
        kind,
        calls: Vec::new(),
        shadowed: BTreeSet::new(),
        owner: None,
        is_static: false,
    });
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    add_direct_methods(parsed, body, source, &canonical);
}

fn add_direct_methods(parsed: &mut ParsedFile, body: Node<'_>, source: &str, owner: &str) {
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        if child.kind() == "method_declaration" {
            add_method(parsed, child, source, owner);
        } else if body.kind() == "enum_body" && child.kind() == "enum_body_declarations" {
            let mut declarations = child.walk();
            for declaration in child.named_children(&mut declarations) {
                if declaration.kind() == "method_declaration" {
                    add_method(parsed, declaration, source, owner);
                }
            }
        }
    }
}

fn add_method(parsed: &mut ParsedFile, node: Node<'_>, source: &str, owner: &str) {
    let Some(name) = field_text(node, "name", source) else {
        return;
    };
    let mut shadowed = BTreeSet::new();
    if let Some(parameters) = node.child_by_field_name("parameters") {
        binding_names(parameters, source, &mut shadowed);
    }
    let mut calls = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        collect_body_facts(body, source, &mut shadowed, &mut calls);
    }
    let position = node.start_position();
    parsed.symbols.push(SymbolDraft {
        canonical: format!("{owner}.{name}"),
        name,
        file: parsed.path.clone(),
        line: position.row + 1,
        end_line: node.end_position().row + 1,
        column: position.column + 1,
        kind: SymbolKind::Method,
        calls,
        shadowed,
        owner: Some(owner.to_owned()),
        is_static: has_static_modifier(node, source),
    });
}

fn has_static_modifier(node: Node<'_>, source: &str) -> bool {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind() == "modifiers")
        .is_some_and(|modifiers| {
            text(modifiers, source)
                .split_whitespace()
                .any(|part| part == "static")
        })
}

fn collect_body_facts(
    node: Node<'_>,
    source: &str,
    shadowed: &mut BTreeSet<String>,
    calls: &mut Vec<CallDraft>,
) {
    match node.kind() {
        // Calls nested in a lambda, local type, or anonymous class do not belong to
        // the containing method. Those declarations are intentionally not symbols.
        "lambda_expression"
        | "class_declaration"
        | "interface_declaration"
        | "enum_declaration"
        | "record_declaration"
        | "annotation_type_declaration"
        | "method_declaration"
        | "constructor_declaration"
        | "compact_constructor_declaration" => return,
        "class_body"
            if node
                .parent()
                .is_some_and(|parent| parent.kind() == "object_creation_expression") =>
        {
            return;
        }
        "local_variable_declaration" => binding_names(node, source, shadowed),
        "enhanced_for_statement" | "catch_formal_parameter" => {
            simple_binding_names(node, source, shadowed)
        }
        "method_invocation" => {
            calls.push(call_draft(node, source));
            return;
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_body_facts(child, source, shadowed, calls);
    }
}

fn binding_names(node: Node<'_>, source: &str, names: &mut BTreeSet<String>) {
    if node.kind() == "variable_declarator" {
        if let Some(name) = field_text(node, "name", source) {
            names.insert(name);
        }
        return;
    }
    if matches!(
        node.kind(),
        "formal_parameter" | "spread_parameter" | "receiver_parameter"
    ) {
        if let Some(name) = field_text(node, "name", source) {
            names.insert(name);
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        binding_names(child, source, names);
    }
}

fn simple_binding_names(node: Node<'_>, source: &str, names: &mut BTreeSet<String>) {
    if let Some(name) = field_text(node, "name", source) {
        names.insert(name);
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "identifier" {
            names.insert(text(child, source).to_owned());
        }
    }
}

fn call_draft(node: Node<'_>, source: &str) -> CallDraft {
    let position = node.start_position();
    let name = field_text(node, "name", source).unwrap_or_else(|| "<unknown>".to_string());
    let (kind, receiver) = match node.child_by_field_name("object") {
        None => (CallKind::Direct, None),
        Some(object) if object.kind() == "this" => (CallKind::This, None),
        Some(object) if object.kind() == "identifier" => {
            (CallKind::Class, Some(text(object, source).to_owned()))
        }
        _ => (CallKind::Other, None),
    };
    CallDraft {
        kind,
        name,
        receiver,
        line: position.row + 1,
        column: position.column + 1,
    }
}

pub(crate) fn resolve(
    parsed: &BTreeMap<String, ProviderParsedFile>,
    paths: &BTreeSet<String>,
    _: &ProviderContext,
) -> BTreeMap<String, ResolvedFile> {
    let files: Vec<&ParsedFile> = parsed
        .values()
        .filter_map(|file| match file {
            ProviderParsedFile::Java(file) => Some(file),
            ProviderParsedFile::JsTs(_)
            | ProviderParsedFile::Python(_)
            | ProviderParsedFile::Rust(_)
            | ProviderParsedFile::Go(_) => None,
        })
        .collect();
    let context = ResolveContext::new(&files);
    files
        .iter()
        .filter(|file| paths.contains(&file.path))
        .map(|file| (file.path.clone(), resolve_file(file, &context)))
        .collect()
}

struct ResolveContext {
    types: BTreeMap<(String, String), Vec<TypeTarget>>,
    methods: BTreeMap<(String, String), Vec<MethodTarget>>,
}

#[derive(Clone)]
struct MethodTarget {
    canonical: String,
    is_static: bool,
}

impl ResolveContext {
    fn new(files: &[&ParsedFile]) -> Self {
        let mut context = Self {
            types: BTreeMap::new(),
            methods: BTreeMap::new(),
        };
        for file in files {
            for symbol in &file.symbols {
                if matches!(symbol.kind, SymbolKind::Class | SymbolKind::Type) {
                    context
                        .types
                        .entry((file.package.clone(), symbol.name.clone()))
                        .or_default()
                        .push(TypeTarget {
                            canonical: symbol.canonical.clone(),
                            file: file.path.clone(),
                            is_class: symbol.kind == SymbolKind::Class,
                        });
                } else if symbol.kind == SymbolKind::Method
                    && let Some(owner) = &symbol.owner
                {
                    context
                        .methods
                        .entry((owner.clone(), symbol.name.clone()))
                        .or_default()
                        .push(MethodTarget {
                            canonical: symbol.canonical.clone(),
                            is_static: symbol.is_static,
                        });
                }
            }
        }
        context
    }
}

fn resolve_file(file: &ParsedFile, context: &ResolveContext) -> ResolvedFile {
    let mut imports = BTreeSet::new();
    let mut class_imports = BTreeMap::new();
    let mut static_imports = BTreeMap::new();
    let mut issues = Vec::new();
    for import in &file.imports {
        if import.is_static {
            match static_import_target(import, context) {
                Some((local, target, target_file)) => {
                    static_imports
                        .entry(local)
                        .or_insert_with(Vec::new)
                        .push(target);
                    imports.insert(target_file);
                }
                None => issues.push(issue(
                    file,
                    import.line,
                    import.column,
                    &import.path,
                    "unresolved Java static import",
                )),
            }
        } else {
            match type_import_target(import, context) {
                Some((local, target)) => {
                    imports.insert(target.file.clone());
                    class_imports
                        .entry(local)
                        .or_insert_with(Vec::new)
                        .push(target);
                }
                None => issues.push(issue(
                    file,
                    import.line,
                    import.column,
                    &import.path,
                    "unresolved Java import",
                )),
            }
        }
    }
    let mut calls = Vec::new();
    for symbol in &file.symbols {
        if symbol.kind != SymbolKind::Method {
            continue;
        }
        for call in &symbol.calls {
            match call.kind {
                CallKind::Direct | CallKind::This if !symbol.shadowed.contains(&call.name) => {
                    let local = symbol
                        .owner
                        .as_ref()
                        .and_then(|owner| context.methods.get(&(owner.clone(), call.name.clone())))
                        .cloned();
                    if local.is_some() {
                        add_unique(&mut calls, &mut issues, file, symbol, call, local);
                    } else {
                        add_unique(
                            &mut calls,
                            &mut issues,
                            file,
                            symbol,
                            call,
                            static_imports.get(&call.name).cloned(),
                        );
                    }
                }
                CallKind::Class => {
                    let Some(receiver) = call.receiver.as_ref() else {
                        continue;
                    };
                    if symbol.shadowed.contains(receiver) {
                        issues.push(issue(
                            file,
                            call.line,
                            call.column,
                            receiver,
                            "unresolved Java receiver call",
                        ));
                        continue;
                    }
                    let mut owners = context
                        .types
                        .get(&(file.package.clone(), receiver.clone()))
                        .cloned()
                        .unwrap_or_default();
                    owners.extend(class_imports.get(receiver).cloned().unwrap_or_default());
                    owners.sort_by(|left, right| left.canonical.cmp(&right.canonical));
                    owners.dedup_by(|left, right| left.canonical == right.canonical);
                    let candidates = if let [owner] = owners.as_slice() {
                        if !owner.is_class {
                            None
                        } else {
                            context
                                .methods
                                .get(&(owner.canonical.clone(), call.name.clone()))
                                .map(|methods| {
                                    methods
                                        .iter()
                                        .filter(|method| method.is_static)
                                        .cloned()
                                        .collect()
                                })
                        }
                    } else {
                        None
                    };
                    add_unique(&mut calls, &mut issues, file, symbol, call, candidates);
                }
                CallKind::Direct | CallKind::This | CallKind::Other => issues.push(issue(
                    file,
                    call.line,
                    call.column,
                    &call.name,
                    "unresolved Java call",
                )),
            }
        }
    }
    ResolvedFile {
        imports: imports.into_iter().collect(),
        dependencies: Vec::new(),
        calls,
        issues,
    }
}

fn type_import_target(
    import: &ImportDraft,
    context: &ResolveContext,
) -> Option<(String, TypeTarget)> {
    let (package, name) = import.path.rsplit_once('.')?;
    let candidates = context.types.get(&(package.to_owned(), name.to_owned()))?;
    match candidates.as_slice() {
        [target] => Some((name.to_owned(), target.clone())),
        _ => None,
    }
}

fn static_import_target(
    import: &ImportDraft,
    context: &ResolveContext,
) -> Option<(String, MethodTarget, String)> {
    let (class_path, method) = import.path.rsplit_once('.')?;
    let (package, class) = class_path.rsplit_once('.')?;
    let classes = context.types.get(&(package.to_owned(), class.to_owned()))?;
    let [class] = classes.as_slice() else {
        return None;
    };
    if !class.is_class {
        return None;
    }
    let methods = context
        .methods
        .get(&(class.canonical.clone(), method.to_owned()))?;
    let candidates: Vec<_> = methods
        .iter()
        .filter(|target| target.is_static)
        .cloned()
        .collect();
    match candidates.as_slice() {
        [target] => Some((method.to_owned(), target.clone(), class.file.clone())),
        _ => None,
    }
}

fn add_unique(
    calls: &mut Vec<ResolvedCall>,
    issues: &mut Vec<RelationshipIssue>,
    file: &ParsedFile,
    symbol: &SymbolDraft,
    call: &CallDraft,
    candidates: Option<Vec<MethodTarget>>,
) {
    match candidates {
        Some(candidates) if candidates.len() == 1 => calls.push(ResolvedCall {
            from: symbol.canonical.clone(),
            to: candidates[0].canonical.clone(),
            line: call.line,
            column: call.column,
        }),
        Some(candidates) if candidates.len() > 1 => issues.push(RelationshipIssue {
            status: RelationshipStatus::Ambiguous,
            source: file.path.clone(),
            line: call.line,
            column: call.column,
            name: call.name.clone(),
            detail: "ambiguous Java method target".to_string(),
        }),
        _ => issues.push(issue(
            file,
            call.line,
            call.column,
            &call.name,
            "unresolved Java method target",
        )),
    }
}

fn issue(
    file: &ParsedFile,
    line: usize,
    column: usize,
    name: &str,
    detail: &str,
) -> RelationshipIssue {
    RelationshipIssue {
        status: RelationshipStatus::Unresolved,
        source: file.path.clone(),
        line,
        column,
        name: name.to_owned(),
        detail: detail.to_owned(),
    }
}

fn field_text(node: Node<'_>, field: &str, source: &str) -> Option<String> {
    node.child_by_field_name(field)
        .map(|node| text(node, source).to_owned())
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;

    fn resolved(files: &[(&str, &str)]) -> BTreeMap<String, ResolvedFile> {
        let parsed = files
            .iter()
            .map(|(path, source)| (path.to_string(), parse(path, source).unwrap()))
            .collect();
        resolve(
            &parsed,
            &parsed.keys().cloned().collect::<BTreeSet<_>>(),
            &ProviderContext::default(),
        )
    }

    #[test]
    fn indexes_only_top_level_types_and_direct_methods() {
        let parsed = parse(
            "src/Example.java",
            "package demo;\nclass Example {\n  void top() {}\n  class Nested { void hidden() {} }\n}\ninterface Face { void run(); }\nenum State { ON; void tick() {} }\n",
        )
        .unwrap();
        let ProviderParsedFile::Java(parsed) = parsed else {
            panic!("expected Java parse artifact");
        };
        let facts: Vec<_> = parsed
            .symbols
            .iter()
            .map(|symbol| (symbol.canonical.as_str(), symbol.kind))
            .collect();
        assert_eq!(
            facts,
            vec![
                ("src/Example.java::Example", SymbolKind::Class),
                ("src/Example.java::Example.top", SymbolKind::Method),
                ("src/Example.java::Face", SymbolKind::Type),
                ("src/Example.java::Face.run", SymbolKind::Method),
                ("src/Example.java::State", SymbolKind::Type),
                ("src/Example.java::State.tick", SymbolKind::Method),
            ]
        );
    }

    #[test]
    fn resolves_only_proven_same_class_and_local_import_calls() {
        let facts = resolved(&[
            (
                "src/demo/Util.java",
                "package demo; public class Util { public static void ping() {} }",
            ),
            (
                "src/demo/Worker.java",
                "package demo; import demo.Util; class Worker { void leaf() {} void entry() { leaf(); this.leaf(); Util.ping(); } }",
            ),
            (
                "src/other/Use.java",
                "package other; import demo.Util; class Use { void entry() { Util.ping(); } }",
            ),
        ]);
        let worker = &facts["src/demo/Worker.java"].calls;
        assert_eq!(worker.len(), 3);
        assert!(
            worker
                .iter()
                .any(|call| call.to == "src/demo/Worker.java::Worker.leaf")
        );
        assert!(
            worker
                .iter()
                .any(|call| call.to == "src/demo/Util.java::Util.ping")
        );
        assert_eq!(facts["src/other/Use.java"].calls.len(), 1);
        assert!(
            facts["src/other/Use.java"]
                .imports
                .contains(&"src/demo/Util.java".to_string())
        );
    }

    #[test]
    fn local_bindings_and_overloads_remain_unresolved() {
        let facts = resolved(&[(
            "src/demo/Worker.java",
            "package demo; class Util { static void ping() {} } class Worker { void leaf() {} void leaf(int n) {} void entry(Util Util, Runnable leaf) { leaf.run(); leaf(); Util.ping(); } }",
        )]);
        assert!(facts["src/demo/Worker.java"].calls.is_empty());
        assert!(facts["src/demo/Worker.java"].issues.len() >= 3);
    }

    #[test]
    fn anonymous_and_lambda_bodies_do_not_leak_calls_to_the_enclosing_method() {
        let facts = resolved(&[(
            "src/demo/Worker.java",
            "package demo; class Worker { void leaf() {} void entry() { Runnable lambda = () -> leaf(); Runnable anonymous = new Runnable() { public void run() { leaf(); } }; } }",
        )]);
        assert!(facts["src/demo/Worker.java"].calls.is_empty());
    }

    #[test]
    fn enhanced_for_bindings_do_not_become_class_receivers() {
        let facts = resolved(&[(
            "src/demo/Worker.java",
            "package demo; class Util { static void ping() {} } class Worker { void entry(Iterable<Util> utilities) { for (Util Util : utilities) { Util.ping(); } } }",
        )]);
        assert!(facts["src/demo/Worker.java"].calls.is_empty());
    }

    #[test]
    fn explicit_static_imports_are_resolved_only_when_unique() {
        let facts = resolved(&[
            (
                "src/demo/Util.java",
                "package demo; class Util { static void ping() {} }",
            ),
            (
                "src/other/Use.java",
                "package other; import static demo.Util.ping; class Use { void entry() { ping(); } }",
            ),
        ]);
        assert_eq!(facts["src/other/Use.java"].calls.len(), 1);
        assert_eq!(
            facts["src/other/Use.java"].calls[0].to,
            "src/demo/Util.java::Util.ping"
        );
    }
}
