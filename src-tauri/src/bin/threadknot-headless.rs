//! Headless server (no Tauri window) — for smoke tests and running Threadknot as a
//! pure LAN service: `threadknot-headless` then open the printed URL from any browser.

use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();
    let (state, info) = threadknot_lib::build_server_state()?;
    println!("Threadknot headless — local: http://127.0.0.1:{}/?token={}", info.port, info.token);
    println!("Threadknot headless — LAN:   {}", info.lan_url);
    let browsers = Arc::clone(&state.browsers);
    tokio::select! {
        _ = threadknot_lib::server::run(state) => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    // Close browsers gracefully: a signed-in Chrome that dies with the process
    // keeps its profile locked and its last cookie writes unflushed.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), browsers.shutdown_all()).await;
    Ok(())
}
