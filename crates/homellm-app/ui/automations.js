// The Automations tab: scenarios on a schedule. Uses $, el, invoke, listen and submit from app.js.

/** Actions the form offers; any other tool (made from the chat) shows as «другое действие». */
const ACTIONS = [
  { tool: "close_app", label: "Закрыть программу", fields: [
    { k: "app", ph: "word, chrome, telegram" },
    { k: "save", type: "check", label: "сначала сохранить (Ctrl+S)" },
    { k: "force", type: "check", label: "принудительно, если не закроется" } ] },
  { tool: "open_app", label: "Открыть программу", fields: [{ k: "name", ph: "spotify, notepad, telegram" }] },
  { tool: "open_url", label: "Открыть сайт", fields: [{ k: "url", ph: "https://…" }] },
  { tool: "play_music", label: "Включить музыку", fields: [{ k: "query", ph: "исполнитель, песня или жанр" }] },
  { tool: "media", label: "Музыка: управление", fields: [{ k: "action", type: "select", options: [
    ["play_pause", "Пауза / продолжить"], ["next", "Следующий трек"], ["previous", "Предыдущий трек"], ["mute", "Выключить звук"] ] }] },
  { tool: "set_volume", label: "Громкость", fields: [{ k: "percent", type: "number", ph: "30" }] },
  { tool: "press_keys", label: "Нажать клавиши", fields: [{ k: "keys", ph: "ctrl+s" }, { k: "window", ph: "в окне (необязательно)" }] },
  { tool: "notify", label: "Уведомление", fields: [{ k: "text", ph: "Пора сделать перерыв" }] },
  { tool: "wait", label: "Подождать", fields: [{ k: "seconds", type: "number", ph: "секунд" }] },
  { tool: "power", label: "Компьютер", fields: [{ k: "action", type: "select", options: [
    ["shutdown", "Выключить (через минуту)"], ["restart", "Перезагрузить (через минуту)"], ["sleep", "Сон"], ["lock", "Заблокировать"] ] }] },
];
const OTHER = "__other";
const DAY_NAMES = ["Пн", "Вт", "Ср", "Чт", "Пт", "Сб", "Вс"];

let editingId = 0;

/** One step in words, for the list. */
function describeStep(step) {
  const a = step.args || {};
  switch (step.tool) {
    case "close_app": return `закрыть ${a.app || "?"}${a.save ? " с сохранением" : ""}`;
    case "open_app": return `открыть ${a.name || "?"}`;
    case "open_url": return `открыть ${a.url || "сайт"}`;
    case "play_music": return `включить «${a.query || "музыку"}»`;
    case "media": return ({ play_pause: "пауза/продолжить", next: "следующий трек", previous: "предыдущий трек", mute: "без звука" })[a.action] || "музыка";
    case "set_volume": return `громкость ${a.percent}%`;
    case "press_keys": return `нажать ${a.keys || "?"}`;
    case "notify": return `уведомление «${a.text || ""}»`;
    case "wait": return `подождать ${a.seconds} с`;
    case "power": return ({ shutdown: "выключить компьютер", restart: "перезагрузить", sleep: "сон", lock: "заблокировать", cancel: "отменить выключение" })[a.action] || "компьютер";
    case "scroll": return `листать ${({ up: "вверх", top: "в начало", bottom: "в конец" })[a.direction] || "вниз"}`;
    case "focus_window": return `перейти в ${a.window || "окно"}`;
    case "click_element": return `нажать «${a.name || "?"}»`;
    case "type_text": return `напечатать «${a.text || ""}»`;
    case "remind": return `напомнить «${a.text || ""}»`;
    default: return step.tool;
  }
}

async function showAutomations() {
  const list = $("auto-list");
  const all = await invoke("list_automations");
  list.innerHTML = "";
  if (!all.length) {
    list.append(el("p", "hint", "Пока нет ни одной. Создайте ниже — или напишите в чате: «каждый будний день в 18:00 сохрани и закрой Word, потом выключи компьютер»."));
    return;
  }
  for (const a of all) {
    const card = el("div", `auto${a.enabled ? "" : " off"}`);
    const head = el("div", "auto-head");
    const toggle = el("input");
    toggle.type = "checkbox";
    toggle.checked = a.enabled;
    toggle.title = a.enabled ? "Выключить" : "Включить";
    toggle.onchange = () => invoke("toggle_automation", { id: a.id, enabled: toggle.checked });
    head.append(toggle, el("b", "", a.name), el("span", "badge", a.when));
    card.append(head, el("div", "auto-steps", a.steps.map(describeStep).join(" → ")));
    if (a.last_run) card.append(el("div", "hint", `Последний запуск ${a.last_run}: ${a.last_result.split("\n").pop()}`));
    const actions = el("div", "auto-actions");
    const run = el("button", "", "Запустить сейчас");
    run.onclick = async () => {
      run.disabled = true;
      run.textContent = "Выполняется…";
      try { await invoke("run_automation_now", { id: a.id }); } catch (e) { alert(e); }
      showAutomations();
    };
    const edit = el("button", "", "Изменить");
    edit.onclick = () => fillForm(a);
    const del = el("button", "", "Удалить");
    del.onclick = async () => {
      if (!confirm(`Удалить «${a.name}»?`)) return;
      await invoke("delete_automation", { id: a.id });
      if (editingId === a.id) fillForm(null);
    };
    actions.append(run, edit, del);
    card.append(actions);
    list.append(card);
  }
}

