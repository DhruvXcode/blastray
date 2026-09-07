use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use tree_sitter::{Node, Parser};

use crate::index::{RelationshipIssue, RelationshipStatus, SymbolKind};
use crate::language::{
    ParsedFile as ProviderParsedFile, ProviderContext, ResolvedCall, ResolvedFile,
};

pub(crate) const EXTENSIONS: &[&str] = &["go"];

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
    receiver_types: BTreeMap<String, String>,
    owner: Option<String>,
    receiver: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
struct ImportDraft {
    path: String,
    alias: Option<String>,
    line: usize,
    column: usize,
}
#[derive(Clone, Deserialize, Serialize)]
struct CallDraft {
    kind: CallKind,
    name: String,
    member: Option<String>,
    line: usize,
    column: usize,
}
#[derive(Clone, Copy, Deserialize, Serialize)]
enum CallKind {
    Direct,
    Receiver,
    ChainedReceiver,
    Other,
}

pub(crate) fn supports_path(path: &Path) -> bool {
    path.extension().and_then(|x| x.to_str()) == Some("go")
}
pub(crate) fn is_context_path(path: &Path) -> bool {
    path.file_name().and_then(|x| x.to_str()) == Some("go.mod")
}

pub(crate) fn parse(path: &str, source: &str) -> Result<ProviderParsedFile, String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .map_err(|e| format!("cannot configure parser for {path}: {e}"))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| format!("cannot parse {path}"))?;
    let root = tree.root_node();
    let package = root
        .named_children(&mut root.walk())
        .find(|n| n.kind() == "package_clause")
        .and_then(|n| field_text(n, "name", source))
        .unwrap_or_default();
    let mut parsed = ParsedFile {
        path: path.to_owned(),
        symbols: Vec::new(),
        package,
        imports: Vec::new(),
    };
    let mut cursor = root.walk();
    for node in root.named_children(&mut cursor) {
        match node.kind() {
            "function_declaration" => add_function(&mut parsed, node, source, None, None),
            "method_declaration" => {
                let receiver = node
                    .child_by_field_name("receiver")
                    .and_then(|n| receiver_parts(n, source));
                if let Some((owner, receiver)) = receiver {
                    add_function(&mut parsed, node, source, Some(owner), Some(receiver));
                }
            }
            "type_declaration" => add_types(&mut parsed, node, source),
            "import_declaration" => add_imports(&mut parsed, node, source),
            _ => {}
        }
    }
    Ok(ProviderParsedFile::Go(parsed))
}

fn add_types(parsed: &mut ParsedFile, node: Node<'_>, source: &str) {
    let mut cursor = node.walk();
    for spec in node
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "type_spec" || n.kind() == "type_alias")
    {
        let Some(name) = field_text(spec, "name", source) else {
            continue;
        };
        let p = spec.start_position();
        parsed.symbols.push(SymbolDraft {
            canonical: format!("{}::{name}", parsed.path),
            name,
            file: parsed.path.clone(),
            line: p.row + 1,
            end_line: spec.end_position().row + 1,
            column: p.column + 1,
            kind: SymbolKind::Type,
            calls: vec![],
            shadowed: BTreeSet::new(),
            receiver_types: BTreeMap::new(),
            owner: None,
            receiver: None,
        });
    }
}

fn add_function(
    parsed: &mut ParsedFile,
    node: Node<'_>,
    source: &str,
    owner: Option<String>,
    receiver: Option<String>,
) {
    let Some(name) = field_text(node, "name", source) else {
        return;
    };
    let p = node.start_position();
    let canonical = owner
        .as_ref()
        .map(|o| format!("{}::{o}.{name}", parsed.path))
        .unwrap_or_else(|| format!("{}::{name}", parsed.path));
    let mut shadowed = BTreeSet::new();
    let mut receiver_types = BTreeMap::new();
    if let Some(params) = node.child_by_field_name("parameters") {
        bindings(params, source, &mut shadowed);
        receiver_types.extend(declared_types(params, source));
    }
    if let Some(receiver) = &receiver {
        shadowed.insert(receiver.clone());
        if let Some(owner) = &owner {
            receiver_types.insert(receiver.clone(), owner.clone());
        }
    }
    let mut calls = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        receiver_types.extend(direct_local_types(body, source));
        collect_calls(body, source, &mut shadowed, &mut calls);
    }
    parsed.symbols.push(SymbolDraft {
        canonical,
        name,
        file: parsed.path.clone(),
        line: p.row + 1,
        end_line: node.end_position().row + 1,
        column: p.column + 1,
        kind: if owner.is_some() {
            SymbolKind::Method
        } else {
            SymbolKind::Function
        },
        calls,
        shadowed,
        receiver_types,
        owner,
        receiver,
    });
}

