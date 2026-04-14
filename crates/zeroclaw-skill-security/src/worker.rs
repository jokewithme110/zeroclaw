use crate::service::run_scan_cycle;
use anyhow::Result;
use std::future;
use tokio::time::{self, Duration};
use zeroclaw_config::schema::Config;

pub async fn run_skill_scan_worker<F>(config: Config, mut mark_component_ok: F) -> Result<()>
where
    F: FnMut() + Send + 'static,
{
    if config.skills.scan.startup_scan {
        tracing::info!(
            "skill scan startup cycle begin (workspace={}, poll_timeout_secs={})",
            config.workspace_dir.display(),
            config.skills.scan.poll_timeout_secs
        );
        run_skill_scan_cycle(&config).await?;
        tracing::info!("skill scan startup cycle complete");
    }

    if !config.skills.scan.periodic_scan_enabled {
        tracing::info!("Skill scan periodic loop disabled by config");
        future::pending::<()>().await;
    }

    let interval_secs = config.skills.scan.interval_secs.max(1);
    tracing::info!("skill scan periodic loop enabled (interval_secs={interval_secs})");
    let mut interval = time::interval(Duration::from_secs(interval_secs));
    loop {
        interval.tick().await;
        tracing::info!("skill scan periodic cycle begin");
        if let Err(err) = run_skill_scan_cycle(&config).await {
            tracing::warn!("skill scan periodic cycle failed: {err:#}");
        } else {
            mark_component_ok();
            tracing::info!("skill scan periodic cycle complete");
        }
    }
}

async fn run_skill_scan_cycle(config: &Config) -> Result<()> {
    let workspace_dir = config.workspace_dir.clone();
    let scan_cfg = config.skills.scan.clone();
    tokio::task::spawn_blocking(move || run_scan_cycle(&workspace_dir, &scan_cfg))
        .await
        .map_err(|err| anyhow::anyhow!("skill scan worker join error: {err}"))?
}
