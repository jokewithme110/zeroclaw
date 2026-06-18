//! FIFO-based gateway auto-discovery for node client.
//!
//! This module listens on a FIFO named pipe (`/var/claw_fifo_in`) for gateway
//! information published by the soft-bus middleware, and automatically connects
//! to the discovered gateway.

use anyhow::{Context, Result};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tokio::io::AsyncReadExt;
use tokio::io::BufReader;
use tokio::net::unix::pipe;
use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;

/// Gateway information received via FIFO pipe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayInfo {
    pub ip: String,
    pub port: u16,
    pub token: String,
    pub timestamp: i64,
}

impl GatewayInfo {
    /// Validate the gateway info payload.
    pub fn validate(&self) -> Result<()> {
        // Validate IP (basic check)
        if self.ip.trim().is_empty() {
            anyhow::bail!("Invalid gateway IP: empty");
        }

        // Exclude loopback and link-local
        // if self.ip.starts_with("127.") || self.ip.starts_with("169.254.") {
        //     anyhow::bail!("Invalid gateway IP: {}", self.ip);
        // }

        // Validate port
        if self.port == 0 {
            anyhow::bail!("Invalid gateway port: 0");
        }

        // Validate token
        if self.token.trim().is_empty() {
            anyhow::bail!("Invalid gateway token: empty");
        }
        Ok(())
    }
}

/// Listen on FIFO pipe for gateway information.
///
/// This function blocks until gateway info is received or timeout occurs.
pub async fn listen_for_gateway_info(fifo_dir: &str, timeout_secs: u64) -> Result<GatewayInfo> {
    let fifo_path = Path::new(fifo_dir).join("claw_fifo_in");

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({
                "event": "node_fifo_listener_starting",
                "path": fifo_path.display().to_string(),
                "timeout_secs": timeout_secs,
            })
        ),
        "Starting FIFO listener for gateway discovery"
    );

    // Create FIFO if it doesn't exist (Unix only)
    #[cfg(unix)]
    {
        create_fifo_if_missing(&fifo_path)?;
    }

    // Wait for data with timeout
    let result = timeout(
        Duration::from_secs(timeout_secs),
        read_from_fifo(&fifo_path),
    )
    .await;

    match result {
        Ok(Ok(info)) => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "event": "node_gateway_discovered",
                        "ip": info.ip.as_str(),
                        "port": info.port,
                    })),
                "Gateway discovered via FIFO"
            );
            Ok(info)
        }
        Ok(Err(e)) => {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "event": "node_fifo_read_failed",
                        "error": e.to_string(),
                    })),
                "Failed to read from FIFO"
            );
            Err(e)
        }
        Err(_) => {
            let err = anyhow::anyhow!("FIFO listener timeout after {} seconds", timeout_secs);
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "event": "node_fifo_timeout",
                        "path": fifo_path.display().to_string(),
                        "error": err.to_string(),
                    })),
                "FIFO listener timeout"
            );
            Err(err)
        }
    }
}

/// Read gateway info from FIFO pipe.
async fn read_from_fifo(fifo_path: &Path) -> Result<GatewayInfo> {
    // Open FIFO in non-blocking mode so Ctrl-C can cancel the pending read.
    ::zeroclaw_log::record!(
        DEBUG,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({
                "path": fifo_path.display().to_string(),
            })
        ),
        "Opening FIFO receiver for reading"
    );

    let receiver = pipe::OpenOptions::new()
        .open_receiver(fifo_path)
        .context(format!("Failed to open FIFO {}", fifo_path.display()))?;

    let mut reader = BufReader::new(receiver);
    let mut contents = String::new();

    // Read until EOF (writer closes pipe)
    reader
        .read_to_string(&mut contents)
        .await
        .context("Failed to read from FIFO")?;

    ::zeroclaw_log::record!(
        DEBUG,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({
                "path": fifo_path.display().to_string(),
                "bytes_read": contents.len(),
                "raw_data": contents.as_str(),
            })
        ),
        "Data received from FIFO"
    );

    // Parse JSON (trim whitespace/newlines and null terminators from C++ strings)
    let cleaned = contents.trim().split('\0').next().unwrap_or("");
    ::zeroclaw_log::record!(
        DEBUG,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({
                "cleaned_data": cleaned,
            })
        ),
        "Parsing JSON"
    );
    let info: GatewayInfo =
        serde_json::from_str(cleaned).context("Failed to parse gateway info JSON")?;

    // Validate payload
    info.validate()?;

    Ok(info)
}

