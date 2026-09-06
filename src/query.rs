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
    let representation = QueryRepresentation::parse(query);
    let mut matches = discovery_matches(graph, query, &representation);

    if matches.is_empty() {
        return format!("No symbols found for '{query}'.");
    }

    matches.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| right.matched_terms.cmp(&left.matched_terms))
            .then_with(|| left.symbol.canonical.cmp(&right.symbol.canonical))
    });
    let total = matches.len();
    let shown = total.min(FIND_LIMIT);
    let mut output = String::new();
    let exact_lookup = matches
        .first()
        .is_some_and(|found| found.lexical_score >= 1_000_000.0);
    if !exact_lookup {
        let top = &matches[0];
        let runner_up = matches.get(1);
        let close_alternative = runner_up.is_some_and(|candidate| {
            candidate.score >= top.score * 0.88 && candidate.symbol.file != top.symbol.file
        });
        let weak_structure = top.structural_score < 0.2 && top.coverage < 0.72;
        if close_alternative || weak_structure {
            output.push_str("Retrieval confidence: limited — several relevant areas are close or confirmed structural links are incomplete. Treat the leading results as alternatives, not a proven runtime path.\n");
        }
    }
    if shown < total {
        output.push_str(&format!(
            "Showing the top {shown} of {total} ranked matches. Inspect the best plausible result before refining; additional matches are omitted.\n"
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
    id: usize,
    symbol: &'a Symbol,
    score: f64,
    lexical_score: f64,
    structural_score: f64,
    matched_terms: usize,
    #[allow(dead_code)]
    coverage: f64,
    reason: String,
}

fn discovery_matches<'a>(
    graph: &'a Graph,
    query: &str,
    representation: &QueryRepresentation,
) -> Vec<FindMatch<'a>> {
    if representation.terms.is_empty() {
        return graph
            .symbols
            .iter()
            .enumerate()
            .map(|(id, symbol)| FindMatch {
                id,
                symbol,
                score: 1.0,
                lexical_score: 1.0,
                structural_score: 0.0,
                matched_terms: 0,
                coverage: 0.0,
                reason: "all symbols".to_owned(),
            })
            .collect();
    }
    let document_frequency: Vec<usize> = representation
        .terms
        .iter()
        .map(|term| {
            graph
                .search
                .iter()
                .filter(|fact| fact_matches(fact, term))
                .count()
        })
        .collect();
    let document_count = graph.symbols.len() as f64;
    let query_lower = query.trim().to_lowercase();
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
            let mut contribution_sum = 0.0;
            let mut matched = 0;
            let mut coverage_sum = 0.0;
            let mut reasons = Vec::new();
            for (position, term) in representation.terms.iter().enumerate() {
                let fields = matching_fields(fact, term);
                if fields.is_empty() {
                    continue;
                }
                matched += 1;
                // BM25F-like bounded field evidence. IDF is logarithmic and
                // capped, each term selects its strongest field, and extra
                // fields only corroborate it slightly. This deliberately
                // prevents a rare comment word from growing without bound.
                let idf = ((document_count - document_frequency[position] as f64 + 0.5)
                    / (document_frequency[position] as f64 + 0.5)
                    + 1.0)
                    .ln()
                    .min(3.0);
                let best = fields
                    .iter()
                    .map(|evidence| evidence.field.weight() * evidence.quality)
                    .fold(0.0_f64, f64::max);
                coverage_sum += fields
                    .iter()
                    .map(|evidence| evidence.field.coverage_weight() * evidence.quality)
                    .fold(0.0_f64, f64::max);
                let corroboration = (fields.len().saturating_sub(1) as f64 * 0.12).min(0.36);
                contribution_sum += (idf * (best + corroboration)).min(8.0);
                for field in fields {
                    let label = field.field.label();
                    if !reasons.contains(&label) {
                        reasons.push(label);
                    }
                }
            }
            let minimum_task_terms = if representation.terms.len() >= 4 {
                2
            } else {
                1
            };
            if matched < minimum_task_terms && exact.is_none() {
                return None;
            }
            // A body-only mention still admits a candidate, but cannot claim
            // the same concept coverage as an identifier/path/declaration
            // match. This stops a long enclosing class body from making every
            // one of its methods look like a complete task answer.
            let coverage = coverage_sum / representation.terms.len() as f64;
            // Mean evidence avoids rewarding a long query merely for having
            // more words. Coverage is intentionally nonlinear: several
            // independent concepts are more useful than one lexical accident.
            let mut lexical = (contribution_sum / matched.max(1) as f64) * (0.35 + 0.65 * coverage)
                + 12.0 * coverage * coverage;
            if is_non_production(graph, id) && !representation.explicit_test_intent {
                // Test/example trees often repeat an entire task phrase in
                // setup names and comments. A clear production prior keeps
                // them discoverable but prevents that repetition from taking
                // over ordinary implementation questions.
                lexical -= 8.0;
                reasons.push("non-production penalty");
            }
            if representation.flow_intent {
                lexical += match symbol.kind {
                    crate::index::SymbolKind::Function | crate::index::SymbolKind::Method => 0.7,
                    crate::index::SymbolKind::Class => 0.15,
                    crate::index::SymbolKind::Type => -0.8,
                };
            }
            if let Some(reason) = exact {
                // Exact lookup is a distinct product path, not a lexical
                // accident; it remains overwhelmingly decisive by design.
                lexical += 1_000_000.0;
                reasons.insert(0, reason);
            }
            Some(FindMatch {
                id,
                symbol,
                score: lexical,
                lexical_score: lexical,
                structural_score: 0.0,
                matched_terms: matched,
                coverage,
                reason: format!(
                    "{}; coverage {matched}/{}",
                    reasons.join(" + "),
                    representation.terms.len()
                ),
            })
        })
        .collect();

    structural_rerank(graph, &mut matches);
    file_area_rerank(graph, &mut matches);
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
    fn weight(self) -> f64 {
        match self {
            Self::Identifier => 3.5,
            Self::Path => 2.2,
            Self::Declaration => 1.7,
            Self::Comments => 0.8,
            Self::Strings => 0.65,
            Self::Body => 0.25,
            Self::Test => 0.9,
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

    fn coverage_weight(self) -> f64 {
        match self {
            Self::Identifier | Self::Path => 1.0,
            Self::Declaration => 0.8,
            Self::Comments => 0.45,
            Self::Strings => 0.35,
            Self::Body => 0.18,
            Self::Test => 0.5,
        }
    }
}

struct FieldEvidence {
    field: SearchField,
    quality: f64,
}

fn matching_fields(fact: &SearchFact, term: &QueryTerm) -> Vec<FieldEvidence> {
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
            .filter_map(|value| term_match(term, value))
            .fold(None, |best, quality| {
                Some(best.unwrap_or(0.0_f64).max(quality))
            })
            .map(|quality| FieldEvidence { field, quality })
    })
    .collect()
}