fn add_imports(parsed: &mut ParsedFile, node: Node<'_>, source: &str) {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind() == "import_spec" {
            let Some(path) = field_text(current, "path", source).map(unquote) else {
                continue;
            };
            let alias = field_text(current, "name", source);
            let p = current.start_position();
            parsed.imports.push(ImportDraft {
                path,
                alias,
                line: p.row + 1,
                column: p.column + 1,
            });
        } else {
            let mut c = current.walk();
            stack.extend(current.named_children(&mut c));
        }
    }
}

fn receiver_parts(node: Node<'_>, source: &str) -> Option<(String, String)> {
    let mut ids = Vec::new();
    collect_identifiers(node, source, &mut ids);
    if ids.len() < 2 {
        None
    } else {
        Some((ids.last()?.clone(), ids[0].clone()))
    }
}

fn bindings(node: Node<'_>, source: &str, into: &mut BTreeSet<String>) {
    let mut ids = Vec::new();
    collect_identifiers(node, source, &mut ids);
    into.extend(ids);
}

/// Extract parameter bindings whose type is written directly in the signature.
/// This intentionally does not follow assignments, constructors, factories,
/// interfaces, or control flow.
fn declared_types(node: Node<'_>, source: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind() == "parameter_declaration" {
            add_declared_type(current, source, &mut out);
            continue;
        }
        let mut cursor = current.walk();
        stack.extend(current.named_children(&mut cursor));
    }
    out
}

/// A function body's direct `var` declarations share its lexical scope. Do
/// not walk nested blocks: lifting a shadow from a child scope would turn a
/// later interface call into an invented concrete call.
fn direct_local_types(body: Node<'_>, source: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let scope = if body.kind() == "block" {
        let mut body_cursor = body.walk();
        body.named_children(&mut body_cursor)
            .find(|child| child.kind() == "statement_list")
            .unwrap_or(body)
    } else {
        body
    };
    let mut cursor = scope.walk();
    for statement in scope.named_children(&mut cursor) {
        let declarations: Vec<_> = if statement.kind() == "var_declaration" {
            vec![statement]
        } else if statement.kind() == "declaration_statement" {
            let mut declaration_cursor = statement.walk();
            statement.named_children(&mut declaration_cursor).collect()
        } else {
            Vec::new()
        };
        for declaration in declarations {
            if declaration.kind() != "var_declaration" {
                continue;
            }
            let mut var_cursor = declaration.walk();
            for child in declaration.named_children(&mut var_cursor) {
                if child.kind() == "var_spec" {
                    add_declared_type(child, source, &mut out);
                } else if child.kind() == "var_spec_list" {
                    let mut spec_cursor = child.walk();
                    for spec in child.named_children(&mut spec_cursor) {
                        if spec.kind() == "var_spec" {
                            add_declared_type(spec, source, &mut out);
                        }
                    }
                }
            }
        }
    }
    out
}

fn add_declared_type(node: Node<'_>, source: &str, out: &mut BTreeMap<String, String>) {
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };
    let type_name = text(type_node, source)
        .trim()
        .trim_start_matches('*')
        .trim();
    if type_name.is_empty()
        || !type_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "identifier" && child.id() != type_node.id() {
            out.insert(text(child, source).to_owned(), type_name.to_owned());
        }
    }
}

fn collect_identifiers(node: Node<'_>, source: &str, into: &mut Vec<String>) {
    if matches!(node.kind(), "identifier" | "type_identifier") {
        into.push(text(node, source).to_owned());
    }
    let mut c = node.walk();
    for child in node.named_children(&mut c) {
        collect_identifiers(child, source, into);
    }
}

