//! Seeing and using other programs' windows: which windows are open, what is in them (Windows
//! UI Automation: the buttons, links and fields a screen reader sees, by name), focusing one,
//! scrolling, pressing keys, typing and clicking an element by its name.
//!
//! Without a window named, the target is the topmost window of another program: while the
//! user types to HomeLLM, HomeLLM itself is the active window.

use anyhow::{Result, anyhow, bail};
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use serde_json::{Value, json};

use super::{Risk, Tool};

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "list_windows",
            description: "Какие окна программ открыты (заголовок и программа) и какое было активным. Начни с этого, если нужно что-то сделать в другой программе.",
            parameters: || json!({"type": "object", "properties": {}}),
            risk: Risk::Safe,
            run: list_windows,
        },
        Tool {
            name: "focus_window",
            description: "Переключиться на окно программы по части заголовка или имени программы (chrome, spotify, telegram, «YouTube»).",
            parameters: || json!({"type": "object", "properties": {"window": {"type": "string"}}, "required": ["window"]}),
            risk: Risk::Safe,
            run: focus_window,
        },
        Tool {
            name: "read_window",
            description: "Что есть в окне: кнопки, ссылки, поля, вкладки и текст — по названиям. Без window — последнее активное окно другой программы. Перед click_element. У браузера видны вкладки и кнопки, но не сама страница: для неё look_at_screen или read_page по ссылке.",
            parameters: || json!({"type": "object", "properties": {"window": {"type": "string"}}}),
            risk: Risk::Safe,
            run: read_window,
        },
        Tool {
            name: "click_element",
            description: "Нажать кнопку, ссылку, вкладку или пункт в окне по названию из read_window («Следующий трек», «Подписаться»).",
            parameters: || {
                json!({"type": "object", "properties": {"name": {"type": "string"}, "window": {"type": "string"}},
                    "required": ["name"]})
            },
            risk: Risk::Confirm,
            run: click_element,
        },
        Tool {
            name: "scroll",
            description: "Листать окно (браузер, документ): direction down/up — на страницу, top/bottom — в начало/конец; pages — сколько страниц.",
            parameters: || {
                json!({"type": "object", "properties": {
                    "direction": {"type": "string", "enum": ["down", "up", "top", "bottom"]},
                    "pages": {"type": "integer", "minimum": 1, "maximum": 20},
                    "window": {"type": "string"}},
                    "required": ["direction"]})
            },
            risk: Risk::Safe,
            run: scroll,
        },
        Tool {
            name: "press_keys",
            description: "Нажать сочетание клавиш в окне: «ctrl+t» (новая вкладка), «ctrl+w», «alt+left» (назад), «f5», «space», «enter». Несколько — через пробел.",
            parameters: || {
                json!({"type": "object", "properties": {"keys": {"type": "string"}, "window": {"type": "string"}},
                    "required": ["keys"]})
            },
            risk: Risk::Confirm,
            run: press_keys,
        },
        Tool {
            name: "close_app",
            description: "Закрыть программу (все её окна), как крестиком. save=true — сначала сохранить (Ctrl+S в каждом окне). force=true — завершить принудительно, если не закрывается (несохранённое пропадёт).",
            parameters: || {
                json!({"type": "object", "properties": {"app": {"type": "string"}, "save": {"type": "boolean"},
                    "force": {"type": "boolean"}}, "required": ["app"]})
            },
            risk: Risk::Confirm,
            run: close_app,
        },
        Tool {
            name: "type_text",
            description: "Напечатать текст в окне (в поле, где стоит курсор); enter=true — нажать Enter после.",
            parameters: || {
                json!({"type": "object", "properties": {"text": {"type": "string"}, "window": {"type": "string"},
                    "enter": {"type": "boolean"}}, "required": ["text"]})
            },
            risk: Risk::Confirm,
            run: type_text,
        },
    ]
}

fn window_arg(args: &Value) -> Option<&str> {
    args["window"]
        .as_str()
        .map(str::trim)
        .filter(|w| !w.is_empty())
}