fn fact_matches(fact: &SearchFact, term: &QueryTerm) -> bool {
    !matching_fields(fact, term).is_empty()
}

fn term_match(term: &QueryTerm, value: &str) -> Option<f64> {
    if value == term.original {
        return Some(1.0);
    }
    if term.original.len() >= 4
        && (value.starts_with(&term.original) || term.original.starts_with(value))
    {
        return Some(0.82);
    }
    term.variants.iter().find_map(|variant| {
        (value == variant
            || (variant.len() >= 4 && (value.starts_with(variant) || variant.starts_with(value))))
        .then_some(0.62)
    })
}

#[derive(Clone, Debug)]
enum QueryTermKind {
    Identifier,
    Acronym,
    Domain,
}

#[derive(Clone, Debug)]
struct QueryTerm {
    original: String,
    variants: BTreeSet<String>,
    #[allow(dead_code)]
    kind: QueryTermKind,
}

/// Query representation is deliberately richer than the index representation:
/// source spellings remain intact in the cache while the query may add a small
/// number of lower-confidence morphological variants.
#[derive(Clone, Debug)]
struct QueryRepresentation {
    terms: Vec<QueryTerm>,
    explicit_test_intent: bool,
    flow_intent: bool,
    #[allow(dead_code)]
    removed_procedural_clauses: Vec<String>,
}

