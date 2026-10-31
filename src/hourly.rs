//! 定时测试：按设置的间隔和套件逐个提交后台测试，用糖果题结果判定降智，状态变化时发邮件。
//! Dispatch uses only fresh official maintenance scopes; model work is detached.
use crate::{
    background, management,
    model::{CandyResult, Credential, PendingMail, PendingRound, RoundSummary, Verdict},
    notify,
    state::StateManager,
};
use chrono::{DateTime, FixedOffset, Utc};
use gateway_plugin_sdk::client::HostClient;
use serde_json::{json, Value};
use std::sync::Arc;

/// 邮件发送失败后的最多尝试次数；退避 5 分钟起，最长 1 小时。
const MAIL_ATTEMPTS: u32 = 12;

const BOOL_SETTINGS: [&str; 5] = [
    "hourly_candy_enabled",
    "schedule_candy",
    "schedule_pelican",
    "schedule_fingerprint",
    "email_enabled",
];
const CHOICE_SETTINGS: [(&str, &[&str]); 6] = [
    ("schedule_interval", &["30m", "1h", "2h", "3h", "6h", "12h", "24h"]),
    ("hourly_candy_effort", &["none", "low", "medium", "high", "xhigh", "max"]),
    ("schedule_candy_runs", &["1", "2", "3", "4", "5"]),
    ("judge_pass_min", &["1", "2", "3", "4", "5"]),
    ("judge_confirm_rounds", &["1", "2", "3"]),
    ("smtp_port", &["465", "587"]),
];
/// 文本设置及长度上限；授权码不在其中，只走 CPR 的加密敏感字段。
const TEXT_SETTINGS: [(&str, usize); 5] = [
    ("hourly_candy_model", 200),
    ("smtp_host", 253),
    ("smtp_username", 254),
    ("email_to", 1000),
    // 不参与定时测试的账号 ID，逗号分隔；新加的账号默认参与。
    ("schedule_skip", 8000),
];

/// 页面设置覆盖 CPR 插件配置里的同名值（1.8.0 时这些项在 CPR 配置里）。
pub(crate) fn effective(config: &Value, settings: &serde_json::Map<String, Value>) -> Value {
    let mut merged = config.as_object().cloned().unwrap_or_default();
    for (key, value) in settings {
        merged.insert(key.clone(), value.clone());
    }
    Value::Object(merged)
}

/// 校验页面提交的设置；只接受已知字段和取值，未知字段或授权码直接拒绝。
pub(crate) fn validate_settings(input: &Value) -> Result<serde_json::Map<String, Value>, String> {
    let object = input
        .as_object()
        .ok_or_else(|| "设置格式不对".to_owned())?;
    let mut settings = serde_json::Map::new();
    for (key, value) in object {
        let key = key.as_str();
        if BOOL_SETTINGS.contains(&key) {
            let flag = value
                .as_bool()
                .ok_or_else(|| format!("{key} 必须是开或关"))?;
            settings.insert(key.to_owned(), Value::Bool(flag));
        } else if let Some((_, values)) = CHOICE_SETTINGS.iter().find(|(name, _)| *name == key) {
            let choice = value
                .as_str()
                .filter(|choice| values.contains(choice))
                .ok_or_else(|| format!("{key} 的取值不对"))?;
            settings.insert(key.to_owned(), Value::String(choice.to_owned()));
        } else if let Some((_, limit)) = TEXT_SETTINGS.iter().find(|(name, _)| *name == key) {
            let text = value
                .as_str()
                .map(str::trim)
                .ok_or_else(|| format!("{key} 必须是文本"))?;
            if text.chars().count() > *limit || text.chars().any(char::is_control) {
                return Err(format!("{key} 太长或含有控制字符"));
            }
            if key == "hourly_candy_model" && !crate::host_calls::valid_model_name(text) {
                return Err("请选择不含推理强度后缀的模型".to_owned());
            }
            if key == "schedule_skip"
                && !text
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | ','))
            {
                return Err("参与账号的设置格式不对".to_owned());
            }
            settings.insert(key.to_owned(), Value::String(text.to_owned()));
        } else {
            return Err(format!("不支持的设置项：{key}"));
        }
    }
    Ok(settings)
}

