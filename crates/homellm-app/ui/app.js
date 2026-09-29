const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = (id) => document.getElementById(id);

function el(tag, cls, text) {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  return node;
}

function icon(name, cls = "ico") {
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("class", cls);
  const use = document.createElementNS(ns, "use");
  use.setAttribute("href", `#${name}`);
  svg.append(use);
  return svg;
}

// ---------- tool-call markup ----------
// Small models drop the <tool_call> tags, so a reply that starts as a JSON object is a call too.
function stripThink(text) {
  return text.replace(/<think>[\s\S]*?(<\/think>|$)/g, "");
}

function visible(text) {
  const shown = stripThink(text).replace(/<tool_call>[\s\S]*$/, "").replace(/<\/tool_call>/g, "").trim();
  return shown.startsWith("{") ? "" : shown;
}

function callOf(text) {
  const body = stripThink(text).replace(/^[\s\S]*<tool_call>/, "").trim();
  if (!body.startsWith("{")) return null;
  const name = body.match(/"name"\s*:\s*"([^"]+)"/);
  const args = body.match(/"arguments"\s*:\s*(\{[^{}]*\})/);
  return name ? `${name[1]} ${args ? args[1] : ""}` : null;
}

// ---------- tiny markdown: code blocks, inline code, bold ----------
function escape(text) {
  return text.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
}

function markdown(text) {
  return escape(text)
    .split(/```/)
    .map((part, i) => {
      if (i % 2) return `<pre><code>${part.replace(/^[\w+-]*\n/, "")}</code></pre>`;
      return part
        .split(/\n{2,}/)
        .map((p) => p.trim())
        .filter(Boolean)
        .map((p) => `<p>${p.replace(/`([^`]+)`/g, "<code>$1</code>").replace(/\*\*([^*]+)\*\*/g, "<b>$1</b>").replace(/\n/g, "<br>")}</p>`)
        .join("");
    })
    .join("");
}

// ---------- messages ----------
const messages = $("messages");
let bubble = null; // the answer being streamed
let raw = "";
let busy = false;

function scrollDown() {
  messages.scrollTop = messages.scrollHeight;
}

function addMessage(who, text) {
  messages.querySelector(".welcome")?.remove();
  const row = el("div", `row-msg ${who}`);
  const avatar = el("div", `avatar ${who === "user" ? "me" : ""}`);
  if (who === "user") avatar.textContent = "Я";
  else avatar.append(icon("logo", ""));
  const body = el("div", "bubble");
  if (who === "user") body.textContent = text;
  else body.innerHTML = markdown(text);
  row.append(avatar, body);
  messages.append(row);
  scrollDown();
  return body;
}

function addAction(text, done) {
  messages.querySelector(".welcome")?.remove();
  const line = el("div", `action ${done ? "done" : ""}`);
  line.append(el("span", "", `${done ? "✓" : "→"} ${text}`));
  messages.append(line);
  scrollDown();
}

const SUGGESTIONS = ["Включи что-нибудь из Кино", "Сделай громкость 30%", "Открой калькулятор", "Какие модели есть?", "Какое у меня железо?"];

function showWelcome() {
  messages.innerHTML = "";
  const box = el("div", "welcome");
  box.append(icon("logo", ""), el("h2", "", "Чем помочь?"));
  box.append(el("p", "", "Я работаю на вашем компьютере: включаю музыку, меняю громкость, открываю программы и сайты. Можно просто поболтать."));
  const chips = el("div", "suggestions");
  for (const s of SUGGESTIONS) {
    const b = el("button", "", s);
    b.onclick = () => submit(s);
    chips.append(b);
  }
  box.append(chips);
  messages.append(box);
}

// A saved chat, as the model saw it: tool calls and results become action lines.
function renderHistory(history) {
  messages.innerHTML = "";
  if (!history.length) return showWelcome();
  history.forEach((m, i) => {
    const next = history[i + 1];
    if (m.role === "user") {
      if (m.content.startsWith("<tool_response>")) {
        addAction(m.content.replace(/<\/?tool_response>/g, "").trim(), true);
      } else if (!m.content.startsWith("Ты не вызвал инструмент")) {
        addMessage("user", m.content);
      }
    } else if (m.role === "assistant") {
      if (next?.content.startsWith("Ты не вызвал инструмент")) return; // a claim we rejected
      const call = callOf(m.content);
      if (call) addAction(call, false);
      else if (visible(m.content)) addMessage("bot", visible(m.content));
    }
  });
}

listen("token", ({ payload }) => {
  if (!bubble) {
    raw = "";
    bubble = addMessage("bot", "");
    bubble.classList.add("typing");
  }
  raw += payload;
  bubble.innerHTML = markdown(visible(raw));
  scrollDown();
});

listen("tool", ({ payload }) => {
  // Whatever streamed before a call is the call itself (or a claim the agent rejected).
  bubble?.closest(".row-msg")?.remove();
  bubble = null;
  addAction(`${payload.name} ${JSON.stringify(payload.args)}`, false);
});