/// Create FIFO pipe if it doesn't exist (Unix only).
#[cfg(unix)]
fn create_fifo_if_missing(fifo_path: &Path) -> Result<()> {
    use std::fs;
    use std::os::unix::fs::FileTypeExt;

    // Check if already exists
    if fifo_path.exists() {
        match fs::metadata(fifo_path) {
            Ok(metadata) => {
                if metadata.file_type().is_fifo() {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({
                                "path": fifo_path.display().to_string(),
                            })),
                        "FIFO already exists"
                    );
                    return Ok(());
                } else {
                    anyhow::bail!("Path {} exists but is not a FIFO pipe", fifo_path.display());
                }
            }
            Err(e) => {
                anyhow::bail!("Failed to check path {}: {}", fifo_path.display(), e);
            }
        }
    }

    // Create parent directory if needed
    if let Some(parent) = fifo_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create parent directory for {}",
                    fifo_path.display()
                )
            })?;
        }
    }

    // Create FIFO using mkfifo command
    use std::process::Command;
    let output = Command::new("mkfifo")
        .arg(fifo_path)
        .output()
        .context("Failed to execute mkfifo command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("mkfifo failed: {}", stderr.trim());
    }

    // Set permissions (owner read/write, others read-only for soft-bus)
    fs::set_permissions(fifo_path, fs::Permissions::from_mode(0o640))
        .context("Failed to set FIFO permissions")?;

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({
                "event": "node_fifo_created",
                "path": fifo_path.display().to_string(),
            })
        ),
        "FIFO pipe created for gateway discovery"
    );

    Ok(())
}

/// Continuous listener with reconnection logic.
///
/// This function will:
/// 1. Listen for gateway info via FIFO
/// 2. Call the provided handler with the gateway info
/// 3. If connection fails, retry after backoff
/// 4. If FIFO closes, re-listen for new gateway info
/// 5. Stops when cancellation token is triggered
pub async fn run_fifo_listener<F, Fut>(
    fifo_dir: &str,
    timeout_secs: u64,
    cancel_token: CancellationToken,
    mut handler: F,
) -> Result<()>
where
    F: FnMut(GatewayInfo) -> Fut + Send,
    Fut: std::future::Future<Output = Result<()>> + Send,
{
    let mut attempt = 0;
    let max_backoff = Duration::from_secs(300); // 5 minutes

    loop {
        attempt += 1;

        // Check for cancellation at start of each iteration
        if cancel_token.is_cancelled() {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "event": "node_fifo_listener_stopped",
                    })),
                "FIFO listener stopped by cancellation"
            );
            return Ok(());
        }

        // Listen for gateway info with cancellation support
        let listen_result = tokio::select! {
            _ = cancel_token.cancelled() => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({
                            "event": "node_fifo_listener_cancelled",
                        })),
                    "FIFO listener cancelled during wait"
                );
                return Ok(());
            }
            result = listen_for_gateway_info(fifo_dir, timeout_secs) => {
                result
            }
        };

        match listen_result {
            Ok(gateway_info) => {
                // Reset attempt counter on successful discovery
                attempt = 0;

                // Try to connect
                match handler(gateway_info.clone()).await {
                    Ok(()) => {
                        // Connection succeeded and completed normally
                        // This shouldn't happen in normal operation (WS should stay connected)
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "event": "node_connection_completed",
                            })),
                            "WebSocket connection completed normally - reconnecting"
                        );
                    }
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "event": "node_connection_failed",
                                "error": e.to_string(),
                                "ip": gateway_info.ip.as_str(),
                                "port": gateway_info.port,
                            })),
                            "Connection to gateway failed"
                        );
                    }
                }

                // Calculate backoff (exponential with jitter)
                let backoff = calculate_backoff(attempt, max_backoff);
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({
                            "event": "node_reconnecting",
                            "attempt": attempt,
                            "backoff_secs": backoff.as_secs_f64(),
                        })),
                    "Waiting before reconnection attempt"
                );

                // Sleep with cancellation check
                tokio::select! {
                    _ = cancel_token.cancelled() => {
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                .with_attrs(::serde_json::json!({
                                    "event": "node_reconnect_cancelled",
                                })),
                            "Reconnect cancelled during backoff"
                        );
                        return Ok(());
                    }
                    _ = tokio::time::sleep(backoff) => {}
                }
            }
            Err(e) => {
                // FIFO listen failed or timed out
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "event": "node_fifo_listen_failed",
                            "error": e.to_string(),
                        })),
                    "FIFO listener failed, retrying"
                );

                // Calculate backoff
                let backoff = calculate_backoff(attempt, max_backoff);

                // Sleep with cancellation check
                tokio::select! {
                    _ = cancel_token.cancelled() => {
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                .with_attrs(::serde_json::json!({
                                    "event": "node_retry_cancelled",
                                })),
                            "Retry cancelled during backoff"
                        );
                        return Ok(());
                    }
                    _ = tokio::time::sleep(backoff) => {}
                }
            }
        }
    }
}

