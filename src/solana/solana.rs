use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tokio_tungstenite::tungstenite::Message;
use tokio::time::{interval, sleep, Duration};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::Level;
use tracing::{error, info, warn};
use serde_json::Value;
use futures_util::{StreamExt, SinkExt};

#[derive(Debug)]
enum WebSocketError {
    AlreadyRunning,
    ConnectionError(String),
}

impl std::error::Error for WebSocketError {}

impl std::fmt::Display for WebSocketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WebSocketError::AlreadyRunning => write!(f, "WebSocket connection is already running"),
            WebSocketError::ConnectionError(msg) => write!(f, "{}", msg),
        }
    }
}
pub struct SolanaWSClient {
    url: String,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task_handle: Option<JoinHandle<()>>
}

impl SolanaWSClient {
    pub async fn new(url: &str) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            url: url.to_string(),
            shutdown_tx: None,
            task_handle: None
        })
    }

    // start should spawn a new task to handle the websocket connection
    pub async fn start(&mut self, commitment: &str, method: &str, mention: &str) -> Result<(), Box<dyn std::error::Error>> {
        let span = tracing::span!(target: "solana", Level::INFO, "start");
        let _enter = span.enter();
        
        // Check if already running
        if self.shutdown_tx.is_some() || self.task_handle.is_some() {
            return Err(Box::new(WebSocketError::AlreadyRunning));
        }

        // Create a oneshot channel for shutdown signaling
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        self.shutdown_tx = Some(shutdown_tx);
        let mut shutdown_rx = Box::pin(shutdown_rx);

        let sub = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method.to_string(),
            "params": [
                {"mentions": [mention.to_string()]},
                {"commitment": commitment.to_string()}
            ]
        });

        let (stream, _) = connect_async(&self.url).await?;
        let (mut write, mut read) = stream.split();

        let msg = serde_json::to_string(&sub)?;
        write.send(Message::Text(msg)).await?;

        // Spawn the WebSocket handling task
        let handle = tokio::spawn(async move {
            let mut last_message_time = std::time::Instant::now();
            let mut heartbeat_interval = interval(Duration::from_secs(30));

            loop {
                tokio::select! {
                    // Check for shutdown signal
                    _ = &mut shutdown_rx => {
                        info!("Received shutdown signal, closing WebSocket connection");
                        break;
                    }

                    msg = read.next() => {
                        match msg {
                            Some(Ok(tokio_tungstenite::tungstenite::Message::Text(txt))) => {
                                last_message_time = std::time::Instant::now();
                                if let Ok(v) = serde_json::from_str::<Value>(&txt) {
                                    info!("{}", v);
                                }
                            }
                            Some(Ok(tokio_tungstenite::tungstenite::Message::Pong(_))) => {
                                last_message_time = std::time::Instant::now();
                            }
                            Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) => {
                                warn!("WebSocket connection closed by server");
                                break;
                            }
                            Some(Ok(_)) => {
                                // Other message types (binary, etc.)
                                last_message_time = std::time::Instant::now();
                            }
                            Some(Err(e)) => {
                                error!(error=?e, "read error");
                                break;
                            }
                            None => {
                                warn!("WebSocket stream ended");
                                break;
                            }
                        }
                    }

                    // Send heartbeat pings
                    _ = heartbeat_interval.tick() => {
                        let ping_msg = tokio_tungstenite::tungstenite::Message::Ping(vec![]);
                        if let Err(e) = write.send(ping_msg).await {
                            error!(error=?e, "failed to send ping");
                            break;
                        }
                    }
                    
                    // Check for connection timeout (no messages for 2 minutes)
                    _ = sleep(Duration::from_secs(5)) => {
                        if last_message_time.elapsed() > Duration::from_secs(120) {
                            warn!("no messages received for 2 minutes, considering connection dead");
                            break;
                        }
                    }
                }
            }
        });

        self.task_handle = Some(handle);
        Ok(())
    }

    pub async fn stop(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }

        if let Some(handle) = self.task_handle.take() {
            handle.await?;
        }

        Ok(())
    }
}