/// 页面表单用的当前设置；授权码只报告有没有填。
pub(crate) fn settings_view(config: &Value) -> Value {
    let mut view = serde_json::Map::new();
    for key in BOOL_SETTINGS {
        let default = key == "schedule_candy";
        view.insert(key.to_owned(), Value::Bool(flag(config, key, default)));
    }
    let defaults = [
        ("schedule_interval", "1h"),
        ("hourly_candy_effort", "medium"),
        ("schedule_candy_runs", "3"),
        ("judge_pass_min", "2"),
        ("judge_confirm_rounds", "1"),
        ("smtp_port", "465"),
        ("hourly_candy_model", "gpt-6.1-sol"),
        ("smtp_host", "smtp.qq.com"),
        ("smtp_username", ""),
        ("email_to", ""),
        ("schedule_skip", ""),
    ];
    for (key, default) in defaults {
        let value = config.get(key).and_then(Value::as_str).unwrap_or(default);
        view.insert(key.to_owned(), Value::String(value.to_owned()));
    }
    let password_set = config
        .get("smtp_password")
        .and_then(Value::as_str)
        .is_some_and(|password| !password.is_empty());
    json!({"settings":view, "password_set":password_set})
}

fn enabled(config: &Value) -> bool {
    config.get("hourly_candy_enabled").and_then(Value::as_bool) == Some(true)
}

fn flag(config: &Value, field: &str, default: bool) -> bool {
    config
        .get(field)
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

fn choice(config: &Value, field: &str, default: u32, range: std::ops::RangeInclusive<u32>) -> u32 {
    config
        .get(field)
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| range.contains(value))
        .unwrap_or(default)
}

pub(crate) fn interval(config: &Value) -> i64 {
    match config
        .get("schedule_interval")
        .and_then(Value::as_str)
        .unwrap_or("1h")
    {
        "30m" => 1800,
        "2h" => 7200,
        "3h" => 10800,
        "6h" => 21600,
        "12h" => 43200,
        "24h" => 86400,
        _ => 3600,
    }
}

fn model(config: &Value) -> &str {
    config
        .get("hourly_candy_model")
        .and_then(Value::as_str)
        .unwrap_or("gpt-6.1-sol")
}

fn effort(config: &Value) -> &str {
    config
        .get("hourly_candy_effort")
        .and_then(Value::as_str)
        .unwrap_or("medium")
}

/// 选中的套件，按糖果、鹈鹕、指纹的顺序提交。
pub(crate) fn suites(config: &Value) -> Vec<String> {
    [
        ("candy", "schedule_candy", true),
        ("pelican", "schedule_pelican", false),
        ("fingerprint", "schedule_fingerprint", false),
    ]
    .into_iter()
    .filter(|(_, field, default)| flag(config, field, *default))
    .map(|(suite, _, _)| suite.to_owned())
    .collect()
}

/// 用户取消勾选、不参与定时测试的账号。
pub(crate) fn skipped(config: &Value) -> std::collections::BTreeSet<String> {
    config
        .get("schedule_skip")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .collect()
}

fn candy_runs(config: &Value) -> u32 {
    choice(config, "schedule_candy_runs", 3, 1..=5)
}

/// 判定规则：每轮至少答对 `pass_min` 次算正常，连续 `confirm` 轮一致才改变状态。
pub struct JudgeRule {
    pub pass_min: u32,
    pub confirm: u32,
    pub mail: bool,
}

pub(crate) fn rule(config: &Value) -> JudgeRule {
    JudgeRule {
        pass_min: choice(config, "judge_pass_min", 2, 1..=5),
        confirm: choice(config, "judge_confirm_rounds", 1, 1..=3),
        mail: flag(config, "email_enabled", false),
    }
}

