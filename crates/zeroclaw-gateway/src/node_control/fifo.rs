//! FIFO pipe I/O utilities for gateway information publication.

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::Path;
#[cfg(not(unix))]
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
#[cfg(unix)]
use tokio::net::unix::pipe;
use tokio::time::{Duration, timeout};

/// Gateway announcement payload sent to soft-bus via FIFO.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayAnnouncement {
    pub ip: String,
    pub port: u16,
    pub token: String,
    pub timestamp: i64,
}

/// Wait for FIFO pipe to become available.
///
/// The FIFO pipe is expected to be created by the soft-bus middleware.
/// This function only waits for it to appear, it does NOT create it.
pub async fn wait_for_fifo(fifo_path: &str, timeout_secs: u64) -> Result<()> {
    let path = Path::new(fifo_path);

    // Initial check - provide helpful error message if FIFO doesn't exist
    if !path.exists() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Read)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "event": "fifo_pipe_not_found",
                    "path": fifo_path,
                })),
            "FIFO pipe does not exist yet. Waiting for soft-bus middleware to create it..."
        );
    }

    timeout(Duration::from_secs(timeout_secs), async {
        loop {
            // Check if FIFO exists
            if path.exists() {
                // Verify it's actually a FIFO (named pipe)
                #[cfg(unix)]
                {
                    use std::os::unix::fs::FileTypeExt;
                    if let Ok(metadata) = std::fs::metadata(path)
                        && !metadata.file_type().is_fifo()
                    {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Validate
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "path": fifo_path,
                            })),
                            "Path exists but is not a FIFO pipe, waiting..."
                        );
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        continue;
                    }
                }

                // Try to open for writing. On Unix, use Tokio's FIFO sender API so
                // "no reader yet" fails fast with ENXIO instead of blocking open().
                #[cfg(unix)]
                let ready_result = pipe::OpenOptions::new().open_sender(path);
                #[cfg(not(unix))]
                let ready_result = OpenOptions::new().write(true).open(path).await;

                match ready_result {
                    Ok(_) => {
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Connect
                            )
                            .with_attrs(::serde_json::json!({
                                "event": "fifo_pipe_ready",
                                "path": fifo_path,
                            })),
                            "FIFO pipe is ready"
                        );
                        return Ok(());
                    }
                    Err(e) => {
                        // FIFO exists but not ready yet (e.g., no reader)
                        ::zeroclaw_log::record!(
                            TRACE,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Connect
                            )
                            .with_attrs(::serde_json::json!({
                                "path": fifo_path,
                                "error": e.to_string(),
                            })),
                            "FIFO exists but not ready for writing"
                        );
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                }
            } else {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "FIFO pipe {} not ready within {} seconds. \
             Ensure the soft-bus middleware is running and has created the pipe. \
             You can manually create it with: mkfifo {}",
            fifo_path,
            timeout_secs,
            fifo_path
        )
    })?
}

/// Write gateway announcement to FIFO pipe with retries.
pub async fn write_to_fifo(fifo_path: &str, payload: &GatewayAnnouncement) -> Result<()> {
    let json =
        serde_json::to_string(payload).context("Failed to serialize gateway announcement")?;

    #[cfg(unix)]
    {
        let mut file = pipe::OpenOptions::new()
            .open_sender(fifo_path)
            .context(format!("Failed to open FIFO pipe {}", fifo_path))?;

        file.write_all(json.as_bytes())
            .await
            .context("Failed to write to FIFO pipe")?;

        file.flush().await.context("Failed to flush FIFO pipe")?;
    }

    #[cfg(not(unix))]
    {
        let mut file = OpenOptions::new()
            .write(true)
            .open(fifo_path)
            .await
            .context(format!("Failed to open FIFO pipe {}", fifo_path))?;

        file.write_all(json.as_bytes())
            .await
            .context("Failed to write to FIFO pipe")?;

        file.flush().await.context("Failed to flush FIFO pipe")?;
    }

    Ok(())
}

