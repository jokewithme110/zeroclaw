use super::types::{ScanResult, UploadResult};
use anyhow::{Context, Result, anyhow};
use reqwest::blocking::{Client, multipart};
use serde::Deserialize;
use std::error::Error as StdError;
use std::time::Duration;
use zeroclaw_config::schema::runtime_proxy_config;

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
        let builder = Client::builder()
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(10));
        let client = apply_runtime_proxy_to_blocking_builder(builder, "skills.scan")
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
            .map_err(|err| {
                let hint = network_hint(&self.upload_url, &err);
                anyhow!(
                    "upload scan request failed: {} ({err}){hint}",
                    self.upload_url
                )
            })?;
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
            .map_err(|err| {
                let hint = network_hint(&self.result_url, &err);
                anyhow!(
                    "query scan result failed: {} ({err}){hint}",
                    self.result_url
                )
            })?;
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

fn apply_runtime_proxy_to_blocking_builder(
    mut builder: reqwest::blocking::ClientBuilder,
    service_key: &str,
) -> reqwest::blocking::ClientBuilder {
    let cfg = runtime_proxy_config();
    if !cfg.should_apply_to_service(service_key) {
        return builder;
    }

    let no_proxy = {
        let list = cfg.normalized_no_proxy();
        (!list.is_empty())
            .then(|| list.join(","))
            .and_then(|joined| reqwest::NoProxy::from_string(&joined))
    };

    builder = push_blocking_proxy(builder, cfg.all_proxy.as_deref(), &no_proxy, |u| {
        reqwest::Proxy::all(u)
    });
    builder = push_blocking_proxy(builder, cfg.http_proxy.as_deref(), &no_proxy, |u| {
        reqwest::Proxy::http(u)
    });
    builder = push_blocking_proxy(builder, cfg.https_proxy.as_deref(), &no_proxy, |u| {
        reqwest::Proxy::https(u)
    });
    builder
}

fn push_blocking_proxy<F>(
    builder: reqwest::blocking::ClientBuilder,
    raw: Option<&str>,
    no_proxy: &Option<reqwest::NoProxy>,
    build: F,
) -> reqwest::blocking::ClientBuilder
where
    F: FnOnce(&str) -> Result<reqwest::Proxy, reqwest::Error>,
{
    let Some(url) = normalize_proxy_url(raw) else {
        return builder;
    };
    match build(&url) {
        Ok(proxy) => builder.proxy(proxy.no_proxy(no_proxy.clone())),
        Err(_) => builder,
    }
}

fn normalize_proxy_url(raw: Option<&str>) -> Option<String> {
    let value = raw?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn network_hint(url: &str, err: &reqwest::Error) -> String {
    let mut hints = Vec::new();
    let err_text = error_chain_to_string(err).to_ascii_lowercase();
    if err.is_connect() && err_text.contains("dns") {
        hints.push("DNS resolution failed");
    }
    if cfg!(target_os = "android")
        && (url.contains("localhost") || url.contains("127.0.0.1"))
        && (err.is_connect() || err_text.contains("dns"))
    {
        hints.push("Android emulator cannot resolve host machine localhost; use 10.0.2.2 or device-reachable LAN IP");
    }
    if hints.is_empty() {
        String::new()
    } else {
        format!("; hint: {}", hints.join(" | "))
    }
}

fn error_chain_to_string(err: &reqwest::Error) -> String {
    let mut chain = Vec::new();
    let mut current: Option<&(dyn StdError + 'static)> = Some(err);
    while let Some(e) = current {
        chain.push(e.to_string());
        current = e.source();
    }
    chain.join(": ")
}
