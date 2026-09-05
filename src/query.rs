use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::index::{DependencyEdge, DependencyKind, Graph, RelationshipIssue, SearchFact, Symbol};

const MAX_TRAVERSAL: usize = 500;
const FIND_LIMIT: usize = 20;
const INSPECT_LIST_LIMIT: usize = 8;
const INSPECT_TEST_LIMIT: usize = 3;
const INSPECT_SOURCE_LINES: usize = 24;
const INSPECT_SOURCE_LINE_CHARS: usize = 180;

pub(crate) struct ReverseImpact {
    pub by_depth: BTreeMap<usize, Vec<ImpactNode>>,
    pub truncated: bool,
}

#[derive(Clone)]
pub(crate) struct ImpactNode {
    pub symbol: usize,
    pub via: DependencyEdge,
}

pub(crate) fn reverse_impact(graph: &Graph, roots: &BTreeSet<usize>) -> ReverseImpact {
    let mut queue: VecDeque<(usize, usize)> = roots.iter().map(|root| (*root, 0)).collect();
    let mut visited = roots.clone();
    let mut by_depth = BTreeMap::new();

    while let Some((current, depth)) = queue.pop_front() {
        if visited.len() >= MAX_TRAVERSAL {
            break;
        }
        for edge in graph.dependents(current) {
            if visited.insert(edge.from) {
                by_depth
                    .entry(depth + 1)
                    .or_insert_with(Vec::new)
                    .push(ImpactNode {
                        symbol: edge.from,
                        via: edge.clone(),
                    });
                queue.push_back((edge.from, depth + 1));
            }
        }
    }
    ReverseImpact {
        by_depth,
        truncated: visited.len() >= MAX_TRAVERSAL,
    }
}

pub fn find(graph: &Graph, query: &str) -> String {
    let query_tokens = discovery_terms(query);
    let mut matches = discovery_matches(graph, query, &query_tokens);

    if matches.is_empty() {
        return format!("No symbols found for '{query}'.");
    }

    matches.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| right.matched_tokens.cmp(&left.matched_tokens))
            .then_with(|| left.symbol.canonical.cmp(&right.symbol.canonical))
    });
    let total = matches.len();
    let shown = total.min(FIND_LIMIT);
    let mut output = String::new();
    if shown < total {
        output.push_str(&format!(
            "Showing {shown} of {total} matches; refine the query.\n"
        ));
    }
    output.push_str(&format!("{shown} symbol(s):\n"));
    for found in matches.into_iter().take(FIND_LIMIT) {
        output.push_str(&symbol_line(graph, found.symbol));
        output.push_str(&format!(" [{}]", found.reason));
        output.push('\n');
    }
    output.trim_end().to_string()
}

struct FindMatch<'a> {
    symbol: &'a Symbol,
    score: i64,
    matched_tokens: usize,
    reason: String,
}