/// Publish gateway information to FIFO pipe with retry logic.
///
/// Parameters:
/// - `fifo_dir`: Directory containing the FIFO pipe (e.g., "/var")
/// - `lan_ip`: Gateway LAN IP address to advertise
/// - `port`: Gateway listening port
/// - `token`: Node auth token (plaintext)
/// - `wait_timeout_secs`: How long to wait for FIFO to appear
/// - `max_retries`: Number of write attempts
/// - `auto_create`: If true, create the FIFO if it doesn't exist
pub async fn publish_gateway_info(
    fifo_dir: &str,
    lan_ip: &str,
    port: u16,
    token: &str,
    wait_timeout_secs: u64,
    max_retries: u32,
    auto_create: bool,
) -> Result<()> {
    let fifo_path = format!("{}/claw_fifo_out", fifo_dir);

    // Auto-create FIFO if it doesn't exist (Unix only)
    if auto_create {
        #[cfg(unix)]
        {
            match create_fifo_if_missing(&fifo_path) {
                Ok(created) => {
                    if created {
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Write
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Success)
                            .with_attrs(::serde_json::json!({
                                "event": "fifo_pipe_created",
                                "path": fifo_path,
                            })),
                            "FIFO pipe created successfully"
                        );
                    } else {
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Read
                            )
                            .with_attrs(::serde_json::json!({
                                "path": fifo_path,
                            })),
                            "FIFO pipe already exists"
                        );
                    }
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "event": "fifo_auto_create_failed",
                                "path": fifo_path,
                                "error": e.to_string(),
                            })),
                        "Failed to auto-create FIFO, will attempt to wait for it"
                    );
                }
            }
        }
        #[cfg(not(unix))]
        {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "event": "fifo_auto_create_unsupported",
                    })),
                "FIFO auto-creation is only supported on Unix systems, will attempt to wait"
            );
        }
    }

    // Wait for FIFO readiness (whether auto-created or pre-existing)
    wait_for_fifo(&fifo_path, wait_timeout_secs).await?;

    // Build payload
    let payload = GatewayAnnouncement {
        ip: lan_ip.to_string(),
        port,
        token: token.to_string(),
        timestamp: current_timestamp_millis(),
    };

    // Attempt to publish with retries
    let mut last_error = None;
    for attempt in 1..=max_retries {
        match write_to_fifo(&fifo_path, &payload).await {
            Ok(()) => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                        .with_outcome(::zeroclaw_log::EventOutcome::Success)
                        .with_attrs(::serde_json::json!({
                            "event": "gateway_info_published",
                            "ip": payload.ip,
                            "port": payload.port,
                        })),
                    "Gateway info published to soft-bus"
                );
                return Ok(());
            }
            Err(e) => {
                if attempt < max_retries {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Retry)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "event": "gateway_info_publish_retry",
                                "attempt": attempt,
                                "error": e.to_string(),
                            })),
                        "Retrying FIFO write"
                    );
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    last_error = Some(e);
                } else {
                    return Err(e);
                }
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Unknown error publishing gateway info")))
}

/// Create a FIFO named pipe if it doesn't exist.
///
/// Returns:
/// - `Ok(true)` if FIFO was created
/// - `Ok(false)` if FIFO already existed
/// - `Err(...)` if creation failed
///
/// This function is called automatically when publishing gateway info.
#[cfg(unix)]
pub fn create_fifo_if_missing(fifo_path: &str) -> Result<bool> {
    use std::fs;

    let path = Path::new(fifo_path);

    // Check if already exists
    if path.exists() {
        // Verify it's a FIFO
        use std::os::unix::fs::FileTypeExt;
        match fs::metadata(path) {
            Ok(metadata) => {
                if metadata.file_type().is_fifo() {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Read)
                            .with_attrs(::serde_json::json!({
                                "path": fifo_path,
                            })),
                        "FIFO already exists"
                    );
                    return Ok(false); // Already exists, not created by us
                } else {
                    anyhow::bail!(
                        "Path {} exists but is not a FIFO pipe (may be a regular file or directory)",
                        fifo_path
                    );
                }
            }
            Err(e) => {
                anyhow::bail!("Failed to check path {}: {}", fifo_path, e);
            }
        }
    }

    // Create parent directory if needed
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent directory for {}", fifo_path))?;
    }

    // Create FIFO using mkfifo command
    use std::process::Command;
    let output = Command::new("mkfifo")
        .arg(path)
        .output()
        .context("Failed to execute mkfifo command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("mkfifo failed: {}", stderr.trim());
    }

    // Set restrictive permissions (owner read/write only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .context("Failed to set FIFO permissions")?;
    }

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write)
            .with_outcome(::zeroclaw_log::EventOutcome::Success)
            .with_attrs(::serde_json::json!({
                "event": "fifo_pipe_created",
                "path": fifo_path,
            })),
        "FIFO pipe created successfully"
    );

    Ok(true) // Successfully created
}

/// Get current timestamp in milliseconds since Unix epoch.
fn current_timestamp_millis() -> i64 {
    Utc::now().timestamp_millis()
}