function stepRow(step) {
  const row = el("div", "step");
  const kind = el("select");
  for (const action of ACTIONS) kind.append(new Option(action.label, action.tool));
  kind.append(new Option("Другое действие", OTHER));
  const known = ACTIONS.some((a) => a.tool === step.tool);
  kind.value = known ? step.tool : OTHER;
  const fields = el("div", "step-fields");
  const remove = el("button", "icon-btn", "✕");
  remove.type = "button";
  remove.title = "Убрать действие";
  remove.onclick = () => row.remove();

  const render = (args) => {
    fields.innerHTML = "";
    if (kind.value === OTHER) {
      const tool = el("input");
      tool.dataset.k = "__tool";
      tool.placeholder = "инструмент";
      tool.value = known ? "" : step.tool || "";
      const json = el("input");
      json.dataset.k = "__args";
      json.placeholder = '{"параметр": "значение"}';
      json.value = known ? "{}" : JSON.stringify(step.args || {});
      fields.append(tool, json);
      return;
    }
    const action = ACTIONS.find((a) => a.tool === kind.value);
    for (const f of action.fields) {
      let input;
      if (f.type === "select") {
        input = el("select");
        for (const [value, label] of f.options) input.append(new Option(label, value));
        if (args[f.k]) input.value = args[f.k];
      } else if (f.type === "check") {
        const label = el("label", "check");
        input = el("input");
        input.type = "checkbox";
        input.checked = !!args[f.k];
        input.dataset.k = f.k;
        label.append(input, document.createTextNode(` ${f.label}`));
        fields.append(label);
        continue;
      } else {
        input = el("input");
        if (f.type === "number") input.type = "number";
        input.placeholder = f.ph || "";
        if (args[f.k] !== undefined) input.value = args[f.k];
      }
      input.dataset.k = f.k;
      fields.append(input);
    }
  };
  kind.onchange = () => render({});
  render(known ? step.args || {} : {});
  row.append(kind, fields, remove);
  return row;
}

/** The step a form row describes, or an error text. */
function readStep(row) {
  const tool = row.querySelector("select").value;
  const inputs = [...row.querySelectorAll("[data-k]")];
  if (tool === OTHER) {
    const name = inputs.find((i) => i.dataset.k === "__tool").value.trim();
    let args;
    try { args = JSON.parse(inputs.find((i) => i.dataset.k === "__args").value || "{}"); } catch { return "параметры «другого действия» — не JSON"; }
    return name ? { tool: name, args } : "у «другого действия» не указан инструмент";
  }
  const args = {};
  for (const input of inputs) {
    if (input.type === "checkbox") { if (input.checked) args[input.dataset.k] = true; continue; }
    const value = input.value.trim();
    if (!value) continue;
    args[input.dataset.k] = input.type === "number" ? Number(value) : value;
  }
  const action = ACTIONS.find((a) => a.tool === tool);
  const required = action.fields.find((f) => f.type !== "check" && !f.ph?.includes("необязательно") && args[f.k] === undefined);
  if (required) return `«${action.label}»: заполните поле`;
  return { tool, args };
}

function fillForm(a) {
  const form = $("auto-form");
  editingId = a ? a.id : 0;
  form.elements.name.value = a ? a.name : "";
  form.elements.time.value = a ? a.time : "10:00";
  form.elements.once.checked = a ? a.once : false;
  for (const box of form.querySelectorAll("[name=day]")) box.checked = a ? a.days.includes(Number(box.value)) : false;
  const steps = $("auto-steps");
  steps.innerHTML = "";
  for (const step of a ? a.steps : [{ tool: "close_app", args: {} }]) steps.append(stepRow(step));
  $("auto-form-title").textContent = a ? `Изменить «${a.name}»` : "Новая автоматизация";
  $("auto-cancel").hidden = !a;
  $("auto-saved").textContent = "";
  if (a) form.scrollIntoView({ behavior: "smooth" });
}

function buildDays() {
  const box = $("auto-days");
  DAY_NAMES.forEach((name, i) => {
    const label = el("label", "day");
    const input = el("input");
    input.type = "checkbox";
    input.name = "day";
    input.value = String(i + 1);
    label.append(input, el("span", "", name));
    box.append(label);
  });
}

$("auto-add-step").onclick = () => $("auto-steps").append(stepRow({ tool: "close_app", args: {} }));
$("auto-cancel").onclick = () => fillForm(null);

$("auto-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const form = e.target;
  const steps = [...$("auto-steps").children].map(readStep);
  const error = steps.find((s) => typeof s === "string");
  if (error) { $("auto-saved").textContent = error; return; }
  const automation = {
    id: editingId,
    name: form.elements.name.value,
    enabled: true,
    time: form.elements.time.value,
    days: [...form.querySelectorAll("[name=day]:checked")].map((b) => Number(b.value)),
    once: form.elements.once.checked,
    steps,
  };
  try {
    await invoke("save_automation", { automation });
    fillForm(null);
    $("auto-saved").textContent = "Сохранено";
  } catch (err) {
    $("auto-saved").textContent = `Ошибка: ${err}`;
  }
});

// Described in words: the model builds it with create_automation, in the chat.
$("auto-describe").onclick = () => {
  const text = $("auto-text").value.trim();
  if (!text) return;
  $("auto-text").value = "";
  $("auto-dialog").close();
  submit(`Создай автоматизацию: ${text}`);
};

$("auto-btn").onclick = () => {
  showAutomations();
  $("auto-dialog").showModal();
};
listen("automations-changed", () => $("auto-dialog").open && showAutomations());

buildDays();
fillForm(null);