fn discovery_matches<'a>(
    graph: &'a Graph,
    query: &str,
    query_tokens: &[String],
) -> Vec<FindMatch<'a>> {
    if query_tokens.is_empty() {
        return graph
            .symbols
            .iter()
            .map(|symbol| FindMatch {
                symbol,
                score: 1,
                matched_tokens: 0,
                reason: "all symbols".to_owned(),
            })
            .collect();
    }
    let document_frequency: Vec<usize> = query_tokens
        .iter()
        .map(|term| {
            graph
                .search
                .iter()
                .filter(|fact| fact_matches(fact, term))
                .count()
        })
        .collect();
    let document_count = graph.symbols.len() as i64;
    let query_lower = query.trim().to_lowercase();
    let compact_identifier = !query.trim().is_empty()
        && query.trim().chars().all(char::is_alphanumeric)
        && query_tokens.len() > 1
        && !graph.symbols.iter().any(|symbol| {
            symbol.canonical.eq_ignore_ascii_case(&query_lower)
                || symbol.name.eq_ignore_ascii_case(&query_lower)
        });
    let mut matches: Vec<_> = graph
        .symbols
        .iter()
        .enumerate()
        .filter_map(|(id, symbol)| {
            let exact = if symbol.canonical.to_lowercase() == query_lower {
                Some("exact identity")
            } else if symbol.name == query || symbol.name.to_lowercase() == query_lower {
                Some("exact name")
            } else {
                None
            };
            let fact = graph.search.get(id)?;
            let mut score = 0_i64;
            let mut matched = 0;
            let mut reasons = Vec::new();
            for (position, term) in query_tokens.iter().enumerate() {
                let fields = matching_fields(fact, term);
                if fields.is_empty() {
                    continue;
                }
                matched += 1;
                let rarity =
                    (document_count + 1) * 1_000 / (document_frequency[position] as i64 + 1);
                let weight = fields.iter().map(|field| field.weight()).max().unwrap_or(0);
                score += rarity * weight;
                for field in fields {
                    let label = field.label();
                    if !reasons.contains(&label) {
                        reasons.push(label);
                    }
                }
            }
            let minimum_task_terms = if query_tokens.len() >= 3 { 2 } else { 1 };
            if (matched < minimum_task_terms
                || (compact_identifier && matched < query_tokens.len()))
                && exact.is_none()
            {
                return None;
            }
            // Reward a coherent task match more than a single accidental word.
            score += (matched * matched * 2_000) as i64 / query_tokens.len() as i64;
            if let Some(reason) = exact {
                score += 1_000_000_000;
                reasons.insert(0, reason);
            }
            Some(FindMatch {
                symbol,
                score,
                matched_tokens: matched,
                reason: reasons.join(" + "),
            })
        })
        .collect();

    // Confirmed CALLS are optional relevance evidence only. They never add a
    // candidate or alter the graph; they merely nudge a textual candidate near
    // several strong textual candidates upward.
    let strong_by_symbol: BTreeMap<_, _> = matches
        .iter()
        .map(|found| {
            let id = graph.symbol_candidates(&found.symbol.canonical)[0];
            (
                id,
                found.score >= 1_000_000_000 || found.matched_tokens * 2 >= query_tokens.len(),
            )
        })
        .collect();
    let score_by_symbol: BTreeMap<_, _> = matches
        .iter()
        .map(|found| {
            let id = graph.symbol_candidates(&found.symbol.canonical)[0];
            (id, found.score)
        })
        .collect();
    for found in &mut matches {
        let id = graph.symbol_candidates(&found.symbol.canonical)[0];
        let neighbors = graph.callers(id).iter().chain(graph.callees(id));
        let boost: i64 = neighbors
            .filter(|neighbor| strong_by_symbol.get(neighbor).copied().unwrap_or(false))
            .map(|neighbor| score_by_symbol.get(neighbor).copied().unwrap_or(0) / 12)
            .take(3)
            .sum::<i64>()
            .min(20_000);
        if boost > 0 && !strong_by_symbol.get(&id).copied().unwrap_or(false) {
            found.score += boost;
            found.reason.push_str(" + structural-neighborhood boost");
        }
    }
    matches
}

#[derive(Clone, Copy)]
enum SearchField {
    Identifier,
    Path,
    Declaration,
    Comments,
    Strings,
    Body,
    Test,
}

impl SearchField {
    fn weight(self) -> i64 {
        match self {
            Self::Identifier => 220,
            Self::Path => 130,
            Self::Declaration => 55,
            Self::Comments => 190,
            Self::Strings => 75,
            Self::Body => 8,
            Self::Test => 60,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Identifier => "identifier match",
            Self::Path => "path match",
            Self::Declaration => "declaration match",
            Self::Comments => "comment/doc match",
            Self::Strings => "string evidence",
            Self::Body => "source evidence",
            Self::Test => "test evidence",
        }
    }
}

fn matching_fields(fact: &SearchFact, term: &str) -> Vec<SearchField> {
    [
        (SearchField::Identifier, &fact.identifier),
        (SearchField::Path, &fact.path),
        (SearchField::Declaration, &fact.declaration),
        (SearchField::Comments, &fact.comments),
        (SearchField::Strings, &fact.strings),
        (SearchField::Body, &fact.body),
        (SearchField::Test, &fact.test),
    ]
    .into_iter()
    .filter_map(|(field, values)| {
        values
            .iter()
            .any(|value| term_match(term, value))
            .then_some(field)
    })
    .collect()
}

