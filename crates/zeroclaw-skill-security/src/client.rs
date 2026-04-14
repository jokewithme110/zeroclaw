use super::types::{ScanResult, UploadResult};
use anyhow::{Context, Result, anyhow};
use reqwest::blocking::{Client, multipart};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct UploadEnvelope {
    code: i64,
    message: Option<String>,
    data: Option<UploadData>,
}

#[derive(Debug, Deserialize)]
struct UploadData {
    task_no: String,
    file_sha256: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResultEnvelope {
    code: i64,
    message: Option<String>,
    data: Option<ResultData>,
}

#[derive(Debug, Deserialize)]
struct ResultData {
    status: i32,
    status_text: Option<String>,
    is_safe: Option<bool>,
    max_severity: Option<String>,
    file_sha256: Option<String>,
    analysis_level: Option<String>,
    analysis_reason: Option<String>,
    analysis_suggestion: Option<String>,
}

pub struct SkillScanClient {
    client: Client,
    upload_url: String,
    result_url: String,
}

impl SkillScanClient {
    pub fn new(upload_url: String, result_url: String) -> Result<Self> {
        let client = Client::builder()
            .build()
            .context("failed to build scan HTTP client")?;
        Ok(Self {
            client,
            upload_url,
            result_url,
        })
    }

    pub fn upload_archive(&self, archive_bytes: Vec<u8>, filename: &str) -> Result<UploadResult> {
        let part = multipart::Part::bytes(archive_bytes).file_name(filename.to_string());
        let form = multipart::Form::new().part("file", part);
        let resp = self
            .client
            .post(&self.upload_url)
            .multipart(form)
            .send()
            .with_context(|| format!("upload scan request failed: {}", self.upload_url))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow!("upload scan request failed with HTTP {status}"));
        }
        let body: UploadEnvelope = resp
            .json()
            .context("failed to decode upload scan response")?;
        if body.code != 0 {
            return Err(anyhow!(
                "upload scan API returned code {} ({})",
                body.code,
                body.message.unwrap_or_else(|| "unknown".to_string())
            ));
        }
        let data = body
            .data
            .ok_or_else(|| anyhow!("upload scan response missing data"))?;
        Ok(UploadResult {
            task_no: data.task_no,
            file_sha256: data.file_sha256,
        })
    }

    pub fn query_result(&self, task_no: &str) -> Result<ScanResult> {
        let resp = self
            .client
            .get(&self.result_url)
            .query(&[("task_no", task_no)])
            .send()
            .with_context(|| format!("query scan result failed: {}", self.result_url))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow!("query scan result failed with HTTP {status}"));
        }
        let body: ResultEnvelope = resp
            .json()
            .context("failed to decode scan result response")?;
        if body.code != 0 {
            return Err(anyhow!(
                "scan result API returned code {} ({})",
                body.code,
                body.message.unwrap_or_else(|| "unknown".to_string())
            ));
        }
        let data = body
            .data
            .ok_or_else(|| anyhow!("scan result response missing data"))?;
        Ok(ScanResult {
            status: data.status,
            status_text: data
                .status_text
                .unwrap_or_else(|| "unknown".to_string())
                .to_ascii_lowercase(),
            is_safe: data.is_safe,
            max_severity: data.max_severity,
            file_sha256: data.file_sha256,
            analysis_level: data.analysis_level,
            analysis_reason: data.analysis_reason,
            analysis_suggestion: data.analysis_suggestion,
        })
    }
}
