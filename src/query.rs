use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::index::{Graph, RelationshipIssue, Symbol};

const MAX_TRAVERSAL: usize = 500;
const FIND_LIMIT: usize = 20;

pub(crate) struct ReverseImpact {
    pub by_depth: BTreeMap<usize, Vec<usize>>,
    pub truncated: bool,
}

pub(crate) fn reverse_impact(graph: &Graph, roots: &BTreeSet<usize>) -> ReverseImpact {
    let mut queue: VecDeque<(usize, usize)> = roots.iter().map(|root| (*root, 0)).collect();
    let mut visited = roots.clone();
    let mut by_depth = BTreeMap::new();

    while let Some((current, depth)) = queue.pop_front() {
        if visited.len() >= MAX_TRAVERSAL {
            break;
        }
        for &caller in graph.callers(current) {
            if visited.insert(caller) {
                by_depth
                    .entry(depth + 1)
                    .or_insert_with(Vec::new)
                    .push(caller);
                queue.push_back((caller, depth + 1));
            }
        }
    }
    ReverseImpact {
        by_depth,
        truncated: visited.len() >= MAX_TRAVERSAL,
    }
}

pub fn find(graph: &Graph, query: &str) -> String {
    let query_tokens = tokens(query);
    let mut matches: Vec<_> = graph
        .symbols
        .iter()
        .filter_map(|symbol| find_match(graph, symbol, query, &query_tokens))
        .collect();

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
    score: usize,
    matched_tokens: usize,
    reason: &'static str,
}

fn find_match<'a>(
    graph: &Graph,
    symbol: &'a Symbol,
    query: &str,
    query_tokens: &[String],
) -> Option<FindMatch<'a>> {
    if query_tokens.is_empty() {
        return Some(FindMatch {
            symbol,
            score: 1,
            matched_tokens: 0,
            reason: "all symbols",
        });
    }
    let name = symbol.name.to_lowercase();
    let canonical = symbol.canonical.to_lowercase();
    if canonical == query.to_lowercase() {
        return Some(FindMatch {
            symbol,
            score: 1_000,
            matched_tokens: query_tokens.len(),
            reason: "exact identity",
        });
    }
    if symbol.name == query {
        return Some(FindMatch {
            symbol,
            score: 900,
            matched_tokens: query_tokens.len(),
            reason: "exact name",
        });
    }
    if name == query.to_lowercase() {
        return Some(FindMatch {
            symbol,
            score: 850,
            matched_tokens: query_tokens.len(),
            reason: "exact name",
        });
    }

    let name_tokens = tokens(&symbol.name);
    let path_tokens = tokens(&graph.files[symbol.file].path);
    let mut score = 0;
    let mut matched_tokens = 0;
    let mut reason = "substring match";
    for token in query_tokens {
        if name_tokens.iter().any(|part| part == token) {
            score += 700;
            matched_tokens += 1;
            reason = "name token";
        } else if name_tokens.iter().any(|part| part.starts_with(token)) {
            score += 600;
            matched_tokens += 1;
            reason = "name prefix";
        } else if name.contains(token) {
            score += 400;
            matched_tokens += 1;
            reason = "name substring";
        } else if path_tokens.iter().any(|part| part == token) {
            score += 300;
            matched_tokens += 1;
            if reason == "substring match" {
                reason = "path match";
            }
        } else if path_tokens.iter().any(|part| part.starts_with(token)) {
            score += 250;
            matched_tokens += 1;
            if reason == "substring match" {
                reason = "path prefix";
            }
        } else if canonical.contains(token) {
            score += 100;
            matched_tokens += 1;
        }
    }
    let minimum_tokens = if query_tokens.len() > 1 { 2 } else { 1 };
    (matched_tokens >= minimum_tokens).then_some(FindMatch {
        symbol,
        score,
        matched_tokens,
        reason,
    })
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
        "{}\nDefining file: {}\nDirect callers:",
        symbol_line(graph, symbol),
        graph.files[defining_file].path
    );
    append_symbols(&mut output, graph, callers);
    output.push_str("\nDirect callees:");
    append_callees(&mut output, graph, target, callees);
    output.push_str("\nDefining file imports:");
    append_files(&mut output, graph, imports);
    output.push_str("\nUnresolved or ambiguous outgoing calls:");
    if issues.is_empty() {
        output.push_str(" none");
    } else {
        for issue in issues {
            output.push_str(&format!(
                "\n- {} {}:{} call '{}' — {}",
                issue.status.label(),
                issue.line,
                issue.column,
                issue.name,
                issue.detail
            ));
        }
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
        "Confirmed impact for {}\nDirect callers:",
        graph.symbols[target].canonical
    );
    append_symbols(&mut output, graph, &direct);
    if !direct.is_empty() {
        output.push_str("\nDirect caller evidence:");
        for caller in &direct {
            output.push_str(&format!(
                "\n- {}{}",
                graph.symbols[*caller].canonical,
                call_sites(graph, *caller, target)
            ));
        }
    }
    output.push_str("\nTransitive callers:");
    let mut has_transitive = false;
    for (depth, symbols) in result.by_depth.range(2..) {
        has_transitive = true;
        output.push_str(&format!("\nDepth {depth}:"));
        append_symbols(&mut output, graph, symbols);
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
        output.push_str("\nCompleteness: no unresolved or ambiguous calls name-match this symbol.");
    } else {
        output.push_str("\nCompleteness: conservative/incomplete; unresolved or ambiguous calls could conceal callers:");
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

fn append_symbols(output: &mut String, graph: &Graph, symbols: &[usize]) {
    if symbols.is_empty() {
        output.push_str(" none");
        return;
    }
    for symbol in symbols {
        output.push_str(&format!(
            "\n- {}",
            symbol_line(graph, &graph.symbols[*symbol])
        ));
    }
}

fn append_callees(output: &mut String, graph: &Graph, source: usize, callees: &[usize]) {
    if callees.is_empty() {
        output.push_str(" none");
        return;
    }
    for callee in callees {
        output.push_str(&format!(
            "\n- {}{}",
            symbol_line(graph, &graph.symbols[*callee]),
            call_sites(graph, source, *callee)
        ));
    }
}

fn call_sites(graph: &Graph, from: usize, to: usize) -> String {
    let source = &graph.files[graph.symbols[from].file].path;
    let sites = graph.call_sites(from, to);
    match sites {
        [] => String::new(),
        [site] => format!(" [call at {source}:{}:{}]", site.line, site.column),
        _ => format!(
            " [calls at {}]",
            sites
                .iter()
                .map(|site| format!("{source}:{}:{}", site.line, site.column))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn append_files(output: &mut String, graph: &Graph, files: &[usize]) {
    if files.is_empty() {
        output.push_str(" none");
        return;
    }
    for file in files {
        output.push_str(&format!("\n- {}", graph.files[*file].path));
    }
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