fn fact_matches(fact: &SearchFact, term: &str) -> bool {
    !matching_fields(fact, term).is_empty()
}

fn term_match(query: &str, value: &str) -> bool {
    query == value || (query.len() >= 4 && (value.starts_with(query) || query.starts_with(value)))
}

fn discovery_terms(value: &str) -> Vec<String> {
    let mut terms = BTreeSet::new();
    for token in tokens(value) {
        if !is_query_stop_word(&token) {
            terms.insert(stem_query_token(&token));
        }
    }
    terms.into_iter().collect()
}

fn stem_query_token(token: &str) -> String {
    if token.len() > 5 && token.ends_with("ies") {
        format!("{}y", &token[..token.len() - 3])
    } else if token.len() > 6 && token.ends_with("ation") {
        token[..token.len() - 5].to_owned()
    } else if token.len() > 5 && token.ends_with("ing") {
        token[..token.len() - 3].to_owned()
    } else if token.len() > 4 && token.ends_with("ed") {
        token[..token.len() - 2].to_owned()
    } else if token.len() > 3 && token.ends_with('s') {
        token[..token.len() - 1].to_owned()
    } else {
        token.to_owned()
    }
}

fn is_query_stop_word(token: &str) -> bool {
    matches!(
        token,
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

fn tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut previous_lowercase = false;
    for character in value.chars() {
        if !character.is_alphanumeric() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            previous_lowercase = false;
            continue;
        }
        if character.is_uppercase() && previous_lowercase && !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
        previous_lowercase = character.is_lowercase();
        current.extend(character.to_lowercase());
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens.sort();
    tokens.dedup();
    tokens
}

pub fn inspect(graph: &Graph, target: &str) -> Result<String, String> {
    inspect_with_source(graph, target, None)
}

pub fn inspect_with_source(
    graph: &Graph,
    target: &str,
    source: Option<&str>,
) -> Result<String, String> {
    let target = select(graph, target)?;
    let symbol = &graph.symbols[target];
    let callers = graph.callers(target);
    let callees = graph.callees(target);
    let issues = issues_for(graph, symbol);
    let defining_file = graph
        .defining_file(target)
        .expect("every indexed symbol has one DEFINES edge");
    let imports = graph.imported_files(defining_file);
    let mut output = format!(
        "{}\nDefining file: {}",
        symbol_line(graph, symbol),
        graph.files[defining_file].path
    );
    append_source_context(&mut output, source, symbol);
    output.push_str("\nDirect callers:");
    append_callers(&mut output, graph, target, callers);
    output.push_str("\nDirect callees:");
    append_callees(&mut output, graph, target, callees);
    output.push_str("\nDefining file imports:");
    append_limited_files(&mut output, graph, imports, INSPECT_LIST_LIMIT);
    append_likely_tests(&mut output, graph, target);
    output.push_str("\nUnresolved or ambiguous outgoing calls:");
    if issues.is_empty() {
        output.push_str(" none");
    } else {
        for issue in issues.iter().take(INSPECT_LIST_LIMIT) {
            output.push_str(&format!(
                "\n- {} {}:{} call '{}' — {}",
                issue.status.label(),
                issue.line,
                issue.column,
                issue.name,
                issue.detail
            ));
        }
        append_remaining(&mut output, issues.len(), INSPECT_LIST_LIMIT, "boundaries");
    }
    Ok(output)
}

pub fn trace(graph: &Graph, from: &str, to: &str) -> Result<String, String> {
    let from = select(graph, from)?;
    let to = select(graph, to)?;
    let mut queue = VecDeque::from([from]);
    let mut previous = BTreeMap::new();
    let mut visited = BTreeSet::from([from]);

    while let Some(current) = queue.pop_front() {
        if current == to || visited.len() >= MAX_TRAVERSAL {
            break;
        }
        for &next in graph.callees(current) {
            if visited.insert(next) {
                previous.insert(next, current);
                queue.push_back(next);
            }
        }
    }

    if !visited.contains(&to) {
        return Ok(format!(
            "No known CALLS path from {} to {}. Unresolved and ambiguous calls are excluded, so this does not prove no runtime path exists.",
            graph.symbols[from].canonical, graph.symbols[to].canonical
        ));
    }

    let mut path = vec![to];
    while let Some(parent) = previous.get(path.last().expect("path is non-empty")) {
        path.push(*parent);
    }
    path.reverse();
    let mut output = String::from("Known CALLS path:\n");
    for (index, symbol) in path.iter().enumerate() {
        if index > 0 {
            output.push_str(" -> ");
        }
        output.push_str(&graph.symbols[*symbol].canonical);
        if index + 1 < path.len() {
            output.push('\n');
        }
    }
    output.push_str("\nOnly RESOLVED calls participate in this path.");
    output.push_str("\nCall-site evidence:");
    for hop in path.windows(2) {
        output.push_str(&format!(
            "\n- {} -> {}{}",
            graph.symbols[hop[0]].canonical,
            graph.symbols[hop[1]].canonical,
            call_sites(graph, hop[0], hop[1])
        ));
    }
    Ok(output)
}

pub fn impact(graph: &Graph, target: &str) -> Result<String, String> {
    let target = select(graph, target)?;
    let roots = BTreeSet::from([target]);
    let result = reverse_impact(graph, &roots);

    let direct = result.by_depth.get(&1).cloned().unwrap_or_default();
    let mut output = format!(
        "Confirmed impact for {}\nDirect confirmed dependents:",
        graph.symbols[target].canonical
    );
    append_impact_symbols(&mut output, graph, &direct);
    if !direct.is_empty() {
        output.push_str("\nDirect dependency evidence:");
        for dependent in &direct {
            output.push_str(&format!(
                "\n- {} -> {} {}{}",
                graph.symbols[dependent.symbol].canonical,
                dependent.via.kind.label(),
                graph.symbols[dependent.via.to].canonical,
                dependency_site(graph, &dependent.via)
            ));
        }
    }
    output.push_str("\nTransitive confirmed dependents:");
    let mut has_transitive = false;
    for (depth, symbols) in result.by_depth.range(2..) {
        has_transitive = true;
        output.push_str(&format!("\nDepth {depth}:"));
        append_impact_symbols(&mut output, graph, symbols);
    }
    if !has_transitive {
        output.push_str(" none");
    }
    output.push_str(&format!(
        "\nTotal confirmed affected symbols: {}",
        result.by_depth.values().map(Vec::len).sum::<usize>()
    ));
    if result.truncated {
        output.push_str(&format!(" (truncated at {MAX_TRAVERSAL})"));
    }

    let potentially_relevant: Vec<&RelationshipIssue> = graph
        .issues
        .iter()
        .filter(|issue| issue.name == graph.symbols[target].name)
        .collect();
    if potentially_relevant.is_empty() {
        output.push_str("\nCompleteness: no unresolved or ambiguous supported relationship name-matches this symbol; dynamic, framework, type-system, and unsupported structural dependencies remain outside the proven graph.");
    } else {
        output.push_str("\nCompleteness: conservative/incomplete; unresolved or ambiguous supported relationships could conceal dependents:");
        for issue in potentially_relevant {
            output.push_str(&format!(
                "\n- {} {}:{}:{} '{}' — {}",
                issue.status.label(),
                issue.source,
                issue.line,
                issue.column,
                issue.name,
                issue.detail
            ));
        }
    }
    Ok(output)
}

fn append_impact_symbols(output: &mut String, graph: &Graph, symbols: &[ImpactNode]) {
    if symbols.is_empty() {
        output.push_str(" none");
        return;
    }
    for node in symbols {
        output.push_str(&format!(
            "\n- {}",
            symbol_line(graph, &graph.symbols[node.symbol])
        ));
    }
}

pub(crate) fn dependency_evidence(graph: &Graph, edge: &DependencyEdge) -> String {
    format!(
        "{} -> {} {}{}",
        graph.symbols[edge.from].canonical,
        edge.kind.label(),
        graph.symbols[edge.to].canonical,
        dependency_site(graph, edge)
    )
}

fn dependency_site(graph: &Graph, edge: &DependencyEdge) -> String {
    if edge.kind == DependencyKind::Calls {
        return call_sites(graph, edge.from, edge.to);
    }
    let source = &graph.files[graph.symbols[edge.from].file].path;
    format!(" [declared at {source}:{}:{}]", edge.line, edge.column)
}

fn select(graph: &Graph, selector: &str) -> Result<usize, String> {
    let candidates = graph.symbol_candidates(selector);
    match candidates.as_slice() {
        [symbol] => Ok(*symbol),
        [] => Err(format!("No symbol matches '{selector}'.")),
        _ => {
            let mut output = format!("Target '{selector}' is ambiguous. Use a canonical selector:");
            for symbol in candidates {
                output.push_str(&format!(
                    "\n- {}",
                    symbol_line(graph, &graph.symbols[symbol])
                ));
            }
            Err(output)
        }
    }
}

fn issues_for<'a>(graph: &'a Graph, symbol: &Symbol) -> Vec<&'a RelationshipIssue> {
    graph
        .issues
        .iter()
        .filter(|issue| issue.source == symbol.canonical)
        .collect()
}

