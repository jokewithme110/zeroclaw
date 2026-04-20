use crate::dt_nodes::handlers::{Handler, InvokeOutcome, camera_snap, file_save, system_run};
use crate::dt_nodes::node_runtime_trace::NodeTraceCtx;

pub async fn handle_invoke(
    command: &str,
    params_json: &str,
    trace: Option<&NodeTraceCtx<'_>>,
) -> InvokeOutcome {
    tracing::info!(command, "dt_nodes: invoke started");
    let outcome = match command {
        "system.run" => system_run::handle_system_run(params_json, trace).await,
        "media.saveImage" => file_save::FileSaveHandler::new().handle(params_json),
        "camera.snap" => camera_snap::CameraSnapHandler::new().handle(params_json),
        other => InvokeOutcome {
            ok: false,
            payload_json: None,
            error: Some(serde_json::json!({
                "code": "unsupported_command",
                "message": format!("command '{other}' is not implemented on zeroclaw node"),
            })),
        },
    };
    tracing::info!(
        command,
        ok = outcome.ok,
        has_error = outcome.error.is_some(),
        "dt_nodes: invoke finished"
    );
    outcome
}
