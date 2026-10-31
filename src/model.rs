//! 面向前端的状态与记录类型；字段名与 cpa-plugin-codex-candy-eval 的 Web 控制台契约一致。

use serde::{Deserialize, Serialize};

use crate::fingerprint::FingerprintResult;
use crate::modeltrace::TraceResult;

/// 每个凭证每类测试保留的最近记录条数。
pub const HISTORY_LIMIT: usize = 20;

/// 一次测试的历史记录上限与插件状态命名空间的单记录上限（256 KiB）对齐。
pub const CANDY_MAX_RUNS: u32 = 10;
pub const PELICAN_MAX_HTML: usize = 256 * 1024;
pub const ANSWER_MAX_CHARS: usize = 4000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credential {
    pub id: String,
    pub provider: String,
    pub source: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base_url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub email: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub plan_type: String,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub unavailable: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub status_message: String,
    /// 不可用时卡片标签上的短名，例如「额度用完」；空时页面显示「不可用」。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub status_label: String,
    /// 预计恢复时间（RFC 3339）；页面显示「预计 … 恢复」。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub next_retry_after: String,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandyResult {
    #[serde(default, skip_serializing_if = "is_false")]
    pub skipped: bool,
    pub time: String,
    pub model: String,
    pub effort: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub answer: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub reasoning_tokens: i64,
    #[serde(default)]
    pub duration_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PelicanResult {
    pub id: String,
    pub time: String,
    pub model: String,
    pub effort: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub skipped: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub html: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub reasoning_tokens: i64,
    #[serde(default)]
    pub duration_ms: i64,
}

impl PelicanResult {
    /// 摘要丢弃 HTML 正文，预览按记录单独加载。
    pub fn summary(&self) -> Self {
        let mut summary = self.clone();
        summary.html.clear();
        summary
    }
}

/// 测试进行中的进度，前端按 `done/total` 显示。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RunProgress {
    pub done: u32,
    pub total: u32,
}

/// 指纹测试进度：`phase` 为 collecting / comparing / cancelling。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FingerprintProgress {
    pub model: String,
    pub mode: String,
    pub done: u32,
    pub total: u32,
    pub valid: u32,
    pub errors: u32,
    pub phase: String,
    pub concurrency: u32,
}

/// 深度追踪进度：`phase` 为 collecting / cancelling。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceProgress {
    pub model: String,
    pub phase: String,
    pub done: u32,
    pub concurrency: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialView {
    #[serde(flatten)]
    pub credential: Credential,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running: Option<RunProgress>,
    pub results: Vec<CandyResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pelican_running: Option<bool>,
    pub pelicans: Vec<PelicanResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint_running: Option<FingerprintProgress>,
    pub fingerprints: Vec<FingerprintResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modeltrace_running: Option<TraceProgress>,
    pub modeltraces: Vec<TraceResult>,
}

/// 插件持久化状态；每个凭证的每类历史独立保留。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersistedState {
    #[serde(default)]
    pub results: std::collections::BTreeMap<String, Vec<CandyResult>>,
    #[serde(default)]
    pub pelicans: std::collections::BTreeMap<String, Vec<PelicanResult>>,
    #[serde(default)]
    pub fingerprints: std::collections::BTreeMap<String, Vec<FingerprintResult>>,
    #[serde(default)]
    pub traces: std::collections::BTreeMap<String, Vec<TraceResult>>,
    /// Persisted dispatch watermark: restarts never replay the previous hourly slot.
    #[serde(default)]
    pub hourly_candy_at: i64,
    /// 定时测试对每个账号的降智判定、待结算轮次与待发邮件。
    #[serde(default)]
    pub verdicts: std::collections::BTreeMap<String, Verdict>,
    /// 插件页面保存的定时测试与邮件设置（不含授权码），覆盖 CPR 插件配置里的同名旧值。
    #[serde(default)]
    pub settings: serde_json::Map<String, serde_json::Value>,
}

/// 一个账号的降智判定状态；`state` 为 ok / degraded，空表示尚未判定。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Verdict {
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub since: String,
    /// 与当前状态不同、正在等待连续确认的判定及其连续轮数。
    #[serde(default)]
    pub candidate: String,
    #[serde(default)]
    pub streak: u32,
    #[serde(default)]
    pub round: Option<PendingRound>,
    #[serde(default)]
    pub last: Option<RoundSummary>,
    #[serde(default)]
    pub mail: Option<PendingMail>,
    #[serde(default)]
    pub mail_error: String,
}

/// 定时调度已提交、尚未结算的一轮糖果题。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingRound {
    pub started_at: String,
    pub model: String,
    pub effort: String,
    pub runs: u32,
}

/// 一轮糖果题的结算结果；`result` 为 ok / degraded / unknown。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundSummary {
    pub time: String,
    pub model: String,
    pub effort: String,
    pub correct: u32,
    pub answered: u32,
    pub failed: u32,
    pub result: String,
}

/// 状态变化后尚未成功发出的通知邮件，失败后按退避重试。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingMail {
    pub subject: String,
    pub body: String,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub next_at: i64,
}

/// 把新记录追加到历史尾部并裁剪到上限。
pub fn append_recent<T>(history: &mut Vec<T>, record: T) {
    history.push(record);
    if history.len() > HISTORY_LIMIT {
        let excess = history.len() - HISTORY_LIMIT;
        history.drain(..excess);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_is_trimmed_to_the_limit() {
        let mut history = Vec::new();
        for index in 0..HISTORY_LIMIT + 5 {
            append_recent(&mut history, index);
        }
        assert_eq!(history.len(), HISTORY_LIMIT);
        assert_eq!(history[0], 5);
    }

    #[test]
    fn pelican_summary_drops_html() {
        let record = PelicanResult {
            id: "1".to_owned(),
            time: "2026-01-01T00:00:00Z".to_owned(),
            model: "gpt-6.1-sol".to_owned(),
            effort: "low".to_owned(),
            skipped: false,
            html: "<html></html>".to_owned(),
            error: String::new(),
            input_tokens: 1,
            output_tokens: 2,
            reasoning_tokens: 3,
            duration_ms: 4,
        };
        assert!(record.summary().html.is_empty());
        assert_eq!(record.summary().id, "1");
    }
}
