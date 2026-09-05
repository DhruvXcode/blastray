use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use tree_sitter::{Node, Parser};

use crate::index::{RelationshipIssue, RelationshipStatus, SymbolKind};
use crate::language::{
    ParsedFile as ProviderParsedFile, ProviderContext, ResolvedCall, ResolvedFile,
};

pub(crate) const EXTENSIONS: &[&str] = &["rs"];

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct ParsedFile {
    pub path: String,
    pub symbols: Vec<SymbolDraft>,
    uses: Vec<UseDraft>,
    modules: Vec<ModuleDraft>,
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
    inherent: bool,
    instance_receiver: bool,
}

#[derive(Clone, Deserialize, Serialize)]
struct CallDraft {
    name: String,
    line: usize,
    column: usize,
    kind: CallKind,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
enum CallKind {
    Direct,
    SelfMethod,
    Other,
}

#[derive(Clone, Deserialize, Serialize)]
struct UseDraft {
    base: UseBase,
    modules: Vec<String>,
    bindings: Vec<UseBinding>,
    line: usize,
    column: usize,
    wildcard: bool,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
enum UseBase {
    Crate,
    SelfModule,
    Super,
    External,
}

#[derive(Clone, Deserialize, Serialize)]
struct UseBinding {
    local: String,
    imported: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct ModuleDraft {
    name: String,
    line: usize,
    column: usize,
    inline: bool,
}

#[derive(Clone)]
enum BindingTarget {
    Resolved(String),
    Unresolved,
    Ambiguous,
}

#[derive(Clone)]
struct ModulePosition {
    root: String,
    segments: Vec<String>,
}

pub(crate) fn supports_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| EXTENSIONS.contains(&extension))
}

pub(crate) fn parse(path: &str, source: &str) -> Result<ProviderParsedFile, String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .map_err(|error| format!("cannot configure parser for {path}: {error}"))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| format!("cannot parse {path}"))?;
    let mut parsed = ParsedFile {
        path: path.to_string(),
        symbols: Vec::new(),
        uses: Vec::new(),
        modules: Vec::new(),
    };
    let mut cursor = tree.root_node().walk();
    for child in tree.root_node().named_children(&mut cursor) {
        add_top_level(&mut parsed, child, source);
    }
    Ok(ProviderParsedFile::Rust(parsed))
}

fn add_top_level(parsed: &mut ParsedFile, node: Node<'_>, source: &str) {
    match node.kind() {
        "function_item" => add_function(parsed, node, source, None, false),
        "struct_item" | "enum_item" | "trait_item" => add_type(parsed, node, source),
        "impl_item" => add_impl(parsed, node, source),
        "use_declaration" => parsed.uses.extend(use_drafts(node, source)),
        "mod_item" => {
            if let Some(name) = field_text(node, "name", source) {
                let position = node.start_position();
                parsed.modules.push(ModuleDraft {
                    name,
                    line: position.row + 1,
                    column: position.column + 1,
                    inline: node.child_by_field_name("body").is_some(),
                });
            }
        }
        _ => {}
    }
}

fn add_type(parsed: &mut ParsedFile, node: Node<'_>, source: &str) {
    let Some(name) = field_text(node, "name", source) else {
        return;
    };
    let position = node.start_position();
    parsed.symbols.push(SymbolDraft {
        canonical: format!("{}::{name}", parsed.path),
        name,
        file: parsed.path.clone(),
        line: position.row + 1,
        end_line: node.end_position().row + 1,
        column: position.column + 1,
        kind: SymbolKind::Type,
        calls: Vec::new(),
        shadowed: BTreeSet::new(),
        owner: None,
        inherent: false,
        instance_receiver: false,
    });
}

fn add_impl(parsed: &mut ParsedFile, node: Node<'_>, source: &str) {
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };
    if type_node.kind() != "type_identifier" {
        return;
    }
    let owner = text(type_node, source).to_string();
    let inherent = node.child_by_field_name("trait").is_none();
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        if child.kind() == "function_item" {
            add_function(parsed, child, source, Some(&owner), inherent);
        }
    }
}

