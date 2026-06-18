use crate::dt_nodes::handlers::event_store::EventSubscriptionsStore;
use crate::dt_nodes::handlers::{
    Handler, InvokeOutcome, camera_snap, event_subscribe, event_subscribe_query, event_unsubscribe,
    file_save, system_run,
};
use crate::dt_nodes::node_runtime_trace::NodeTraceCtx;
use std::path::Path;

pub async fn handle_invoke(
    command: &str,
    params_json: &str,
    trace: Option<&NodeTraceCtx<'_>>,
    workspace_dir: Option<&Path>,
    allowed_events: Vec<String>,
) -> InvokeOutcome {
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Start)
            .with_category(::zeroclaw_log::EventCategory::Tool)
            .with_attrs(::serde_json::json!({ "command": command })),
        "dt_nodes: invoke started"
    );

    // Reuse one store instance for the long-lived node process so event
    // handlers do not create separate in-memory views of the same file.
    let event_store =
        workspace_dir.and_then(|dir| EventSubscriptionsStore::global_instance(dir).ok());

    let outcome = match command {
        "system.run" => system_run::handle_system_run(params_json, trace).await,
        "media.saveImage" => {
            let workspace_dir = workspace_dir.map(|p| p.to_path_buf()).unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf())
            });
            file_save::FileSaveHandler::new(workspace_dir).handle(params_json)
        }
        "camera.snap" => camera_snap::CameraSnapHandler::new().handle(params_json),
        "event.subscribe" => {
            event_subscribe::EventSubscribeHandler::new(event_store.cloned(), allowed_events)
                .handle(params_json)
        }
        "event.unsubscribe" => {
            event_unsubscribe::EventUnsubscribeHandler::new(event_store.cloned())
                .handle(params_json)
        }
        "event.subscribe.query" => {
            event_subscribe_query::EventSubscribeListHandler::new(event_store.cloned())
                .handle(params_json)
        }
        other => InvokeOutcome {
            ok: false,
            payload_json: None,
            error: Some(serde_json::json!({
                "code": "unsupported_command",
                "message": format!("command '{other}' is not implemented on zeroclaw node"),
            })),
        },
    };
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)
            .with_category(::zeroclaw_log::EventCategory::Tool)
            .with_outcome(if outcome.ok {
                ::zeroclaw_log::EventOutcome::Success
            } else {
                ::zeroclaw_log::EventOutcome::Failure
            })
            .with_attrs(::serde_json::json!({
                "command": command,
                "ok": outcome.ok,
                "has_error": outcome.error.is_some(),
            })),
        "dt_nodes: invoke finished"
    );
    outcome
}