/// Calculate exponential backoff with jitter.
fn calculate_backoff(attempt: u32, max_backoff: Duration) -> Duration {
    // Exponential backoff: 2^attempt seconds
    let base_secs = 2u64.pow(attempt.min(8)); // Cap at 2^8 = 256 seconds
    let backoff = Duration::from_secs(base_secs);

    // Add jitter: ±25% randomization (range: 0.75 to 1.25)
    let mut rng = rand::rng();
    let jitter_factor = rng.random_range(0.75..1.25);
    let jittered = backoff.mul_f32(jitter_factor);

    // Clamp to max
    jittered.min(max_backoff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio::fs::File;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn test_gateway_info_validation() {
        let valid_info = GatewayInfo {
            ip: "192.168.1.1".to_string(),
            port: 3000,
            token: "zc_test".to_string(),
            timestamp: chrono::Utc::now().timestamp_millis(),
        };
        assert!(valid_info.validate().is_ok());

        let invalid_ip = GatewayInfo {
            ip: String::new(),
            port: 3000,
            token: "zc_test".to_string(),
            timestamp: chrono::Utc::now().timestamp_millis(),
        };
        assert!(invalid_ip.validate().is_err());

        let invalid_port = GatewayInfo {
            ip: "192.168.1.1".to_string(),
            port: 0,
            token: "zc_test".to_string(),
            timestamp: chrono::Utc::now().timestamp_millis(),
        };
        assert!(invalid_port.validate().is_err());

        let invalid_token = GatewayInfo {
            ip: "192.168.1.1".to_string(),
            port: 3000,
            token: String::new(),
            timestamp: chrono::Utc::now().timestamp_millis(),
        };
        assert!(invalid_token.validate().is_err());
    }

    #[tokio::test]
    async fn test_fifo_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let fifo_path = tmp.path().join("claw_fifo_in");

        // Create FIFO
        #[cfg(unix)]
        {
            use std::process::Command;
            Command::new("mkfifo").arg(&fifo_path).status().unwrap();
        }

        let expected_info = GatewayInfo {
            ip: "192.168.1.100".to_string(),
            port: 42618,
            token: "zc_test_token".to_string(),
            timestamp: chrono::Utc::now().timestamp_millis(),
        };

        // Start reader
        let fifo_path_clone = fifo_path.clone();
        let read_task = tokio::spawn(async move { read_from_fifo(&fifo_path_clone).await });

        // Give reader time to open
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Write
        let mut file = File::create(&fifo_path).await.unwrap();
        let json = serde_json::to_string(&expected_info).unwrap();
        file.write_all(json.as_bytes()).await.unwrap();
        drop(file); // Close to signal EOF

        // Verify
        let received_info = read_task.await.unwrap().unwrap();
        assert_eq!(received_info.ip, expected_info.ip);
        assert_eq!(received_info.port, expected_info.port);
        assert_eq!(received_info.token, expected_info.token);
    }

    #[test]
    fn test_backoff_calculation() {
        let max_backoff = Duration::from_secs(300);

        // First attempt should be short
        let backoff1 = calculate_backoff(1, max_backoff);
        assert!(backoff1 >= Duration::from_secs(1));
        assert!(backoff1 <= Duration::from_secs(10));

        // Higher attempts should have longer backoff
        let backoff5 = calculate_backoff(5, max_backoff);
        assert!(backoff5 > backoff1);

        // Should be capped at max
        let backoff100 = calculate_backoff(100, max_backoff);
        assert!(backoff100 <= max_backoff);
    }

    #[tokio::test]
    async fn test_fifo_listener_cancellation_during_idle_wait() {
        let tmp = TempDir::new().unwrap();
        let fifo_dir = tmp.path().to_string_lossy().to_string();
        let cancel_token = CancellationToken::new();
        let cancel_clone = cancel_token.clone();

        let listener = tokio::spawn(async move {
            run_fifo_listener(&fifo_dir, 300, cancel_token, |_gateway_info| async {
                Ok(())
            })
            .await
        });

        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel_clone.cancel();

        let result = tokio::time::timeout(Duration::from_secs(1), listener)
            .await
            .expect("listener should stop promptly after cancellation")
            .expect("listener task should join successfully");

        assert!(result.is_ok());
    }
}
