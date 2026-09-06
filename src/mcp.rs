use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::schemars::JsonSchema;
use rmcp::{ServiceExt, tool, tool_router};
use serde::Deserialize;

use crate::{
    diff,
    index::{Index, no_supported_source_files_message},
    query,
};

pub fn run() -> Result<(), String> {
    let root = std::env::current_dir()
        .map_err(|error| format!("cannot read current directory: {error}"))?;
    let server = Server::new(&root)?;
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|error| format!("cannot start MCP runtime: {error}"))?;
    runtime.block_on(async move {
        let server = server
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|error| format!("cannot start MCP server: {error}"))?;
        server
            .waiting()
            .await
            .map_err(|error| format!("MCP server stopped unexpectedly: {error}"))?;
        Ok(())
    })
}

struct Server {
    root: PathBuf,
    index: Mutex<Index>,
}

impl Server {
    fn new(root: &Path) -> Result<Self, String> {
        let root = root
            .canonicalize()
            .map_err(|error| format!("cannot read repository root {}: {error}", root.display()))?;
        let index = Index::open(&root)?;
        Ok(Self {
            root,
            index: Mutex::new(index),
        })
    }

    fn answer(&self, query_fn: impl FnOnce(&Index) -> Result<String, String>) -> CallToolResult {
        let mut index = match self.index.lock() {
            Ok(index) => index,
            Err(_) => return error("BlastRay's live index lock was poisoned."),
        };
        if let Err(message) = index.sync() {
            return error(&message);
        }
        if !index.has_supported_source_files() {
            return text(no_supported_source_files_message());
        }
        match query_fn(&index) {
            Ok(output) => text(output),
            Err(message) => error(&message),
        }
    }
}

#[derive(Deserialize, JsonSchema)]
struct FindInput {
    /// Natural-language coding task, exact symbol, or canonical selector. For example:
    /// "where is session refresh handled after browser reload?"
    query: String,
}

#[derive(Deserialize, JsonSchema)]
struct InspectInput {
    /// A relevant symbol from find, or a known canonical selector such as
    /// src/auth/session.ts::refreshSession.
    target: String,
}

#[derive(Deserialize, JsonSchema)]
struct TraceInput {
    /// Known starting symbol for a confirmed CALLS-path question.
    from: String,
    /// Known destination symbol for a confirmed CALLS-path question.
    to: String,
}

#[derive(Deserialize, JsonSchema)]
struct ImpactInput {
    /// Shared or structural symbol to assess before editing, or "@diff" after edits.
    target: String,
}

#[tool_router]
impl Server {
    #[tool(
        name = "find",
        title = "Start code investigation",
        description = "PRIMARY ENTRY POINT for an unfamiliar coding task, bug, feature, or question: before any broad repository grep, search, or file-read loop, call this with the natural language task in the user's own words. Example: \"where is session refresh handled after browser reload?\" Exact symbols and canonical selectors/path-like identities also work. Deterministic local retrieval: ranking suggests relevance, not graph proof.",
        annotations(
            title = "Start code investigation",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn find(&self, Parameters(FindInput { query }): Parameters<FindInput>) -> CallToolResult {
        self.answer(|index| Ok(query::find(index.graph(), &query)))
    }

    #[tool(
        name = "inspect",
        title = "Inspect symbol context",
        description = "After find identifies a likely symbol, call this before opening broad source files; also use it when a relevant symbol is already known. Returns bounded CURRENT source context plus confirmed callers/callees, call-site evidence, import context, likely-test relevance when available, and local unresolved boundaries. Treat shown source as already read; open files only for deliberately omitted local details.",
        annotations(
            title = "Inspect symbol context",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn inspect(
        &self,
        Parameters(InspectInput { target }): Parameters<InspectInput>,
    ) -> CallToolResult {
        self.answer(|index| index.inspect(&target))
    }

    #[tool(
        name = "trace",
        title = "Trace confirmed calls",
        description = "Use when BOTH endpoints are known and you need a confirmed directed CALLS path; it is not the vague-task discovery tool. A missing path means unknown from the proven graph, not proof that no runtime path exists.",
        annotations(
            title = "Trace confirmed calls",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn trace(&self, Parameters(TraceInput { from, to }): Parameters<TraceInput>) -> CallToolResult {
        self.answer(|index| query::trace(index.graph(), &from, &to))
    }

    #[tool(
        name = "impact",
        title = "Assess change impact",
        description = "Use before changing a shared or structural symbol; use target=\"@diff\" after edits. Returns only proven reverse dependencies (CALLS plus supported EXTENDS/IMPLEMENTS contracts) and explicit completeness boundaries. An empty result is not universal proof of safety.",
        annotations(
            title = "Assess change impact",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn impact(
        &self,
        Parameters(ImpactInput { target }): Parameters<ImpactInput>,
    ) -> CallToolResult {
        if target == "@diff" {
            self.answer(|index| diff::impact(index.graph(), &self.root))
        } else {
            self.answer(|index| query::impact(index.graph(), &target))
        }
    }
}

#[rmcp::tool_handler(
    router = Self::tool_router(),
    name = "blastray",
    instructions = "BlastRay is local repository analysis. For an unfamiliar coding task, bug, or feature, start with find using the user's natural-language task before broad grep/read. When find identifies a likely symbol, inspect it before opening broad source: its bounded current source context and confirmed relationships are already read. Use trace only when both endpoint symbols are known and a confirmed CALLS path is needed. Before a shared or structural edit use impact(symbol); after edits use impact(\"@diff\").\n\nFallback: use normal grep/read for exact literals, configs or docs, unsupported or empty BlastRay results, and source details inspect deliberately omitted. Find ranking suggests relevance; graph relationships are proven and conservative. Unresolved or unsupported does not mean impossible."
)]
impl rmcp::ServerHandler for Server {}

fn text(value: String) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(value)])
}

fn error(value: &str) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(value)])
}