pub(crate) fn due(last: i64, now: i64, interval: i64) -> bool {
    last == 0 || now >= last.saturating_add(interval)
}

fn time(value: i64) -> Option<String> {
    (value > 0)
        .then(|| DateTime::<Utc>::from_timestamp(value, 0).map(|t| t.to_rfc3339()))
        .flatten()
}

pub(crate) fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// 邮件里的时间统一用北京时间。
fn beijing(value: &str) -> String {
    let offset = FixedOffset::east_opt(8 * 3600).expect("fixed offset");
    DateTime::parse_from_rfc3339(value)
        .map(|t| t.with_timezone(&offset).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| value.to_owned())
}

pub(crate) fn label(credential: &Credential) -> String {
    [&credential.email, &credential.name, &credential.id]
        .into_iter()
        .map(|value| value.trim())
        .find(|value| !value.is_empty())
        .unwrap_or_default()
        .to_owned()
}

fn state_name(state: &str) -> &'static str {
    match state {
        "ok" => "正常",
        "degraded" => "降智",
        _ => "尚未判定",
    }
}

/// 汇总本轮在派发之后、同模型同强度的糖果结果。手动测试若恰好同模型同强度，也会计入。
pub(crate) fn summarize(round: &PendingRound, history: &[CandyResult], pass_min: u32) -> RoundSummary {
    let mut summary = RoundSummary {
        time: now_iso(),
        model: round.model.clone(),
        effort: round.effort.clone(),
        correct: 0,
        answered: 0,
        failed: 0,
        result: "unknown".to_owned(),
    };
    for result in history.iter().filter(|r| {
        !r.skipped && r.time >= round.started_at && r.model == round.model && r.effort == round.effort
    }) {
        if !result.error.is_empty() {
            summary.failed += 1;
        } else {
            summary.answered += 1;
            if result.ok {
                summary.correct += 1;
            }
        }
    }
    let pass = pass_min.min(round.runs).max(1);
    if summary.answered >= pass {
        summary.result = if summary.correct >= pass { "ok" } else { "degraded" }.to_owned();
    }
    summary
}

/// 结算一轮并更新判定；状态改变且开了邮件时排队一封通知。
pub(crate) fn settle(
    verdict: &mut Verdict,
    round: &PendingRound,
    history: &[CandyResult],
    rule: &JudgeRule,
    label: &str,
) {
    let summary = summarize(round, history, rule.pass_min);
    let result = summary.result.clone();
    verdict.last = Some(summary.clone());
    if result == "unknown" {
        return;
    }
    if result == verdict.state {
        verdict.candidate.clear();
        verdict.streak = 0;
        return;
    }
    if verdict.candidate == result {
        verdict.streak += 1;
    } else {
        verdict.candidate = result.clone();
        verdict.streak = 1;
    }
    if verdict.streak < rule.confirm {
        return;
    }
    let previous = std::mem::replace(&mut verdict.state, result);
    verdict.since = summary.time.clone();
    verdict.candidate.clear();
    verdict.streak = 0;
    if rule.mail {
        let (subject, body) = transition_mail(label, &previous, &verdict.state, &summary, rule);
        verdict.mail = Some(PendingMail {
            subject,
            body,
            attempts: 0,
            next_at: 0,
        });
        verdict.mail_error.clear();
    }
}