fn add_function(
    parsed: &mut ParsedFile,
    node: Node<'_>,
    source: &str,
    owner: Option<&str>,
    inherent: bool,
) {
    let Some(name) = field_text(node, "name", source) else {
        return;
    };
    let (calls, shadowed, instance_receiver) = callable_facts(node, source);
    let canonical_name = owner.map_or_else(|| name.clone(), |owner| format!("{owner}.{name}"));
    let position = node.start_position();
    parsed.symbols.push(SymbolDraft {
        canonical: format!("{}::{canonical_name}", parsed.path),
        name,
        file: parsed.path.clone(),
        line: position.row + 1,
        end_line: node.end_position().row + 1,
        column: position.column + 1,
        kind: if owner.is_some() {
            SymbolKind::Method
        } else {
            SymbolKind::Function
        },
        calls,
        shadowed,
        owner: owner.map(str::to_string),
        inherent,
        instance_receiver,
    });
}

fn callable_facts(node: Node<'_>, source: &str) -> (Vec<CallDraft>, BTreeSet<String>, bool) {
    let mut shadowed = BTreeSet::new();
    let mut instance_receiver = false;
    if let Some(parameters) = node.child_by_field_name("parameters") {
        let mut cursor = parameters.walk();
        for parameter in parameters.named_children(&mut cursor) {
            if parameter.kind() == "self_parameter" {
                instance_receiver = true;
            } else {
                binding_names(parameter, source, &mut shadowed);
            }
        }
    }
    let mut calls = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        collect_body_facts(body, source, &mut shadowed, &mut calls);
    }
    (calls, shadowed, instance_receiver)
}