fn collect_calls(
    node: Node<'_>,
    source: &str,
    shadowed: &mut BTreeSet<String>,
    calls: &mut Vec<CallDraft>,
) {
    if node.kind() == "func_literal" {
        return;
    }
    if matches!(node.kind(), "short_var_declaration" | "var_declaration") {
        bindings(node, source, shadowed);
    }
    if node.kind() == "call_expression" {
        let p = node.start_position();
        let Some(fun) = node.child_by_field_name("function") else {
            return;
        };
        let (kind, name, member) = match fun.kind() {
            "identifier" => (CallKind::Direct, text(fun, source).to_owned(), None),
            "selector_expression" => {
                let operand = fun.child_by_field_name("operand");
                let field = fun.child_by_field_name("field");
                match (operand, field) {
                    (Some(a), Some(b))
                        if a.kind() == "identifier" && b.kind() == "field_identifier" =>
                    {
                        (
                            CallKind::Receiver,
                            text(a, source).to_owned(),
                            Some(text(b, source).to_owned()),
                        )
                    }
                    (Some(_), Some(b)) if b.kind() == "field_identifier" => {
                        (CallKind::ChainedReceiver, text(b, source).to_owned(), None)
                    }
                    _ => (CallKind::Other, String::new(), None),
                }
            }
            _ => (CallKind::Other, String::new(), None),
        };
        calls.push(CallDraft {
            kind,
            name,
            member,
            line: p.row + 1,
            column: p.column + 1,
        });
        return;
    }
    let mut c = node.walk();
    for child in node.named_children(&mut c) {
        collect_calls(child, source, shadowed, calls);
    }
}

pub(crate) fn resolve(
    parsed: &BTreeMap<String, ProviderParsedFile>,
    paths: &BTreeSet<String>,
    context: &ProviderContext,
) -> BTreeMap<String, ResolvedFile> {
    let files: Vec<&ParsedFile> = parsed
        .values()
        .filter_map(|f| {
            if let ProviderParsedFile::Go(f) = f {
                Some(f)
            } else {
                None
            }
        })
        .collect();
    let modules = modules(context);
    let packages = package_files(&files);
    paths
        .iter()
        .filter_map(|path| {
            parsed.get(path).and_then(|f| {
                if let ProviderParsedFile::Go(f) = f {
                    Some((path, f))
                } else {
                    None
                }
            })
        })
        .map(|(path, file)| {
            (
                path.clone(),
                resolve_file(file, &files, &packages, &modules),
            )
        })
        .collect()
}

pub(crate) fn resolution_scope(
    parsed: &BTreeMap<String, ProviderParsedFile>,
    path: &str,
) -> BTreeSet<String> {
    let Some(ProviderParsedFile::Go(changed)) = parsed.get(path) else {
        return BTreeSet::from([path.to_owned()]);
    };
    let key = package_key(changed);
    parsed
        .iter()
        .filter_map(|(candidate_path, file)| match file {
            ProviderParsedFile::Go(candidate) if package_key(candidate) == key => {
                Some(candidate_path.clone())
            }
            _ => None,
        })
        .collect()
}

