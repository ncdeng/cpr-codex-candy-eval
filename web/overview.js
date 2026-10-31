"use strict";
// 总览：每个账号一张卡片（最近一张鹈鹕画、糖果题结果、定时测试的降智判定），以及页面内的定时与邮件设置。
// 设置存在插件自己的状态里；SMTP 授权码只留在 CPR 的加密敏感字段，本页只显示有没有填。

const ovFilter = { search: "", state: "all" };
// 每张卡片当前显示哪一张鹈鹕画（记录 ID）；没选就显示最新一张。
const ovPick = new Map();
const OV_STRIP = 6;
let ovSettings = null;
let ovSettingsDirty = false;
let ovSettingsTouched = false;
let ovUpdatedAt = 0;
const OV_STATE = { ok: "正常", degraded: "降智", "": "未判定" };
const OV_CANDY = { ok: "通过（答出 21）", bad: "答错", err: "请求出错", skip: "已跳过" };

const ovVerdict = (id) => hourlyCandy?.verdicts?.find((v) => v.id === id) || null;
const ovLabel = (a) => a.email || a.name || a.id;
function ovPlan(a) {
  const id = String(a.plan_type || "").trim();
  return id ? PLAN_BY_ID.get(id.toLowerCase()) || { label: id, badge: "" } : null;
}

function ovVisible() {
  const query = ovFilter.search.trim().toLowerCase();
  return [...credentials]
    .filter((a) => !query || ovLabel(a).toLowerCase().includes(query))
    .filter((a) => ovFilter.state === "all" || (ovVerdict(a.id)?.state || "unknown") === ovFilter.state)
    .sort((x, y) => Number(!!x.disabled) - Number(!!y.disabled) || planRank(x) - planRank(y) || ovLabel(x).localeCompare(ovLabel(y)));
}

const ovSkipped = (id) => !!hourlyCandy?.skip?.includes(id);

function ovThumb(a) {
  const history = a.pelicans || [];
  const shown = history.find((r) => r.id === ovPick.get(a.id)) || [...history].reverse().find((r) => !r.error && !r.skipped);
  if (shown) return `<div class="ov-thumb">${pelicanPreview(a.id, shown)}</div>`;
  const text = a.pelican_running ? `${icon("loader-circle", "spin")}正在画鹈鹕…` : "还没有鹈鹕画图";
  return `<div class="ov-thumb empty"><span>${text}</span></div>`;
}

// 缩略图下的时间条：最近几张鹈鹕画，点一下换到那一张；更早的在「历史」里。
function ovStrip(a) {
  const history = (a.pelicans || []).filter((r) => !r.skipped);
  if (history.length < 2) return "";
  const picked = ovPick.get(a.id) || [...history].reverse().find((r) => !r.error)?.id;
  const chips = history.slice(-OV_STRIP).map((r) => {
    const tip = `${fmtTime(r.time, false)} · ${r.error ? "未生成：" + r.error : modelName(r) + (r.duration_ms ? " · " + fmtSec(r.duration_ms) : "")}`;
    return `<button class="ov-shot ${r.error ? "err" : ""} ${r.id === picked ? "on" : ""}" type="button" data-ov-pick="${esc(r.id)}" data-ov-id="${esc(a.id)}" data-tip="${esc(tip)}">${esc(fmtTime(r.time, false).slice(6))}</button>`;
  }).join("");
  const more = history.length > OV_STRIP ? `<button class="ov-shot more" type="button" data-ov-history="${esc(a.id)}" data-tip="查看全部 ${history.length} 张">+${history.length - OV_STRIP}</button>` : "";
  return `<div class="ov-strip" aria-label="最近的鹈鹕画">${chips}${more}</div>`;
}

function ovStatus(a) {
  const busy = busyTest(a);
  if (busy) return `<div class="ov-line running">${icon("loader-circle", "spin")}<span>${esc(busy)}进行中…</span></div>`;
  const times = [a.results?.at(-1)?.time, a.pelicans?.at(-1)?.time].filter(Boolean).sort();
  const latest = times.at(-1), pelican = a.pelicans?.at(-1);
  const duration = pelican?.duration_ms ? `<span class="ov-right meta mono">鹈鹕 ${fmtSec(pelican.duration_ms)}</span>` : "";
  // 额度用完等暂不可用时，插件不测这个账号；写明原因和预计恢复时间。
  const why = !a.disabled && a.unavailable ? `<div class="ov-line warn">${icon("circle-alert")}<span>${esc(unavailableReason(a).replace(/\n/g, "，"))}</span></div>` : "";
  return `<div class="ov-line"><span class="ov-led ${latest ? "on" : ""}"></span><span>${latest ? `最近测试 ${esc(fmtTime(latest, false))}` : "还没测过"}</span>${duration}</div>${why}`;
}

