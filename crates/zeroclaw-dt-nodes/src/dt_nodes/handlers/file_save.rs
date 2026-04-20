use crate::dt_nodes::handlers::{Handler, InvokeOutcome};
use base64::Engine;
use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
struct FileSaveParams {
    #[serde(rename = "base64")]
    base64_data: String,
    #[serde(rename = "mimeType")]
    mime_type: String,
    #[serde(rename = "fileName")]
    file_name: String,
}

pub struct FileSaveHandler;

impl FileSaveHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Handler for FileSaveHandler {
    fn handle(&self, params_json: &str) -> InvokeOutcome {
        let trimmed = params_json.trim();
        if trimmed.is_empty() {
            return InvokeOutcome {
                ok: false,
                payload_json: None,
                error: Some(invalid_request("paramsJSON required")),
            };
        }
        let params: FileSaveParams = match serde_json::from_str(trimmed) {
            Ok(p) => p,
            Err(e) => {
                return InvokeOutcome {
                    ok: false,
                    payload_json: None,
                    error: Some(invalid_request(&format!("invalid paramsJSON: {}", e))),
                };
            }
        };
        if params.base64_data.is_empty() {
            return InvokeOutcome {
                ok: false,
                payload_json: None,
                error: Some(invalid_request("base64 required")),
            };
        }
        let data: Vec<u8> = match base64::engine::general_purpose::STANDARD
            .decode(params.base64_data.replace(['\n', '\r'], ""))
        {
            Ok(bytes) => bytes,
            Err(e) => {
                return InvokeOutcome {
                    ok: false,
                    payload_json: None,
                    error: Some(invalid_request(&format!("base64 decode failed: {}", e))),
                };
            }
        };
        let mut filename = sanitize_filename(&params.file_name, &params.mime_type);
        let save_dir = default_save_dir();
        if let Err(e) = fs::create_dir_all(&save_dir) {
            return InvokeOutcome {
                ok: false,
                payload_json: None,
                error: Some(unavailable(&format!("mkdir failed: {}", e))),
            };
        }
        let out_path = save_dir.join(&filename);
        if let Err(e) = fs::write(&out_path, &data) {
            return InvokeOutcome {
                ok: false,
                payload_json: None,
                error: Some(unavailable(&format!("write file failed: {}", e))),
            };
        }
        if let Some(name) = out_path.file_name().and_then(|v| v.to_str()) {
            filename = name.to_string();
        }
        let abs_path = out_path.canonicalize().unwrap_or_else(|_| out_path.clone());
        let payload = serde_json::json!({
            "path": abs_path.to_string_lossy(),
            "bytes": data.len(),
            "mimeType": params.mime_type,
            "fileName": filename,
        });
        InvokeOutcome {
            ok: true,
            payload_json: Some(payload.to_string()),
            error: None,
        }
    }
}

fn invalid_request(msg: &str) -> Value {
    serde_json::json!({ "code": "INVALID_REQUEST", "message": msg })
}

fn unavailable(msg: &str) -> Value {
    serde_json::json!({ "code": "UNAVAILABLE", "message": msg })
}

fn sanitize_filename(file_name: &str, mime_type: &str) -> String {
    let mut filename = file_name.trim().to_string();
    if filename.is_empty() {
        let mut ext = "bin".to_string();
        let mime = mime_type.trim().to_ascii_lowercase();
        if !mime.is_empty() {
            ext = match mime.as_str() {
                "image/jpeg" | "image/jpg" => "jpg".to_string(),
                "image/png" => "png".to_string(),
                "image/gif" => "gif".to_string(),
                "image/webp" => "webp".to_string(),
                _ => {
                    if let Some(idx) = mime.rfind('/') {
                        if idx + 1 < mime.len() {
                            mime[idx + 1..].to_string()
                        } else {
                            "bin".to_string()
                        }
                    } else {
                        "bin".to_string()
                    }
                }
            };
        }
        filename = format!("file.{}", ext);
    }
    let base = Path::new(&filename)
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("file.bin");
    if base.is_empty() || base == "." {
        "file.bin".to_string()
    } else {
        base.to_string()
    }
}

fn default_save_dir() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("saved")
}