fn enigo() -> Result<Enigo> {
    Enigo::new(&Settings::default()).map_err(|e| anyhow!("{e}"))
}

fn list_windows(_: &Value) -> Result<String> {
    let windows = platform::windows()?;
    if windows.is_empty() {
        return Ok("открытых окон нет".into());
    }
    Ok(windows
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let mark = if i == 0 {
                " (последнее активное)"
            } else {
                ""
            };
            format!("{} — {}{mark}", w.title, w.app)
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn focus_window(args: &Value) -> Result<String> {
    let query = window_arg(args).ok_or_else(|| anyhow!("missing argument `window`"))?;
    let w = platform::focus(Some(query))?;
    Ok(format!("активно окно: {} — {}", w.title, w.app))
}

fn read_window(args: &Value) -> Result<String> {
    let w = platform::find(window_arg(args))?;
    let elements = platform::elements(&w)?;
    if elements.is_empty() {
        return Ok(format!(
            "{} — {}: элементов не видно (программа не открывает их системе; попробуй look_at_screen или клавиши)",
            w.title, w.app
        ));
    }
    let mut out = format!("{} — {}:\n", w.title, w.app);
    out.push_str(&elements.join("\n"));
    Ok(out)
}

fn click_element(args: &Value) -> Result<String> {
    let name = args["name"]
        .as_str()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| anyhow!("missing argument `name`"))?;
    let w = platform::find(window_arg(args))?;
    let done = platform::click(&w, name)?;
    Ok(format!("{done} в окне {}", w.app))
}

fn scroll(args: &Value) -> Result<String> {
    let direction = args["direction"].as_str().unwrap_or("down");
    let pages = args["pages"].as_u64().unwrap_or(1).clamp(1, 20) as usize;
    let (key, times) = match direction {
        "up" => (Key::PageUp, pages),
        "top" => (Key::Home, 1),
        "bottom" => (Key::End, 1),
        _ => (Key::PageDown, pages),
    };
    let w = platform::focus(window_arg(args))?;
    let mut enigo = enigo()?;
    for _ in 0..times {
        enigo
            .key(key, Direction::Click)
            .map_err(|e| anyhow!("{e}"))?;
        std::thread::sleep(std::time::Duration::from_millis(60));
    }
    Ok(format!("пролистано ({direction}) в окне {}", w.app))
}

fn press_keys(args: &Value) -> Result<String> {
    let keys = args["keys"]
        .as_str()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .ok_or_else(|| anyhow!("missing argument `keys`"))?;
    // Parse everything before touching the keyboard: a typo presses nothing.
    let combos = keys
        .split_whitespace()
        .map(parse_combo)
        .collect::<Result<Vec<_>>>()?;
    let w = platform::focus(window_arg(args))?;
    let mut enigo = enigo()?;
    for (mods, key) in combos {
        press_combo(&mut enigo, &mods, key)?;
        std::thread::sleep(std::time::Duration::from_millis(80));
    }
    Ok(format!("нажато {keys} в окне {}", w.app))
}

fn type_text(args: &Value) -> Result<String> {
    let text = args["text"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| anyhow!("missing argument `text`"))?;
    let w = platform::focus(window_arg(args))?;
    let mut enigo = enigo()?;
    enigo.text(text).map_err(|e| anyhow!("{e}"))?;
    if args["enter"].as_bool().unwrap_or(false) {
        enigo
            .key(Key::Return, Direction::Click)
            .map_err(|e| anyhow!("{e}"))?;
    }
    Ok(format!("напечатано в окне {}", w.app))
}

fn close_app(args: &Value) -> Result<String> {
    let query = args["app"]
        .as_str()
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .ok_or_else(|| anyhow!("missing argument `app`"))?;
    let target = pick(platform::windows()?, Some(query))?;
    let app = target.app.clone();
    let of_app =
        |all: Vec<Window>| -> Vec<Window> { all.into_iter().filter(|w| w.app == app).collect() };
    let windows = of_app(platform::windows()?);
    if args["save"].as_bool().unwrap_or(false) {
        let mut enigo = enigo()?;
        for w in &windows {
            platform::focus(Some(&w.title))?;
            press_combo(&mut enigo, &[Key::Control], char_key('s'))?;
            // A save may take a moment, or open a «Save as» dialog for a new file.
            std::thread::sleep(std::time::Duration::from_millis(1500));
        }
    }
    for w in &windows {
        platform::close(w);
    }
    // Programs ask «save changes?» or take a while to quit: wait a little.
    for _ in 0..16 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if of_app(platform::windows()?).is_empty() {
            return Ok(format!("{app} закрыта"));
        }
    }
    if args["force"].as_bool().unwrap_or(false) {
        platform::kill(&app)?;
        return Ok(format!("{app} завершена принудительно"));
    }
    let left: Vec<String> = of_app(platform::windows()?)
        .into_iter()
        .map(|w| w.title)
        .collect();
    Ok(format!(
        "{app} не закрылась — открыто: {}. Возможно, она спрашивает, сохранить ли изменения: посмотри read_window",
        left.join("; ")
    ))
}

