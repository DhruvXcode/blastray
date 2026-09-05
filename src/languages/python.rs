use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use tree_sitter::{Node, Parser};

use crate::index::{RelationshipIssue, RelationshipStatus, SymbolKind};
use crate::language::{ParsedFile as ProviderParsedFile, ResolvedCall, ResolvedFile};

pub(crate) const EXTENSIONS: &[&str] = &["py"];

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct ParsedFile {
    pub path: String,
    pub symbols: Vec<SymbolDraft>,
    imports: Vec<ImportDraft>,
    module_shadowed: BTreeSet<String>,
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
    class: Option<String>,
    instance_receiver: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
struct CallDraft {
    name: String,
    receiver: Option<String>,
    line: usize,
    column: usize,
    direct: bool,
}

#[derive(Clone, Deserialize, Serialize)]
struct ImportDraft {
    module: String,
    line: usize,
    column: usize,
    bindings: Vec<ImportBinding>,
    wildcard: bool,
    relative: bool,
}

#[derive(Clone, Deserialize, Serialize)]
struct ImportBinding {
    local: String,
    imported: String,
}

#[derive(Clone)]
enum BindingTarget {
    Resolved(String),
    Unresolved,
    Ambiguous,
}

pub(crate) fn supports_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| EXTENSIONS.contains(&extension))
}

pub(crate) fn parse(path: &str, source: &str) -> Result<ProviderParsedFile, String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .map_err(|error| format!("cannot configure parser for {path}: {error}"))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| format!("cannot parse {path}"))?;
    let mut parsed = ParsedFile {
        path: path.to_string(),
        symbols: Vec::new(),
        imports: Vec::new(),
        module_shadowed: BTreeSet::new(),
    };
    let mut cursor = tree.root_node().walk();
    for child in tree.root_node().named_children(&mut cursor) {
        add_top_level(&mut parsed, child, source);
    }
    Ok(ProviderParsedFile::Python(parsed))
}

fn add_top_level(parsed: &mut ParsedFile, node: Node<'_>, source: &str) {
    match node.kind() {
        "function_definition" => add_function(parsed, node, source, None, None),
        "class_definition" => add_class(parsed, node, source),
        "decorated_definition" => {
            if let Some(definition) = node.child_by_field_name("definition") {
                match definition.kind() {
                    "function_definition" => add_function(parsed, definition, source, None, None),
                    "class_definition" => add_class(parsed, definition, source),
                    _ => {}
                }
            }
        }
        "import_from_statement" | "import_statement" => {
            parsed.imports.extend(import_drafts(node, source));
        }
        "assignment" | "augmented_assignment" => {
            add_module_assignment(parsed, node, source);
        }
        "expression_statement" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if matches!(child.kind(), "assignment" | "augmented_assignment") {
                    add_module_assignment(parsed, child, source);
                }
            }
        }
        _ => {}
    }
}

fn add_module_assignment(parsed: &mut ParsedFile, node: Node<'_>, source: &str) {
    if let Some(left) = node.child_by_field_name("left")
        && left.kind() == "identifier"
    {
        parsed
            .module_shadowed
            .insert(text(left, source).to_string());
    }
}

fn add_class(parsed: &mut ParsedFile, node: Node<'_>, source: &str) {
    let Some(name) = field_text(node, "name", source) else {
        return;
    };
    parsed.symbols.push(SymbolDraft {
        canonical: format!("{}::{name}", parsed.path),
        name: name.clone(),
        file: parsed.path.clone(),
        line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        column: node.start_position().column + 1,
        kind: SymbolKind::Class,
        calls: Vec::new(),
        shadowed: BTreeSet::new(),
        class: None,
        instance_receiver: None,
    });
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        match child.kind() {
            "function_definition" => add_function(parsed, child, source, Some(&name), None),
            "decorated_definition" => {
                let Some(definition) = child.child_by_field_name("definition") else {
                    continue;
                };
                if definition.kind() == "function_definition" {
                    add_function(
                        parsed,
                        definition,
                        source,
                        Some(&name),
                        decorator_kind(child, source),
                    );
                }
            }
            _ => {}
        }
    }
}