function ovCandy(a) {
  const history = a.results || [], last = history.at(-1);
  if (!last) return `<div class="ov-line muted">糖果题：还没测过</div>`;
  const dots = history.slice(-12).map((r) =>
    `<i class="ov-dot ${kind(r)}" data-tip="${esc(`${fmtTime(r.time, false)} ${VERDICT[kind(r)]} · ${modelName(r)}`)}"></i>`).join("");
  return `<div class="ov-line">${statusPill(kind(last), "糖果题：" + OV_CANDY[kind(last)])}<span class="ov-dots" aria-label="最近 ${Math.min(history.length, 12)} 次糖果题">${dots}</span></div>`;
}

function ovJudge(a) {
  const v = ovVerdict(a.id);
  if (!v || (!v.state && !v.last && !v.pending)) return `<div class="ov-line muted">降智判定：还没有糖果题结果（点「测糖果」，或在定时测试里勾选糖果题）</div>`;
  const facts = [
    v.since ? `自 ${fmtTime(v.since, false)}` : "",
    v.last ? `上一轮答对 ${v.last.correct}/${v.last.answered}${v.last.failed ? `，出错 ${v.last.failed}` : ""}` : "",
    v.pending ? "本轮进行中" : "",
    v.candidate ? `待确认：${OV_STATE[v.candidate] || v.candidate} 已 ${v.streak} 轮` : "",
  ].filter(Boolean).join(" · ");
  const mail = v.mail_error ? `<div class="ov-line warn">${icon("circle-alert")}<span>${esc(v.mail_error)}</span></div>`
    : v.mail_pending ? `<div class="ov-line muted">通知邮件待发送</div>` : "";
  return `<div class="ov-line"><span class="ov-state ${v.state || "none"}">${OV_STATE[v.state || ""]}</span><span class="meta">${esc(facts)}</span></div>${mail}`;
}

function ovActions(a) {
  const blocked = a.disabled ? "账号已停用" : a.unavailable ? unavailableReason(a) : credentialBusy(a) ? `${busyTest(a)}进行中` : pending ? "正在处理上一个操作" : "";
  const off = blocked ? `disabled data-tip="${esc(blocked)}"` : "";
  const tested = (a.results?.length || 0) + (a.pelicans?.length || 0);
  return `<div class="ov-actions">
    <button class="btn ghost" type="button" data-ov-run="candy" data-ov-id="${esc(a.id)}" ${blocked ? off : `data-tip="按定时设置的模型和次数测一轮，计入降智判定"`}>${icon("candy")}测糖果</button>
    <button class="btn ghost" type="button" data-ov-run="pelican" data-ov-id="${esc(a.id)}" ${off}>${icon("play")}画鹈鹕</button>
    <button class="btn ghost" type="button" data-ov-history="${esc(a.id)}" ${tested ? "" : "disabled"}>${icon("calendar")}历史</button>
  </div>`;
}

function ovCard(a) {
  const plan = ovPlan(a), state = ovVerdict(a.id)?.state || "";
  return `<article class="ov-card card ${state ? "is-" + state : ""} ${a.disabled ? "is-off" : ""}" data-ov-card="${esc(a.id)}">
    <div class="ov-head">${plan ? `<span class="tag ${plan.badge ? "plan-" + plan.badge : ""}"><span>${esc(plan.label)}</span></span>` : ""}<span class="ov-name name" data-tip="${esc(ovLabel(a))}">${esc(ovLabel(a))}</span>${a.disabled ? `<span class="tag warn"><span>已停用</span></span>` : a.unavailable ? `<span class="tag warn" data-tip="${esc(unavailableReason(a))}"><span>${esc(a.status_label || "不可用")}</span></span>` : ""}${ovSkipped(a.id) ? `<span class="tag" data-tip="在「定时与通知设置」里取消了勾选，定时测试跳过这个账号"><span>不参与定时</span></span>` : ""}</div>
    ${ovThumb(a)}${ovStrip(a)}
    <div class="ov-body">${ovStatus(a)}${ovCandy(a)}${ovJudge(a)}</div>
    ${ovActions(a)}
  </article>`;
}