pub(crate) fn transition_mail(
    label: &str,
    previous: &str,
    current: &str,
    summary: &RoundSummary,
    rule: &JudgeRule,
) -> (String, String) {
    let subject = match (previous, current) {
        (_, "degraded") => format!("【降智告警】{label} 判定为降智"),
        ("", _) => format!("【降智测试】{label} 首次判定：正常"),
        _ => format!("【降智恢复】{label} 已恢复正常"),
    };
    let body = format!(
        "账号：{label}\n判定：{}（之前：{}）\n时间：{}（北京时间）\n本轮糖果题：答对 {}/{}，另有 {} 次请求出错；模型 {}，推理强度 {}\n规则：每轮至少答对 {} 次算正常，连续 {} 轮结果一致才改变状态\n\n详情请到 CPR 的「Codex 降智与指纹测试」插件页面查看。\n—— Codex 降智测试插件（CPR）",
        state_name(current),
        state_name(previous),
        beijing(&summary.time),
        summary.correct,
        summary.answered,
        summary.failed,
        summary.model,
        summary.effort,
        rule.pass_min,
        rule.confirm,
    );
    (subject, body)
}

pub fn test_mail_body() -> String {
    format!(
        "这是一封测试邮件，说明 CPR 降智测试插件的邮件设置可以用。\n发送时间：{}（北京时间）\n\n以后账号判定为降智、或从降智恢复正常时，会发到这个邮箱。\n—— Codex 降智测试插件（CPR）",
        beijing(&now_iso())
    )
}

pub enum MailOutcome {
    Sent,
    Failed(String),
    /// 邮件通知已关闭：丢弃待发邮件。
    Dropped,
}

pub(crate) fn apply_mail_outcome(verdict: &mut Verdict, outcome: MailOutcome, now: i64) {
    match outcome {
        MailOutcome::Sent | MailOutcome::Dropped => {
            verdict.mail = None;
            verdict.mail_error.clear();
        }
        MailOutcome::Failed(error) => {
            let Some(mail) = verdict.mail.as_mut() else {
                return;
            };
            mail.attempts += 1;
            if mail.attempts >= MAIL_ATTEMPTS {
                verdict.mail = None;
                verdict.mail_error = format!("通知邮件多次发送失败，已放弃：{error}");
            } else {
                mail.next_at = now + (300 * i64::from(mail.attempts)).min(3600);
                verdict.mail_error = format!("通知邮件发送失败，稍后重试：{error}");
            }
        }
    }
}

pub async fn status(state: &StateManager, config: &Value) -> Value {
    let (last, dispatch) = state.hourly_candy().await;
    let interval = interval(config);
    let rule = rule(config);
    let password_set = settings_view(config)["password_set"].as_bool() == Some(true);
    let mail = match notify::mail_config(config) {
        Ok(Some(mail)) => json!({"enabled":true, "ready":true, "recipients":mail.to.len(), "password_set":password_set}),
        Ok(None) => json!({"enabled":false, "ready":false, "password_set":password_set}),
        Err(error) => json!({"enabled":true, "ready":false, "error":error, "password_set":password_set}),
    };
    let verdicts: Vec<Value> = state
        .verdict_views()
        .await
        .into_iter()
        .map(|(id, label, verdict)| {
            json!({"id":id, "label":label, "state":verdict.state, "since":verdict.since,
                "pending":verdict.round.is_some(), "last":verdict.last,
                "candidate":verdict.candidate, "streak":verdict.streak,
                "mail_pending":verdict.mail.is_some(), "mail_error":verdict.mail_error})
        })
        .collect();
    json!({"enabled":enabled(config), "interval_seconds":interval,
        "model":model(config), "effort":effort(config), "suites":suites(config),
        "candy_runs":candy_runs(config), "pass_min":rule.pass_min, "confirm_rounds":rule.confirm,
        "last_started_at":time(last),
        "next_run_at":if enabled(config) && last > 0 { time(last.saturating_add(interval)) } else { None },
        "last_dispatch":dispatch, "queue":state.schedule_queue().await,
        "skip":skipped(config),
        "email":mail, "verdicts":verdicts})
}