// This module is only compiled when running `cargo test`
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;
    use std::time::Duration;
    use assert_matches::assert_matches;

    /// Spawns a mock WebSocket server that listens on a random available port.
    /// It performs a simple handshake:
    /// 1. Expects a subscription message and asserts its content.
    /// 2. Sends one mock notification back to the client.
    /// 3. Responds to Pings with Pongs to keep the connection alive.
    ///
    /// Returns the server's URL and a handle to its task.
    async fn setup_mock_server() -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("ws://{}", addr);

        let server_handle = tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                let mut websocket = accept_async(stream).await.expect("Failed to accept websocket");

                // 1. Expect a specific subscription message from the client
                let msg = websocket.next().await.unwrap().unwrap();
                assert_matches!(msg, Message::Text(_));
                if let Message::Text(txt) = msg {
                    let v: Value = serde_json::from_str(&txt).unwrap();
                    assert_eq!(v["method"], "logsSubscribe");
                    assert_eq!(v["params"][0]["mentions"][0], "MyTestMention");
                    assert_eq!(v["params"][1]["commitment"], "confirmed");
                }

                // 2. Send a mock log message back to the client
                let response = serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "logsNotification",
                    "params": { "result": { "test": "data" }, "subscription": 1 }
                });
                websocket.send(Message::Text(response.to_string())).await.unwrap();
                
                // 3. Loop to handle pings or close signals
                while let Some(Ok(msg)) = websocket.next().await {
                    match msg {
                        Message::Ping(data) => {
                            websocket.send(Message::Pong(data)).await.unwrap();
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
            }
        });

        (url, server_handle)
    }

    #[tokio::test]
    async fn test_new_client_initial_state() {
        let client = SolanaWSClient::new("ws://localhost:8080").await.unwrap();
        assert_eq!(client.url, "ws://localhost:8080");
        assert!(client.shutdown_tx.is_none(), "shutdown_tx should be None initially");
        assert!(client.task_handle.is_none(), "task_handle should be None initially");
    }

    #[tokio::test]
    async fn test_start_and_stop() {
        let (url, server_handle) = setup_mock_server().await;
        let mut client = SolanaWSClient::new(&url).await.unwrap();
        
        // Start the client
        let start_result = client.start("confirmed", "logsSubscribe", "MyTestMention").await;
        assert!(start_result.is_ok());
        assert!(client.shutdown_tx.is_some());
        assert!(client.task_handle.is_some());

        // Give the client and server a moment to interact
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Stop the client
        let stop_result = client.stop().await;
        assert!(stop_result.is_ok());
        assert!(client.shutdown_tx.is_none());
        assert!(client.task_handle.is_none());

        // Ensure the server task finishes cleanly
        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_start_already_running() {
        let (url, server_handle) = setup_mock_server().await;
        let mut client = SolanaWSClient::new(&url).await.unwrap();

        // First start should succeed
        client.start("confirmed", "logsSubscribe", "MyTestMention").await.unwrap();
        assert!(client.task_handle.is_some());

        // Second start should fail with the correct error
        let second_start_result = client.start("confirmed", "logsSubscribe", "MyTestMention").await;
        assert!(second_start_result.is_err());
        let err = second_start_result.unwrap_err();
        
        let ws_error = err.downcast_ref::<WebSocketError>().unwrap();
        assert_matches!(ws_error, WebSocketError::AlreadyRunning);

        // Cleanup
        client.stop().await.unwrap();
        server_handle.await.unwrap();
    }
    
    #[tokio::test]
    async fn test_connection_failure() {
        // Use a port that is guaranteed to be closed
        let url = "ws://127.0.0.1:1"; 
        let mut client = SolanaWSClient::new(&url).await.unwrap();
        
        let result = client.start("confirmed", "logsSubscribe", "MyTestMention").await;
        
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_stop_when_not_running() {
        let mut client = SolanaWSClient::new("ws://dummy-url").await.unwrap();
        // Stopping a client that was never started should be a no-op and succeed
        let result = client.stop().await;
        assert!(result.is_ok());
    }
}