//! Spotify through its Web API: the user's own app on developer.spotify.com gives a Client ID
//! (no secret: the PKCE flow), then search and playback on their devices (Spotify Connect,
//! needs Premium). The tokens live in `spotify.json` next to the settings, never in the
//! settings the window sees.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Must be in the app's Redirect URIs on developer.spotify.com exactly like this.
pub const REDIRECT_URI: &str = "http://127.0.0.1:8898/callback";
const SCOPES: &str =
    "user-read-playback-state user-modify-playback-state user-read-currently-playing";
const API: &str = "https://api.spotify.com/v1";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Default, Serialize, Deserialize)]
struct Tokens {
    client_id: String,
    access: String,
    refresh: String,
    /// Unix seconds.
    expires: u64,
}

fn tokens_file() -> Option<PathBuf> {
    crate::settings::config_file("spotify.json")
}

fn load() -> Option<Tokens> {
    let text = std::fs::read_to_string(tokens_file()?).ok()?;
    serde_json::from_str::<Tokens>(&text)
        .ok()
        .filter(|t| !t.refresh.is_empty())
}

fn store(tokens: &Tokens) -> Result<()> {
    let path = tokens_file().context("no config folder")?;
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(path, serde_json::to_string(tokens)?)?;
    Ok(())
}

pub fn connected() -> bool {
    load().is_some()
}

pub fn disconnect() {
    if let Some(path) = tokens_file() {
        let _ = std::fs::remove_file(path);
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Opens Spotify's consent page in the browser and waits for its redirect; returns the
/// account's name.
pub fn login(client_id: &str) -> Result<String> {
    let client_id = client_id.trim();
    if client_id.is_empty() {
        bail!("укажите Client ID приложения Spotify");
    }
    let listener =
        TcpListener::bind("127.0.0.1:8898").context("порт 8898 занят другой программой")?;
    let verifier = random_string(64);
    let state = random_string(16);
    let url = format!(
        "https://accounts.spotify.com/authorize?response_type=code&client_id={}&scope={}&redirect_uri={}\
         &code_challenge_method=S256&code_challenge={}&state={state}",
        encode(client_id),
        encode(SCOPES),
        encode(REDIRECT_URI),
        challenge(&verifier),
    );
    open::that_detached(&url)?;
    let code = wait_for_code(&listener, &state)?;
    let reply = token_request(&[
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", REDIRECT_URI),
        ("client_id", client_id),
        ("code_verifier", &verifier),
    ])?;
    let mut tokens = Tokens {
        client_id: client_id.into(),
        ..Default::default()
    };
    apply(&mut tokens, &reply)?;
    store(&tokens)?;
    let (_, me) = call("GET", "/me", None)?;
    Ok(me["display_name"]
        .as_str()
        .or(me["id"].as_str())
        .unwrap_or("Spotify")
        .to_string())
}

fn wait_for_code(listener: &TcpListener, state: &str) -> Result<String> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + LOGIN_TIMEOUT;
    while Instant::now() < deadline {
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        // Windows hands out accepted sockets in the listener's non-blocking mode.
        stream.set_nonblocking(false)?;
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line)?;
        // "GET /callback?code=…&state=… HTTP/1.1"; anything else (a favicon) is not ours.
        let Some(query) = line
            .split_whitespace()
            .nth(1)
            .and_then(|path| path.strip_prefix("/callback?"))
        else {
            respond(&mut stream, "404 Not Found", "");
            continue;
        };
        let param = |name: &str| {
            query
                .split('&')
                .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
                .map(str::to_string)
        };
        if param("state").as_deref() != Some(state) {
            respond(&mut stream, "400 Bad Request", "Неверный ответ Spotify.");
            continue;
        }
        if let Some(code) = param("code") {
            respond(
                &mut stream,
                "200 OK",
                "Spotify подключён к HomeLLM. Эту вкладку можно закрыть.",
            );
            return Ok(code);
        }
        respond(
            &mut stream,
            "200 OK",
            "Spotify не подключён: доступ не дан.",
        );
        bail!("доступ не дан: {}", param("error").unwrap_or_default());
    }
    bail!("Spotify не ответил за 3 минуты")
}