fn add_function(
    parsed: &mut ParsedFile,
    node: Node<'_>,
    source: &str,
    class: Option<&str>,
    decorator: Option<DecoratorKind>,
) {
    let Some(name) = field_text(node, "name", source) else {
        return;
    };
    let parameters = node.child_by_field_name("parameters");
    let first_parameter = parameters.and_then(|parameters| first_parameter(parameters, source));
    let instance_receiver = match (class, decorator) {
        (Some(_), Some(DecoratorKind::Static | DecoratorKind::Class)) => None,
        (Some(_), _) => first_parameter.clone(),
        (None, _) => None,
    };
    let mut shadowed = parameters
        .map(|parameters| parameter_names(parameters, source))
        .unwrap_or_default();
    if let Some(receiver) = &instance_receiver {
        shadowed.remove(receiver);
    }
    let calls = node
        .child_by_field_name("body")
        .map(|body| collect_body_facts(body, source, &mut shadowed))
        .unwrap_or_default();
    let canonical_name = class.map_or_else(|| name.clone(), |class| format!("{class}.{name}"));
    parsed.symbols.push(SymbolDraft {
        canonical: format!("{}::{canonical_name}", parsed.path),
        name,
        file: parsed.path.clone(),
        line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        column: node.start_position().column + 1,
        kind: if class.is_some() {
            SymbolKind::Method
        } else {
            SymbolKind::Function
        },
        calls,
        shadowed,
        class: class.map(str::to_string),
        instance_receiver,
    });
}

#[derive(Clone, Copy)]
enum DecoratorKind {
    Static,
    Class,
}

fn decorator_kind(node: Node<'_>, source: &str) -> Option<DecoratorKind> {
    let mut cursor = node.walk();
    for decorator in node.named_children(&mut cursor) {
        if decorator.kind() != "decorator" {
            continue;
        }
        match text(decorator, source).trim() {
            "@staticmethod" => return Some(DecoratorKind::Static),
            "@classmethod" => return Some(DecoratorKind::Class),
            _ => {}
        }
    }
    None
}

fn import_drafts(node: Node<'_>, source: &str) -> Vec<ImportDraft> {
    if node.kind() == "import_statement" {
        let mut cursor = node.walk();
        return node
            .named_children(&mut cursor)
            .filter(|child| matches!(child.kind(), "dotted_name" | "aliased_import"))
            .map(|child| ImportDraft {
                module: import_name(child, source),
                line: child.start_position().row + 1,
                column: child.start_position().column + 1,
                bindings: Vec::new(),
                wildcard: false,
                relative: false,
            })
            .collect();
    }
    let Some(module_node) = node.child_by_field_name("module_name") else {
        return Vec::new();
    };
    let module = text(module_node, source).to_string();
    let mut cursor = node.walk();
    let bindings = node
        .children_by_field_name("name", &mut cursor)
        .map(|child| {
            let imported = import_name(child, source);
            let local = if child.kind() == "aliased_import" {
                field_text(child, "alias", source).unwrap_or_else(|| imported.clone())
            } else {
                imported.clone()
            };
            ImportBinding { local, imported }
        })
        .collect();
    vec![ImportDraft {
        relative: module.starts_with('.'),
        module,
        line: node.start_position().row + 1,
        column: node.start_position().column + 1,
        bindings,
        wildcard: text(node, source).contains('*'),
    }]
}

fn import_name(node: Node<'_>, source: &str) -> String {
    field_text(node, "name", source).unwrap_or_else(|| text(node, source).to_string())
}

fn first_parameter(node: Node<'_>, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .next()
        .and_then(|parameter| parameter_name(parameter, source))
}