// Cards are keyed by account and patched in place, so loaded thumbnails are not reloaded on every poll.
const ovRendered = { cards: new Map(), empty: "" };
function renderOverviewGrid() {
  const grid = $("ov-grid"), visible = ovVisible();
  if (!visible.length) {
    const message = loadError || (loadController && !credentials.length ? "正在读取 CPR 账号和测试记录…"
      : credentials.length ? "没有符合筛选条件的账号。" : "CPR 里还没有账号。请先在 CPR 的「账号管理」里登录账号，再回来测试。");
    const empty = `<div class="empty card">${esc(message)}</div>`;
    if (ovRendered.empty !== empty) grid.innerHTML = empty;
    ovRendered.cards = new Map();
    ovRendered.empty = empty;
    return;
  }
  if (ovRendered.empty) {
    grid.innerHTML = "";
    ovRendered.empty = "";
  }
  const cards = new Map(visible.map((a) => {
    const html = ovCard(a), previous = ovRendered.cards.get(a.id);
    if (previous?.html === html) return [a.id, previous];
    return [a.id, { html, node: previous ? patch(previous.node, rowNode(html)) : rowNode(html) }];
  }));
  const kept = new Set([...cards.values()].map((card) => card.node));
  for (const child of [...grid.children]) if (!kept.has(child)) child.remove();
  let cursor = grid.firstElementChild;
  for (const { node } of cards.values()) {
    if (node === cursor) cursor = cursor.nextElementSibling;
    else grid.insertBefore(node, cursor);
  }
  ovRendered.cards = cards;
  fitOverviewThumbs();
}

// Thumbnails lay a page out at 800×600 and scale it to the card width; every card in the grid is equally wide.
function fitOverviewThumbs() {
  const thumb = $("ov-grid").querySelector(".ov-thumb");
  if (thumb?.clientWidth) $("ov-grid").style.setProperty("--preview-scale", thumb.clientWidth / 800);
}

function renderOverviewSummary() {
  const states = credentials.map((a) => ovVerdict(a.id)?.state || "");
  const count = (state) => states.filter((s) => s === state).length;
  const updated = ovUpdatedAt ? `自动刷新 · ${fmtTime(new Date(ovUpdatedAt).toISOString())} 更新` : "";
  const html = `<span>${credentials.length} 个账号</span>
    <span class="ov-chip degraded">降智 ${count("degraded")}</span><span class="ov-chip ok">正常 ${count("ok")}</span><span class="ov-chip none">未判定 ${count("")}</span>
    <span class="ov-schedule">${hourlyCandy ? esc(scheduleLine(hourlyCandy)) : ""}</span><span class="ov-updated">${esc(updated)}</span>`;
  if ($("ov-summary").innerHTML !== html) $("ov-summary").innerHTML = html;
}

function fillOverviewSettings() {
  const s = ovSettings?.settings;
  if (!s || ovSettingsDirty) return;
  const models = [...$("model").options].map((option) => option.value);
  if (s.hourly_candy_model && !models.includes(s.hourly_candy_model)) models.unshift(s.hourly_candy_model);
  fillSelect("ov-model", models, s.hourly_candy_model, DEFAULT_MODEL);
  fillSelect("ov-effort", DEFAULT_EFFORTS, s.hourly_candy_effort, "medium");
  for (const field of $("ov-settings-form").elements) {
    if (!field.name || !(field.name in s) || field.id === "ov-model" || field.id === "ov-effort") continue;
    if (field.type === "checkbox") field.checked = !!s[field.name];
    else field.value = s[field.name];
  }
  renderOverviewAccounts();
  $("ov-password").innerHTML = ovSettings.password_set
    ? `${icon("circle-check")}<span>SMTP 授权码已在 CPR 里加密保存。要改授权码，请到 CPR「插件管理」→ 本插件 →「设置」。</span>`
    : `${icon("circle-alert")}<span>还没填 SMTP 授权码：请到 CPR「插件管理」→ 本插件 →「设置」里填写（CPR 加密保存，本页不经手授权码）。</span>`;
  $("ov-settings-status").textContent = "";
  // 第一次打开且还没开定时测试时，直接展开设置。
  if (!ovSettingsTouched && !s.hourly_candy_enabled) setOverviewSettingsOpen(true);
}

