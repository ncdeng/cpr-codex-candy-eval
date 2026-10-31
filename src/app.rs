use crate::{command, management, state::StateManager};
use gateway_plugin_sdk::client::{methods, AuthorError, ComposedPlugin, PluginBuilder, TypedReply};
use std::sync::Arc;

pub fn plugin() -> Result<ComposedPlugin, AuthorError> {
    plugin_with_config(serde_json::json!({}))
}

pub fn plugin_with_config(configuration: serde_json::Value) -> Result<ComposedPlugin, AuthorError> {
    let state = Arc::new(StateManager::new());
    let mgmt_state = Arc::clone(&state);
    let cmd_state = Arc::clone(&state);
    let maintenance_state = Arc::clone(&state);
    let maintenance_config = configuration.clone();
    PluginBuilder::from_json(include_bytes!("../plugin.json"))?
        .management(management::registration(), move |call| {
            let state = Arc::clone(&mgmt_state);
            let config = configuration.clone();
            async move { management::handle_with_config(&state, call, &config).await }
        })?
        .command_line(command::command_registration(), move |call| {
            let state = Arc::clone(&cmd_state);
            async move { command::command_line(&state, call).await }
        })?
        .on(methods::RECONCILE, move |call| {
            let state = Arc::clone(&maintenance_state);
            let config = maintenance_config.clone();
            async move {
                // 停留到预算快用完：期间每 30 秒做一次定时检查，并把宿主连接借给后台测试。
                let window = crate::live::Window::new(call.host.remaining_budget());
                state.ensure_loaded(&call.host).await;
                let live = state.live();
                live.publish(&call.host, &call.cancellation);
                loop {
                    state.flush_pending(&call.host).await;
                    crate::hourly::tick(&state, &call.host, &config).await;
                    let next = tokio::time::Instant::now() + crate::live::TICK;
                    if next >= window.serve_until {
                        break;
                    }
                    live.rest(next).await;
                }
                live.withdraw(window.return_by).await;
                state.flush_pending(&call.host).await;
                Ok(TypedReply::new(Default::default()))
            }
        })?
        .build()
}