fn collect_body_facts(
    node: Node<'_>,
    source: &str,
    shadowed: &mut BTreeSet<String>,
    calls: &mut Vec<CallDraft>,
) {
    match node.kind() {
        "function_item" | "struct_item" | "enum_item" | "trait_item" | "impl_item" | "mod_item" => {
            if let Some(name) = field_text(node, "name", source) {
                shadowed.insert(name);
            }
            return;
        }
        "let_declaration" => {
            if let Some(pattern) = node.child_by_field_name("pattern") {
                binding_names(pattern, source, shadowed);
            }
        }
        "call_expression" => calls.push(call_draft(node, source)),
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_body_facts(child, source, shadowed, calls);
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

fn call_draft(node: Node<'_>, source: &str) -> CallDraft {
    let position = node.start_position();
    let Some(function) = node.child_by_field_name("function") else {
        return CallDraft {
            name: "<unknown>".to_string(),
            line: position.row + 1,
            column: position.column + 1,
            kind: CallKind::Other,
        };
    };
    if function.kind() == "identifier" {
        return CallDraft {
            name: text(function, source).to_string(),
            line: position.row + 1,
            column: position.column + 1,
            kind: CallKind::Direct,
        };
    }
    if function.kind() == "field_expression"
        && function
            .child_by_field_name("value")
            .is_some_and(|value| value.kind() == "self")
        && let Some(field) = function.child_by_field_name("field")
    {
        return CallDraft {
            name: text(field, source).to_string(),
            line: position.row + 1,
            column: position.column + 1,
            kind: CallKind::SelfMethod,
        };
    }
    CallDraft {
        name: text(function, source).to_string(),
        line: position.row + 1,
        column: position.column + 1,
        kind: CallKind::Other,
    }
}

fn use_drafts(node: Node<'_>, source: &str) -> Vec<UseDraft> {
    let position = node.start_position();
    let mut value = text(node, source).trim().trim_end_matches(';').trim();
    if let Some(rest) = value.strip_prefix("pub ") {
        value = rest.trim_start();
    }
    let Some(value) = value.strip_prefix("use ") else {
        return Vec::new();
    };
    let (base, remainder) = if let Some(remainder) = value.strip_prefix("crate::") {
        (UseBase::Crate, remainder)
    } else if let Some(remainder) = value.strip_prefix("self::") {
        (UseBase::SelfModule, remainder)
    } else if let Some(remainder) = value.strip_prefix("super::") {
        (UseBase::Super, remainder)
    } else {
        (UseBase::External, value)
    };
    let (prefix, items) = if let Some(open) = remainder.find("::{") {
        let close = remainder.rfind('}').unwrap_or(remainder.len());
        (
            &remainder[..open],
            remainder[open + 3..close].split(',').collect(),
        )
    } else {
        ("", vec![remainder])
    };
    let prefix: Vec<String> = prefix
        .split("::")
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect();
    let mut result = Vec::new();
    for item in items {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let mut parts: Vec<String> = if prefix.is_empty() {
            item.split("::").map(str::to_string).collect()
        } else {
            let mut parts = prefix.clone();
            parts.extend(item.split("::").map(str::to_string));
            parts
        };
        let wildcard = parts.last().is_some_and(|part| part == "*");
        if wildcard {
            parts.pop();
        }
        let Some(last) = parts.pop() else {
            continue;
        };
        let (imported, local) = split_alias(&last);
        result.push(UseDraft {
            base,
            modules: parts,
            bindings: (!wildcard)
                .then_some(UseBinding { local, imported })
                .into_iter()
                .collect(),
            line: position.row + 1,
            column: position.column + 1,
            wildcard,
        });
    }
    result
}

fn split_alias(value: &str) -> (String, String) {
    let words: Vec<_> = value.split_whitespace().collect();
    let imported = words.first().copied().unwrap_or_default().to_string();
    let local = words
        .windows(2)
        .find(|words| words[0] == "as")
        .map(|words| words[1].to_string())
        .unwrap_or_else(|| imported.clone());
    (imported, local)
}

pub(crate) fn resolve(
    parsed: &BTreeMap<String, ProviderParsedFile>,
    paths: &BTreeSet<String>,
    _: &ProviderContext,
) -> BTreeMap<String, ResolvedFile> {
    let parsed: BTreeMap<String, &ParsedFile> = parsed
        .iter()
        .filter_map(|(path, file)| match file {
            ProviderParsedFile::Rust(file) => Some((path.clone(), file)),
            ProviderParsedFile::JsTs(_)
            | ProviderParsedFile::Python(_)
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
    functions: BTreeMap<(String, String), Vec<String>>,
    methods: BTreeMap<(String, String, String), Vec<MethodTarget>>,
    module_positions: BTreeMap<String, Vec<ModulePosition>>,
    module_files: BTreeMap<(String, Vec<String>), String>,
}

#[derive(Clone)]
struct MethodTarget {
    canonical: String,
    inherent: bool,
    instance_receiver: bool,
}

impl ResolveContext {
    fn new(parsed: &BTreeMap<String, &ParsedFile>) -> Self {
        let mut context = Self {
            files: parsed.keys().cloned().collect(),
            functions: BTreeMap::new(),
            methods: BTreeMap::new(),
            module_positions: BTreeMap::new(),
            module_files: BTreeMap::new(),
        };
        for file in parsed.values() {
            for symbol in &file.symbols {
                if symbol.kind == SymbolKind::Function {
                    context
                        .functions
                        .entry((file.path.clone(), symbol.name.clone()))
                        .or_default()
                        .push(symbol.canonical.clone());
                } else if symbol.kind == SymbolKind::Method
                    && let Some(owner) = &symbol.owner
                {
                    context
                        .methods
                        .entry((file.path.clone(), owner.clone(), symbol.name.clone()))
                        .or_default()
                        .push(MethodTarget {
                            canonical: symbol.canonical.clone(),
                            inherent: symbol.inherent,
                            instance_receiver: symbol.instance_receiver,
                        });
                }
            }
        }
        let roots: Vec<_> = parsed
            .keys()
            .filter(|path| is_crate_root(path))
            .cloned()
            .collect();
        for root in roots {
            context.add_module_position(parsed, &root, &root, Vec::new(), &mut BTreeSet::new());
        }
        context
    }

    fn add_module_position(
        &mut self,
        parsed: &BTreeMap<String, &ParsedFile>,
        root: &str,
        path: &str,
        segments: Vec<String>,
        visited: &mut BTreeSet<(String, String)>,
    ) {
        if !visited.insert((root.to_string(), path.to_string())) {
            return;
        }
        self.module_positions
            .entry(path.to_string())
            .or_default()
            .push(ModulePosition {
                root: root.to_string(),
                segments: segments.clone(),
            });
        self.module_files
            .insert((root.to_string(), segments.clone()), path.to_string());
        let Some(file) = parsed.get(path) else {
            return;
        };
        for module in &file.modules {
            if module.inline {
                continue;
            }
            let candidates = child_module_candidates(path, &module.name, &self.files);
            if let [child] = candidates.as_slice() {
                let mut child_segments = segments.clone();
                child_segments.push(module.name.clone());
                self.add_module_position(parsed, root, child, child_segments, visited);
            }
        }
    }

    fn module_targets(&self, file: &str, base: UseBase, modules: &[String]) -> Vec<String> {
        if matches!(base, UseBase::External) {
            return Vec::new();
        }
        let mut targets = BTreeSet::new();
        for position in self.module_positions.get(file).into_iter().flatten() {
            let mut segments = match base {
                UseBase::Crate => Vec::new(),
                UseBase::SelfModule => position.segments.clone(),
                UseBase::Super => {
                    let mut segments = position.segments.clone();
                    if segments.pop().is_none() {
                        continue;
                    }
                    segments
                }
                UseBase::External => Vec::new(),
            };
            segments.extend(modules.iter().cloned());
            if let Some(target) = self.module_files.get(&(position.root.clone(), segments)) {
                targets.insert(target.clone());
            }
        }
        targets.into_iter().collect()
    }
}

fn resolve_file(file: &ParsedFile, context: &ResolveContext) -> ResolvedFile {
    let mut result = ResolvedFile::default();
    let mut bindings = BTreeMap::new();
    for module in &file.modules {
        if module.inline {
            result.issues.push(issue(
                RelationshipStatus::Unresolved,
                &file.path,
                module.line,
                module.column,
                &module.name,
                "inline Rust modules are outside the first Rust subset",
            ));
            continue;
        }
        match child_module_candidates(&file.path, &module.name, &context.files).as_slice() {
            [target] => result.imports.push(target.clone()),
            [] => result.issues.push(issue(
                RelationshipStatus::Unresolved,
                &file.path,
                module.line,
                module.column,
                &module.name,
                "declared Rust module source file was not found",
            )),
            _ => result.issues.push(issue(
                RelationshipStatus::Ambiguous,
                &file.path,
                module.line,
                module.column,
                &module.name,
                "more than one Rust module source file matches this declaration",
            )),
        }
    }
    for use_item in &file.uses {
        resolve_use(use_item, file, context, &mut bindings, &mut result);
    }
    for symbol in &file.symbols {
        if !matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method) {
            continue;
        }
        for call in &symbol.calls {
            match call.kind {
                CallKind::SelfMethod => resolve_self_method(symbol, call, file, context, &mut result),
                CallKind::Direct => resolve_direct_call(symbol, call, file, context, &bindings, &mut result),
                CallKind::Other => result.issues.push(issue(
                    RelationshipStatus::Unresolved,
                    &symbol.canonical,
                    call.line,
                    call.column,
                    &call.name,
                    "receiver, associated, macro, or dynamic Rust call syntax is outside the first Rust subset",
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

fn resolve_use(
    use_item: &UseDraft,
    file: &ParsedFile,
    context: &ResolveContext,
    bindings: &mut BTreeMap<String, Vec<BindingTarget>>,
    result: &mut ResolvedFile,
) {
    if use_item.wildcard {
        result.issues.push(issue(
            RelationshipStatus::Unresolved,
            &file.path,
            use_item.line,
            use_item.column,
            "*",
            "glob Rust use imports are outside the first Rust subset",
        ));
        return;
    }
    if matches!(use_item.base, UseBase::External) {
        result.issues.push(issue(
            RelationshipStatus::Unresolved,
            &file.path,
            use_item.line,
            use_item.column,
            "use",
            "unrooted or external Rust use path is outside the first Rust subset",
        ));
        for binding in &use_item.bindings {
            bindings
                .entry(binding.local.clone())
                .or_default()
                .push(BindingTarget::Unresolved);
        }
        return;
    }
    let targets = context.module_targets(&file.path, use_item.base, &use_item.modules);
    match targets.as_slice() {
        [target] => {
            result.imports.push(target.clone());
            for binding in &use_item.bindings {
                let target = match context
                    .functions
                    .get(&(target.clone(), binding.imported.clone()))
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
                {
                    [symbol] => BindingTarget::Resolved(symbol.clone()),
                    [] => BindingTarget::Unresolved,
                    _ => BindingTarget::Ambiguous,
                };
                if !matches!(target, BindingTarget::Resolved(_)) {
                    result.issues.push(issue(
                        if matches!(target, BindingTarget::Ambiguous) {
                            RelationshipStatus::Ambiguous
                        } else {
                            RelationshipStatus::Unresolved
                        },
                        &file.path,
                        use_item.line,
                        use_item.column,
                        &binding.local,
                        "the imported Rust symbol was not uniquely a supported function in the resolved module",
                    ));
                }
                bindings
                    .entry(binding.local.clone())
                    .or_default()
                    .push(target);
            }
        }
        [] => {
            result.issues.push(issue(
                RelationshipStatus::Unresolved,
                &file.path,
                use_item.line,
                use_item.column,
                "use",
                "rooted Rust use path did not resolve to one declared local module",
            ));
            for binding in &use_item.bindings {
                bindings
                    .entry(binding.local.clone())
                    .or_default()
                    .push(BindingTarget::Unresolved);
            }
        }
        _ => {
            result.issues.push(issue(
                RelationshipStatus::Ambiguous,
                &file.path,
                use_item.line,
                use_item.column,
                "use",
                "rooted Rust use path matches more than one local module",
            ));
            for binding in &use_item.bindings {
                bindings
                    .entry(binding.local.clone())
                    .or_default()
                    .push(BindingTarget::Ambiguous);
            }
        }
    }
}

fn resolve_direct_call(
    symbol: &SymbolDraft,
    call: &CallDraft,
    file: &ParsedFile,
    context: &ResolveContext,
    bindings: &BTreeMap<String, Vec<BindingTarget>>,
    result: &mut ResolvedFile,
) {
    if symbol.shadowed.contains(&call.name) {
        result.issues.push(issue(
            RelationshipStatus::Unresolved,
            &symbol.canonical,
            call.line,
            call.column,
            &call.name,
            "an obvious local Rust binding could shadow this name",
        ));
        return;
    }
    let mut candidates = if let Some(entries) = bindings.get(&call.name) {
        binding_targets(entries, &mut result.issues, &symbol.canonical, call)
    } else {
        context
            .functions
            .get(&(file.path.clone(), call.name.clone()))
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
        [] if !bindings.contains_key(&call.name) => result.issues.push(issue(
            RelationshipStatus::Unresolved,
            &symbol.canonical,
            call.line,
            call.column,
            &call.name,
            "no matching local Rust function or resolved use binding",
        )),
        [] => {}
        _ => result.issues.push(issue(
            RelationshipStatus::Ambiguous,
            &symbol.canonical,
            call.line,
            call.column,
            &call.name,
            "multiple callable Rust definitions match this name",
        )),
    }
}

fn resolve_self_method(
    symbol: &SymbolDraft,
    call: &CallDraft,
    file: &ParsedFile,
    context: &ResolveContext,
    result: &mut ResolvedFile,
) {
    let Some(owner) = &symbol.owner else {
        result.issues.push(issue(
            RelationshipStatus::Unresolved,
            &symbol.canonical,
            call.line,
            call.column,
            &call.name,
            "Rust self method call is outside an inherent implementation",
        ));
        return;
    };
    if !symbol.inherent || !symbol.instance_receiver {
        result.issues.push(issue(
            RelationshipStatus::Unresolved,
            &symbol.canonical,
            call.line,
            call.column,
            &call.name,
            "Rust self method call lacks a proven inherent instance receiver",
        ));
        return;
    }
    let candidates: Vec<_> = context
        .methods
        .get(&(file.path.clone(), owner.clone(), call.name.clone()))
        .into_iter()
        .flatten()
        .filter(|target| target.inherent && target.instance_receiver)
        .map(|target| target.canonical.clone())
        .collect();
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
            "no matching direct inherent Rust method",
        )),
        _ => result.issues.push(issue(
            RelationshipStatus::Ambiguous,
            &symbol.canonical,
            call.line,
            call.column,
            &call.name,
            "multiple direct inherent Rust methods match this self call",
        )),
    }
}

fn binding_targets(
    entries: &[BindingTarget],
    issues: &mut Vec<RelationshipIssue>,
    source: &str,
    call: &CallDraft,
) -> Vec<String> {
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
            "the imported Rust binding is not uniquely resolved",
        ));
        return Vec::new();
    }
    targets
}

fn is_crate_root(path: &str) -> bool {
    path.ends_with("/lib.rs")
        || path.ends_with("/main.rs")
        || path == "lib.rs"
        || path == "main.rs"
        || path.starts_with("src/bin/") && path.matches('/').count() == 2
}

fn child_module_candidates(path: &str, name: &str, files: &BTreeSet<String>) -> Vec<String> {
    let source = Path::new(path);
    let parent = source.parent().unwrap_or_else(|| Path::new(""));
    let stem = source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    let directory = if matches!(stem, "lib" | "main" | "mod") {
        parent.to_path_buf()
    } else {
        parent.join(stem)
    };
    let mut candidates = BTreeSet::new();
    for candidate in [
        directory.join(format!("{name}.rs")),
        directory.join(name).join("mod.rs"),
    ] {
        let candidate = candidate.to_string_lossy().replace('\\', "/");
        if files.contains(&candidate) {
            candidates.insert(candidate);
        }
    }
    candidates.into_iter().collect()
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
        resolve(
            &parsed,
            &parsed.keys().cloned().collect::<BTreeSet<_>>(),
            &crate::language::ProviderContext::default(),
        )
    }

    #[test]
    fn extracts_functions_types_and_impl_methods() {
        let parsed = parse(
            "src/lib.rs",
            "pub async fn work() {}\nstruct Item {}\nenum State { Ready }\ntrait Store {}\nimpl Item { fn first(&self) {} }\nimpl Item { fn second(self) {} }\n",
        )
        .unwrap();
        let symbols = parsed.symbols();
        let names: Vec<_> = symbols
            .iter()
            .map(|symbol| symbol.canonical.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "src/lib.rs::work",
                "src/lib.rs::Item",
                "src/lib.rs::State",
                "src/lib.rs::Store",
                "src/lib.rs::Item.first",
                "src/lib.rs::Item.second"
            ]
        );
        assert_eq!(symbols[1].kind, crate::index::SymbolKind::Type);
        assert_eq!(symbols[4].kind, crate::index::SymbolKind::Method);
    }

    #[test]
    fn resolves_local_use_and_self_method_calls() {
        let facts = resolved(&[
            (
                "src/lib.rs",
                "mod util;\nuse crate::util::leaf as imported;\nfn entry() { imported(); }\nstruct Worker;\nimpl Worker { fn leaf(&self) {} fn entry(&mut self) { self.leaf(); self.leaf(); } }\n",
            ),
            ("src/util.rs", "pub fn leaf() {}\n"),
        ]);
        let calls = &facts["src/lib.rs"].calls;
        assert_eq!(calls.len(), 3);
        assert!(calls.iter().any(|call| call.to == "src/util.rs::leaf"));
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.to == "src/lib.rs::Worker.leaf")
                .count(),
            2
        );
    }

    #[test]
    fn keeps_shadowed_external_and_trait_dispatch_unresolved() {
        let facts = resolved(&[(
            "src/lib.rs",
            "fn leaf() {}\nfn entry(leaf: u8) { leaf(); }\nuse serde::Thing;\nstruct Worker; trait Run { fn leaf(&self); } impl Run for Worker { fn leaf(&self) {} fn entry(&self) { self.leaf(); } }\n",
        )]);
        assert!(facts["src/lib.rs"].calls.is_empty());
        assert!(
            facts["src/lib.rs"]
                .issues
                .iter()
                .any(|issue| issue.detail.contains("shadow"))
        );
        assert!(
            facts["src/lib.rs"]
                .issues
                .iter()
                .any(|issue| issue.detail.contains("unrooted or external"))
        );
        assert!(
            facts["src/lib.rs"]
                .issues
                .iter()
                .any(|issue| issue.detail.contains("inherent"))
        );
    }

    #[test]
    fn module_layouts_are_conservative() {
        let facts = resolved(&[
            ("src/lib.rs", "mod one; mod two;\n"),
            ("src/one.rs", "pub fn a() {}\n"),
            ("src/two/mod.rs", "pub fn b() {}\n"),
            ("src/two.rs", "pub fn c() {}\n"),
        ]);
        assert!(
            facts["src/lib.rs"]
                .imports
                .contains(&"src/one.rs".to_string())
        );
        assert!(
            facts["src/lib.rs"]
                .issues
                .iter()
                .any(|issue| issue.status == crate::index::RelationshipStatus::Ambiguous)
        );
    }

    #[test]
    fn grouped_self_and_super_use_paths_resolve_only_declared_modules() {
        let facts = resolved(&[
            (
                "src/lib.rs",
                "mod util; mod parent; use crate::util::{one as first, two}; fn entry() { first(); two(); }\n",
            ),
            ("src/util.rs", "pub fn one() {} pub fn two() {}\n"),
            (
                "src/parent/mod.rs",
                "mod child; pub fn leaf() {} fn entry() { self::child::run(); }\n",
            ),
            (
                "src/parent/child.rs",
                "use super::leaf; pub fn run() { leaf(); }\n",
            ),
        ]);
        let lib_targets: BTreeSet<_> = facts["src/lib.rs"]
            .calls
            .iter()
            .map(|call| call.to.as_str())
            .collect();
        assert_eq!(
            lib_targets,
            BTreeSet::from(["src/util.rs::one", "src/util.rs::two"])
        );
        assert!(
            facts["src/parent/child.rs"]
                .calls
                .iter()
                .any(|call| call.to == "src/parent/mod.rs::leaf")
        );
        assert!(
            facts["src/parent/mod.rs"]
                .issues
                .iter()
                .any(|issue| issue.name.contains("self::child::run"))
        );
    }

    #[test]
    fn macros_associated_calls_and_object_receivers_do_not_become_edges() {
        let facts = resolved(&[(
            "src/lib.rs",
            "struct Worker; impl Worker { fn leaf(&self) {} fn entry(&self, other: Worker) { println!(\"x\"); Worker::helper(); other.leaf(); } fn helper() {} }\n",
        )]);
        assert!(facts["src/lib.rs"].calls.is_empty());
        assert!(
            facts["src/lib.rs"]
                .issues
                .iter()
                .any(|issue| issue.detail.contains("receiver, associated, macro"))
        );
    }

    #[test]
    fn self_call_does_not_target_an_associated_function() {
        let facts = resolved(&[(
            "src/lib.rs",
            "struct Worker; impl Worker { fn helper() {} fn entry(&self) { self.helper(); } }\n",
        )]);
        assert!(facts["src/lib.rs"].calls.is_empty());
        assert!(facts["src/lib.rs"].issues.iter().any(|issue| {
            issue
                .detail
                .contains("no matching direct inherent Rust method")
        }));
    }
}