/// Get LAN IP address (non-loopback IPv4).
///
/// If `intf_name` is provided, returns the IP of that specific interface.
/// Otherwise, returns the first non-virtual IPv4 interface.
pub fn get_lan_ip(intf_name: Option<&str>) -> Option<String> {
    use local_ip_address::list_afinet_netifas;

    let exclude_keywords = ["nbif", "vmnet", "veth", "docker", "br-", "vbox"];

    match list_afinet_netifas() {
        Ok(interfaces) => {
            if let Some(name) = intf_name {
                interfaces
                    .iter()
                    .find(|(iface_name, ip)| iface_name == name && ip.is_ipv4())
                    .map(|(_, ip)| ip.to_string())
            } else {
                interfaces
                    .iter()
                    .find(|(name, ip)| {
                        ip.is_ipv4()
                            && !exclude_keywords.iter().any(|&kw| name.contains(kw))
                            && name != "lo"
                    })
                    .map(|(_, ip)| ip.to_string())
            }
        }
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio::fs::File;
    use tokio::io::AsyncReadExt;

    #[test]
    fn test_get_lan_ip_not_loopback() {
        if let Some(ip) = get_lan_ip(None) {
            assert!(!ip.starts_with("127."), "Should not return loopback IP");
            assert!(
                !ip.starts_with("169.254."),
                "Should not return link-local IP"
            );
        }
        // If no LAN IP found, that's OK for test environment
    }

    #[test]
    fn test_current_timestamp_millis() {
        let now = Utc::now().timestamp_millis();
        let ts = current_timestamp_millis();
        // Should be within 1 second of current time
        assert!((ts - now).abs() < 1000);
    }

    #[tokio::test]
    async fn test_write_to_fifo() {
        let tmp = TempDir::new().unwrap();
        let fifo_path = tmp.path().join("test_fifo");

        // Create FIFO using mkfifo
        #[cfg(unix)]
        {
            use std::process::Command;
            let status = Command::new("mkfifo")
                .arg(&fifo_path)
                .status()
                .expect("Failed to run mkfifo");
            assert!(status.success());
        }

        let payload = GatewayAnnouncement {
            ip: "192.168.1.1".to_string(),
            port: 3000,
            token: "test-token".to_string(),
            timestamp: 1710432000000,
        };

        // Start reader task
        let fifo_path_clone = fifo_path.clone();
        let read_task = tokio::spawn(async move {
            let mut file = File::open(&fifo_path_clone).await.unwrap();
            let mut contents = String::new();
            file.read_to_string(&mut contents).await.unwrap();
            contents
        });

        // Give reader time to open
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Write
        let result = write_to_fifo(fifo_path.to_str().unwrap(), &payload).await;
        assert!(result.is_ok());

        // Verify content
        let contents = read_task.await.unwrap();
        assert!(contents.contains("192.168.1.1"));
        assert!(contents.contains("3000"));
        assert!(contents.contains("test-token"));
    }

    #[tokio::test]
    async fn test_wait_for_fifo_timeout() {
        let tmp = TempDir::new().unwrap();
        let fifo_path = tmp.path().join("nonexistent_fifo");

        let result = wait_for_fifo(fifo_path.to_str().unwrap(), 1).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not ready"));
    }

    #[tokio::test]
    async fn test_wait_for_fifo_existing_pipe_without_reader_times_out_promptly() {
        let tmp = TempDir::new().unwrap();
        let fifo_path = tmp.path().join("existing_fifo_no_reader");

        #[cfg(unix)]
        {
            use std::process::Command;
            let status = Command::new("mkfifo")
                .arg(&fifo_path)
                .status()
                .expect("Failed to run mkfifo");
            assert!(status.success());
        }

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            wait_for_fifo(fifo_path.to_str().unwrap(), 1),
        )
        .await
        .expect("wait_for_fifo should not hang when the FIFO has no reader");

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_write_to_fifo_without_reader_fails_promptly() {
        let tmp = TempDir::new().unwrap();
        let fifo_path = tmp.path().join("write_fifo_no_reader");

        #[cfg(unix)]
        {
            use std::process::Command;
            let status = Command::new("mkfifo")
                .arg(&fifo_path)
                .status()
                .expect("Failed to run mkfifo");
            assert!(status.success());
        }

        let payload = GatewayAnnouncement {
            ip: "192.168.1.1".to_string(),
            port: 3000,
            token: "test-token".to_string(),
            timestamp: 1710432000000,
        };

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            write_to_fifo(fifo_path.to_str().unwrap(), &payload),
        )
        .await
        .expect("write_to_fifo should not hang when the FIFO has no reader");

        assert!(result.is_err());
    }
}