fn append_callers(output: &mut String, graph: &Graph, target: usize, callers: &[usize]) {
    if callers.is_empty() {
        output.push_str(" none");
        return;
    }
    for caller in callers.iter().take(INSPECT_LIST_LIMIT) {
        output.push_str(&format!(
            "\n- {}{}",
            symbol_line(graph, &graph.symbols[*caller]),
            call_sites(graph, *caller, target)
        ));
    }
    append_remaining(output, callers.len(), INSPECT_LIST_LIMIT, "callers");
}

fn append_callees(output: &mut String, graph: &Graph, source: usize, callees: &[usize]) {
    if callees.is_empty() {
        output.push_str(" none");
        return;
    }
    for callee in callees.iter().take(INSPECT_LIST_LIMIT) {
        output.push_str(&format!(
            "\n- {}{}",
            symbol_line(graph, &graph.symbols[*callee]),
            call_sites(graph, source, *callee)
        ));
    }
    append_remaining(output, callees.len(), INSPECT_LIST_LIMIT, "callees");
}

fn call_sites(graph: &Graph, from: usize, to: usize) -> String {
    let source = &graph.files[graph.symbols[from].file].path;
    let sites = graph.call_sites(from, to);
    match sites {
        [] => String::new(),
        [site] => format!(" [call at {source}:{}:{}]", site.line, site.column),
        _ => format!(
            " [calls at {}{}]",
            sites
                .iter()
                .take(INSPECT_LIST_LIMIT)
                .map(|site| format!("{source}:{}:{}", site.line, site.column))
                .collect::<Vec<_>>()
                .join(", "),
            if sites.len() > INSPECT_LIST_LIMIT {
                format!(", … {} more", sites.len() - INSPECT_LIST_LIMIT)
            } else {
                String::new()
            }
        ),
    }
}

