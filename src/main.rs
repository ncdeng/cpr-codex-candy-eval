use codex_proxy_plugin_candy_eval::{live, manifest, plugin_with_config, PLUGIN_ID};
use gateway_plugin_sdk::client::{PluginSession, SessionConfig, SessionError};
use gateway_plugin_sdk::{CallBudgets, Stage};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut session = PluginSession::accept(
        tokio::io::stdin(),
        tokio::io::stdout(),
        SessionConfig::default(),
    )
    .await?;
    let manifest = manifest()?;
    if session.handshake().plugin_id != PLUGIN_ID
        || session.handshake().contributes != manifest.contributes
    {
        return Err(SessionError::Handshake.into());
    }
    // 维护调用要停留到预算快用完，好让后台测试借用它的宿主连接（见 live.rs）。
    session.set_call_budgets(CallBudgets::from([(
        Stage::Maintenance,
        live::MAINTENANCE_BUDGET_MS,
    )]))?;
    let plugin = plugin_with_config(session.handshake().configuration.clone())?;
    session.run(plugin).await?;
    Ok(())
}