fn parameter_names(node: Node<'_>, source: &str) -> BTreeSet<String> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter_map(|parameter| parameter_name(parameter, source))
        .collect()
}

fn parameter_name(node: Node<'_>, source: &str) -> Option<String> {
    if node.kind() == "identifier" {
        return Some(text(node, source).to_string());
    }
    field_text(node, "name", source).or_else(|| first_identifier(node, source))
}

fn first_identifier(node: Node<'_>, source: &str) -> Option<String> {
    if node.kind() == "identifier" {
        return Some(text(node, source).to_string());
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find_map(|child| first_identifier(child, source))
}

fn collect_body_facts(
    node: Node<'_>,
    source: &str,
    shadowed: &mut BTreeSet<String>,
) -> Vec<CallDraft> {
    let mut calls = Vec::new();
    collect_body_facts_inner(node, source, shadowed, &mut calls);
    calls
}

fn collect_body_facts_inner(
    node: Node<'_>,
    source: &str,
    shadowed: &mut BTreeSet<String>,
    calls: &mut Vec<CallDraft>,
) {
    match node.kind() {
        "function_definition" | "class_definition" => {
            if let Some(name) = field_text(node, "name", source) {
                shadowed.insert(name);
            }
            return;
        }
        "decorated_definition" => {
            if let Some(definition) = node.child_by_field_name("definition")
                && let Some(name) = field_text(definition, "name", source)
            {
                shadowed.insert(name);
            }
            return;
        }
        "assignment" | "augmented_assignment" => {
            if let Some(left) = node.child_by_field_name("left")
                && left.kind() == "identifier"
            {
                shadowed.insert(text(left, source).to_string());
            }
        }
        "import_from_statement" | "import_statement" => {
            shadowed.extend(imported_local_names(node, source));
        }
        "call" => calls.push(call_draft(node, source)),
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_body_facts_inner(child, source, shadowed, calls);
    }
}

fn imported_local_names(node: Node<'_>, source: &str) -> BTreeSet<String> {
    let mut cursor = node.walk();
    node.children_by_field_name("name", &mut cursor)
        .filter_map(|child| {
            if child.kind() == "aliased_import" {
                field_text(child, "alias", source)
            } else {
                import_name(child, source)
                    .split('.')
                    .next()
                    .map(str::to_string)
            }
        })
        .collect()
}

fn call_draft(node: Node<'_>, source: &str) -> CallDraft {
    let function = node.child_by_field_name("function");
    let line = node.start_position().row + 1;
    let column = node.start_position().column + 1;
    let Some(function) = function else {
        return CallDraft {
            name: "<dynamic>".to_string(),
            receiver: None,
            line,
            column,
            direct: false,
        };
    };
    if function.kind() == "identifier" {
        return CallDraft {
            name: text(function, source).to_string(),
            receiver: None,
            line,
            column,
            direct: true,
        };
    }
    if function.kind() == "attribute"
        && let (Some(object), Some(attribute)) = (
            function.child_by_field_name("object"),
            function.child_by_field_name("attribute"),
        )
        && object.kind() == "identifier"
    {
        return CallDraft {
            name: text(attribute, source).to_string(),
            receiver: Some(text(object, source).to_string()),
            line,
            column,
            direct: false,
        };
    }
    CallDraft {
        name: text(function, source).to_string(),
        receiver: None,
        line,
        column,
        direct: false,
    }
}

pub(crate) fn resolve(
    parsed: &BTreeMap<String, ProviderParsedFile>,
    paths: &BTreeSet<String>,
) -> BTreeMap<String, ResolvedFile> {
    let parsed: BTreeMap<String, &ParsedFile> = parsed
        .iter()
        .filter_map(|(path, file)| match file {
            ProviderParsedFile::Python(file) => Some((path.clone(), file)),
            ProviderParsedFile::JsTs(_) => None,
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
    local_functions: BTreeMap<(String, String), Vec<String>>,
    methods: BTreeMap<(String, String, String), Vec<String>>,
}

impl ResolveContext {
    fn new(parsed: &BTreeMap<String, &ParsedFile>) -> Self {
        let mut context = Self {
            files: parsed.keys().cloned().collect(),
            local_functions: BTreeMap::new(),
            methods: BTreeMap::new(),
        };
        for file in parsed.values() {
            for symbol in &file.symbols {
                match symbol.kind {
                    SymbolKind::Function => context
                        .local_functions
                        .entry((file.path.clone(), symbol.name.clone()))
                        .or_default()
                        .push(symbol.canonical.clone()),
                    SymbolKind::Method => {
                        if let Some(class) = &symbol.class {
                            context
                                .methods
                                .entry((file.path.clone(), class.clone(), symbol.name.clone()))
                                .or_default()
                                .push(symbol.canonical.clone());
                        }
                    }
                    SymbolKind::Class => {}
                }
            }
        }
        context
    }
}

fn resolve_file(file: &ParsedFile, context: &ResolveContext) -> ResolvedFile {
    let mut result = ResolvedFile::default();
    let mut bindings = BTreeMap::new();
    for import in &file.imports {
        resolve_import(import, file, context, &mut bindings, &mut result);
    }
    for symbol in &file.symbols {
        if symbol.kind != SymbolKind::Function && symbol.kind != SymbolKind::Method {
            continue;
        }
        for call in &symbol.calls {
            if let Some(receiver) = &call.receiver {
                resolve_instance_call(symbol, receiver, call, file, context, &mut result);
                continue;
            }
            if !call.direct {
                result.issues.push(issue(
                    RelationshipStatus::Unresolved,
                    &symbol.canonical,
                    call.line,
                    call.column,
                    &call.name,
                    "receiver or dynamic call syntax is outside the Python subset",
                ));
                continue;
            }
            if symbol.shadowed.contains(&call.name) {
                result.issues.push(issue(
                    RelationshipStatus::Unresolved,
                    &symbol.canonical,
                    call.line,
                    call.column,
                    &call.name,
                    "an obvious local Python binding could shadow this name",
                ));
                continue;
            }
            let key = (file.path.clone(), call.name.clone());
            let mut candidates = if bindings.contains_key(&key) {
                binding_targets(
                    bindings.get(&key),
                    &mut result.issues,
                    &symbol.canonical,
                    call,
                )
            } else if file.module_shadowed.contains(&call.name) {
                Vec::new()
            } else {
                context
                    .local_functions
                    .get(&key)
                    .cloned()
                    .unwrap_or_default()
            };
            candidates.sort();
            candidates.dedup();
            match candidates.as_slice() {
                [target] => result.calls.push(ResolvedCall {
                    from: symbol.canonical.clone(),
                    to: target.clone(),
                    line: call.line,
                    column: call.column,
                }),
                [] if file.module_shadowed.contains(&call.name) => result.issues.push(issue(
                    RelationshipStatus::Unresolved,
                    &symbol.canonical,
                    call.line,
                    call.column,
                    &call.name,
                    "a module-level Python assignment could shadow this name",
                )),
                [] if !bindings.contains_key(&key) => result.issues.push(issue(
                    RelationshipStatus::Unresolved,
                    &symbol.canonical,
                    call.line,
                    call.column,
                    &call.name,
                    "no matching local Python function or resolved import",
                )),
                [] => {}
                _ => result.issues.push(issue(
                    RelationshipStatus::Ambiguous,
                    &symbol.canonical,
                    call.line,
                    call.column,
                    &call.name,
                    "multiple callable Python definitions match this name",
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

fn resolve_instance_call(
    symbol: &SymbolDraft,
    receiver: &str,
    call: &CallDraft,
    file: &ParsedFile,
    context: &ResolveContext,
    result: &mut ResolvedFile,
) {
    let Some(class) = &symbol.class else {
        result.issues.push(issue(
            RelationshipStatus::Unresolved,
            &symbol.canonical,
            call.line,
            call.column,
            &call.name,
            "receiver or dynamic call syntax is outside the Python subset",
        ));
        return;
    };
    if symbol.instance_receiver.as_deref() != Some(receiver) || symbol.shadowed.contains(receiver) {
        result.issues.push(issue(
            RelationshipStatus::Unresolved,
            &symbol.canonical,
            call.line,
            call.column,
            &call.name,
            "receiver or dynamic call syntax is outside the Python subset",
        ));
        return;
    }
    let candidates = context
        .methods
        .get(&(file.path.clone(), class.clone(), call.name.clone()))
        .cloned()
        .unwrap_or_default();
    match candidates.as_slice() {
        [target] => result.calls.push(ResolvedCall {
            from: symbol.canonical.clone(),
            to: target.clone(),
            line: call.line,
            column: call.column,
        }),
        [] => result.issues.push(issue(
            RelationshipStatus::Unresolved,
            &symbol.canonical,
            call.line,
            call.column,
            &call.name,
            "no matching direct same-class Python method",
        )),
        _ => result.issues.push(issue(
            RelationshipStatus::Ambiguous,
            &symbol.canonical,
            call.line,
            call.column,
            &call.name,
            "multiple direct same-class Python methods match this member call",
        )),
    }
}

fn resolve_import(
    import: &ImportDraft,
    file: &ParsedFile,
    context: &ResolveContext,
    bindings: &mut BTreeMap<(String, String), Vec<BindingTarget>>,
    result: &mut ResolvedFile,
) {
    let candidates = relative_module_candidates(&file.path, &import.module, &context.files);
    let target = match candidates.as_slice() {
        [target] if import.relative && !import.module.trim_matches('.').is_empty() => {
            result.imports.push(target.clone());
            Some(target.clone())
        }
        [] if import.relative && !import.module.trim_matches('.').is_empty() => {
            result.issues.push(issue(
                RelationshipStatus::Unresolved,
                &file.path,
                import.line,
                import.column,
                &import.module,
                "relative Python module was not found",
            ));
            None
        }
        [] => {
            let detail = if import.relative {
                "relative Python package import without an explicit module is unsupported"
            } else {
                "absolute or package Python imports are unsupported"
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
                "more than one Python source file matches this relative module",
            ));
            None
        }
    };
    if import.wildcard {
        result.issues.push(issue(
            RelationshipStatus::Unresolved,
            &file.path,
            import.line,
            import.column,
            &import.module,
            "wildcard Python imports are unsupported",
        ));
    }
    for binding in &import.bindings {
        let value = if let Some(target) = &target {
            match context
                .local_functions
                .get(&(target.clone(), binding.imported.clone()))
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
            result.issues.push(issue(
                match value {
                    BindingTarget::Ambiguous => RelationshipStatus::Ambiguous,
                    _ => RelationshipStatus::Unresolved,
                },
                &file.path,
                import.line,
                import.column,
                &binding.local,
                if target.is_some() {
                    "the imported Python symbol was not uniquely available in the resolved module"
                } else {
                    "the imported Python binding cannot be resolved until its module is resolved"
                },
            ));
        }
        bindings
            .entry((file.path.clone(), binding.local.clone()))
            .or_default()
            .push(value);
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
            "the imported Python binding is not uniquely resolved",
        ));
        return Vec::new();
    }
    targets
}

fn relative_module_candidates(from: &str, module: &str, files: &BTreeSet<String>) -> Vec<String> {
    if !module.starts_with('.') {
        return Vec::new();
    }
    let dots = module
        .chars()
        .take_while(|character| *character == '.')
        .count();
    let suffix = &module[dots..];
    if suffix.is_empty() {
        return Vec::new();
    }
    let mut base = PathBuf::from(from)
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .to_path_buf();
    for _ in 1..dots {
        if !base.pop() {
            return Vec::new();
        }
    }
    for part in suffix.split('.') {
        base.push(part);
    }
    let Some(base) = normalize_relative(&base) else {
        return Vec::new();
    };
    let mut candidates = BTreeSet::new();
    for candidate in [format!("{base}.py"), format!("{base}/__init__.py")] {
        if files.contains(&candidate) {
            candidates.insert(candidate);
        }
    }
    candidates.into_iter().collect()
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

fn field_text(node: Node<'_>, field: &str, source: &str) -> Option<String> {
    node.child_by_field_name(field)
        .map(|child| text(child, source).to_string())
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use crate::language::ParsedFile as ProviderParsedFile;

    use super::{parse, resolve};

    fn resolved(files: &[(&str, &str)]) -> BTreeMap<String, crate::language::ResolvedFile> {
        let parsed: BTreeMap<String, ProviderParsedFile> = files
            .iter()
            .map(|(path, source)| (path.to_string(), parse(path, source).unwrap()))
            .collect();
        resolve(&parsed, &parsed.keys().cloned().collect::<BTreeSet<_>>())
    }

    #[test]
    fn extracts_top_level_async_decorated_and_direct_class_symbols() {
        let parsed = parse(
            "pkg/main.py",
            "@decorator\ndef decorated():\n    pass\n\nasync def async_work():\n    pass\n\nclass Worker:\n    async def run(self):\n        pass\n",
        )
        .unwrap();
        let symbols = parsed.symbols();
        let canonical: Vec<_> = symbols
            .iter()
            .map(|symbol| symbol.canonical.as_str())
            .collect();
        assert_eq!(
            canonical,
            [
                "pkg/main.py::decorated",
                "pkg/main.py::async_work",
                "pkg/main.py::Worker",
                "pkg/main.py::Worker.run",
            ]
        );
        assert_eq!(symbols[3].line, 9);
        assert_eq!(symbols[3].end_line, 10);
    }

    #[test]
    fn resolves_local_calls_and_keeps_parameter_and_assignment_shadowing_unknown() {
        let facts = resolved(&[(
            "main.py",
            "def leaf():\n    pass\n\ndef entry():\n    leaf()\n\ndef parameter(leaf):\n    leaf()\n\ndef assignment():\n    leaf = other\n    leaf()\n\ndef module_entry():\n    leaf()\n\nleaf = other\n",
        )]);
        let facts = &facts["main.py"];
        assert!(
            facts.calls.is_empty(),
            "{:?}",
            facts
                .calls
                .iter()
                .map(|call| (&call.from, &call.to))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            facts
                .issues
                .iter()
                .filter(|issue| issue.detail.contains("local Python binding"))
                .count(),
            2
        );
        assert!(
            facts
                .issues
                .iter()
                .any(|issue| issue.detail.contains("module-level Python assignment"))
        );
    }

    #[test]
    fn resolves_unique_relative_imports_and_stays_conservative_elsewhere() {
        let facts = resolved(&[
            ("pkg/util.py", "def helper():\n    pass\n\nvalue = 1\n"),
            (
                "pkg/main.py",
                "from .util import helper as h, missing\nfrom external import thing\n\ndef entry():\n    h()\n    missing()\n    thing()\n",
            ),
        ]);
        let facts = &facts["pkg/main.py"];
        assert_eq!(facts.imports, ["pkg/util.py"]);
        assert_eq!(facts.calls.len(), 1);
        assert_eq!(facts.calls[0].to, "pkg/util.py::helper");
        assert!(
            facts
                .issues
                .iter()
                .any(|issue| issue.detail.contains("absolute or package Python imports"))
        );
        assert!(
            facts
                .issues
                .iter()
                .any(|issue| issue.name == "missing" && issue.status.label() == "UNRESOLVED")
        );
    }

    #[test]
    fn relative_module_collisions_are_ambiguous() {
        let facts = resolved(&[
            ("pkg/util.py", "def helper():\n    pass\n"),
            ("pkg/util/__init__.py", "def helper():\n    pass\n"),
            (
                "pkg/main.py",
                "from .util import helper\n\ndef entry():\n    helper()\n",
            ),
        ]);
        assert!(
            facts["pkg/main.py"]
                .issues
                .iter()
                .any(|issue| issue.status.label() == "AMBIGUOUS")
        );
    }

    #[test]
    fn parent_relative_and_package_init_imports_remain_filesystem_proven() {
        let facts = resolved(&[
            ("pkg/util.py", "def parent_helper():\n    pass\n"),
            (
                "pkg/feature/__init__.py",
                "def package_helper():\n    pass\n",
            ),
            (
                "pkg/sub/main.py",
                "from ..util import parent_helper\nfrom ..feature import package_helper\n\ndef entry():\n    parent_helper()\n    package_helper()\n",
            ),
        ]);
        let calls = &facts["pkg/sub/main.py"].calls;
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].to, "pkg/feature/__init__.py::package_helper");
        assert_eq!(calls[1].to, "pkg/util.py::parent_helper");
    }

    #[test]
    fn namespace_and_wildcard_imports_do_not_become_object_receiver_edges() {
        let facts = resolved(&[(
            "main.py",
            "import package.util as util\nfrom .local import *\n\ndef entry():\n    util.helper()\n",
        )]);
        let facts = &facts["main.py"];
        assert!(facts.calls.is_empty());
        assert!(
            facts
                .issues
                .iter()
                .any(|issue| issue.detail.contains("absolute or package Python imports"))
        );
        assert!(
            facts
                .issues
                .iter()
                .any(|issue| issue.detail.contains("wildcard Python imports"))
        );
        assert!(facts
            .issues
            .iter()
            .any(|issue| issue.name == "helper" && issue.detail.contains("receiver or dynamic")));
    }

    #[test]
    fn unresolved_module_or_local_import_bindings_do_not_fall_back_to_same_named_functions() {
        let facts = resolved(&[(
            "main.py",
            "def leaf():\n    pass\n\nfrom external import leaf\n\ndef entry():\n    leaf()\n\ndef nested():\n    from external import leaf\n    leaf()\n",
        )]);
        let facts = &facts["main.py"];
        assert!(facts.calls.is_empty());
        assert_eq!(
            facts
                .issues
                .iter()
                .filter(|issue| issue
                    .detail
                    .contains("imported Python binding is not uniquely resolved"))
                .count(),
            1
        );
        assert!(
            facts
                .issues
                .iter()
                .any(|issue| issue.source == "main.py::nested"
                    && issue.detail.contains("local Python binding"))
        );
    }

    #[test]
    fn resolves_only_direct_instance_methods_on_the_declared_receiver() {
        let facts = resolved(&[(
            "worker.py",
            "class Base:\n    def inherited(self):\n        pass\n\nclass Worker(Base):\n    def leaf(self):\n        pass\n\n    def entry(me):\n        me.leaf()\n        me.inherited()\n\n    @staticmethod\n    def static():\n        self.leaf()\n",
        )]);
        let facts = &facts["worker.py"];
        assert_eq!(facts.calls.len(), 1);
        assert_eq!(facts.calls[0].from, "worker.py::Worker.entry");
        assert_eq!(facts.calls[0].to, "worker.py::Worker.leaf");
        assert!(
            facts
                .issues
                .iter()
                .any(|issue| issue.name == "inherited" && issue.detail.contains("same-class"))
        );
        assert!(
            facts
                .issues
                .iter()
                .any(|issue| issue.name == "leaf" && issue.source == "worker.py::Worker.static")
        );
    }
}