fn append_limited_files(output: &mut String, graph: &Graph, files: &[usize], limit: usize) {
    if files.is_empty() {
        output.push_str(" none");
        return;
    }
    for file in files.iter().take(limit) {
        output.push_str(&format!("\n- {}", graph.files[*file].path));
    }
    append_remaining(output, files.len(), limit, "imports");
}

fn append_remaining(output: &mut String, total: usize, shown: usize, label: &str) {
    if total > shown {
        output.push_str(&format!("\n- … {} more {label}", total - shown));
    }
}

fn append_source_context(output: &mut String, source: Option<&str>, symbol: &Symbol) {
    let Some(source) = source else {
        output.push_str("\nSource context: unavailable");
        return;
    };
    let lines: Vec<&str> = source.lines().collect();
    let start = symbol.line.saturating_sub(1);
    if start >= lines.len() {
        output.push_str("\nSource context: unavailable (symbol span is outside current source)");
        return;
    }
    let mut context_start = start;
    while context_start > 0 && start - context_start < 3 {
        let previous = lines[context_start - 1].trim_start();
        if previous.starts_with("//")
            || previous.starts_with("/*")
            || previous.starts_with('*')
            || previous.ends_with("*/")
            || previous.starts_with('#')
            || previous.starts_with("\"\"\"")
            || previous.starts_with("'''")
        {
            context_start -= 1;
        } else {
            break;
        }
    }
    let end = symbol.end_line.min(lines.len());
    let total = end.saturating_sub(context_start);
    output.push_str(&format!(
        "\nSource context ({}-{}):",
        context_start + 1,
        end
    ));
    let shown_head = INSPECT_SOURCE_LINES.saturating_sub(4).min(total);
    append_source_range(output, &lines, context_start, context_start + shown_head);
    if total > INSPECT_SOURCE_LINES {
        output.push_str(&format!(
            "\n  … {} source lines omitted …",
            total - INSPECT_SOURCE_LINES
        ));
        append_source_range(output, &lines, end.saturating_sub(4), end);
    } else {
        append_source_range(output, &lines, context_start + shown_head, end);
    }
}