fn press_combo(enigo: &mut Enigo, mods: &[Key], key: Key) -> Result<()> {
    let err = |e: enigo::InputError| anyhow!("{e}");
    for m in mods {
        enigo.key(*m, Direction::Press).map_err(err)?;
    }
    let result = enigo.key(key, Direction::Click).map_err(err);
    // Modifiers always go up again, even if the key failed: a stuck Ctrl is worse.
    for m in mods.iter().rev() {
        let _ = enigo.key(*m, Direction::Release);
    }
    result
}

/// «ctrl+shift+t» into modifiers and the key. Latin letters and digits go as virtual key codes
/// on Windows: with a Russian layout the character 't' has no key, and Ctrl+T would not fire.
fn parse_combo(combo: &str) -> Result<(Vec<Key>, Key)> {
    let parts: Vec<String> = combo.split('+').map(|p| p.trim().to_lowercase()).collect();
    let (last, mods) = parts
        .split_last()
        .ok_or_else(|| anyhow!("empty key combination"))?;
    let mods = mods
        .iter()
        .map(|m| match m.as_str() {
            "ctrl" | "control" | "ctl" => Ok(Key::Control),
            "alt" => Ok(Key::Alt),
            "shift" => Ok(Key::Shift),
            "win" | "meta" | "cmd" | "super" => Ok(Key::Meta),
            other => bail!("unknown modifier `{other}` in `{combo}`"),
        })
        .collect::<Result<Vec<_>>>()?;
    let key = match last.as_str() {
        "enter" | "return" => Key::Return,
        "esc" | "escape" => Key::Escape,
        "tab" => Key::Tab,
        "space" | "пробел" => Key::Space,
        "backspace" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "up" => Key::UpArrow,
        "down" => Key::DownArrow,
        "left" => Key::LeftArrow,
        "right" => Key::RightArrow,
        "pageup" | "pgup" => Key::PageUp,
        "pagedown" | "pgdn" => Key::PageDown,
        "home" => Key::Home,
        "end" => Key::End,
        f if f.len() >= 2
            && f.starts_with('f')
            && f[1..].parse::<u8>().is_ok_and(|n| (1..=12).contains(&n)) =>
        {
            function_key(f[1..].parse().unwrap())
        }
        k if k.chars().count() == 1 => char_key(k.chars().next().unwrap()),
        other => bail!("unknown key `{other}` in `{combo}`"),
    };
    Ok((mods, key))
}

fn function_key(n: u8) -> Key {
    [
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::F11,
        Key::F12,
    ][(n - 1) as usize]
}

fn char_key(c: char) -> Key {
    #[cfg(windows)]
    if c.is_ascii_alphanumeric() {
        // VK_A..VK_Z and VK_0..VK_9 are the upper-case ASCII codes.
        return Key::Other(c.to_ascii_uppercase() as u32);
    }
    Key::Unicode(c)
}