impl QueryRepresentation {
    fn parse(query: &str) -> Self {
        let (substantive, removed_procedural_clauses) = strip_procedural_clauses(query);
        let explicit_test_intent = identifier_words(&substantive).iter().any(|word| {
            matches!(
                word.to_lowercase().as_str(),
                "test" | "tests" | "spec" | "specs" | "fixture" | "fixtures"
            )
        });
        let flow_intent = ["incoming", "reaches", "through", "flow", "path"]
            .iter()
            .any(|needle| substantive.contains(needle));
        let mut terms = BTreeMap::<String, QueryTerm>::new();
        for raw in identifier_words(&substantive) {
            let kind =
                if raw.chars().all(|character| character.is_ascii_uppercase()) && raw.len() >= 2 {
                    QueryTermKind::Acronym
                } else if raw.contains('_') || raw.chars().any(char::is_uppercase) {
                    QueryTermKind::Identifier
                } else {
                    QueryTermKind::Domain
                };
            for token in split_identifier(&raw) {
                if token.len() < 2 || is_query_stop_word(&token) {
                    continue;
                }
                terms.entry(token.clone()).or_insert_with(|| QueryTerm {
                    variants: morphology_variants(&token),
                    original: token,
                    kind: kind.clone(),
                });
            }
        }
        Self {
            terms: terms.into_values().collect(),
            explicit_test_intent,
            flow_intent,
            removed_procedural_clauses,
        }
    }
}