listen("tool-result", ({ payload }) => addAction(payload.result, true));

// The answer goes back on the button click itself: a dialog's `close` event is not
// reliable (it is not fired while the window is hidden), and a lost answer hangs the chat.
listen("confirm", ({ payload }) => {
  const dialog = $("confirm");
  $("confirm-text").textContent = `${payload.tool} ${JSON.stringify(payload.args)}`;
  let answered = false;
  const reply = (allow) => {
    if (answered) return;
    answered = true;
    if (dialog.open) dialog.close();
    invoke("answer", { id: payload.id, allow });
  };
  dialog.querySelectorAll("button").forEach((b) => (b.onclick = (e) => {
    e.preventDefault();
    reply(b.value === "yes");
  }));
  dialog.oncancel = () => reply(false);
  dialog.onclose = () => reply(false);
  dialog.showModal();
});

async function submit(text) {
  text = text.trim();
  if (!text || busy) return;
  busy = true;
  $("send").disabled = true;
  $("input").value = "";
  autosize();
  addMessage("user", text);
  bubble = null;
  try {
    const answer = await invoke("send", { text });
    if (!bubble) bubble = addMessage("bot", "");
    bubble.innerHTML = markdown(answer);
  } catch (e) {
    if (!bubble) bubble = addMessage("bot", "");
    bubble.textContent = String(e);
    bubble.classList.add("error");
  }
  bubble?.classList.remove("typing");
  bubble = null;
  busy = false;
  $("send").disabled = false;
  $("input").focus();
  scrollDown();
}

$("composer").addEventListener("submit", (e) => {
  e.preventDefault();
  submit($("input").value);
});

$("input").addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    submit($("input").value);
  }
});

function autosize() {
  const input = $("input");
  input.style.height = "auto";
  input.style.height = `${Math.min(input.scrollHeight, 180)}px`;
}
$("input").addEventListener("input", autosize);

// ---------- chats ----------
async function showChats() {
  const { chats, current } = await invoke("list_chats");
  const list = $("chat-list");
  list.innerHTML = "";
  for (const chat of chats) {
    const item = el("div", `chat-item ${chat.id === current ? "active" : ""}`);
    const title = el("span", "title", chat.title);
    const rename = el("button", "act");
    rename.title = "Переименовать";
    rename.append(icon("i-edit"));
    const remove = el("button", "act");
    remove.title = "Удалить";
    remove.append(icon("i-trash"));
    item.append(title, rename, remove);
    item.onclick = () => openChat(chat.id);
    rename.onclick = (e) => {
      e.stopPropagation();
      const input = el("input");
      input.value = chat.title;
      title.replaceWith(input);
      input.focus();
      input.select();
      input.onclick = (ev) => ev.stopPropagation();
      let finished = false;
      const done = async (save) => {
        if (finished) return;
        finished = true;
        if (save) await invoke("rename_chat", { id: chat.id, title: input.value.trim() || chat.title });
        showChats();
      };
      input.onblur = () => done(true);
      input.onkeydown = (ev) => {
        if (ev.key === "Enter") done(true);
        if (ev.key === "Escape") done(false);
      };
    };
    remove.onclick = async (e) => {
      e.stopPropagation();
      if (!confirm(`Удалить чат «${chat.title}»?`)) return;
      await invoke("delete_chat", { id: chat.id });
      if (chat.id === current) showWelcome();
      showChats();
    };
    list.append(item);
  }
}

async function openChat(id) {
  if (busy) return;
  renderHistory(await invoke("open_chat", { id }));
  showChats();
}

$("new-chat").onclick = async () => {
  if (busy) return;
  await invoke("new_chat");
  showWelcome();
  showChats();
  $("input").focus();
};

listen("chats-changed", showChats);

// ---------- model, hardware ----------
function showModel(info) {
  $("model-name").textContent = info ? `${info.name} · ${info.where}` : "Модель не выбрана";
  document.querySelector(".model-btn .dot").classList.toggle("on", !!info);
}

listen("model-changed", ({ payload }) => {
  showModel(payload);
  if ($("models-dialog").open) showModels();
});

async function showHardware() {
  const hw = await invoke("hw_info");
  $("gpu-name").textContent = hw.gpu ? `${hw.gpu.replace(/\s*\(.*\)$/, "")} · ${hw.vram}` : "без видеокарты";
  $("hw").innerHTML = "";
  $("hw").append("Память ", el("b", "", hw.ram), ` · процессор ${hw.cpu_threads} потоков · видеокарта `, el("b", "", hw.gpu ? `${hw.gpu}, ${hw.vram}` : "не найдена"));
}

// ---------- models dialog ----------
const downloading = new Map(); // id -> permille