fn respond(stream: &mut TcpStream, status: &str, text: &str) {
    let body = format!("<!doctype html><meta charset=utf-8><title>HomeLLM</title><p>{text}</p>");
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

fn token_request(form: &[(&str, &str)]) -> Result<Value> {
    let mut reply = ureq::post("https://accounts.spotify.com/api/token")
        .config()
        .http_status_as_error(false)
        .build()
        .send_form(form.iter().copied())?;
    let ok = reply.status().is_success();
    let body: Value = serde_json::from_str(&reply.body_mut().read_to_string()?).unwrap_or_default();
    if !ok {
        let why = body["error_description"]
            .as_str()
            .or(body["error"].as_str());
        bail!("Spotify отказал: {}", why.unwrap_or("неизвестная ошибка"));
    }
    Ok(body)
}

fn apply(tokens: &mut Tokens, reply: &Value) -> Result<()> {
    tokens.access = reply["access_token"]
        .as_str()
        .ok_or_else(|| anyhow!("Spotify не выдал токен"))?
        .into();
    tokens.expires = now() + reply["expires_in"].as_u64().unwrap_or(3600);
    // A refresh may or may not rotate the refresh token.
    if let Some(refresh) = reply["refresh_token"].as_str() {
        tokens.refresh = refresh.into();
    }
    Ok(())
}

fn access_token() -> Result<String> {
    let mut tokens = load().ok_or_else(|| anyhow!("Spotify не подключён"))?;
    if tokens.expires > now() + 30 {
        return Ok(tokens.access);
    }
    let reply = token_request(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", &tokens.refresh),
        ("client_id", &tokens.client_id),
    ])
    .context("подключите Spotify в настройках заново")?;
    apply(&mut tokens, &reply)?;
    store(&tokens)?;
    Ok(tokens.access)
}

/// A Web API call; the status is returned, not turned into an error, and an empty body is `Null`.
fn call(method: &str, path: &str, body: Option<&Value>) -> Result<(u16, Value)> {
    let url = format!("{API}{path}");
    let auth = format!("Bearer {}", access_token()?);
    let mut reply = match method {
        "PUT" => ureq::put(&url)
            .header("Authorization", &auth)
            .header("Content-Type", "application/json")
            .config()
            .http_status_as_error(false)
            .build()
            .send(body.map(Value::to_string).unwrap_or_default())?,
        _ => ureq::get(&url)
            .header("Authorization", &auth)
            .config()
            .http_status_as_error(false)
            .build()
            .call()?,
    };
    let status = reply.status().as_u16();
    let text = reply.body_mut().read_to_string().unwrap_or_default();
    Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
}

fn explain(status: u16, body: &Value) -> anyhow::Error {
    match status {
        401 => anyhow!("Spotify не принял вход: подключите его в настройках заново"),
        403 => anyhow!("Spotify отказал: управлять воспроизведением можно только с Premium"),
        429 => anyhow!("слишком много запросов к Spotify, попробуйте через минуту"),
        _ => anyhow!(
            "Spotify ответил {status}: {}",
            body["error"]["message"].as_str().unwrap_or("без пояснений")
        ),
    }
}

/// Finds a track, artist, album or playlist and starts it; returns what plays.
pub fn play(query: &str, kind: &str) -> Result<String> {
    let kind = match kind {
        "artist" | "album" | "playlist" => kind,
        _ => "track",
    };
    let (status, found) = call(
        "GET",
        &format!("/search?type={kind}&limit=5&q={}", encode(query)),
        None,
    )?;
    if status != 200 {
        return Err(explain(status, &found));
    }
    // Playlist results may contain nulls.
    let item = found[format!("{kind}s")]["items"]
        .as_array()
        .and_then(|items| items.iter().find(|i| i["uri"].is_string()))
        .ok_or_else(|| anyhow!("в Spotify ничего не нашлось по запросу «{query}»"))?;
    let uri = item["uri"].as_str().unwrap_or_default();
    let body = if kind == "track" {
        json!({"uris": [uri]})
    } else {
        json!({"context_uri": uri})
    };
    start(&body)?;
    Ok(format!("играет в Spotify: {}", describe(item)))
}