async fn send_mails(state: &StateManager, host: &HostClient, config: &Value) {
    let now = Utc::now().timestamp();
    let due = state.due_mails(now).await;
    if due.is_empty() {
        return;
    }
    let mail = notify::mail_config(config);
    for (id, pending) in due {
        let outcome = match &mail {
            Ok(Some(mail)) => match notify::send(mail, &pending.subject, &pending.body).await {
                Ok(()) => MailOutcome::Sent,
                Err(error) => MailOutcome::Failed(error),
            },
            Ok(None) => MailOutcome::Dropped,
            Err(error) => MailOutcome::Failed(error.clone()),
        };
        state.mail_result(host, &id, outcome, now).await;
    }
}

/// `ids` 为空表示全部账号；糖果题带 `judge`，跑完按判定规则结算。
fn payload(suite: &str, config: &Value, ids: &[String]) -> (&'static str, Value) {
    let (path, mut body) = match suite {
        "pelican" => (
            "api/pelican/run",
            json!({"model":model(config), "effort":effort(config)}),
        ),
        "fingerprint" => (
            "api/fingerprint/run",
            json!({"model":model(config), "effort":effort(config), "mode":"quick"}),
        ),
        _ => (
            "api/run",
            json!({"runs":candy_runs(config), "model":model(config), "effort":effort(config), "judge":true}),
        ),
    };
    if ids.is_empty() {
        body["all"] = json!(true);
    } else {
        body["auth_ids"] = json!(ids);
    }
    (path, body)
}

async fn dispatch_next(state: &Arc<StateManager>, host: &HostClient, config: &Value) {
    let Some(suite) = state.next_scheduled_suite().await else {
        return;
    };
    let skip = skipped(config);
    let ids: Vec<String> = if skip.is_empty() {
        Vec::new()
    } else {
        let _ = management::refresh_credentials(state, host).await;
        let ids: Vec<String> = state
            .credentials()
            .await
            .into_iter()
            .map(|credential| credential.id)
            .filter(|id| !skip.contains(id))
            .collect();
        if ids.is_empty() {
            state
                .set_hourly_dispatch(json!({"status":"no_accounts", "suite":suite}))
                .await;
            return;
        }
        ids
    };
    let (path, body) = payload(&suite, config, &ids);
    let dispatch = match background::submit(state, host, config, path, body.to_string().as_bytes()).await {
        Ok(reply) if reply.result.status == 202 => {
            let reply = serde_json::from_slice::<Value>(&reply.payload).unwrap_or_default();
            json!({"status":"dispatched", "suite":suite, "started":reply["started"],
                "busy":reply["busy"].as_array().map_or(0, Vec::len),
                "skipped":reply["skipped"].as_array().map_or(0, Vec::len)})
        }
        _ => json!({"status":"dispatch_error", "suite":suite}),
    };
    state.set_hourly_dispatch(dispatch).await;
}

/// 立即排上一轮：不等间隔，下一次定时从现在起算。
pub async fn run_now(state: &Arc<StateManager>, host: &HostClient, config: &Value) -> Result<(), String> {
    let suites = suites(config);
    if suites.is_empty() {
        return Err("请先在设置里勾选至少一个测试项".to_owned());
    }
    let Some(revision) = state.claim_now(Utc::now().timestamp(), suites).await else {
        return Err("上一轮还没提交完，或测试记录还没读取完成，请稍后再试".to_owned());
    };
    state.flush_pending(host).await;
    if !state.revision_committed(revision).await {
        state.clear_schedule_queue().await;
        return Err("保存调度状态失败，请检查 CPR 服务状态".to_owned());
    }
    dispatch_next(state, host, config).await;
    Ok(())
}

