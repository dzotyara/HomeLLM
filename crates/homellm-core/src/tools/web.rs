//! Internet search, off unless the user turns it on in the settings: HomeLLM is
//! offline by default. No API keys: DuckDuckGo's HTML page and a plain page reader.

use std::sync::OnceLock;

use anyhow::{Result, bail};
use regex::Regex;
use serde_json::{Value, json};

use super::{Risk, Tool};

const AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) HomeLLM";
const PAGE_LIMIT: usize = 4000;

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "web_search",
            description: "Найти в интернете (свежие новости, факты, которых ты не знаешь). Возвращает заголовки, ссылки и кратко.",
            parameters: || json!({"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]}),
            risk: Risk::Safe,
            run: search,
        },
        Tool {
            name: "read_page",
            description: "Прочитать текст веб-страницы по ссылке (например из web_search).",
            parameters: || json!({"type": "object", "properties": {"url": {"type": "string"}}, "required": ["url"]}),
            risk: Risk::Safe,
            run: read_page,
        },
    ]
}

fn get(url: &str) -> Result<String> {
    Ok(ureq::get(url)
        .header("User-Agent", AGENT)
        .call()?
        .body_mut()
        .read_to_string()?)
}

fn search(args: &Value) -> Result<String> {
    let query = args["query"].as_str().filter(|q| !q.trim().is_empty());
    let Some(query) = query else {
        bail!("missing argument `query`")
    };
    let html = get(&format!(
        "https://html.duckduckgo.com/html/?q={}",
        encode(query)
    ))?;
    let results = parse_results(&html);
    if results.is_empty() {
        return Ok("ничего не нашлось".into());
    }
    Ok(results
        .iter()
        .take(5)
        .map(|(title, url, snippet)| format!("{title}\n{url}\n{snippet}"))
        .collect::<Vec<_>>()
        .join("\n\n"))
}

/// (title, url, snippet) from DuckDuckGo's HTML results.
fn parse_results(html: &str) -> Vec<(String, String, String)> {
    static LINK: OnceLock<Regex> = OnceLock::new();
    static SNIPPET: OnceLock<Regex> = OnceLock::new();
    let link = LINK
        .get_or_init(|| Regex::new(r#"(?s)class="result__a" href="([^"]+)">(.*?)</a>"#).unwrap());
    let snippet = SNIPPET
        .get_or_init(|| Regex::new(r#"(?s)class="result__snippet"[^>]*>(.*?)</a>"#).unwrap());
    let snippets: Vec<String> = snippet.captures_iter(html).map(|c| text(&c[1])).collect();
    link.captures_iter(html)
        .enumerate()
        .map(|(i, c)| {
            let href = c[1].replace("&amp;", "&");
            // Results go through a redirect: the target is in `uddg`.
            let url = href
                .split("uddg=")
                .nth(1)
                .map(|u| decode(u.split('&').next().unwrap_or(u)))
                .unwrap_or(href);
            (
                text(&c[2]),
                url,
                snippets.get(i).cloned().unwrap_or_default(),
            )
        })
        .collect()
}

fn read_page(args: &Value) -> Result<String> {
    let url = args["url"].as_str().unwrap_or_default();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        bail!("only http(s) links");
    }
    static DROP: OnceLock<Regex> = OnceLock::new();
    let drop = DROP.get_or_init(|| {
        // No backreferences in Rust regex: one alternative per tag.
        Regex::new(
            r"(?is)<script\b.*?</script>|<style\b.*?</style>|<nav\b.*?</nav>|<header\b.*?</header>|<footer\b.*?</footer>|<svg\b.*?</svg>",
        )
        .unwrap()
    });
    let body = text(&drop.replace_all(&get(url)?, " "));
    let mut cut: String = body.chars().take(PAGE_LIMIT).collect();
    if body.chars().count() > PAGE_LIMIT {
        cut.push_str(" …(обрезано)");
    }
    Ok(cut)
}

/// Tags out, entities decoded, whitespace squeezed.
fn text(html: &str) -> String {
    static TAG: OnceLock<Regex> = OnceLock::new();
    let tag = TAG.get_or_init(|| Regex::new(r"(?s)<[^>]*>").unwrap());
    let plain = tag
        .replace_all(html, " ")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ");
    plain.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(b) = text
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hits the network: `cargo test -p homellm-core -- --ignored live`.
    #[test]
    #[ignore]
    fn live_search_and_read() {
        let found = search(&json!({"query": "Tauri framework"})).unwrap();
        assert!(found.contains("https://"), "{found}");
        let page = read_page(&json!({"url": "https://tauri.app/"})).unwrap();
        assert!(page.to_lowercase().contains("tauri"));
    }

    #[test]
    fn parses_duckduckgo_results() {
        let html = r#"<a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Ftauri.app%2F&amp;rut=1">Tauri <b>2</b></a>
            <a class="result__snippet" href="x">Build <b>tiny</b> apps</a>"#;
        let r = parse_results(html);
        assert_eq!(
            r[0],
            (
                "Tauri 2".into(),
                "https://tauri.app/".into(),
                "Build tiny apps".into()
            )
        );
    }
}