/// A top-level window of another program.
#[derive(Debug, Clone)]
pub struct Window {
    #[allow(dead_code)]
    handle: isize,
    title: String,
    /// The program, e.g. chrome.exe.
    app: String,
}

/// The window the query names (part of the title or the program's name, any case), else the
/// topmost one of another program.
fn pick(windows: Vec<Window>, query: Option<&str>) -> Result<Window> {
    let Some(query) = query else {
        return windows
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("открытых окон нет"));
    };
    let q = query.to_lowercase();
    let q = q.trim_end_matches(".exe");
    let app = |w: &Window| w.app.to_lowercase().trim_end_matches(".exe").to_string();
    // The program's exact name first («chrome»), then a part of it or of the title.
    let found = windows
        .iter()
        .find(|w| app(w) == q)
        .or_else(|| {
            windows
                .iter()
                .find(|w| app(w).contains(q) || w.title.to_lowercase().contains(q))
        })
        .cloned();
    found.ok_or_else(|| anyhow!("окно «{query}» не найдено; открытые окна покажет list_windows"))
}

#[cfg(windows)]
mod platform {
    use std::time::{Duration, Instant};

    use anyhow::{Result, anyhow, bail};
    use windows::Win32::Foundation::WPARAM;
    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
    use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::Win32::UI::Accessibility::*;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GWL_EXSTYLE, GetForegroundWindow, GetWindowLongW, GetWindowTextW,
        GetWindowThreadProcessId, IsIconic, IsWindowVisible, PostMessageW, SW_RESTORE,
        SetForegroundWindow, ShowWindow, WM_CLOSE, WS_EX_TOOLWINDOW,
    };
    use windows::core::{BOOL, PWSTR};

    use super::{Window, pick};

    /// Elements visited at most: a browser page holds thousands.
    const MAX_VISITED: usize = 4000;
    const MAX_LISTED: usize = 150;
    const TIME_BUDGET: Duration = Duration::from_secs(5);

    /// Top-level windows of other programs, topmost first (the z-order).
    pub fn windows() -> Result<Vec<Window>> {
        let mut handles: Vec<HWND> = vec![];
        unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
            // SAFETY: lparam is the Vec passed to EnumWindows below, alive for the call.
            let handles = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
            handles.push(hwnd);
            BOOL(1)
        }
        // SAFETY: the callback only pushes into the Vec behind lparam.
        unsafe {
            EnumWindows(
                Some(collect),
                LPARAM(&mut handles as *mut Vec<HWND> as isize),
            )?
        };
        let own = std::process::id();
        Ok(handles
            .into_iter()
            .filter_map(|hwnd| {
                // SAFETY: plain queries on a window handle; a closed window just fails them.
                unsafe {
                    if !IsWindowVisible(hwnd).as_bool() {
                        return None;
                    }
                    if GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
                        return None;
                    }
                    // Suspended UWP windows are «visible» but cloaked.
                    let mut cloaked = 0u32;
                    let _ = DwmGetWindowAttribute(
                        hwnd,
                        DWMWA_CLOAKED,
                        &mut cloaked as *mut u32 as *mut _,
                        4,
                    );
                    if cloaked != 0 {
                        return None;
                    }
                    let mut buf = [0u16; 512];
                    let len = GetWindowTextW(hwnd, &mut buf);
                    let title = String::from_utf16_lossy(&buf[..len.max(0) as usize])
                        .trim()
                        .to_string();
                    if title.is_empty() {
                        return None;
                    }
                    let mut pid = 0u32;
                    GetWindowThreadProcessId(hwnd, Some(&mut pid));
                    if pid == own {
                        return None;
                    }
                    let app = process_name(pid).unwrap_or_else(|| "?".into());
                    if app.eq_ignore_ascii_case("explorer.exe") && title == "Program Manager" {
                        return None;
                    }
                    Some(Window {
                        handle: hwnd.0 as isize,
                        title,
                        app,
                    })
                }
            })
            .collect())
    }

    fn process_name(pid: u32) -> Option<String> {
        // SAFETY: the handle is closed below; the buffer outlives the call.
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
            .is_ok();
            let _ = CloseHandle(process);
            if !ok {
                return None;
            }
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            path.rsplit('\\').next().map(str::to_string)
        }
    }

    pub fn find(query: Option<&str>) -> Result<Window> {
        pick(windows()?, query)
    }

    fn hwnd(w: &Window) -> HWND {
        HWND(w.handle as *mut _)
    }

    /// Brings the window forward. Windows lets a program take the foreground only right after
    /// input: an Alt tap counts as that.
    pub fn focus(query: Option<&str>) -> Result<Window> {
        let w = find(query)?;
        let h = hwnd(&w);
        // SAFETY: plain window calls on a handle from EnumWindows.
        unsafe {
            if GetForegroundWindow() == h {
                return Ok(w);
            }
            if IsIconic(h).as_bool() {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            if let Ok(mut enigo) = super::enigo() {
                use enigo::{Direction, Key, Keyboard};
                let _ = enigo.key(Key::Alt, Direction::Click);
            }
            let _ = SetForegroundWindow(h);
        }
        // The window needs a moment before keys reach it.
        std::thread::sleep(Duration::from_millis(250));
        // SAFETY: as above.
        if unsafe { GetForegroundWindow() } != h {
            bail!(
                "не удалось переключиться на окно {} — переключитесь на него сами",
                w.app
            );
        }
        Ok(w)
    }

    /// Asks the window to close, as its close button does: the program may still ask to save.
    pub fn close(w: &Window) {
        // SAFETY: posting a message to a window handle; a closed window just fails it.
        let _ = unsafe { PostMessageW(Some(hwnd(w)), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }

    pub fn kill(app: &str) -> Result<()> {
        use std::os::windows::process::CommandExt;
        let status = std::process::Command::new("taskkill")
            .args(["/IM", app, "/F"])
            .creation_flags(0x0800_0000)
            .status()?;
        if !status.success() {
            bail!("не удалось завершить {app}");
        }
        Ok(())
    }

    /// UI Automation runs on its own thread with COM initialised there.
    fn with_automation<T: Send + 'static>(
        work: impl FnOnce(IUIAutomation) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        std::thread::spawn(move || {
            // SAFETY: COM is initialised once on this fresh thread before any COM call.
            let automation: IUIAutomation = unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?
            };
            work(automation)
        })
        .join()
        .map_err(|_| anyhow!("UI Automation failed"))?
    }

    /// Elements worth listing, with their kind for the model (in Russian).
    const KINDS: &[(UIA_CONTROLTYPE_ID, &str)] = &[
        (UIA_ButtonControlTypeId, "кнопка"),
        (UIA_SplitButtonControlTypeId, "кнопка"),
        (UIA_HyperlinkControlTypeId, "ссылка"),
        (UIA_MenuItemControlTypeId, "пункт меню"),
        (UIA_TabItemControlTypeId, "вкладка"),
        (UIA_ListItemControlTypeId, "элемент списка"),
        (UIA_DataItemControlTypeId, "элемент списка"),
        (UIA_TreeItemControlTypeId, "пункт дерева"),
        (UIA_CheckBoxControlTypeId, "флажок"),
        (UIA_RadioButtonControlTypeId, "переключатель"),
        (UIA_ComboBoxControlTypeId, "выпадающий список"),
        (UIA_EditControlTypeId, "поле ввода"),
        (UIA_SliderControlTypeId, "ползунок"),
        (UIA_TextControlTypeId, "текст"),
    ];

    fn kind(control: UIA_CONTROLTYPE_ID) -> Option<&'static str> {
        KINDS.iter().find(|(c, _)| *c == control).map(|(_, k)| *k)
    }

    struct Found {
        element: IUIAutomationElement,
        kind: &'static str,
        name: String,
    }

    // SAFETY (for the unsafe blocks below): UI Automation calls on interfaces created on this
    // thread; failures come back as errors and the walk skips them.
    fn walk(
        automation: &IUIAutomation,
        root: HWND,
        mut visit: impl FnMut(Found) -> bool,
    ) -> Result<()> {
        let started = Instant::now();
        let (root, walker) = unsafe {
            (
                automation.ElementFromHandle(root)?,
                automation.ControlViewWalker()?,
            )
        };
        let mut stack = vec![root];
        let mut visited = 0;
        while let Some(element) = stack.pop() {
            visited += 1;
            if visited > MAX_VISITED || started.elapsed() > TIME_BUDGET {
                break;
            }
            let offscreen = unsafe { element.CurrentIsOffscreen() }.is_ok_and(|b| b.as_bool());
            if !offscreen
                && let Ok(control) = unsafe { element.CurrentControlType() }
                && let Some(kind) = kind(control)
            {
                let name = unsafe { element.CurrentName() }
                    .map(|n| n.to_string())
                    .unwrap_or_default();
                let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
                if !name.is_empty()
                    && !visit(Found {
                        element: element.clone(),
                        kind,
                        name,
                    })
                {
                    return Ok(());
                }
            }
            // Children pushed in reverse: they come off the stack in reading order.
            let mut children = vec![];
            let mut child = unsafe { walker.GetFirstChildElement(&element) }.ok();
            while let Some(c) = child {
                child = unsafe { walker.GetNextSiblingElement(&c) }.ok();
                children.push(c);
            }
            stack.extend(children.into_iter().rev());
        }
        Ok(())
    }

    pub fn elements(w: &Window) -> Result<Vec<String>> {
        let root = w.handle;
        with_automation(move |automation| {
            let mut lines: Vec<String> = vec![];
            let mut more = 0;
            walk(&automation, HWND(root as *mut _), |f| {
                let name: String = f.name.chars().take(120).collect();
                let line = format!("[{}] {name}", f.kind);
                if lines.len() < MAX_LISTED {
                    if !lines.contains(&line) {
                        lines.push(line);
                    }
                } else {
                    more += 1;
                }
                true
            })?;
            if more > 0 {
                lines.push(format!("…и ещё {more}"));
            }
            Ok(lines)
        })
    }

    /// What clicking came to: done by the element's own action, or a real click needed.
    enum Outcome {
        Done(String),
        ClickAt { x: i32, y: i32, name: String },
    }

    pub fn click(w: &Window, name: &str) -> Result<String> {
        let root = w.handle;
        let wanted = name.to_lowercase();
        let outcome = with_automation(move |automation| {
            let mut exact = None;
            let mut partial = None;
            walk(&automation, HWND(root as *mut _), |f| {
                if f.kind == "текст" {
                    return true;
                }
                let n = f.name.to_lowercase();
                if n == wanted {
                    exact = Some(f);
                    return false;
                }
                if partial.is_none() && n.contains(&wanted) {
                    partial = Some(f);
                }
                true
            })?;
            let f = exact.or(partial).ok_or_else(|| {
                anyhow!("в окне нет элемента «{wanted}»; что есть — покажет read_window")
            })?;
            if let Ok(how) = press(&f) {
                return Ok(Outcome::Done(format!("{how} «{}» ({})", f.name, f.kind)));
            }
            // No action to call: a real click in the middle of it.
            let rect: RECT = unsafe { f.element.CurrentBoundingRectangle()? };
            Ok(Outcome::ClickAt {
                x: (rect.left + rect.right) / 2,
                y: (rect.top + rect.bottom) / 2,
                name: f.name,
            })
        })?;
        match outcome {
            Outcome::Done(done) => Ok(done),
            Outcome::ClickAt { x, y, name } => {
                focus(Some(&w.title))?;
                use enigo::{Button, Coordinate, Direction, Mouse};
                let mut enigo = super::enigo()?;
                enigo
                    .move_mouse(x, y, Coordinate::Abs)
                    .map_err(|e| anyhow!("{e}"))?;
                enigo
                    .button(Button::Left, Direction::Click)
                    .map_err(|e| anyhow!("{e}"))?;
                Ok(format!("нажато мышью «{name}»"))
            }
        }
    }

    /// The element's own action, as a screen reader would call it.
    fn press(f: &Found) -> Result<&'static str> {
        unsafe {
            if let Ok(p) = f
                .element
                .GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
            {
                p.Invoke()?;
                return Ok("нажато");
            }
            if let Ok(p) = f
                .element
                .GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)
            {
                p.Toggle()?;
                return Ok("переключено");
            }
            if let Ok(p) = f
                .element
                .GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
                    UIA_SelectionItemPatternId,
                )
            {
                p.Select()?;
                return Ok("выбрано");
            }
            if let Ok(p) = f
                .element
                .GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                    UIA_ExpandCollapsePatternId,
                )
            {
                p.Expand()?;
                return Ok("раскрыто");
            }
            if f.kind == "поле ввода" {
                f.element.SetFocus()?;
                return Ok("курсор поставлен в поле");
            }
        }
        bail!("no pattern")
    }
}