fn append_source_range(output: &mut String, lines: &[&str], start: usize, end: usize) {
    for (line, source) in lines.iter().enumerate().skip(start).take(end - start) {
        append_source_line(output, line + 1, source);
    }
}

fn append_source_line(output: &mut String, line: usize, source: &str) {
    let compact = source.trim_end();
    let text: String = compact.chars().take(INSPECT_SOURCE_LINE_CHARS).collect();
    output.push_str(&format!("\n{line:>5} | {text}"));
    if compact.chars().count() > INSPECT_SOURCE_LINE_CHARS {
        output.push('…');
    }
}

fn append_likely_tests(output: &mut String, graph: &Graph, target: usize) {
    if is_test_symbol(graph, target) {
        return;
    }
    let target_symbol = &graph.symbols[target];
    let target_terms = &graph.search[target].identifier;
    let mut tests: Vec<(usize, usize, Vec<&'static str>)> = graph
        .symbols
        .iter()
        .enumerate()
        .filter(|(candidate, _)| *candidate != target && is_test_symbol(graph, *candidate))
        .filter_map(|(candidate, symbol)| {
            let mut score = 0;
            let mut reasons = Vec::new();
            if symbol.file == target_symbol.file {
                score += 100;
                reasons.push("same file");
            }
            if graph.callers(target).contains(&candidate)
                || graph.callees(target).contains(&candidate)
            {
                score += 300;
                reasons.push("confirmed call neighborhood");
            }
            let shared = target_terms
                .iter()
                .filter(|term| graph.search[candidate].identifier.contains(*term))
                .count();
            if shared > 0 {
                score += shared.min(4) * 25;
                reasons.push("identifier overlap");
            }
            (score >= 50).then_some((candidate, score, reasons))
        })
        .collect();
    tests.sort_by(|left, right| {
        right.1.cmp(&left.1).then_with(|| {
            graph.symbols[left.0]
                .canonical
                .cmp(&graph.symbols[right.0].canonical)
        })
    });
    if tests.is_empty() {
        return;
    }
    output.push_str("\nLikely relevant tests (relevance evidence, not TESTS edges):");
    for (test, _, reasons) in tests.into_iter().take(INSPECT_TEST_LIMIT) {
        output.push_str(&format!(
            "\n- {} [{}]",
            symbol_line(graph, &graph.symbols[test]),
            reasons.join(" + ")
        ));
    }
}

fn is_test_symbol(graph: &Graph, symbol: usize) -> bool {
    let path = &graph.files[graph.symbols[symbol].file].path;
    let path_marks_test = path.split('/').any(|part| {
        matches!(part, "test" | "tests" | "spec" | "specs")
            || part.contains(".test.")
            || part.contains(".spec.")
    });
    let name = graph.symbols[symbol].name.to_lowercase();
    path_marks_test
        || (!graph.search[symbol].test.is_empty() && (name == "test" || name.starts_with("test_")))
}

fn symbol_line(graph: &Graph, symbol: &Symbol) -> String {
    format!(
        "{} [{} {}:{}:{}]",
        symbol.canonical,
        symbol.kind.label(),
        graph.files[symbol.file].path,
        symbol.line,
        symbol.column
    )
}
