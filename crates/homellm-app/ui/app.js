const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = (id) => document.getElementById(id);

// --- tabs ---
document.querySelectorAll(".tab").forEach((tab) =>
  tab.addEventListener("click", () => {
    document.querySelectorAll(".tab, .page").forEach((el) => el.classList.remove("active"));
    tab.classList.add("active");
    $(tab.dataset.tab).classList.add("active");
    if (tab.dataset.tab === "chat") $("input").focus();
  }),
);

function el(tag, cls, text) {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  return node;
}

// --- models ---
let current = null;
const downloading = new Set();

async function showHardware() {
  const hw = await invoke("hw_info");
  const gpu = hw.gpu ? `${hw.gpu}, ${hw.vram}` : "не найдена — модели пойдут на процессоре";
  $("hw").innerHTML = "";
  $("hw").append(
    "Память: ", el("b", "", hw.ram), ` · процессор: ${hw.cpu_threads} потоков · видеокарта: `, el("b", "", gpu),
  );
}

async function showModels() {
  const { models, dir } = await invoke("list_models");
  current = await invoke("current");
  $("models-dir").textContent = `Модели лежат в ${dir}`;
  const list = $("model-list");
  list.innerHTML = "";
  for (const m of models) {
    const card = el("div", "card model");
    card.append(el("h4", "", m.name));
    const badges = el("div", "badges");
    if (m.recommended) badges.append(el("span", "badge rec", "рекомендую"));
    badges.append(el("span", `badge ${m.fit}`, m.fit_label), el("span", "badge", m.size));
    if (!m.tools) badges.append(el("span", "badge", "без управления ПК"));
    card.append(badges, el("div", "about", m.about));

    const actions = el("div", "actions");
    if (m.downloaded) {
      const run = el("button", "", current === m.id ? "Запущена" : "Запустить");
      run.disabled = current === m.id;
      run.onclick = () => start(m, run);
      actions.append(run);
    } else if (downloading.has(m.id)) {
      const bar = el("progress");
      bar.id = `bar-${m.id}`;
      bar.max = 1000;
      actions.append(bar, el("span", "hint", "скачиваю…"));
    } else {
      const get = el("button", "secondary", "Скачать");
      get.disabled = m.fit === "TooBig";
      get.onclick = () => pull(m);
      actions.append(get);
    }
    card.append(actions);
    list.append(card);
  }
}

async function pull(m) {
  downloading.add(m.id);
  await showModels();
  try {
    await invoke("pull", { id: m.id });
  } catch (e) {
    alert(`Не скачалось: ${e}\nНажмите «Скачать» ещё раз — загрузка продолжится.`);
  }
  downloading.delete(m.id);
  showModels();
}

listen("progress", ({ payload }) => {
  const bar = $(`bar-${payload.id}`);
  if (bar) bar.value = Math.floor((payload.done * 1000) / payload.total);
});

async function start(m, button) {
  button.disabled = true;
  button.textContent = "Загружаю…";
  $("status").textContent = `Загружаю ${m.name}…`;
  try {
    const where = await invoke("load", { id: m.id });
    $("status").textContent = `${m.name} · ${where}`;
    document.querySelector('[data-tab="chat"]').click();
  } catch (e) {
    $("status").textContent = "Модель не выбрана";
    alert(`Не запустилась: ${e}`);
  }
  showModels();
}

// --- chat ---
let bubble = null; // the answer being streamed
let raw = "";

// Hide the model's thinking and the tool-call markup while it streams.
function visible(text) {
  return text
    .replace(/<think>[\s\S]*?(<\/think>|$)/g, "")
    .replace(/<tool_call>[\s\S]*$/, "")
    .trim();
}

function add(cls, text) {
  $("messages").querySelector(".empty")?.remove();
  const node = el("div", cls, text);
  $("messages").append(node);
  node.scrollIntoView({ block: "end" });
  return node;
}

function newBubble() {
  raw = "";
  bubble = add("msg bot", "…");
}

listen("token", ({ payload }) => {
  if (!bubble) newBubble();
  raw += payload;
  bubble.textContent = visible(raw) || "…";
  bubble.scrollIntoView({ block: "end" });
});

listen("tool", ({ payload }) => {
  if (bubble && !visible(raw)) bubble.remove();
  add("tool", `→ ${payload.name} ${JSON.stringify(payload.args)}`);
  bubble = null;
});

listen("tool-result", ({ payload }) => add("tool", `← ${payload.result}`));

listen("confirm", ({ payload }) => {
  const dialog = $("confirm");
  $("confirm-text").textContent = `${payload.tool} ${JSON.stringify(payload.args)}`;
  dialog.onclose = () => invoke("answer", { id: payload.id, allow: dialog.returnValue === "yes" });
  dialog.returnValue = "";
  dialog.showModal();
});

$("composer").addEventListener("submit", async (event) => {
  event.preventDefault();
  const text = $("input").value.trim();
  if (!text) return;
  $("input").value = "";
  $("send").disabled = true;
  add("msg user", text);
  newBubble();
  try {
    const answer = await invoke("send", { text });
    if (!bubble) newBubble();
    bubble.textContent = answer;
  } catch (e) {
    if (!bubble) newBubble();
    bubble.textContent = String(e);
    bubble.classList.add("error");
  }
  bubble = null;
  $("send").disabled = false;
  $("input").focus();
});

$("new-chat").onclick = async () => {
  await invoke("reset");
  $("messages").innerHTML = "";
  $("messages").append(el("div", "empty", "Новый разговор."));
};

// --- settings ---
const form = $("settings-form");

async function showSettings() {
  const s = await invoke("get_settings");
  for (const key of ["music_dir", "music_search", "models_dir"]) form.elements[key].value = s[key] || "";
}

form.addEventListener("submit", async (event) => {
  event.preventDefault();
  const value = Object.fromEntries(new FormData(form));
  try {
    await invoke("save_settings", { value });
    $("saved").textContent = "Сохранено";
    showModels();
  } catch (e) {
    $("saved").textContent = `Ошибка: ${e}`;
  }
  setTimeout(() => ($("saved").textContent = ""), 2500);
});

showHardware();
showModels();
showSettings();