fn resolve_file(
    file: &ParsedFile,
    files: &[&ParsedFile],
    packages: &BTreeMap<(String, String), Vec<String>>,
    modules: &BTreeMap<String, String>,
) -> ResolvedFile {
    let key = package_key(file);
    let local_functions = functions_in(files, &key);
    let methods = methods_in(files, &key);
    let mut imports = BTreeMap::new();
    let mut dependencies = BTreeSet::new();
    let mut issues = Vec::new();
    for import in &file.imports {
        match import_target(file, import, files, packages, modules) {
            Some((name, target_key, target_files)) => {
                if let Some(alias) = &import.alias {
                    if alias != "." && alias != "_" {
                        imports.insert(alias.clone(), target_key);
                    }
                } else {
                    imports.insert(name, target_key);
                }
                dependencies.extend(target_files);
            }
            None => issues.push(issue(
                file,
                import.line,
                import.column,
                &import.path,
                "unresolved Go package import",
            )),
        }
    }
    let mut calls = Vec::new();
    for symbol in &file.symbols {
        for call in &symbol.calls {
            match call.kind {
                CallKind::Direct if !symbol.shadowed.contains(&call.name) => add_unique(
                    &mut calls,
                    &mut issues,
                    &symbol.canonical,
                    call,
                    local_functions.get(&call.name).cloned(),
                ),
                CallKind::Receiver => {
                    if call.name == symbol.receiver.clone().unwrap_or_default() {
                        add_unique(
                            &mut calls,
                            &mut issues,
                            &symbol.canonical,
                            call,
                            methods
                                .get(&(
                                    symbol.owner.clone().unwrap_or_default(),
                                    call.member.clone().unwrap_or_default(),
                                ))
                                .cloned(),
                        );
                    } else if let Some(owner) = symbol.receiver_types.get(&call.name) {
                        add_unique(
                            &mut calls,
                            &mut issues,
                            &symbol.canonical,
                            call,
                            receiver_method_candidates(
                                files,
                                &methods,
                                &imports,
                                owner,
                                call.member.as_deref().unwrap_or_default(),
                            ),
                        );
                    } else if let Some(target) = imports.get(&call.name) {
                        let candidate = functions_in(files, target)
                            .get(call.member.as_deref().unwrap_or_default())
                            .cloned()
                            .filter(|list| {
                                list.iter().all(|name| {
                                    exported(name.rsplit("::").next().unwrap_or_default())
                                })
                            });
                        add_unique(&mut calls, &mut issues, &symbol.canonical, call, candidate);
                    } else {
                        issues.push(call_issue(
                            RelationshipStatus::Unresolved,
                            &symbol.canonical,
                            call.line,
                            call.column,
                            &call.name,
                            "unresolved Go receiver dispatch; receiver type is not locally proven",
                        ));
                    }
                }
                CallKind::ChainedReceiver => issues.push(call_issue(
                    RelationshipStatus::Unresolved,
                    &symbol.canonical,
                    call.line,
                    call.column,
                    &call.name,
                    "unresolved Go chained receiver dispatch; value flow is not modeled",
                )),
                CallKind::Direct | CallKind::Other => issues.push(call_issue(
                    RelationshipStatus::Unresolved,
                    &symbol.canonical,
                    call.line,
                    call.column,
                    &call.name,
                    "unresolved Go call",
                )),
            }
        }
    }
    ResolvedFile {
        imports: Vec::new(),
        dependencies: dependencies.into_iter().collect(),
        calls,
        relationships: Vec::new(),
        issues,
    }
}

fn add_unique(
    calls: &mut Vec<ResolvedCall>,
    issues: &mut Vec<RelationshipIssue>,
    from: &str,
    call: &CallDraft,
    candidates: Option<Vec<String>>,
) {
    match candidates {
        Some(v) if v.len() == 1 => calls.push(ResolvedCall {
            from: from.to_owned(),
            to: v[0].clone(),
            line: call.line,
            column: call.column,
        }),
        Some(v) if v.len() > 1 => issues.push(call_issue(
            RelationshipStatus::Ambiguous,
            from,
            call.line,
            call.column,
            &call.name,
            "ambiguous Go call target",
        )),
        _ => issues.push(call_issue(
            RelationshipStatus::Unresolved,
            from,
            call.line,
            call.column,
            &call.name,
            "unresolved Go call target",
        )),
    }
}

fn functions_in(files: &[&ParsedFile], key: &(String, String)) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for file in files {
        if &package_key(file) == key {
            for s in &file.symbols {
                if s.kind == SymbolKind::Function {
                    out.entry(s.name.clone())
                        .or_insert_with(Vec::new)
                        .push(s.canonical.clone());
                }
            }
        }
    }
    out
}
fn methods_in(
    files: &[&ParsedFile],
    key: &(String, String),
) -> BTreeMap<(String, String), Vec<String>> {
    let mut out = BTreeMap::new();
    for file in files {
        if &package_key(file) == key {
            for s in &file.symbols {
                if s.kind == SymbolKind::Method {
                    out.entry((s.owner.clone().unwrap_or_default(), s.name.clone()))
                        .or_insert_with(Vec::new)
                        .push(s.canonical.clone());
                }
            }
        }
    }
    out
}