pub async fn tick(state: &Arc<StateManager>, host: &HostClient, config: &Value) {
    let config = &effective(config, &state.settings().await);
    if state.has_pending_rounds().await {
        // 结算和邮件需要账号显示名；读取失败时仍按 ID 结算。
        let _ = management::refresh_credentials(state, host).await;
    }
    state.settle_rounds(host, &rule(config)).await;
    send_mails(state, host, config).await;
    let now = Utc::now().timestamp();
    // 关闭定时只停止新的轮次；「立即测一轮」排上的套件照常提交完。
    if enabled(config) {
        if let Some(revision) = state
            .claim_hourly_candy(now, interval(config), suites(config))
            .await
        {
            // Commit before dispatch: a crash may skip a slot, but never creates a duplicate billable run.
            state.flush_pending(host).await;
            if !state.revision_committed(revision).await {
                state.clear_schedule_queue().await;
                state
                    .set_hourly_dispatch(json!({"status":"storage_error"}))
                    .await;
                return;
            }
        }
    }
    dispatch_next(state, host, config).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_follow_the_interval_without_catch_up_bursts_or_clock_reversal_replays() {
        assert!(due(0, 10_000, 3600));
        assert!(!due(10_000, 13_599, 3600));
        assert!(due(10_000, 13_600, 3600));
        assert!(due(10_000, 99_999, 3600));
        assert!(!due(13_600, 13_600, 3600));
        assert!(!due(10_000, 9_999, 3600));
        assert!(due(10_000, 11_800, 1800));
    }

    #[test]
    fn skipped_accounts_are_left_out_and_candy_rounds_are_judged() {
        let config = json!({"schedule_skip":" acct_a, ,acct_b "});
        assert_eq!(skipped(&config).into_iter().collect::<Vec<_>>(), ["acct_a", "acct_b"]);
        assert!(skipped(&json!({})).is_empty());
        let (path, body) = payload("candy", &json!({}), &[]);
        assert_eq!(path, "api/run");
        assert_eq!(body["all"], true);
        assert_eq!(body["judge"], true);
        let (path, body) = payload("pelican", &json!({}), &["acct_c".to_owned()]);
        assert_eq!(path, "api/pelican/run");
        assert_eq!(body["auth_ids"], json!(["acct_c"]));
        assert!(body.get("all").is_none());
        assert!(validate_settings(&json!({"schedule_skip":"acct_1,acct-2"})).is_ok());
        assert!(validate_settings(&json!({"schedule_skip":"acct 1"})).is_err());
    }

    #[test]
    fn automation_is_opt_in_with_candy_by_default() {
        assert!(!enabled(&json!({})));
        assert!(!enabled(&json!({"hourly_candy_enabled":"true"})));
        assert!(enabled(&json!({"hourly_candy_enabled":true})));
        assert_eq!(model(&json!({})), "gpt-6.1-sol");
        assert_eq!(effort(&json!({})), "medium");
        assert_eq!(interval(&json!({})), 3600);
        assert_eq!(interval(&json!({"schedule_interval":"6h"})), 21600);
        assert_eq!(suites(&json!({})), vec!["candy"]);
        assert_eq!(
            suites(&json!({"schedule_candy":false, "schedule_pelican":true, "schedule_fingerprint":true})),
            vec!["pelican", "fingerprint"]
        );
        assert_eq!(candy_runs(&json!({"schedule_candy_runs":"9"})), 3);
    }

    fn answer(time: &str, ok: bool, error: &str) -> CandyResult {
        CandyResult {
            skipped: false,
            time: time.to_owned(),
            model: "m".to_owned(),
            effort: "medium".to_owned(),
            ok,
            answer: String::new(),
            error: error.to_owned(),
            input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: 0,
            duration_ms: 0,
        }
    }

    fn round() -> PendingRound {
        PendingRound {
            started_at: "2026-10-30T10:00:00.000Z".to_owned(),
            model: "m".to_owned(),
            effort: "medium".to_owned(),
            runs: 3,
        }
    }

    #[test]
    fn rounds_count_only_answers_after_dispatch() {
        let history = vec![
            answer("2026-10-30T09:59:59.000Z", true, ""),
            answer("2026-10-30T10:00:01.000Z", true, ""),
            answer("2026-10-30T10:00:02.000Z", false, ""),
            answer("2026-10-30T10:00:03.000Z", false, "timeout"),
        ];
        let summary = summarize(&round(), &history, 2);
        assert_eq!((summary.correct, summary.answered, summary.failed), (1, 2, 1));
        assert_eq!(summary.result, "degraded");
        let summary = summarize(&round(), &history[..1], 2);
        assert_eq!(summary.result, "unknown");
    }

    #[test]
    fn state_changes_need_confirmation_and_queue_one_mail() {
        let rule = JudgeRule { pass_min: 2, confirm: 2, mail: true };
        let bad = vec![
            answer("2026-10-30T10:00:01.000Z", false, ""),
            answer("2026-10-30T10:00:02.000Z", false, ""),
            answer("2026-10-30T10:00:03.000Z", false, ""),
        ];
        let mut verdict = Verdict::default();
        settle(&mut verdict, &round(), &bad, &rule, "a@example.com");
        assert_eq!((verdict.state.as_str(), verdict.streak), ("", 1));
        assert!(verdict.mail.is_none());
        settle(&mut verdict, &round(), &bad, &rule, "a@example.com");
        assert_eq!(verdict.state, "degraded");
        let mail = verdict.mail.clone().unwrap();
        assert!(mail.subject.contains("降智告警"));
        assert!(mail.body.contains("答对 0/3"));

        let good = vec![
            answer("2026-10-30T10:00:01.000Z", true, ""),
            answer("2026-10-30T10:00:02.000Z", true, ""),
        ];
        let rule = JudgeRule { pass_min: 2, confirm: 1, mail: true };
        settle(&mut verdict, &round(), &good, &rule, "a@example.com");
        assert_eq!(verdict.state, "ok");
        assert!(verdict.mail.as_ref().unwrap().subject.contains("已恢复正常"));

        // 无法判定的轮次不打断也不推进确认。
        settle(&mut verdict, &round(), &[], &rule, "a@example.com");
        assert_eq!(verdict.state, "ok");
        assert_eq!(verdict.last.as_ref().unwrap().result, "unknown");
    }

    #[test]
    fn page_settings_override_old_config_and_reject_unknown_fields() {
        let old = json!({"hourly_candy_effort":"low", "smtp_password":"secret", "api_base_url":"http://127.0.0.1:8080"});
        let settings = validate_settings(&json!({"hourly_candy_effort":"high", "hourly_candy_enabled":true,
            "email_to":" a@example.com ", "schedule_interval":"6h"}))
        .unwrap();
        let merged = effective(&old, &settings);
        assert_eq!(effort(&merged), "high");
        assert!(enabled(&merged));
        assert_eq!(interval(&merged), 21600);
        assert_eq!(merged["email_to"], "a@example.com");
        assert_eq!(merged["smtp_password"], "secret");
        let view = settings_view(&merged);
        assert_eq!(view["password_set"], true);
        assert!(view["settings"].get("smtp_password").is_none());
        for bad in [
            json!({"smtp_password":"x"}),
            json!({"unknown":1}),
            json!({"schedule_interval":"5m"}),
            json!({"hourly_candy_enabled":"yes"}),
            json!({"email_to":"a@example.com\r\nBcc: b@example.com"}),
            json!({"hourly_candy_model":"gpt(high)"}),
        ] {
            assert!(validate_settings(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn failed_mail_backs_off_then_gives_up() {
        let mut verdict = Verdict {
            mail: Some(PendingMail { subject: "s".into(), body: "b".into(), attempts: 0, next_at: 0 }),
            ..Verdict::default()
        };
        apply_mail_outcome(&mut verdict, MailOutcome::Failed("x".into()), 1000);
        assert_eq!(verdict.mail.as_ref().unwrap().next_at, 1300);
        for _ in 1..MAIL_ATTEMPTS {
            apply_mail_outcome(&mut verdict, MailOutcome::Failed("x".into()), 1000);
        }
        assert!(verdict.mail.is_none());
        assert!(verdict.mail_error.contains("已放弃"));
    }
}
