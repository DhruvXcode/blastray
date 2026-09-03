use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::index::{Graph, RelationshipIssue, Symbol};

const MAX_TRAVERSAL: usize = 500;

pub fn find(graph: &Graph, query: &str) -> String {
    let query = query.to_lowercase();
    let matches: Vec<&Symbol> = graph
        .symbols
        .iter()
        .filter(|symbol| {
            let canonical = symbol.canonical.to_lowercase();
            let name = symbol.name.to_lowercase();
            name == query
                || name.starts_with(&query)
                || name.contains(&query)
                || canonical.contains(&query)
        })
        .collect();

    if matches.is_empty() {
        return format!("No symbols found for '{query}'.");
    }

    let mut output = format!("{} symbol(s):\n", matches.len());
    for symbol in matches {
        output.push_str(&symbol_line(graph, symbol));
        output.push('\n');
    }
    output.trim_end().to_string()
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
    append_symbols(&mut output, graph, callees);
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
    Ok(output)
}

pub fn impact(graph: &Graph, target: &str) -> Result<String, String> {
    let target = select(graph, target)?;
    let mut queue = VecDeque::from([(target, 0usize)]);
    let mut visited = BTreeSet::from([target]);
    let mut by_depth: BTreeMap<usize, Vec<usize>> = BTreeMap::new();

    while let Some((current, depth)) = queue.pop_front() {
        if visited.len() >= MAX_TRAVERSAL {
            break;
        }
        for &caller in graph.callers(current) {
            if visited.insert(caller) {
                by_depth.entry(depth + 1).or_default().push(caller);
                queue.push_back((caller, depth + 1));
            }
        }
    }

    let direct = by_depth.get(&1).cloned().unwrap_or_default();
    let mut output = format!(
        "Confirmed impact for {}\nDirect callers:",
        graph.symbols[target].canonical
    );
    append_symbols(&mut output, graph, &direct);
    output.push_str("\nTransitive callers:");
    let mut has_transitive = false;
    for (depth, symbols) in by_depth.range(2..) {
        has_transitive = true;
        output.push_str(&format!("\nDepth {depth}:"));
        append_symbols(&mut output, graph, symbols);
    }
    if !has_transitive {
        output.push_str(" none");
    }
    output.push_str(&format!(
        "\nTotal confirmed affected symbols: {}",
        visited.len().saturating_sub(1)
    ));
    if visited.len() >= MAX_TRAVERSAL {
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