fn receiver_method_candidates(
    files: &[&ParsedFile],
    local_methods: &BTreeMap<(String, String), Vec<String>>,
    imports: &BTreeMap<String, (String, String)>,
    receiver_type: &str,
    member: &str,
) -> Option<Vec<String>> {
    if let Some((alias, owner)) = receiver_type.split_once('.') {
        let target = imports.get(alias)?;
        let imported_methods = methods_in(files, target);
        return imported_methods
            .get(&(owner.to_owned(), member.to_owned()))
            .cloned();
    }
    local_methods
        .get(&(receiver_type.to_owned(), member.to_owned()))
        .cloned()
}
fn package_files(files: &[&ParsedFile]) -> BTreeMap<(String, String), Vec<String>> {
    let mut out = BTreeMap::new();
    for f in files {
        out.entry(package_key(f))
            .or_insert_with(Vec::new)
            .push(f.path.clone());
    }
    out
}
fn package_key(file: &ParsedFile) -> (String, String) {
    (
        Path::new(&file.path)
            .parent()
            .unwrap_or(Path::new(""))
            .to_string_lossy()
            .to_string(),
        file.package.clone(),
    )
}
fn modules(context: &ProviderContext) -> BTreeMap<String, String> {
    context
        .files
        .iter()
        .filter_map(|(path, text)| {
            text.lines()
                .map(str::trim)
                .find_map(|line| line.strip_prefix("module ").map(str::trim))
                .filter(|s| !s.is_empty() && !s.contains(char::is_whitespace))
                .map(|module| {
                    (
                        Path::new(path)
                            .parent()
                            .unwrap_or(Path::new(""))
                            .to_string_lossy()
                            .to_string(),
                        module.to_owned(),
                    )
                })
        })
        .collect()
}
fn import_target(
    file: &ParsedFile,
    import: &ImportDraft,
    _files: &[&ParsedFile],
    packages: &BTreeMap<(String, String), Vec<String>>,
    modules: &BTreeMap<String, String>,
) -> Option<(String, (String, String), Vec<String>)> {
    let mut chosen: Option<(&String, &String)> = None;
    for (root, module) in modules {
        if (root.is_empty() || file.path.starts_with(&format!("{root}/")))
            && chosen.is_none_or(|(current, _)| root.len() > current.len())
        {
            chosen = Some((root, module));
        }
    }
    let (root, module) = chosen?;
    if !import
        .path
        .strip_prefix(module)
        .is_some_and(|rest| rest.starts_with('/'))
    {
        return None;
    }
    let rest = import.path.strip_prefix(module)?.trim_start_matches('/');
    let target_dir = if root.is_empty() {
        rest.to_owned()
    } else {
        format!("{root}/{rest}")
    };
    let matches: Vec<_> = packages
        .iter()
        .filter(|((dir, _), _)| dir == &target_dir)
        .collect();
    if matches.len() != 1 {
        return None;
    };
    let ((_, name), target_files) = matches[0];
    Some((
        name.clone(),
        (target_dir, name.clone()),
        target_files.clone(),
    ))
}
fn exported(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
}
fn issue(
    file: &ParsedFile,
    line: usize,
    column: usize,
    name: &str,
    detail: &str,
) -> RelationshipIssue {
    issue_with_status(
        RelationshipStatus::Unresolved,
        file,
        line,
        column,
        name,
        detail,
    )
}

fn issue_with_status(
    status: RelationshipStatus,
    file: &ParsedFile,
    line: usize,
    column: usize,
    name: &str,
    detail: &str,
) -> RelationshipIssue {
    RelationshipIssue {
        status,
        source: file.path.clone(),
        line,
        column,
        name: name.to_owned(),
        detail: detail.to_owned(),
    }
}

