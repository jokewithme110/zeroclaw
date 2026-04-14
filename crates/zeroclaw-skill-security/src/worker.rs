use crate::service::run_scan_cycle;
use anyhow::Result;
use std::future;
use tokio::time::{self, Duration};
use zeroclaw_config::schema::Config;
use zeroclaw_log::{Action, Event, EventOutcome, record};

pub async fn run_skill_scan_worker<F>(config: Config, mut mark_component_ok: F) -> Result<()>
where
    F: FnMut() + Send + 'static,
{
    if config.skills.scan.startup_scan {
        run_skill_scan_cycle(&config).await?;
        record!(
            INFO,
            Event::new(module_path!(), Action::Note),
            "skill scan startup cycle complete"
        );
    }

    if !config.skills.scan.periodic_scan_enabled {
        record!(
            INFO,
            Event::new(module_path!(), Action::Note),
            "Skill scan periodic loop disabled by config"
        );
        future::pending::<()>().await;
    }

    let interval_secs = config.skills.scan.interval_secs.max(10);
    record!(
        INFO,
        Event::new(module_path!(), Action::Note),
        &format!("skill scan periodic loop enabled (interval_secs={interval_secs})")
    );
    let mut interval = time::interval(Duration::from_secs(interval_secs));
    loop {
        interval.tick().await;
        record!(
            INFO,
            Event::new(module_path!(), Action::Note),
            "skill scan periodic cycle begin"
        );
        if let Err(err) = run_skill_scan_cycle(&config).await {
            record!(
                WARN,
                Event::new(module_path!(), Action::Note).with_outcome(EventOutcome::Failure),
                &format!("skill scan periodic cycle failed: {err:#}")
            );
        } else {
            mark_component_ok();
            record!(
                INFO,
                Event::new(module_path!(), Action::Note),
                "skill scan periodic cycle complete"
            );
        }
    }
}

async fn run_skill_scan_cycle(config: &Config) -> Result<()> {
    let workspace_dir = config.data_dir.clone();
    let install_root = config.install_root_dir();
    let scan_cfg = config.skills.scan.clone();
    tokio::task::spawn_blocking(move || run_scan_cycle(&workspace_dir, &install_root, &scan_cfg))
        .await
        .map_err(|err| anyhow::Error::msg(format!("skill scan worker join error: {err}")))?
}
