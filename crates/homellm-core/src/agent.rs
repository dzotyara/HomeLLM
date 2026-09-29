//! The agent loop: the model either answers or asks for a tool with
//! `<tool_call>{"name": ..., "arguments": {...}}</tool_call>` (the Hermes/Qwen
//! format, understood by most small models); we run the tool, hand back the
//! result and let the model go on. One text protocol for every engine.

use std::sync::{Arc, OnceLock};

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

/// Tools the shell adds on top of the PC ones: the app's own controls (models,
/// settings), so everything in the window can also be asked for in the chat.
#[async_trait]
pub trait ToolHost: Send + Sync {
    /// `{"name", "description", "parameters"}` for the prompt.
    fn specs(&self) -> Vec<Value>;
    /// `None` when the tool is not one of ours.
    async fn call(&self, name: &str, args: &Value) -> Option<String>;
}

/// What happened during one turn, for the UI.
pub enum Event {
    ToolCall { name: String, args: Value },
    ToolResult { name: String, result: String },
}

pub struct Agent {
    engine: Box<dyn Engine>,
    host: Option<Arc<dyn ToolHost>>,
    history: Vec<Message>,
}

impl Agent {
    pub fn new(engine: Box<dyn Engine>, system_suffix: &str) -> Self {
        Self::with_host(engine, system_suffix, None)
    }

    pub fn with_host(
        engine: Box<dyn Engine>,
        system_suffix: &str,
        host: Option<Arc<dyn ToolHost>>,
    ) -> Self {
        let mut specs: Vec<Value> = tools::all()
            .iter()
            .map(|t| serde_json::json!({"name": t.name, "description": t.description, "parameters": (t.parameters)()}))
            .collect();
        if let Some(host) = &host {
            specs.extend(host.specs());
        }
        let system = format!("{}\n{}", system_prompt(&specs), system_suffix)
            .trim()
            .to_string();
        Self {
            engine,
            host,
            history: vec![Message::new("system", system)],
        }
    }

    /// Starts a new conversation: keeps only the system prompt.
    pub fn reset(&mut self) {
        self.history.truncate(1);
    }

    /// The conversation without the system prompt, to save a chat.
    pub fn history(&self) -> &[Message] {
        &self.history[1..]
    }

    /// Continues a saved chat.
    pub fn set_history(&mut self, messages: Vec<Message>) {
        self.history.truncate(1);
        self.history
            .extend(messages.into_iter().filter(|m| m.role != "system"));
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
        let mut used_tool = false;
        let mut nudged = false;
        for _ in 0..MAX_STEPS {
            let reply = self.engine.complete(&self.history, tokens.clone()).await?;
            let reply = strip_think(&reply);
            self.history.push(Message::new("assistant", reply.clone()));
            let Some((name, args)) = parse_call(&reply) else {
                // Small models report actions they never took ("звук добавлен"): ask once more.
                if !used_tool && !nudged && claims_action(&reply) {
                    nudged = true;
                    self.history.push(Message::new(
                        "user",
                        "Ты не вызвал инструмент, значит ничего не сделано. Вызови нужный инструмент \
                         или честно скажи, что не можешь.",
                    ));
                    continue;
                }
                return Ok(reply);
            };
            used_tool = true;
            let (name, args) = fix_call(text, name, args);
            on_event(Event::ToolCall {
                name: name.clone(),
                args: args.clone(),
            });
            let result = self.run_tool(&name, &args, confirm).await;
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

    async fn run_tool(&self, name: &str, args: &Value, confirm: &dyn Confirm) -> String {
        let Some(tool) = tools::find(name) else {
            if let Some(host) = &self.host
                && let Some(out) = host.call(name, args).await
            {
                return out;
            }
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
}

/// Small models answer «громкость 70» with a volume step: when the user named a volume
/// level and the model reached for a step or mute, set that exact level instead.
fn fix_call(user_text: &str, name: String, args: Value) -> (String, Value) {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?i)(громкост|звук)\D{0,20}?(\d{1,3})\s*%?").unwrap());
    let is_step = name == "media"
        && args["action"]
            .as_str()
            .is_some_and(|a| a.starts_with("volume"));
    if (is_step || name == "set_volume")
        && let Some(level) = re
            .captures(user_text)
            .and_then(|c| c[2].parse::<u64>().ok())
            .filter(|l| *l <= 100)
    {
        return ("set_volume".into(), serde_json::json!({"percent": level}));
    }
    (name, args)
}

/// "Сделал", "включил", "готово"... — the reply says something was done.
fn claims_action(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(сдела(л|ла|но|на)|(вы)?включ(ил|ила|ен|ена|ено)|запуст(ил|ила)|запущен\w*|откры(л|ла|т|та|то)|(до|у|при)бав(ил|ила|лен|лена|лено)|(увелич|уменьш)(ил|ила|ен|ена|ено)|(по|у)став(ил|ила|лен|лена|лено)|установ(ил|ила|лен|лена|лено)|переключ(ил|ила|ен|ено)|готово)\b",
        )
        .unwrap()
    });
    re.is_match(text)
}

