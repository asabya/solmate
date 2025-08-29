
mod solana;
mod utils;

use tracing_subscriber::fmt::time;
use tracing::Level;
use crate::solana::solana::SolanaWSClient;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_timer(time::LocalTime::rfc_3339())
        .init();

    let span = tracing::span!(target: "solmate", Level::INFO, "main");
    let _enter = span.enter();

    let mut solana_ws_client = SolanaWSClient::new("wss://api.mainnet-beta.solana.com").await?;
    solana_ws_client.start("confirmed", "logsSubscribe", "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").await?;
    // wait for interrupt
    tokio::signal::ctrl_c().await?;
    solana_ws_client.stop().await?;
    Ok(())
}