async function showModels() {
  const { models, dir } = await invoke("list_models");
  const current = await invoke("current");
  $("models-dir").textContent = `Модели лежат в ${dir}`;
  const list = $("model-list");
  list.innerHTML = "";
  const GROUPS = { chat: "Для разговора и управления ПК", code: "Для кода", voice: "Голос — заработает с голосовым режимом" };
  let group = null;
  for (const m of models.sort((a, b) => Object.keys(GROUPS).indexOf(a.kind) - Object.keys(GROUPS).indexOf(b.kind))) {
    if (m.kind !== group) {
      group = m.kind;
      list.append(el("div", "group", GROUPS[group] || group));
    }
    const isCurrent = current?.id === m.id;
    const card = el("div", `model ${isCurrent ? "current" : ""}`);
    card.append(el("h4", "", m.name));
    const badges = el("div", "badges");
    if (m.recommended) badges.append(el("span", "badge rec", "рекомендую"));
    badges.append(el("span", `badge ${m.fit}`, m.fit_label), el("span", "badge", m.size));
    if (!m.tools && m.kind !== "voice") badges.append(el("span", "badge", "без управления ПК"));
    card.append(badges, el("div", "about", m.about));
    const actions = el("div", "actions");
    if (m.downloaded && m.kind === "voice") {
      actions.append(el("span", "hint", "скачана"));
    } else if (m.downloaded) {
      const run = el("button", isCurrent ? "" : "primary", isCurrent ? "Работает" : "Запустить");
      run.disabled = isCurrent;
      run.onclick = () => start(m, run);
      actions.append(run);
    } else if (downloading.has(m.id) || m.downloading) {
      const bar = el("progress");
      bar.id = `bar-${m.id}`;
      bar.max = 1000;
      bar.value = downloading.get(m.id) ?? Math.floor((m.partial * 1000) / m.bytes);
      actions.append(bar, el("span", "hint", "скачиваю…"));
    } else {
      const percent = Math.floor((m.partial * 100) / m.bytes);
      const get = el("button", "", m.partial ? `Продолжить (${percent}%)` : "Скачать");
      get.disabled = m.fit === "TooBig";
      get.onclick = () => {
        downloading.set(m.id, 0);
        showModels();
        invoke("pull", { id: m.id }).catch(() => {});
      };
      actions.append(get);
    }
    card.append(actions);
    list.append(card);
  }
}

async function start(m, button) {
  button.disabled = true;
  button.textContent = "Загружаю…";
  $("model-name").textContent = `Загружаю ${m.name}…`;
  try {
    await invoke("load", { id: m.id });
    $("models-dialog").close();
  } catch (e) {
    showModel(await invoke("current"));
    alert(`Не запустилась: ${e}`);
    showModels();
  }
}

listen("progress", ({ payload }) => {
  const permille = Math.floor((payload.done * 1000) / payload.total);
  downloading.set(payload.id, permille);
  const bar = $(`bar-${payload.id}`);
  if (bar) bar.value = permille;
  else if ($("models-dialog").open && !bar) showModels();
  const chip = $("download-chip");
  chip.classList.remove("hidden");
  chip.textContent = `Скачиваю ${payload.id}: ${Math.floor(permille / 10)}%`;
});

listen("download-done", ({ payload }) => {
  downloading.delete(payload.id);
  if (!downloading.size) $("download-chip").classList.add("hidden");
  if (payload.error) alert(`Не скачалось: ${payload.error}\nПопробуйте ещё раз — загрузка продолжится.`);
  if ($("models-dialog").open) showModels();
});

$("model-btn").onclick = () => {
  showModels();
  $("models-dialog").showModal();
};

// ---------- settings dialog ----------
const form = $("settings-form");

const THEMES = ["mint", "lime", "violet", "amber"];

function applyTheme(theme) {
  document.documentElement.dataset.theme = THEMES.includes(theme) ? theme : "mint";
}

async function fillSettings() {
  const s = await invoke("get_settings");
  for (const key of ["music_dir", "music_search", "models_dir"]) form.elements[key].value = s[key] || "";
  form.elements.theme.value = THEMES.includes(s.theme) ? s.theme : "mint";
  applyTheme(s.theme);
}

// Preview a theme as soon as it is picked; closing without saving puts the saved one back.
form.elements.theme.addEventListener("change", () => applyTheme(form.elements.theme.value));
$("settings-dialog").addEventListener("close", fillSettings);
$("settings-dialog").querySelector(".panel-head button").addEventListener("click", fillSettings);

$("settings-btn").onclick = async () => {
  await fillSettings();
  $("settings-dialog").showModal();
};

listen("settings-changed", fillSettings);

form.addEventListener("submit", async (e) => {
  e.preventDefault();
  const value = { ...Object.fromEntries(new FormData(form)), last_model: "", downloads: [] };
  try {
    await invoke("save_settings", { value });
    $("saved").textContent = "Сохранено";
  } catch (err) {
    $("saved").textContent = `Ошибка: ${err}`;
  }
  setTimeout(() => ($("saved").textContent = ""), 2500);
});

// ---------- start ----------
fillSettings();
showWelcome();
showChats();
showHardware();
(async () => {
  $("model-name").textContent = "Запускаю модель…";
  try {
    const info = await invoke("auto_load");
    showModel(info);
    if (!info) {
      showModels();
      $("models-dialog").showModal();
    }
  } catch (e) {
    showModel(null);
  }
})();