fn system_prompt(specs: &[Value]) -> String {
    let specs: Vec<String> = specs.iter().map(Value::to_string).collect();
    format!(
        "Ты — HomeLLM, голосовой помощник на компьютере пользователя. Отвечай по-русски, коротко.\n\
         Ты можешь управлять компьютером и самим приложением через инструменты:\n<tools>\n{}\n</tools>\n\
         Чтобы вызвать инструмент, ответь ТОЛЬКО так, без другого текста:\n\
         <tool_call>\n{{\"name\": \"имя\", \"arguments\": {{...}}}}\n</tool_call>\n\
         Результат придёт в <tool_response>. После него коротко скажи пользователю, что сделано.\n\
         Вызывай инструмент, только когда просят что-то сделать на компьютере или в приложении. \
         На остальное (поболтать, пошутить, написать код, «мяукни») просто ответь текстом.\n\
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
    fn a_named_volume_level_wins_over_a_step() {
        let step = serde_json::json!({"action": "volume_down"});
        assert_eq!(
            fix_call("сделай громкость 70", "media".into(), step.clone()).1["percent"],
            70
        );
        assert_eq!(
            fix_call("сделай мне звук в 100", "media".into(), step.clone()).0,
            "set_volume"
        );
        assert_eq!(
            fix_call("прибавь звук", "media".into(), step.clone()).0,
            "media"
        );
        assert_eq!(
            fix_call(
                "включи трек 5",
                "media".into(),
                serde_json::json!({"action": "next"})
            )
            .0,
            "media"
        );
    }

    #[test]
    fn spots_claimed_actions() {
        assert!(claims_action("Звук добавлен."));
        assert!(claims_action("Готово, громкость 30%."));
        assert!(!claims_action("Мяу! Чем ещё помочь?"));
    }

    #[test]
    fn plain_text_is_an_answer() {
        assert!(parse_call("Привет! Чем помочь?").is_none());
    }

    #[test]
    fn strips_thinking() {
        assert_eq!(strip_think("<think>\nhmm\n</think>\n\nОк"), "Ок");
    }

    /// A model that answers from a script and records what it was shown.
    struct Scripted {
        replies: std::sync::Mutex<Vec<&'static str>>,
        seen: std::sync::Mutex<Vec<Vec<Message>>>,
    }

    impl Scripted {
        fn new(replies: &[&'static str]) -> Self {
            Self {
                replies: std::sync::Mutex::new(replies.iter().rev().copied().collect()),
                seen: Default::default(),
            }
        }
    }

    #[async_trait]
    impl Engine for Scripted {
        fn name(&self) -> String {
            "scripted".into()
        }
        async fn complete(&self, messages: &[Message], _: TokenSink) -> Result<String> {
            self.seen.lock().unwrap().push(messages.to_vec());
            Ok(self
                .replies
                .lock()
                .unwrap()
                .pop()
                .unwrap_or("конец")
                .to_string())
        }
    }

    struct Answer(bool);

    #[async_trait]
    impl Confirm for Answer {
        async fn confirm(&self, _: &str, _: &Value) -> bool {
            self.0
        }
    }

    struct Host;

    #[async_trait]
    impl ToolHost for Host {
        fn specs(&self) -> Vec<Value> {
            vec![
                serde_json::json!({"name": "list_models", "description": "каталог", "parameters": {}}),
            ]
        }
        async fn call(&self, name: &str, _: &Value) -> Option<String> {
            (name == "list_models").then(|| "qwen3-8b".to_string())
        }
    }

    async fn run(
        replies: &[&'static str],
        allow: bool,
        text: &str,
    ) -> (Agent, String, Vec<String>) {
        let mut agent =
            Agent::with_host(Box::new(Scripted::new(replies)), "", Some(Arc::new(Host)));
        let mut events = vec![];
        let answer = agent
            .send(text, &Answer(allow), None, |e| match e {
                Event::ToolCall { name, .. } => events.push(format!("call {name}")),
                Event::ToolResult { result, .. } => events.push(format!("result {result}")),
            })
            .await
            .unwrap();
        (agent, answer, events)
    }

    #[tokio::test]
    async fn runs_a_tool_and_answers() {
        let (agent, answer, events) = run(
            &[
                "<tool_call>{\"name\": \"system_info\", \"arguments\": {}}</tool_call>",
                "Память в порядке.",
            ],
            true,
            "память?",
        )
        .await;
        assert_eq!(answer, "Память в порядке.");
        assert_eq!(events[0], "call system_info");
        assert!(events[1].starts_with("result unix-время"));
        assert!(agent.history()[2].content.starts_with("<tool_response>"));
    }

    #[tokio::test]
    async fn re_asks_once_when_an_action_is_made_up() {
        let (agent, answer, events) = run(
            &["Звук добавлен.", "Не могу, нет такого инструмента."],
            true,
            "прибавь",
        )
        .await;
        assert!(events.is_empty());
        assert_eq!(answer, "Не могу, нет такого инструмента.");
        assert!(
            agent
                .history()
                .iter()
                .any(|m| m.content.starts_with("Ты не вызвал инструмент"))
        );
    }

    #[tokio::test]
    async fn a_refused_risky_tool_does_not_run() {
        let (_, _, events) = run(
            &[
                "{\"name\": \"open_app\", \"arguments\": {\"name\": \"calc\"}}",
                "Ок, не запускаю.",
            ],
            false,
            "калькулятор",
        )
        .await;
        assert_eq!(
            events,
            ["call open_app", "result пользователь запретил это действие"]
        );
    }

    #[tokio::test]
    async fn app_tools_come_from_the_host() {
        let (agent, _, events) = run(
            &[
                "{\"name\": \"list_models\", \"arguments\": {}}",
                "Есть qwen3-8b.",
            ],
            true,
            "модели?",
        )
        .await;
        assert_eq!(events, ["call list_models", "result qwen3-8b"]);
        assert!(
            agent
                .history
                .first()
                .unwrap()
                .content
                .contains("list_models")
        );
    }

    #[tokio::test]
    async fn an_unknown_tool_is_reported_to_the_model() {
        let (_, _, events) = run(
            &["{\"name\": \"fly\", \"arguments\": {}}", "Не умею."],
            true,
            "лети",
        )
        .await;
        assert_eq!(events[1], "result ошибка: нет инструмента fly");
    }

    #[test]
    fn saved_history_replaces_the_conversation_but_keeps_the_prompt() {
        let mut agent = Agent::new(Box::new(Scripted::new(&[])), "");
        agent.set_history(vec![
            Message::new("system", "чужой"),
            Message::new("user", "привет"),
        ]);
        assert_eq!(agent.history().len(), 1);
        assert!(agent.history[0].content.contains("HomeLLM"));
        agent.reset();
        assert!(agent.history().is_empty());
    }
}