#[cfg(not(windows))]
mod platform {
    use anyhow::{Result, bail};

    use super::Window;

    const ONLY_WINDOWS: &str = "управление окнами программ пока есть только в Windows";

    pub fn windows() -> Result<Vec<Window>> {
        bail!(ONLY_WINDOWS)
    }
    pub fn find(_: Option<&str>) -> Result<Window> {
        bail!(ONLY_WINDOWS)
    }
    pub fn focus(_: Option<&str>) -> Result<Window> {
        bail!(ONLY_WINDOWS)
    }
    pub fn elements(_: &Window) -> Result<Vec<String>> {
        bail!(ONLY_WINDOWS)
    }
    pub fn click(_: &Window, _: &str) -> Result<String> {
        bail!(ONLY_WINDOWS)
    }
    pub fn close(_: &Window) {}
    pub fn kill(_: &str) -> Result<()> {
        bail!(ONLY_WINDOWS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(title: &str, app: &str) -> Window {
        Window {
            handle: 0,
            title: title.into(),
            app: app.into(),
        }
    }

    #[test]
    fn windows_are_picked_by_program_then_title() {
        let list = || {
            vec![
                win("Spotify Premium", "Spotify.exe"),
                win("Котики — YouTube — Google Chrome", "chrome.exe"),
            ]
        };
        assert_eq!(pick(list(), Some("chrome")).unwrap().app, "chrome.exe");
        assert_eq!(pick(list(), Some("youtube")).unwrap().app, "chrome.exe");
        assert_eq!(
            pick(list(), Some("spotify.exe")).unwrap().app,
            "Spotify.exe"
        );
        assert_eq!(
            pick(list(), None).unwrap().app,
            "Spotify.exe",
            "topmost by default"
        );
        assert!(pick(list(), Some("telegram")).is_err());
    }

    #[test]
    fn key_combinations_are_parsed() {
        let (mods, key) = parse_combo("Ctrl+Shift+T").unwrap();
        assert_eq!(mods, [Key::Control, Key::Shift]);
        #[cfg(windows)]
        assert_eq!(
            key,
            Key::Other('T' as u32),
            "layout-independent virtual key"
        );
        #[cfg(not(windows))]
        assert_eq!(key, Key::Unicode('t'));
        assert_eq!(
            parse_combo("alt+left").unwrap(),
            (vec![Key::Alt], Key::LeftArrow)
        );
        assert_eq!(parse_combo("f5").unwrap(), (vec![], Key::F5));
        assert_eq!(parse_combo("space").unwrap(), (vec![], Key::Space));
        assert!(parse_combo("hyper+x").is_err());
        assert!(parse_combo("ctrl+nothing").is_err());
    }
}