// 参与定时测试的账号：存的是取消勾选的账号，所以新加的账号默认参与。
let ovAccountsKey = "";
function renderOverviewAccounts() {
  const skip = new Set(String(ovSettings?.settings?.schedule_skip || "").split(",").map((id) => id.trim()).filter(Boolean));
  const accounts = [...credentials].sort((x, y) => planRank(x) - planRank(y) || ovLabel(x).localeCompare(ovLabel(y)));
  const key = JSON.stringify([accounts.map((a) => [a.id, ovLabel(a), !!a.disabled]), [...skip]]);
  if (key === ovAccountsKey) return;
  ovAccountsKey = key;
  $("ov-accounts").innerHTML = `<span class="field-label">参与账号</span>` + (accounts.length
    ? accounts.map((a) => `<label${a.disabled ? ` data-tip="账号已停用，定时测试本来就会跳过"` : ""}><input type="checkbox" data-ov-account="${esc(a.id)}" ${skip.has(a.id) ? "" : "checked"}><span class="ov-name name">${esc(ovLabel(a))}</span></label>`).join("")
    : `<span class="meta">还没有账号</span>`);
}

function setOverviewSettingsOpen(open) {
  $("ov-settings").hidden = !open;
  $("ov-settings-toggle").setAttribute("aria-expanded", open);
}

function collectOverviewSettings() {
  const out = {};
  for (const field of $("ov-settings-form").elements) {
    if (field.name) out[field.name] = field.type === "checkbox" ? field.checked : field.value;
  }
  out.schedule_skip = [...$("ov-settings-form").querySelectorAll("[data-ov-account]")]
    .filter((box) => !box.checked).map((box) => box.dataset.ovAccount).join(",");
  return out;
}

async function loadOverviewSettings() {
  try {
    ovSettings = await api(BASE + "/settings");
    fillOverviewSettings();
  } catch (err) {
    if (err instanceof AuthError) return showLogin(err.message);
    $("ov-settings-status").textContent = "读取设置失败：" + err.message;
  }
}

function renderOverview() {
  renderOverviewSummary();
  renderOverviewGrid();
  $("ov-run-now").disabled = pending || !credentials.some(usableCredential);
  if (ovSettings && !ovSettingsDirty && !$("ov-model").options.length) fillOverviewSettings();
  if (ovSettings && !ovSettingsDirty) renderOverviewAccounts();
  if ($("ov-history").open) renderOverviewHistory();
  refreshTip();
}

async function overviewRequest(path, body, done) {
  if (pending) return;
  pending = true;
  render();
  try {
    const result = await api(BASE + path, { method: "POST", body });
    done(result);
  } catch (err) {
    if (err instanceof AuthError) return showLogin(err.message);
    setNotice("flash", err.message || String(err));
  } finally {
    pending = false;
    load({ quiet: true });
  }
}

// 一个账号的全部历史：鹈鹕画（可放大）和糖果题回答。插件最多留每个账号最近 20 条，存储满时先删最旧的。
function ovCandyEntry(r) {
  const answer = r.error || r.skipped ? `<span class="meta">${esc(r.error || "已跳过")}</span>`
    : `<details class="ov-answer"><summary>看回答</summary><div class="markdown">${answerHTML(r)}</div></details>`;
  return `<div class="ov-candy-row">${verdict(r)}<span class="mono">${esc(fmtTime(r.time, false))}</span><span class="meta">${esc(modelName(r))}${r.duration_ms ? " · " + fmtSec(r.duration_ms) : ""}</span>${answer}</div>`;
}

let ovHistoryKey = "";
function renderOverviewHistory() {
  const dialog = $("ov-history"), a = credentials.find((c) => c.id === dialog.dataset.credential);
  if (!a) return dialog.close();
  const pelicans = [...(a.pelicans || [])].reverse(), results = [...(a.results || [])].reverse();
  const key = JSON.stringify([a.id, pelicans.map((r) => [r.id, pelicanPage(a.id, r.id) === undefined]), results.map((r) => [r.time, r.ok, r.error])]);
  if (key === ovHistoryKey) return;
  ovHistoryKey = key;
  $("ov-history-title").textContent = `历史记录 · ${ovLabel(a)}`;
  $("ov-history-body").innerHTML = `<p class="ov-hint">${icon("circle-help")}<span>每个账号最多保留最近 20 条；存储满时先删最旧的记录。点画面可以放大看动画。</span></p>
    <h3 class="ov-history-head">鹈鹕画（${pelicans.length}）</h3>
    <div class="ov-history-grid">${pelicans.map((r) => pelicanHistory(a, r)).join("") || `<div class="none">还没有记录</div>`}</div>
    <h3 class="ov-history-head">糖果题（${results.length}）</h3>
    <div class="ov-candy-list">${results.map(ovCandyEntry).join("") || `<div class="none">还没有记录</div>`}</div>`;
}

