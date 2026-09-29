//! A minimal MCP client over stdio: start the server, `initialize`, `tools/list`,
//! `tools/call`. Newline-delimited JSON-RPC, one request at a time.

use std::process::Stdio;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

/// A configured server: `name: command args…` in the settings.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerSpec {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

/// Parses the settings lines `name: command arg arg…`; `#` starts a comment.
pub fn parse_specs(text: &str) -> Vec<ServerSpec> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (name, rest) = line.split_once(':')?;
            let mut words = rest.split_whitespace().map(String::from);
            let command = words.next()?;
            let name: String = name
                .trim()
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            (!name.is_empty()).then(|| ServerSpec {
                name,
                command,
                args: words.collect(),
            })
        })
        .collect()
}

pub struct McpTool {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

struct Io {
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

pub struct McpServer {
    pub spec: ServerSpec,
    pub tools: Vec<McpTool>,
    io: Mutex<Io>,
    _child: Child,
}

impl McpServer {
    pub async fn start(spec: ServerSpec) -> Result<Self> {
        // On Windows `npx` and friends are .cmd scripts: go through cmd.
        let mut command = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(&spec.command).args(&spec.args);
            c
        } else {
            let mut c = Command::new(&spec.command);
            c.args(&spec.args);
            c
        };
        #[cfg(windows)]
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        // Python servers on Windows read stdin in the system code page otherwise.
        command
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUTF8", "1");
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("не запустился {}", spec.command))?;
        let io = Io {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()).lines(),
            next_id: 1,
        };
        let mut server = Self {
            spec,
            tools: vec![],
            io: Mutex::new(io),
            _child: child,
        };
        server
            .request(
                "initialize",
                json!({"protocolVersion": "2025-06-18", "capabilities": {},
                       "clientInfo": {"name": "HomeLLM", "version": env!("CARGO_PKG_VERSION")}}),
            )
            .await?;
        server.notify("notifications/initialized").await?;
        let listed = server.request("tools/list", json!({})).await?;
        server.tools = listed["tools"]
            .as_array()
            .map(|tools| {
                tools
                    .iter()
                    .map(|t| McpTool {
                        name: t["name"].as_str().unwrap_or_default().to_string(),
                        description: t["description"].as_str().unwrap_or_default().to_string(),
                        schema: t["inputSchema"].clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(server)
    }

    async fn notify(&self, method: &str) -> Result<()> {
        let mut io = self.io.lock().await;
        let line = json!({"jsonrpc": "2.0", "method": method}).to_string() + "\n";
        io.stdin.write_all(line.as_bytes()).await?;
        Ok(io.stdin.flush().await?)
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let mut io = self.io.lock().await;
        let id = io.next_id;
        io.next_id += 1;
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
            .to_string()
            + "\n";
        io.stdin.write_all(line.as_bytes()).await?;
        io.stdin.flush().await?;
        let wait = async {
            // Skip notifications and anything that is not our answer.
            while let Some(line) = io.stdout.next_line().await? {
                let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if msg["id"].as_u64() != Some(id) {
                    continue;
                }
                if let Some(err) = msg.get("error") {
                    bail!("{}", err["message"].as_str().unwrap_or("ошибка MCP"));
                }
                return Ok(msg["result"].clone());
            }
            Err(anyhow!("сервер {} закрылся", self.spec.name))
        };
        tokio::time::timeout(std::time::Duration::from_secs(60), wait)
            .await
            .map_err(|_| anyhow!("сервер {} не ответил за минуту", self.spec.name))?
    }

    /// Calls a tool; the text parts of the result, joined.
    pub async fn call(&self, tool: &str, args: &Value) -> Result<String> {
        let result = self
            .request("tools/call", json!({"name": tool, "arguments": args}))
            .await?;
        let text: Vec<&str> = result["content"]
            .as_array()
            .map(|parts| parts.iter().filter_map(|p| p["text"].as_str()).collect())
            .unwrap_or_default();
        let text = text.join("\n");
        if result["isError"].as_bool() == Some(true) {
            bail!("{text}");
        }
        Ok(text.chars().take(4000).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs `python` on PATH: `cargo test -p homellm-core -- --ignored fake_mcp`.
    #[tokio::test]
    #[ignore]
    async fn talks_to_a_fake_mcp_server() {
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake_mcp.py");
        let spec = ServerSpec {
            name: "fake".into(),
            command: "python".into(),
            args: vec![script.into()],
        };
        let server = McpServer::start(spec).await.unwrap();
        assert_eq!(server.tools.len(), 1);
        assert_eq!(server.tools[0].name, "echo");
        assert_eq!(
            server
                .call("echo", &json!({"text": "привет"}))
                .await
                .unwrap(),
            "echo: привет"
        );
    }

    #[test]
    fn parses_server_lines() {
        let specs = parse_specs(
            "# мои серверы\nfiles: npx -y @modelcontextprotocol/server-filesystem D:\\Docs\n\nплохая строка\n",
        );
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "files");
        assert_eq!(specs[0].command, "npx");
        assert_eq!(specs[0].args.last().unwrap(), "D:\\Docs");
    }
}
