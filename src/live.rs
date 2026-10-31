//! 不新建 Key 的测试通道：借用 CPR 定时维护调用的宿主连接，用账号所在分组里已有 Key 的编号
//! 直接指定账号执行（只用编号，不取 Key 明文）。宿主连接只在那一次维护调用里有效，所以维护调用
//! 会停留到预算快用完，期间照常每 30 秒做一次定时检查；退出前先等进行中的模型调用结束。
use crate::{
    execution::{ExecutionContext, StorageContext},
    host_calls::{self, ModelAnswer, ModelError, ModelFailure},
};
use gateway_plugin_sdk::client::{CallCancellation, HostClient};
use serde_json::Value;
use std::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::sync::{watch, Notify, Semaphore};
use tokio::time::Instant;

/// 向 CPR 声明的维护调用预算。
pub const MAINTENANCE_BUDGET_MS: u64 = 15 * 60 * 1000;
/// 停止接新调用后，留给进行中模型调用收尾的时间。
const DRAIN: Duration = Duration::from_secs(4 * 60);
/// 返回前再留的余量，保证在 CPR 的期限之前回复。
const MARGIN: Duration = Duration::from_secs(20);
/// 维护调用里两次定时检查的间隔，和 CPR 平时的维护间隔一致。
pub const TICK: Duration = Duration::from_secs(30);
/// 一次宿主调用最多同时开 16 个模型流，留些余量。
const STREAMS: usize = 12;
/// 两次维护调用之间最多隔约 30 秒；等这么久还没有连接就报错。
const WAIT_FOR_HOST: Duration = Duration::from_secs(150);

pub struct LiveHost {
    host: watch::Sender<Option<(u64, HostClient)>>,
    generation: AtomicU64,
    in_flight: AtomicUsize,
    idle: Notify,
    wake: Notify,
    streams: Arc<Semaphore>,
}

impl Default for LiveHost {
    fn default() -> Self {
        Self {
            host: watch::Sender::new(None),
            generation: AtomicU64::new(0),
            in_flight: AtomicUsize::new(0),
            idle: Notify::new(),
            wake: Notify::new(),
            streams: Arc::new(Semaphore::new(STREAMS)),
        }
    }
}

/// 一次维护调用里各阶段的截止时间。
pub struct Window {
    pub serve_until: Instant,
    pub return_by: Instant,
}

impl Window {
    pub fn new(remaining: Option<Duration>) -> Self {
        let now = Instant::now();
        let remaining = remaining.unwrap_or_default();
        Self {
            serve_until: now + remaining.saturating_sub(DRAIN + MARGIN),
            return_by: now + remaining.saturating_sub(MARGIN),
        }
    }
}

struct Flight<'a>(&'a LiveHost);
impl Drop for Flight<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

impl LiveHost {
    /// 开始提供本次维护调用的连接；调用被 CPR 取消时自动撤下。
    pub fn publish(self: &Arc<Self>, host: &HostClient, cancellation: &CallCancellation) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.host.send_replace(Some((generation, host.clone())));
        let live = Arc::clone(self);
        let cancellation = cancellation.clone();
        tokio::spawn(async move {
            // 调用正常结束时取消信号的发送端被丢弃，这里也会返回。
            cancellation.cancelled().await;
            live.host
                .send_if_modified(|current| match current {
                    Some((published, _)) if *published == generation => {
                        *current = None;
                        true
                    }
                    _ => false,
                });
        });
    }

    /// 停止提供连接，等进行中的模型调用结束，最晚等到 `deadline`。
    pub async fn withdraw(&self, deadline: Instant) {
        self.host.send_replace(None);
        loop {
            let idle = self.idle.notified();
            tokio::pin!(idle);
            idle.as_mut().enable();
            if self.in_flight.load(Ordering::SeqCst) == 0 {
                return;
            }
            if tokio::time::timeout_at(deadline, idle).await.is_err() {
                return;
            }
        }
    }

    /// 等到下一次定时检查；有测试写完结果时提前醒来，尽快保存。
    pub async fn rest(&self, until: Instant) {
        tokio::select! {
            () = tokio::time::sleep_until(until) => {},
            () = self.wake.notified() => {},
        }
    }

    pub fn wake(&self) {
        self.wake.notify_one();
    }

    fn release(&self) {
        if self.in_flight.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.idle.notify_waiters();
        }
    }

    /// 先登记再读取，撤下连接的一方因此一定会等到这次调用结束。
    async fn acquire(&self) -> Option<(HostClient, Flight<'_>)> {
        let deadline = Instant::now() + WAIT_FOR_HOST;
        let mut changes = self.host.subscribe();
        loop {
            self.in_flight.fetch_add(1, Ordering::SeqCst);
            let flight = Flight(self);
            let current = changes.borrow_and_update().clone();
            if let Some((_, host)) = current {
                return Some((host, flight));
            }
            drop(flight);
            match tokio::time::timeout_at(deadline, changes.changed()).await {
                Ok(Ok(())) => continue,
                _ => return None,
            }
        }
    }
}

/// 用已有 Key 的编号在指定账号上执行；Key 明文不经过插件。
#[derive(Clone)]
pub struct LiveExecutor {
    live: Arc<LiveHost>,
    key_id: String,
    account: String,
    provider: String,
}

impl LiveExecutor {
    pub fn new(live: Arc<LiveHost>, key_id: String, account: String, provider: String) -> Self {
        Self {
            live,
            key_id,
            account,
            provider,
        }
    }
}

impl StorageContext for LiveExecutor {
    fn storage_host(&self) -> Option<&HostClient> {
        None
    }
}

impl ExecutionContext for LiveExecutor {
    async fn execute(
        &self,
        account: &str,
        provider: &str,
        model: &str,
        body: &Value,
    ) -> Result<ModelAnswer, ModelError> {
        if account != self.account || provider != self.provider {
            return Err(ModelError::other(
                ModelFailure::Host,
                "后台测试账号作用域不匹配",
            ));
        }
        let _stream = Arc::clone(&self.live.streams)
            .acquire_owned()
            .await
            .map_err(|_| ModelError::other(ModelFailure::Host, "测试通道已关闭"))?;
        let Some((host, _flight)) = self.live.acquire().await else {
            return Err(ModelError::other(
                ModelFailure::Host,
                "等了两分半还没等到 CPR 的插件维护调用，请确认插件在运行后再试",
            ));
        };
        host_calls::execute_body_with_key(&host, self.key_id.clone(), account, provider, model, body)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_leaves_time_to_drain_and_reply() {
        let budget = Duration::from_millis(MAINTENANCE_BUDGET_MS);
        let window = Window::new(Some(budget));
        assert!(window.serve_until < window.return_by);
        assert!(window.return_by - window.serve_until >= DRAIN - Duration::from_secs(1));
        // CPR 只给默认 120 秒时不提供连接，直接收尾。
        let short = Window::new(Some(Duration::from_secs(120)));
        assert!(short.serve_until <= Instant::now());
    }

    #[tokio::test]
    async fn withdraw_returns_at_once_without_calls_in_flight() {
        let live = LiveHost::default();
        tokio::time::timeout(Duration::from_secs(1), live.withdraw(Instant::now() + Duration::from_secs(60)))
            .await
            .expect("no call in flight");
    }
}
