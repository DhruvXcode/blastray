use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use blastray::{
    index::{Graph, Index},
    query,
};
use serde_json::{Value, json};

static NEXT_REPO: AtomicUsize = AtomicUsize::new(0);

struct Repo(PathBuf);

impl Repo {
    fn new() -> Self {
        let repo = Self::empty();
        repo.write("src/a.ts", "export function leaf() {}\n");
        repo.write(
            "src/b.ts",
            "import { leaf } from './a';\nexport function entry() { leaf(); }\n",
        );
        repo.git(&["add", "."]);
        repo.git(&["commit", "-qm", "initial"]);
        repo
    }

    fn empty() -> Self {
        let path = std::env::temp_dir().join(format!(
            "blastray-mcp-test-{}-{}",
            std::process::id(),
            NEXT_REPO.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        let repo = Self(path);
        repo.git(&["init", "-q"]);
        repo.git(&["config", "user.name", "BlastRay test"]);
        repo.git(&["config", "user.email", "test@example.invalid"]);
        repo
    }

    fn write(&self, path: &str, source: &str) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }

    fn git(&self, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.0)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Mcp {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next_id: u64,
}

impl Mcp {
    fn start(repo: &Repo) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_blastray"))
            .arg("mcp")
            .current_dir(&repo.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut mcp = Self {
            child,
            input,
            output,
            next_id: 1,
        };
        let initialized = mcp.request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "BlastRay test", "version": "1"}
            }),
        );
        assert_eq!(initialized["result"]["capabilities"], json!({"tools": {}}));
        mcp.notify("notifications/initialized", json!({}));
        mcp
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(
                self.output.read_line(&mut line).unwrap() > 0,
                "MCP server closed stdout"
            );
            let value: Value =
                serde_json::from_str(line.trim()).expect("protocol stdout must be JSON");
            if value["id"] == id {
                return value;
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
    }

    fn send(&mut self, value: Value) {
        writeln!(self.input, "{value}").unwrap();
        self.input.flush().unwrap();
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text(response: &Value) -> &str {
    response["result"]["content"][0]["text"].as_str().unwrap()
}

fn meaning(graph: &Graph) -> String {
    let mut result = String::new();
    for file in &graph.files {
        result.push_str(&format!("F:{}\n", file.path));
    }
    for symbol in &graph.symbols {
        result.push_str(&format!(
            "S:{}:{:?}:{}:{}:{}\n",
            symbol.canonical, symbol.kind, symbol.line, symbol.end_line, symbol.column
        ));
    }
    for edge in &graph.imports {
        result.push_str(&format!(
            "I:{}:{}\n",
            graph.files[edge.from].path, graph.files[edge.to].path
        ));
    }
    for edge in &graph.calls {
        result.push_str(&format!(
            "C:{}:{}\n",
            graph.symbols[edge.from].canonical, graph.symbols[edge.to].canonical
        ));
        for site in graph.call_sites(edge.from, edge.to) {
            result.push_str(&format!("E:{}:{}\n", site.line, site.column));
        }
    }
    for issue in &graph.issues {
        result.push_str(&format!(
            "X:{:?}:{}:{}:{}:{}:{}\n",
            issue.status, issue.source, issue.line, issue.column, issue.name, issue.detail
        ));
    }
    result
}

#[test]
fn stdio_server_explains_unsupported_repositories() {
    let repo = Repo::empty();
    repo.write("lib/main.dart", "void main() {}\n");
    let mut mcp = Mcp::start(&repo);

    let unsupported = text(&mcp.call("find", json!({"query": "main"}))).to_string();
    assert_eq!(
        unsupported,
        "No supported source files found.\nBlastRay currently indexes .ts, .tsx, .js, and .jsx."
    );
    assert_eq!(
        text(&mcp.call("impact", json!({"target": "anything"}))),
        unsupported.as_str()
    );

    repo.write("src/main.ts", "export function supported() {}\n");
    let mixed_response = mcp.call("find", json!({"query": "supported"}));
    let mixed = text(&mixed_response);
    assert!(mixed.contains("src/main.ts::supported"));
    assert!(!mixed.contains("No supported source files"));
}

#[test]
fn stdio_server_exposes_four_tools_and_keeps_one_index_current() {
    let repo = Repo::new();
    repo.write(
        "src/worker.ts",
        "export class Worker { leaf() {} entry() { this.leaf(); } }\n",
    );
    repo.write("src/reexport-source.ts", "export function forwarded() {}\n");
    repo.write(
        "src/reexport-barrel.ts",
        "export { forwarded as publicForwarded } from './reexport-source.js';\n",
    );
    repo.write(
        "src/reexport-use.ts",
        "import { publicForwarded } from './reexport-barrel.js';\nexport function reexportEntry() { publicForwarded(); }\n",
    );
    repo.write(
        "src/local-export-source.ts",
        "export function forwarded() {}\n",
    );
    repo.write(
        "src/local-export-barrel.ts",
        "import { forwarded as localForwarded } from './local-export-source.js';\nexport { localForwarded as publicForwarded };\n",
    );
    repo.write(
        "src/local-export-use.ts",
        "import { publicForwarded } from './local-export-barrel.js';\nexport function localExportEntry() { publicForwarded(); }\n",
    );
    repo.write(
        "src/constructor-service.ts",
        "export class Service { run() {} }\n",
    );
    repo.write(
        "src/constructor-use.ts",
        "import { Service } from './constructor-service.js';\nexport function constructorEntry() { const service = new Service(); service.run(); }\n",
    );
    let mut mcp = Mcp::start(&repo);

    let list = mcp.request("tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<_> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["find", "impact", "inspect", "trace"]);
    assert_eq!(tools.len(), 4);
    assert_eq!(tools[0]["inputSchema"]["required"], json!(["query"]));
    assert_eq!(tools[1]["inputSchema"]["required"], json!(["target"]));
    assert_eq!(tools[2]["inputSchema"]["required"], json!(["target"]));
    assert_eq!(tools[3]["inputSchema"]["required"], json!(["from", "to"]));

    assert!(text(&mcp.call("find", json!({"query": "leaf"}))).contains("src/a.ts::leaf"));
    assert!(
        text(&mcp.call("inspect", json!({"target": "src/a.ts::leaf"}))).contains("src/b.ts::entry")
    );
    assert!(
        text(&mcp.call(
            "trace",
            json!({"from": "src/b.ts::entry", "to": "src/a.ts::leaf"})
        ))
        .contains("Known CALLS path")
    );
    assert!(
        text(&mcp.call(
            "trace",
            json!({
                "from": "src/constructor-use.ts::constructorEntry",
                "to": "src/constructor-service.ts::Service.run"
            })
        ))
        .contains("Known CALLS path")
    );
    assert!(
        text(&mcp.call(
            "trace",
            json!({
                "from": "src/local-export-use.ts::localExportEntry",
                "to": "src/local-export-source.ts::forwarded"
            })
        ))
        .contains("Known CALLS path")
    );
    assert!(
        text(&mcp.call("impact", json!({"target": "src/a.ts::leaf"}))).contains("src/b.ts::entry")
    );
    let worker_response = mcp.call("inspect", json!({"target": "src/worker.ts::Worker.entry"}));
    let worker = text(&worker_response);
    assert!(worker.contains("src/worker.ts::Worker.leaf"));
    assert!(worker.contains("call at src/worker.ts:1:"));
    assert!(
        text(&mcp.call(
            "trace",
            json!({
                "from": "src/reexport-use.ts::reexportEntry",
                "to": "src/reexport-source.ts::forwarded"
            })
        ))
        .contains("Known CALLS path")
    );

    repo.write("src/reexport-source.ts", "export function renamed() {}\n");
    assert!(
        text(&mcp.call("find", json!({"query": "renamed"})))
            .contains("src/reexport-source.ts::renamed")
    );
    assert!(
        text(&mcp.call(
            "inspect",
            json!({"target": "src/reexport-use.ts::reexportEntry"})
        ))
        .contains("Direct callees: none")
    );

    repo.write(
        "src/constructor-service.ts",
        "export class Service { renamed() {} }\n",
    );
    assert!(
        text(&mcp.call(
            "inspect",
            json!({"target": "src/constructor-use.ts::constructorEntry"})
        ))
        .contains("no matching non-static method")
    );

    repo.write(
        "src/local-export-source.ts",
        "export function renamedLocal() {}\n",
    );
    assert!(
        text(&mcp.call(
            "inspect",
            json!({"target": "src/local-export-use.ts::localExportEntry"})
        ))
        .contains("Direct callees: none")
    );

    repo.write(
        "src/worker.ts",
        "export class Worker { leaf() {} next() {} entry() { this.next(); } }\n",
    );
    assert!(text(&mcp.call("find", json!({"query": "next"}))).contains("Worker.next"));
    assert!(
        text(&mcp.call(
            "trace",
            json!({
                "from": "src/worker.ts::Worker.entry",
                "to": "src/worker.ts::Worker.next"
            })
        ))
        .contains("Known CALLS path")
    );

    repo.write(
        "src/a.ts",
        "export const leaf = () => { return; };\nexport const fresh = async () => {};\n",
    );
    let refreshed = mcp.call("find", json!({"query": "fresh"}));
    assert!(text(&refreshed).contains("src/a.ts::fresh"));
    let diff = mcp.call("impact", json!({"target": "@diff"}));
    assert!(text(&diff).contains("Diff impact: HEAD -> working tree"));
    assert_eq!(diff["result"]["isError"], false);

    repo.write(
        "src/b.ts",
        "import { leaf } from './a';\nexport function leaf() {}\nexport function entry() { leaf(); }\n",
    );
    let ambiguous = mcp.call("inspect", json!({"target": "leaf"}));
    assert_eq!(ambiguous["result"]["isError"], true);
    assert!(text(&ambiguous).contains("ambiguous"));

    let fresh = Index::build(&repo.0).unwrap();
    let loaded = Index::open(&repo.0).unwrap();
    assert_eq!(meaning(loaded.graph()), meaning(fresh.graph()));
    assert_eq!(
        query::find(loaded.graph(), "fresh"),
        query::find(fresh.graph(), "fresh")
    );
}
