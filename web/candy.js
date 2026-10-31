"use strict";

const SUITE_NAMES = { candy: "糖果", pelican: "鹈鹕", fingerprint: "指纹" };
const VERDICT_NAMES = { ok: "正常", degraded: "降智" };
const fmtInterval = (s) => (s % 3600 ? `${s / 60} 分钟` : `${s / 3600} 小时`);

function scheduleLine(h) {
  if (!h.enabled) return "定时测试：未开启";
  const next = h.next_run_at ? new Date(h.next_run_at) : null;
  const due = next && next > new Date() ? `下次 ${next.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}` : "等待调度";
  const suites = (h.suites || []).map((s) => SUITE_NAMES[s] || s).join("、") || "未选测试项";
  const error = h.last_dispatch?.status;
  const warning = ["dispatch_error", "storage_error", "credentials_error"].includes(error) ? " · 上次调度失败，请检查服务状态" : "";
  const queued = h.queue?.length ? ` · 排队中：${h.queue.map((s) => SUITE_NAMES[s] || s).join("、")}` : "";
  const last = h.last_started_at ? `上次 ${new Date(h.last_started_at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })} · ` : "";
  const skip = h.skip?.length ? ` · ${h.skip.length} 个账号不参与` : "";
  const candy = (h.suites || []).includes("candy") ? `糖果每轮 ${h.candy_runs} 次、答对 ≥ ${Math.min(h.pass_min, h.candy_runs)} 次算正常` : "没勾糖果题，定时测试不会判定降智";
  const empty = h.last_dispatch?.status === "no_accounts" ? " · 没有参与的账号" : "";
  return `定时测试 · 每 ${fmtInterval(h.interval_seconds)} · ${suites} · ${h.model} / ${h.effort} · ${candy}${skip} · ${last}${due}${queued}${warning}${empty}`;
}

function verdictLine(v) {
  const name = VERDICT_NAMES[v.state] || "尚未判定";
  const since = v.since ? `（自 ${fmtTime(v.since)}）` : "";
  const last = v.last ? ` · 上一轮答对 ${v.last.correct}/${v.last.answered}${v.last.failed ? `，出错 ${v.last.failed}` : ""}` : "";
  const pending = v.pending ? " · 本轮测试中" : "";
  const waiting = v.candidate ? ` · 待确认：${VERDICT_NAMES[v.candidate] || v.candidate} ${v.streak} 轮` : "";
  const mail = v.mail_error ? ` · ${v.mail_error}` : v.mail_pending ? " · 通知邮件待发送" : "";
  return `<li><b>${esc(v.label)}</b>：${esc(name + since + last + pending + waiting + mail)}</li>`;
}

function mailLine(email) {
  if (!email?.enabled) return "邮件通知：未开启";
  if (!email.ready) return `邮件通知：设置不完整（${email.error || "请检查插件设置"}）`;
  return `邮件通知：已开启，发给 ${email.recipients} 个邮箱`;
}

function renderHourlyCandy() {
  const node = $("hourly-candy");
  node.hidden = !hourlyCandy;
  if (!hourlyCandy) return;
  const h = hourlyCandy;
  const verdicts = (h.verdicts || []).filter((v) => v.state || v.pending || v.last);
  node.innerHTML = `<div>${esc(scheduleLine(h))}（在「总览」页设置）</div>`
    + (verdicts.length ? `<ul class="verdicts">${verdicts.map(verdictLine).join("")}</ul>` : "")
    + `<div>${esc(mailLine(h.email))}</div>`;
}

const candyExpanded = new Set();
const candyAnswersExpanded = new Set();
const candySelected = new Set();

const candyRuns = () => boundedInput("runs", 1, 10);

const scopeMatch = (r) => r.model === $("model").value && (r.effort || "none") === $("effort").value;
const kind = (r) => (r.skipped ? "skip" : r.error ? "err" : r.ok ? "ok" : "bad");
const VERDICT = { ok: "答对", bad: "答错", err: "出错", skip: "已跳过" };
const verdict = (r) => statusPill(kind(r), VERDICT[kind(r)]);
const resultMeta = (r) => `<div class="result-meta">${verdict(r)}${metric("calendar", "测试时间", fmtTime(r.time))}${modelMeta(r)}</div>`;
// Answers come from untrusted upstreams: raw HTML is escaped, links and images stay text, and Temml
// refuses commands that need trust, such as \href. The CJK plugin lets emphasis close after full-width
// punctuation, as in **答案：**21.
const markdown = markdownit({ breaks: true }).disable(["link", "image", "autolink"])
  .use(markdownItCjkFriendly).use(texmath, { engine: temml, delimiters: ["dollars", "brackets"] });
const answerHTML = (r) => r.error ? esc(r.error) : markdown.render(r.answer || "");
const answerKey = (id, r) => JSON.stringify([id, r.time, r.model, r.effort]);
const answerToggle = (open) => `${icon("chevron-right", "chev")}${open ? "收起" : "展开"}`;