function openOverviewHistory(id) {
  const dialog = $("ov-history");
  dialog.dataset.credential = id;
  ovHistoryKey = "";
  renderOverviewHistory();
  try {
    showDialog(dialog);
  } catch (error) {
    setNotice("flash", "无法打开历史窗口：" + (error?.message || error));
  }
}

function initializeOverview() {
  $("ov-history-close").addEventListener("click", () => $("ov-history").close());
  $("ov-history").addEventListener("close", () => { $("ov-history-body").innerHTML = ""; ovHistoryKey = ""; });
  $("ov-history-body").addEventListener("click", (e) => {
    const preview = e.target.closest("[data-pl-detail]");
    if (preview) openResultDetail("pl", "pelican", preview.dataset.plCredential, preview.dataset.plDetail, showPelicanDetail);
  });
  $("ov-search").addEventListener("input", (e) => { ovFilter.search = e.target.value; renderOverview(); });
  $("ov-state").addEventListener("change", (e) => { ovFilter.state = e.target.value; renderOverview(); });
  $("ov-settings-toggle").addEventListener("click", () => {
    ovSettingsTouched = true;
    setOverviewSettingsOpen($("ov-settings").hidden);
  });
  const form = $("ov-settings-form");
  form.addEventListener("input", () => { ovSettingsDirty = true; $("ov-settings-status").textContent = "有未保存的修改"; });
  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    $("ov-settings-save").disabled = true;
    try {
      ovSettings = await api(BASE + "/settings", { method: "POST", body: collectOverviewSettings() });
      ovSettingsDirty = false;
      fillOverviewSettings();
      setNotice("flash", "设置已保存，下一次检查时生效（约半分钟内）。", "success");
      load({ quiet: true });
    } catch (err) {
      if (err instanceof AuthError) return showLogin(err.message);
      setNotice("flash", err.message || String(err));
    } finally {
      $("ov-settings-save").disabled = false;
    }
  });
  $("ov-notify-test").addEventListener("click", async () => {
    if (ovSettingsDirty) return setNotice("flash", "请先保存设置，再发送测试邮件。", "warning");
    const button = $("ov-notify-test");
    button.disabled = true;
    setNotice("flash", "正在发送测试邮件…", "info");
    try {
      const result = await api(BASE + "/notify/test", { method: "POST", body: {} });
      setNotice("flash", `测试邮件已发出（${result.recipients} 个收件人），请查收；没收到就看看垃圾箱。`, "success");
    } catch (err) {
      if (err instanceof AuthError) return showLogin(err.message);
      setNotice("flash", err.message || String(err));
    } finally {
      button.disabled = false;
    }
  });
  $("ov-run-now").addEventListener("click", () => overviewRequest("/schedule/run", {}, () =>
    setNotice("flash", "已开始测一轮：按勾选的测试项依次提交，结果出来后页面会自动刷新。", "success")));
  $("ov-grid").addEventListener("click", (e) => {
    const preview = e.target.closest("[data-pl-detail]");
    if (preview) return openResultDetail("pl", "pelican", preview.dataset.plCredential, preview.dataset.plDetail, showPelicanDetail);
    const history = e.target.closest("[data-ov-history]");
    if (history) return history.disabled || openOverviewHistory(history.dataset.ovHistory);
    const shot = e.target.closest("[data-ov-pick]");
    if (shot) {
      ovPick.set(shot.dataset.ovId, shot.dataset.ovPick);
      return renderOverview();
    }
    const button = e.target.closest("[data-ov-run]");
    if (!button || button.disabled) return;
    const s = ovSettings?.settings || {};
    const model = s.hourly_candy_model || hourlyCandy?.model || DEFAULT_MODEL;
    const effort = s.hourly_candy_effort || hourlyCandy?.effort || "medium";
    const body = { auth_ids: [button.dataset.ovId], model, effort };
    if (button.dataset.ovRun === "candy") {
      update("/run", { method: "POST", body: { ...body, runs: Number(s.schedule_candy_runs || hourlyCandy?.candy_runs || 1), judge: true } }, "开始测试失败");
    } else {
      ovPick.delete(button.dataset.ovId);
      update("/pelican/run", { method: "POST", body }, "开始测试失败");
    }
  });
  new ResizeObserver(fitOverviewThumbs).observe($("ov-grid"));
  loadOverviewSettings();
}