fn call_issue(
    status: RelationshipStatus,
    source: &str,
    line: usize,
    column: usize,
    name: &str,
    detail: &str,
) -> RelationshipIssue {
    RelationshipIssue {
        status,
        source: source.to_owned(),
        line,
        column,
        name: name.to_owned(),
        detail: detail.to_owned(),
    }
}
fn field_text(node: Node<'_>, field: &str, source: &str) -> Option<String> {
    node.child_by_field_name(field)
        .map(|n| text(n, source).to_owned())
}
fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}
fn unquote(value: String) -> String {
    value.trim_matches('`').trim_matches('"').to_owned()
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;

    fn resolved(
        files: &[(&str, &str)],
        context: &[(&str, &str)],
    ) -> BTreeMap<String, ResolvedFile> {
        let parsed = files
            .iter()
            .map(|(path, source)| (path.to_string(), parse(path, source).unwrap()))
            .collect();
        let context = ProviderContext {
            files: context
                .iter()
                .map(|(path, text)| (path.to_string(), text.to_string()))
                .collect(),
        };
        resolve(
            &parsed,
            &parsed.keys().cloned().collect::<BTreeSet<_>>(),
            &context,
        )
    }

    #[test]
    fn indexes_types_methods_and_cross_file_package_calls() {
        let facts = resolved(
            &[
                (
                    "app/a.go",
                    "package app\nfunc helper() {}\ntype Server struct{}\nfunc (s *Server) leaf() {}\n",
                ),
                (
                    "app/b.go",
                    "package app\nfunc entry() { helper() }\nfunc (s Server) Run() { s.leaf() }\n",
                ),
            ],
            &[],
        );
        let calls = &facts["app/b.go"].calls;
        assert!(
            calls
                .iter()
                .any(|call| call.from == "app/b.go::entry" && call.to == "app/a.go::helper")
        );
        assert!(
            calls
                .iter()
                .any(|call| call.from == "app/b.go::Server.Run"
                    && call.to == "app/a.go::Server.leaf")
        );
    }

    #[test]
    fn resolves_only_exact_local_module_package_calls() {
        let facts = resolved(
            &[
                (
                    "main.go",
                    "package main\nimport utilx \"example.com/project/internal/util\"\nfunc entry() { utilx.Helper() }\n",
                ),
                (
                    "internal/util/util.go",
                    "package actual\nfunc Helper() {}\nfunc private() {}\n",
                ),
            ],
            &[("go.mod", "module example.com/project\n")],
        );
        assert_eq!(facts["main.go"].calls.len(), 1);
        assert_eq!(
            facts["main.go"].calls[0].to,
            "internal/util/util.go::Helper"
        );
        assert!(
            facts["main.go"]
                .dependencies
                .contains(&"internal/util/util.go".to_string())
        );
    }

    #[test]
    fn shadows_and_external_receivers_do_not_become_edges() {
        let facts = resolved(
            &[(
                "main.go",
                "package main\nfunc helper() {}\nfunc entry(helper func()) { helper() }\nfunc other() { fmt.Println() }\n",
            )],
            &[],
        );
        assert!(facts["main.go"].calls.is_empty());
        assert!(facts["main.go"].issues.len() >= 2);
    }

    #[test]
    fn resolves_explicit_concrete_parameter_pointer_and_local_receivers() {
        let facts = resolved(
            &[(
                "main.go",
                "package main\ntype Worker struct{}\nfunc (worker Worker) Run() {}\nfunc parameter(w Worker) { w.Run() }\nfunc pointer(w *Worker) { w.Run() }\nfunc local() { var w Worker; w.Run() }\n",
            )],
            &[],
        );
        let calls = &facts["main.go"].calls;
        for source in ["parameter", "pointer", "local"] {
            assert!(calls.iter().any(|call| {
                call.from == format!("main.go::{source}") && call.to == "main.go::Worker.Run"
            }));
        }
    }

    #[test]
    fn resolves_cross_file_and_imported_concrete_receivers() {
        let facts = resolved(
            &[
                (
                    "main.go",
                    "package main\nimport worker \"example.com/project/internal/worker\"\nfunc entry(w worker.Worker) { w.Run() }\n",
                ),
                (
                    "internal/worker/worker.go",
                    "package worker\ntype Worker struct{}\nfunc (worker *Worker) Run() {}\n",
                ),
            ],
            &[("go.mod", "module example.com/project\n")],
        );
        assert!(facts["main.go"].calls.iter().any(|call| {
            call.from == "main.go::entry" && call.to == "internal/worker/worker.go::Worker.Run"
        }));
    }

    #[test]
    fn multiple_locally_indexed_methods_are_ambiguous() {
        let facts = resolved(
            &[
                (
                    "a.go",
                    "package main\ntype Worker struct{}\nfunc (worker Worker) Run() {}\n",
                ),
                (
                    "b.go",
                    "package main\nfunc (worker Worker) Run() {}\nfunc entry(worker Worker) { worker.Run() }\n",
                ),
            ],
            &[],
        );
        assert!(facts["b.go"].calls.is_empty());
        assert!(
            facts["b.go"]
                .issues
                .iter()
                .any(|issue| issue.status == RelationshipStatus::Ambiguous)
        );
    }

    #[test]
    fn interface_and_dynamic_receivers_remain_unresolved() {
        let facts = resolved(
            &[(
                "main.go",
                "package main\ntype Runner interface { Run() }\ntype First struct{}\ntype Second struct{}\nfunc (first First) Run() {}\nfunc (second Second) Run() {}\nfunc interfaceCall(r Runner) { r.Run() }\nfunc unknown(x any) { x.(Runner).Run() }\n",
            )],
            &[],
        );
        assert!(facts["main.go"].calls.is_empty());
        assert!(facts["main.go"].issues.len() >= 2);
    }

    #[test]
    fn nested_concrete_shadow_does_not_type_an_interface_parameter() {
        let facts = resolved(
            &[(
                "main.go",
                "package main\ntype Runner interface { Run() }\ntype Worker struct{}\nfunc (worker Worker) Run() {}\nfunc entry(value Runner) { { var value Worker; value.Run() }; value.Run() }\n",
            )],
            &[],
        );
        assert!(facts["main.go"].calls.is_empty());
        assert!(facts["main.go"].issues.len() >= 2);
    }

    #[test]
    fn dynamic_chained_and_callable_members_do_not_become_edges() {
        let facts = resolved(
            &[(
                "main.go",
                "package main\nimport \"net/http\"\ntype Runner interface { Run() }\ntype Container struct { callback func(); child Runner }\ntype First struct{}\ntype Second struct{}\ntype Outer struct { First; Second }\nfunc (first First) Run() {}\nfunc (second Second) Run() {}\nfunc interfaceCall(r Runner) { r.Run() }\nfunc external(handler http.Handler) { handler.ServeHTTP(nil, nil) }\nfunc chained(value Container) { value.child.Run() }\nfunc callback(value Container) { value.callback() }\nfunc promoted(value Outer) { value.Run() }\nfunc factory() Runner { return nil }\nfunc dynamic() { factory().Run() }\n",
            )],
            &[],
        );
        assert!(facts["main.go"].calls.is_empty());
        assert!(facts["main.go"].issues.len() >= 6);
        assert!(
            facts["main.go"]
                .issues
                .iter()
                .all(|issue| issue.status == RelationshipStatus::Unresolved)
        );
    }

    #[test]
    fn nested_modules_do_not_leak_outer_module_ownership() {
        let facts = resolved(
            &[
                (
                    "examples/app/main.go",
                    "package app\nimport \"example.com/nested/util\"\nfunc entry() { util.Helper() }\n",
                ),
                (
                    "examples/app/util/util.go",
                    "package util\nfunc Helper() {}\n",
                ),
                (
                    "examples/app/external.go",
                    "package app\nimport \"example.com/root/util\"\nfunc external() { util.Helper() }\n",
                ),
                ("util/util.go", "package util\nfunc Helper() {}\n"),
            ],
            &[
                ("go.mod", "module example.com/root\n"),
                ("examples/app/go.mod", "module example.com/nested\n"),
            ],
        );
        assert!(facts["examples/app/main.go"].calls.is_empty());
        assert!(facts["examples/app/external.go"].calls.is_empty());
    }
}