/// Plays on the active device; with none, on this PC's Spotify app, started if needed.
fn start(body: &Value) -> Result<()> {
    let (status, reply) = call("PUT", "/me/player/play", Some(body))?;
    match status {
        200..=299 => return Ok(()),
        404 => {}
        _ => return Err(explain(status, &reply)),
    }
    let device = wait_for_device()?;
    let (status, reply) = call(
        "PUT",
        &format!("/me/player/play?device_id={device}"),
        Some(body),
    )?;
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(explain(status, &reply))
    }
}

fn wait_for_device() -> Result<String> {
    for attempt in 0..20 {
        let (_, list) = call("GET", "/me/player/devices", None)?;
        if let Some(id) = pick_device(&list) {
            return Ok(id);
        }
        if attempt == 0 {
            let _ = open::that_detached("spotify:");
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    bail!("Spotify не запущен ни на одном устройстве")
}

/// The active device, else a computer, else any.
fn pick_device(list: &Value) -> Option<String> {
    let devices = list["devices"].as_array()?;
    let usable = |d: &&Value| d["id"].is_string() && d["is_restricted"] != true;
    devices
        .iter()
        .filter(usable)
        .find(|d| d["is_active"] == true)
        .or_else(|| {
            devices
                .iter()
                .filter(usable)
                .find(|d| d["type"] == "Computer")
        })
        .or_else(|| devices.iter().find(usable))
        .and_then(|d| d["id"].as_str().map(str::to_string))
}

/// What plays now, `None` when nothing does.
pub fn now_playing() -> Result<Option<String>> {
    let (status, body) = call("GET", "/me/player/currently-playing", None)?;
    match status {
        204 => return Ok(None),
        200 => {}
        _ => return Err(explain(status, &body)),
    }
    if body["item"].is_null() {
        return Ok(None);
    }
    let paused = if body["is_playing"] == true {
        ""
    } else {
        " (на паузе)"
    };
    Ok(Some(format!(
        "Spotify: {}{paused}",
        describe(&body["item"])
    )))
}

/// «Artist — Track», «Artist — Album», «Playlist» or «Artist».
fn describe(item: &Value) -> String {
    let name = item["name"].as_str().unwrap_or("?");
    let artists: Vec<&str> = item["artists"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x["name"].as_str()).collect())
        .unwrap_or_default();
    if artists.is_empty() {
        name.to_string()
    } else {
        format!("{} — {name}", artists.join(", "))
    }
}

fn random_string(len: usize) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut bytes = vec![0u8; len];
    getrandom::fill(&mut bytes).expect("no system randomness");
    bytes
        .iter()
        .map(|b| CHARS[*b as usize % CHARS.len()] as char)
        .collect()
}

fn challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_matches_rfc_7636() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn the_active_device_wins_then_a_computer() {
        let list = json!({"devices": [
            {"id": "phone", "type": "Smartphone", "is_active": false},
            {"id": "pc", "type": "Computer", "is_active": false},
            {"id": "tv", "type": "TV", "is_active": true},
        ]});
        assert_eq!(pick_device(&list).as_deref(), Some("tv"));
        let list = json!({"devices": [
            {"id": "phone", "type": "Smartphone", "is_active": false},
            {"id": "pc", "type": "Computer", "is_active": false},
        ]});
        assert_eq!(pick_device(&list).as_deref(), Some("pc"));
        assert_eq!(pick_device(&json!({"devices": []})), None);
    }

    #[test]
    fn restricted_devices_are_skipped() {
        let list = json!({"devices": [
            {"id": "speaker", "type": "Speaker", "is_active": true, "is_restricted": true},
            {"id": "phone", "type": "Smartphone", "is_active": false},
        ]});
        assert_eq!(pick_device(&list).as_deref(), Some("phone"));
    }

    #[test]
    fn items_are_described_with_their_artists() {
        let track = json!({"name": "Группа крови", "artists": [{"name": "Кино"}]});
        assert_eq!(describe(&track), "Кино — Группа крови");
        assert_eq!(describe(&json!({"name": "Chill"})), "Chill");
    }

    #[test]
    fn queries_are_percent_encoded() {
        assert_eq!(encode("Кино a&b"), "%D0%9A%D0%B8%D0%BD%D0%BE%20a%26b");
    }
}
