//! The agent loop: the model either answers or asks for a tool with
//! `<tool_call>{"name": ..., "arguments": {...}}</tool_call>` (the Hermes/Qwen
//! format, understood by most small models); we run the tool, hand back the
//! result and let the model go on. One text protocol for every engine.

use std::sync::OnceLock;

use anyhow::Result;
use async_trait::async_trait;
use regex::Regex;
use serde_json::Value;

use crate::engine::{Engine, Message, TokenSink};
use crate::tools::{self, Risk};

/// Tool calls per user message before we give up.
const MAX_STEPS: usize = 5;

/// Asks the user whether a risky tool may run (a y/n prompt in the CLI, a dialog in the app).
#[async_trait]
pub trait Confirm: Send + Sync {
    async fn confirm(&self, tool: &str, args: &Value) -> bool;
}

/// What happened during one turn, for the UI.
pub enum Event {
    ToolCall { name: String, args: Value },
    ToolResult { name: String, result: String },
}

pub struct Agent {
    engine: Box<dyn Engine>,
    history: Vec<Message>,
}

impl Agent {
    pub fn new(engine: Box<dyn Engine>, system_suffix: &str) -> Self {
        let system = format!("{}\n{}", system_prompt(), system_suffix)
            .trim()
            .to_string();
        Self {
            engine,
            history: vec![Message::new("system", system)],
        }
    }

    /// Starts a new conversation: keeps only the system prompt.
    pub fn reset(&mut self) {
        self.history.truncate(1);
    }

    pub fn engine_name(&self) -> String {
        self.engine.name()
    }

    /// Runs one user message to the final answer; returns it without tool markup.
    pub async fn send(
        &mut self,
        text: &str,
        confirm: &dyn Confirm,
        tokens: TokenSink,
        mut on_event: impl FnMut(Event),
    ) -> Result<String> {
        self.history.push(Message::new("user", text));
        for _ in 0..MAX_STEPS {
            let reply = self.engine.complete(&self.history, tokens.clone()).await?;
            let reply = strip_think(&reply);
            self.history.push(Message::new("assistant", reply.clone()));
            let Some((name, args)) = parse_call(&reply) else {
                return Ok(reply);
            };
            on_event(Event::ToolCall {
                name: name.clone(),
                args: args.clone(),
            });
            let result = run_tool(&name, &args, confirm).await;
            on_event(Event::ToolResult {
                name: name.clone(),
                result: result.clone(),
            });
            // Most chat templates have no `tool` role: a user turn works everywhere.
            self.history.push(Message::new(
                "user",
                format!("<tool_response>\n{result}\n</tool_response>"),
            ));
        }
        Ok("Не получилось за несколько шагов, попробуй сформулировать иначе.".into())
    }
}

async fn run_tool(name: &str, args: &Value, confirm: &dyn Confirm) -> String {
    let Some(tool) = tools::find(name) else {
        return format!("ошибка: нет инструмента {name}");
    };
    if tool.risk != Risk::Safe && !confirm.confirm(name, args).await {
        return "пользователь запретил это действие".into();
    }
    match (tool.run)(args) {
        Ok(out) => out,
        Err(e) => format!("ошибка: {e}"),
    }
}

fn system_prompt() -> String {
    let specs: Vec<String> = tools::all()
        .iter()
        .map(|t| {
            serde_json::json!({"name": t.name, "description": t.description, "parameters": (t.parameters)()})
                .to_string()
        })
        .collect();
    format!(
        "Ты — HomeLLM, голосовой помощник на компьютере пользователя. Отвечай по-русски, коротко.\n\
         Ты можешь управлять компьютером через инструменты:\n<tools>\n{}\n</tools>\n\
         Чтобы вызвать инструмент, ответь ТОЛЬКО так, без другого текста:\n\
         <tool_call>\n{{\"name\": \"имя\", \"arguments\": {{...}}}}\n</tool_call>\n\
         Результат придёт в <tool_response>. После него коротко скажи пользователю, что сделано.\n\
         Вызывай инструмент, только когда просят что-то сделать на компьютере. На остальное \
         (поболтать, пошутить, «мяукни») просто ответь текстом.\n\
         Никогда не говори, что что-то сделал, если не получил об этом <tool_response>.",
        specs.join("\n")
    )
}

fn strip_think(text: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?s)<think>.*?</think>").unwrap());
    re.replace_all(text, "").trim().to_string()
}

/// A call is a JSON object with a `name`, normally wrapped in `<tool_call>` tags. Small models
/// drop one or both tags, so the tags are optional; the object must start the (rest of the) reply.
fn parse_call(text: &str) -> Option<(String, Value)> {
    let body = match text.find("<tool_call>") {
        Some(at) => &text[at + "<tool_call>".len()..],
        None => text,
    };
    let body = body.trim_start();
    if !body.starts_with('{') {
        return None;
    }
    // The first JSON value; whatever follows (`</tool_call>`, chatter) is ignored.
    let call: Value = serde_json::Deserializer::from_str(body)
        .into_iter::<Value>()
        .next()?
        .ok()?;
    let name = call["name"].as_str()?.to_string();
    let args = match &call["arguments"] {
        Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
        other => other.clone(),
    };
    Some((name, args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_call() {
        let text = "<tool_call>\n{\"name\": \"media\", \"arguments\": {\"action\": \"next\"}}\n</tool_call>";
        let (name, args) = parse_call(text).unwrap();
        assert_eq!(name, "media");
        assert_eq!(args["action"], "next");
    }

    #[test]
    fn parses_a_bare_json_call() {
        let (name, _) = parse_call("{\"name\": \"system_info\", \"arguments\": {}}").unwrap();
        assert_eq!(name, "system_info");
    }

    #[test]
    fn parses_a_call_without_the_opening_tag() {
        let text = "{\"name\": \"open_app\", \"arguments\": {\"name\": \"notepad\"}}\n</tool_call>";
        let (name, args) = parse_call(text).unwrap();
        assert_eq!(name, "open_app");
        assert_eq!(args["name"], "notepad");
    }

    #[test]
    fn plain_text_is_an_answer() {
        assert!(parse_call("Привет! Чем помочь?").is_none());
    }

    #[test]
    fn strips_thinking() {
        assert_eq!(strip_think("<think>\nhmm\n</think>\n\nОк"), "Ок");
    }
}
