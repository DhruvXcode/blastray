use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::schemars::JsonSchema;
use rmcp::{ServiceExt, tool, tool_router};
use serde::Deserialize;

use crate::{diff, index::Index, query};

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
        match query_fn(&index) {
            Ok(output) => text(output),
            Err(message) => error(&message),
        }
    }
}

#[derive(Deserialize, JsonSchema)]
struct FindInput {
    query: String,
}

#[derive(Deserialize, JsonSchema)]
struct InspectInput {
    target: String,
}

#[derive(Deserialize, JsonSchema)]
struct TraceInput {
    from: String,
    to: String,
}

#[derive(Deserialize, JsonSchema)]
struct ImpactInput {
    target: String,
}

#[tool_router(server_handler)]
impl Server {
    #[tool(
        name = "find",
        description = "Locate indexed code symbols with deterministic structural and textual matching. Use this to find a symbol before deeper inspection."
    )]
    fn find(&self, Parameters(FindInput { query }): Parameters<FindInput>) -> CallToolResult {
        self.answer(|index| Ok(query::find(index.graph(), &query)))
    }

    #[tool(
        name = "inspect",
        description = "Return one symbol's direct structural neighborhood: callers, callees, defining file/import context, and unresolved or ambiguous outgoing relationships. Use this before editing a symbol."
    )]
    fn inspect(
        &self,
        Parameters(InspectInput { target }): Parameters<InspectInput>,
    ) -> CallToolResult {
        self.answer(|index| query::inspect(index.graph(), &target))
    }

    #[tool(
        name = "trace",
        description = "Find a confirmed directed structural CALLS path from one symbol to another. Unknown edges are never fabricated; no known path does not prove no runtime path exists."
    )]
    fn trace(&self, Parameters(TraceInput { from, to }): Parameters<TraceInput>) -> CallToolResult {
        self.answer(|index| query::trace(index.graph(), &from, &to))
    }

    #[tool(
        name = "impact",
        description = "Use impact on a symbol before a risky structural edit. Use target=\"@diff\" after edits to inspect the current Git working tree's confirmed blast radius. Ordinary targets return confirmed reverse-CALLS impact."
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

fn text(value: String) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(value)])
}

fn error(value: &str) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(value)])
}