fn identifier_words(value: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for character in value.chars() {
        if character.is_alphanumeric() || character == '_' {
            current.push(character);
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn split_identifier(value: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut previous_lowercase = false;
    for character in value.chars() {
        if character == '_' {
            if !current.is_empty() {
                parts.push(std::mem::take(&mut current));
            }
            previous_lowercase = false;
            continue;
        }
        if character.is_uppercase() && previous_lowercase && !current.is_empty() {
            parts.push(std::mem::take(&mut current));
        }
        previous_lowercase = character.is_lowercase();
        current.extend(character.to_lowercase());
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

fn morphology_variants(token: &str) -> BTreeSet<String> {
    let mut variants = BTreeSet::new();
    let add = |candidate: String, variants: &mut BTreeSet<String>| {
        if candidate.len() >= 3 && candidate != token {
            variants.insert(candidate);
        }
    };
    if token.len() > 5 && token.ends_with("ies") {
        add(format!("{}y", &token[..token.len() - 3]), &mut variants);
    } else if token.len() > 5 && token.ends_with("es") {
        add(token[..token.len() - 2].to_owned(), &mut variants);
    } else if token.len() > 4 && token.ends_with('s') && !token.ends_with("ss") {
        add(token[..token.len() - 1].to_owned(), &mut variants);
    }
    if token.len() > 5 && token.ends_with("ing") {
        let base = &token[..token.len() - 3];
        add(base.to_owned(), &mut variants);
        add(format!("{base}e"), &mut variants);
        let base_bytes = base.as_bytes();
        if base_bytes.len() > 2
            && base_bytes[base_bytes.len() - 1] == base_bytes[base_bytes.len() - 2]
        {
            add(base[..base.len() - 1].to_owned(), &mut variants);
        }
    }
    if token.len() > 4 && token.ends_with("ed") {
        let base = &token[..token.len() - 2];
        add(base.to_owned(), &mut variants);
        add(format!("{base}e"), &mut variants);
    }
    if token.len() > 6 && (token.ends_with("tion") || token.ends_with("sion")) {
        add(token[..token.len() - 3].to_owned(), &mut variants);
    }
    variants
}

/// Remove operational ceremony only when it appears as a clause. A bare word
/// such as `network` or `test` remains meaningful unless it is inside a phrase
/// like `do not use network access` or `without running tests`.
fn strip_procedural_clauses(query: &str) -> (String, Vec<String>) {
    let lower = query.to_lowercase();
    let mut removed = Vec::new();
    let mut kept = String::new();
    for clause in lower.split_inclusive(['.', ';', '\n']) {
        let trimmed = clause.trim();
        let procedural = trimmed.starts_with("without ")
            || trimmed.starts_with("do not ")
            || trimmed.starts_with("don't ")
            || trimmed.starts_with("stop once ")
            || trimmed.starts_with("do not inspect ")
            || trimmed.starts_with("do not run ");
        if procedural {
            // A common preamble is comma-separated from the actual task.
            if let Some((ceremony, task)) = trimmed.split_once(',') {
                removed.push(ceremony.to_owned());
                kept.push(' ');
                kept.push_str(task);
            } else {
                removed.push(trimmed.to_owned());
            }
        } else {
            kept.push(' ');
            kept.push_str(trimmed);
        }
    }
    (kept, removed)
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
            | "explain"
            | "identify"
            | "main"
            | "code"
            | "repository"
            | "enough"
            | "once"
    )
}

fn is_non_production(graph: &Graph, symbol: usize) -> bool {
    let path = graph.files[graph.symbols[symbol].file].path.to_lowercase();
    path.split('/').any(|part| {
        matches!(
            part,
            "test"
                | "tests"
                | "spec"
                | "specs"
                | "fixture"
                | "fixtures"
                | "example"
                | "examples"
                | "benchmark"
                | "benchmarks"
                | "sample"
                | "samples"
                | "demo"
                | "demos"
        )
    }) || path.contains(".test.")
        || path.contains(".test-")
        || path.contains(".spec.")
        || path.ends_with("_test.go")
}

/// Rerank only the top bounded lexical pool. Confirmed CALLS, EXTENDS and
/// IMPLEMENTS edges are used as relevance corroboration; imports never become
/// symbol-level relationships here. A two-hop contribution is deliberately
/// small, so this is not an unbounded graph walk.
fn structural_rerank(graph: &Graph, matches: &mut [FindMatch<'_>]) {
    const CANDIDATE_POOL: usize = 64;
    let mut order: Vec<usize> = (0..matches.len()).collect();
    order.sort_by(|left, right| {
        matches[*right]
            .lexical_score
            .total_cmp(&matches[*left].lexical_score)
    });
    order.truncate(CANDIDATE_POOL);
    let candidates: BTreeSet<usize> = order.iter().map(|index| matches[*index].id).collect();
    let score_by_symbol: BTreeMap<usize, f64> = order
        .iter()
        .map(|index| (matches[*index].id, matches[*index].lexical_score.max(0.0)))
        .collect();
    let max_score = score_by_symbol
        .values()
        .copied()
        .fold(0.0_f64, f64::max)
        .max(1.0);
    let mut adjacency: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for edge in &graph.dependencies {
        if candidates.contains(&edge.from) && candidates.contains(&edge.to) {
            adjacency.entry(edge.from).or_default().push(edge.to);
            adjacency.entry(edge.to).or_default().push(edge.from);
        }
    }
    // Connected candidate components are a compact approximation of
    // relevance-walk mass: a cluster of independently retrieved symbols with
    // confirmed edges is more credible than a one-file lexical pile. Only the
    // bounded candidate pool participates.
    let mut component_bonus = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for start in &candidates {
        if !seen.insert(*start) {
            continue;
        }
        let mut queue = VecDeque::from([*start]);
        let mut component = vec![*start];
        while let Some(current) = queue.pop_front() {
            for neighbor in adjacency.get(&current).into_iter().flatten() {
                if seen.insert(*neighbor) {
                    component.push(*neighbor);
                    queue.push_back(*neighbor);
                }
            }
        }
        if component.len() < 2 {
            continue;
        }
        let mass = component
            .iter()
            .map(|id| score_by_symbol[id] / max_score)
            .sum::<f64>();
        let bonus = ((mass - 1.0) * 0.55).clamp(0.0, 3.0);
        for symbol in component {
            component_bonus.insert(symbol, bonus);
        }
    }
    for index in order {
        let symbol = matches[index].id;
        let direct_neighbors = adjacency.get(&symbol).cloned().unwrap_or_default();
        let direct = direct_neighbors
            .iter()
            .fold(0.0, |sum, neighbor| {
                sum + score_by_symbol.get(neighbor).copied().unwrap_or(0.0) / max_score
            })
            .min(2.5);
        let two_hop = direct_neighbors
            .iter()
            .flat_map(|neighbor| adjacency.get(neighbor).into_iter().flatten().copied())
            .filter(|neighbor| *neighbor != symbol)
            .fold(0.0, |sum, neighbor| {
                sum + score_by_symbol.get(&neighbor).copied().unwrap_or(0.0) / max_score
            })
            .min(3.0);
        // A confirmed implementation chain is stronger corroboration than a
        // same-file collection of textual hits. Keep it bounded and local,
        // but let a connected dispatch/validation area outrank an isolated
        // comment or middleware-name collision.
        let structural =
            (direct * 2.4 + two_hop * 0.35 + component_bonus.get(&symbol).copied().unwrap_or(0.0))
                .min(6.0);
        if structural > 0.0 {
            matches[index].structural_score = structural;
            matches[index].score += structural;
            matches[index]
                .reason
                .push_str(" + confirmed structural corroboration");
        }
    }
}

/// A task often names a module before it names a symbol. Within the same
/// bounded lexical pool, reward a file whose independently matching symbols
/// agree, while capping the evidence to three symbols so a large file cannot
/// win just by containing many mediocre hits. This is relevance-only file
/// evidence: it creates neither a File symbol nor a graph edge.
fn file_area_rerank(_graph: &Graph, matches: &mut [FindMatch<'_>]) {
    const CANDIDATE_POOL: usize = 64;
    let mut order: Vec<usize> = (0..matches.len()).collect();
    order.sort_by(|left, right| matches[*right].score.total_cmp(&matches[*left].score));
    order.truncate(CANDIDATE_POOL);
    let mut scores_by_file: BTreeMap<usize, Vec<f64>> = BTreeMap::new();
    for index in &order {
        scores_by_file
            .entry(matches[*index].symbol.file)
            .or_default()
            .push(matches[*index].score.max(0.0) * matches[*index].coverage.max(0.25));
    }
    for scores in scores_by_file.values_mut() {
        scores.sort_by(|left, right| right.total_cmp(left));
    }
    for index in order {
        let scores = &scores_by_file[&matches[index].symbol.file];
        let best = scores.first().copied().unwrap_or(1.0).max(1.0);
        let support = scores.iter().skip(1).take(2).sum::<f64>() / best;
        // File agreement is weaker than confirmed symbol structure: a test or
        // helper module can contain many similar names. It is a modest tie
        // breaker, not an alternative graph.
        let bonus = (support * 0.35).min(0.75);
        if bonus > 0.0 {
            matches[index].score += bonus;
            matches[index].reason.push_str(" + coherent file area");
        }
    }
}

/// Compact development-only visibility into the deterministic pipeline.
#[allow(dead_code)]
pub(crate) fn retrieval_diagnostic(graph: &Graph, query: &str) -> String {
    let representation = QueryRepresentation::parse(query);
    let mut output = format!("query: {query}\nterms:");
    for term in &representation.terms {
        let df = graph
            .search
            .iter()
            .filter(|fact| fact_matches(fact, term))
            .count();
        output.push_str(&format!(
            "\n- {} ({:?}); variants={:?}; df={df}",
            term.original, term.kind, term.variants
        ));
    }
    if !representation.removed_procedural_clauses.is_empty() {
        output.push_str(&format!(
            "\nremoved procedural clauses: {:?}",
            representation.removed_procedural_clauses
        ));
    }
    output.push_str(&format!(
        "\nexplicit test intent: {}\ncandidates:",
        representation.explicit_test_intent
    ));
    let mut matches = discovery_matches(graph, query, &representation);
    matches.sort_by(|left, right| right.score.total_cmp(&left.score));
    for found in matches.into_iter().take(12) {
        output.push_str(&format!(
            "\n- {} lexical={:.2} structural={:.2} final={:.2} coverage={:.0}% area={}",
            found.symbol.canonical,
            found.lexical_score,
            found.structural_score,
            found.score,
            found.coverage * 100.0,
            graph.files[found.symbol.file].path
        ));
    }
    output
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

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod retrieval_tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::index::Index;

    use super::{QueryRepresentation, discovery_matches};

    static NEXT_REPOSITORY: AtomicUsize = AtomicUsize::new(0);

    fn repository() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "blastray-retrieval-test-{}-{}",
            std::process::id(),
            NEXT_REPOSITORY.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("tests")).unwrap();
        fs::write(
            root.join("src/router.ts"),
            r#"
            export function matchRoute(request: Request) { return dispatchHandler(request); }
            export function dispatchHandler(request: Request) { return request; }
            export function retryBackoff() { return 1; }
            export function parseGitHistory() { return []; }
            "#,
        )
        .unwrap();
        fs::write(
            root.join("tests/router.test.ts"),
            r#"
            // incoming request route handler middleware fixture
            export function testRouteDiscoveryFixtureLoading() { return true; }
            "#,
        )
        .unwrap();
        root
    }

    fn primary(query: &str) -> String {
        let root = repository();
        let index = Index::build(&root).unwrap();
        let representation = QueryRepresentation::parse(query);
        let mut matches = discovery_matches(index.graph(), query, &representation);
        matches.sort_by(|left, right| right.score.total_cmp(&left.score));
        let answer = matches.first().unwrap().symbol.canonical.clone();
        fs::remove_dir_all(root).unwrap();
        answer
    }

    #[test]
    fn procedural_clauses_do_not_change_a_route_task_area() {
        let concise = primary("incoming HTTP request route handler middleware");
        let verbose = primary(
            "Without modifying files or running tests, explain how an incoming HTTP request is matched to a route and reaches its handler middleware. Stop once you have enough evidence.",
        );
        assert!(concise.starts_with("src/router.ts::"));
        assert!(verbose.starts_with("src/router.ts::"));
    }

    #[test]
    fn procedural_words_do_not_delete_real_domain_terms() {
        let network = QueryRepresentation::parse("network retry backoff");
        assert!(network.terms.iter().any(|term| term.original == "network"));
        assert!(network.terms.iter().any(|term| term.original == "retry"));
        assert!(
            QueryRepresentation::parse("do not use network access")
                .terms
                .is_empty()
        );

        let tests = QueryRepresentation::parse("test discovery fixture loading");
        assert!(tests.explicit_test_intent);
        assert!(
            QueryRepresentation::parse("do not run tests")
                .terms
                .is_empty()
        );

        let history = QueryRepresentation::parse("git history parser");
        assert!(history.terms.iter().any(|term| term.original == "history"));
        assert!(
            QueryRepresentation::parse("do not inspect git history")
                .terms
                .is_empty()
        );
    }

    #[test]
    fn production_beats_a_test_collision_unless_tests_are_requested() {
        assert!(
            primary("incoming request route handler middleware").starts_with("src/router.ts::")
        );
        assert_eq!(
            primary("test discovery fixture loading"),
            "tests/router.test.ts::testRouteDiscoveryFixtureLoading"
        );
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