function candyHistory(a, r, i) {
  const key = answerKey(a.id, r);
  const answerOpen = candyAnswersExpanded.has(key);
  const bodyID = `answer-${encodeURIComponent(a.id)}-${i}`;
  return historyCard(r, `<div class="answer ${answerOpen ? "expanded" : ""}">${verdict(r)}
    <button class="answer-toggle" type="button" data-answer="${esc(key)}" aria-expanded="${answerOpen}" aria-controls="${esc(bodyID)}" aria-label="${answerOpen ? "收起文本" : "展开文本"}">${answerToggle(answerOpen)}</button>
    <div id="${esc(bodyID)}" class="answer-content ${r.error ? "error" : "markdown"}">${answerHTML(r)}</div>
  </div>`);
}

function renderCandyRow(a) {
  const results = a.results;
  const last = results[results.length - 1];
  const graded = results.filter((r) => scopeMatch(r) && !r.error && !r.skipped);
  const correct = graded.filter((r) => r.ok).length;

  const rate = graded.length
    ? `<b class="mono">${Math.round((correct / graded.length) * 100)}%</b><small class="mono">${correct}/${graded.length}</small>`
    : `<span class="none">—</span>`;
  const marks = results.map((r, i) => `<button class="mark-hit" type="button" data-auth="${esc(a.id)}" data-i="${i}" aria-label="${esc(`${fmtTime(r.time)} ${modelName(r)} ${VERDICT[kind(r)]}`)}">${icon(STATUS_ICONS[kind(r)], `mark ${kind(r)} ${scopeMatch(r) ? "" : "dim"}`)}</button>`).join("");
  const progress = a.running && `${a.running.done}/${a.running.total}`;
  const blocker = runBlocker("", a);
  const button = progress
    ? `<button class="btn ghost" type="button" data-candy-cancel="${esc(a.id)}" data-tip="停止测试，已完成 ${progress}">${icon("square")}停止<span class="mono">${progress}</span></button>`
    : `<button class="btn" type="button" data-candy-run="${esc(a.id)}" ${disabledFor(blocker)}>${icon("play")}测试</button>`;

  return credentialRow({
    type: "candy", credential: a, selected: candySelected.has(a.id), open: candyExpanded.has(a.id), selectable: !blocker, button,
    result: latestResult(last, verdict), meta: `<div class="rate">${rate}</div><div class="marks">${marks}</div>`, tested: results.length > 0,
    history: () => [...results].reverse().map((r, i) => candyHistory(a, r, i)).join(""),
  });
}

function runCandy(body) {
  if (!$("toolbar").reportValidity()) return;
  return update("/run", { method: "POST", body: { ...body, model: $("model").value, effort: $("effort").value, runs: candyRuns() } }, "开始测试失败");
}

function candyTip(target) {
  const r = credentials.find((a) => a.id === target.dataset.auth)?.results[target.dataset.i];
  return r ? `${resultMeta(r)}
    <div class="metrics">${r.skipped ? "" : metrics(r)}</div>
    <div class="tip-text ${r.error ? "error" : "markdown"}">${answerHTML(r)}</div>` : "";
}

function renderCandy() {
  $("rate-scope").textContent = `正确率按 ${modelName({ model: $("model").value, effort: $("effort").value })} 统计`;
  renderSelection("", candySelected, "测试");
  $("clear").disabled = pending || credentials.every((a) => !a.results.length);
  renderList("", renderCandyRow);
}

function initializeCandy() {
  fillSelect("effort", DEFAULT_EFFORTS, preference("effort", DEFAULT_EFFORT), DEFAULT_EFFORT);
  $("runs").value = preference("runs", 1);
  $("runs").value = candyRuns();

  for (const id of ["model", "effort"]) $(id).addEventListener("change", render);
  $("runs").addEventListener("change", () => { $("runs").value = candyRuns(); });
  $("rows").addEventListener("click", (e) => {
    if (e.target.closest("label")) return;
    const hit = e.target.closest("[data-auth]");
    if (hit) return showTip(hit);
    const toggle = e.target.closest("[data-answer]");
    if (toggle) {
      const key = toggle.dataset.answer;
      const open = !candyAnswersExpanded.has(key);
      open ? candyAnswersExpanded.add(key) : candyAnswersExpanded.delete(key);
      toggle.closest(".answer").classList.toggle("expanded", open);
      toggle.setAttribute("aria-expanded", open);
      toggle.setAttribute("aria-label", open ? "收起文本" : "展开文本");
      toggle.innerHTML = answerToggle(open);
      return;
    }
    const btn = e.target.closest("[data-candy-run]");
    if (btn) return runCandy({ auth_ids: [btn.dataset.candyRun] });
    const cancel = e.target.closest("[data-candy-cancel]");
    if (cancel) return update("/cancel", { method: "POST", body: { id: cancel.dataset.candyCancel } }, "停止测试失败");
    const row = e.target.closest("[data-row]");
    if (!row) return;
    const id = row.dataset.row;
    candyExpanded.has(id) ? candyExpanded.delete(id) : candyExpanded.add(id);
    render();
  });
}
