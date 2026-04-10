//! HTTP-based tool derived from a skill's `[[tools]]` section.
//!
//! Each `SkillTool` with `kind = "http"` is converted into a `SkillHttpTool`
//! that implements the `Tool` trait. The command field is used as the URL
//! template and args are substituted as query parameters or path segments.

use async_trait::async_trait;
use std::collections::HashMap;
use std::time::Duration;
use zeroclaw_api::tool::{Tool, ToolResult};

/// Maximum response body size (1 MB).
const MAX_RESPONSE_BYTES: usize = 1_048_576;
/// HTTP request timeout (seconds).
const HTTP_TIMEOUT_SECS: u64 = 30;

/// A tool derived from a skill's `[[tools]]` section that makes HTTP requests.
pub struct SkillHttpTool {
    tool_name: String,
    tool_description: String,
    url_template: String,
    args: HashMap<String, String>,
    method: Option<String>,
    headers: HashMap<String, String>,
    body: Option<serde_json::Value>,
}

impl SkillHttpTool {
    /// Create a new skill HTTP tool.
    ///
    /// The tool name is prefixed with the skill name (`skill_name.tool_name`)
    /// to prevent collisions with built-in tools.
    pub fn new(skill_name: &str, tool: &crate::skills::SkillTool) -> Self {
        Self {
            tool_name: format!("{}.{}", skill_name, tool.name),
            tool_description: tool.description.clone(),
            url_template: tool.command.clone(),
            args: tool.args.clone(),
            method: tool.method.clone(),
            headers: tool.headers.clone(),
            body: tool.body.clone(),
        }
    }

    fn build_parameters_schema(&self) -> serde_json::Value {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();

        for (name, description) in &self.args {
            properties.insert(
                name.clone(),
                serde_json::json!({
                    "type": "string",
                    "description": description
                }),
            );
            required.push(serde_json::Value::String(name.clone()));
        }

        serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": required
        })
    }

    fn value_to_placeholder_text(value: &serde_json::Value) -> String {
        if let Some(text) = value.as_str() {
            return text.to_string();
        }
        if value.is_null() {
            return String::new();
        }
        value.to_string()
    }

    /// Substitute `{{arg_name}}` placeholders in template strings.
    fn substitute_template(template: &str, args: &serde_json::Value) -> String {
        let mut output = template.to_string();
        if let Some(obj) = args.as_object() {
            for (key, value) in obj {
                let placeholder = format!("{{{{{}}}}}", key);
                let replacement = Self::value_to_placeholder_text(value);
                output = output.replace(&placeholder, &replacement);
            }
        }
        output
    }

    fn substitute_body_templates(
        body: &serde_json::Value,
        args: &serde_json::Value,
    ) -> serde_json::Value {
        match body {
            serde_json::Value::String(s) => {
                serde_json::Value::String(Self::substitute_template(s, args))
            }
            serde_json::Value::Array(items) => serde_json::Value::Array(
                items
                    .iter()
                    .map(|item| Self::substitute_body_templates(item, args))
                    .collect(),
            ),
            serde_json::Value::Object(obj) => {
                let mut mapped = serde_json::Map::with_capacity(obj.len());
                for (k, v) in obj {
                    mapped.insert(k.clone(), Self::substitute_body_templates(v, args));
                }
                serde_json::Value::Object(mapped)
            }
            _ => body.clone(),
        }
    }

    fn parse_method(&self, args: &serde_json::Value) -> anyhow::Result<reqwest::Method> {
        let configured = self
            .method
            .as_deref()
            .map(|m| Self::substitute_template(m, args))
            .unwrap_or_else(|| "GET".to_string());
        let method = args
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or(&configured)
            .to_uppercase();

        match method.as_str() {
            "GET" => Ok(reqwest::Method::GET),
            "POST" => Ok(reqwest::Method::POST),
            "PUT" => Ok(reqwest::Method::PUT),
            "DELETE" => Ok(reqwest::Method::DELETE),
            "PATCH" => Ok(reqwest::Method::PATCH),
            "HEAD" => Ok(reqwest::Method::HEAD),
            "OPTIONS" => Ok(reqwest::Method::OPTIONS),
            _ => anyhow::bail!(
                "Unsupported HTTP method: {}. Supported: GET, POST, PUT, DELETE, PATCH, HEAD, OPTIONS",
                method
            ),
        }
    }

    fn parse_headers(&self, args: &serde_json::Value) -> anyhow::Result<Vec<(String, String)>> {
        let mut headers = self
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), Self::substitute_template(v, args)))
            .collect::<HashMap<_, _>>();

        let Some(runtime_headers) = args.get("headers") else {
            return Ok(headers.into_iter().collect());
        };

        let Some(obj) = runtime_headers.as_object() else {
            anyhow::bail!("'headers' must be an object of string key-value pairs");
        };

        for (key, value) in obj {
            let Some(val) = value.as_str() else {
                anyhow::bail!("Header '{}' must be a string", key);
            };
            headers.insert(key.clone(), val.to_string());
        }

        Ok(headers.into_iter().collect())
    }

    fn parse_body(&self, args: &serde_json::Value) -> anyhow::Result<Option<String>> {
        let runtime_body = args.get("body");
        let skill_body = self.body.as_ref();
        let Some(body) = runtime_body.or(skill_body) else {
            return Ok(None);
        };

        if body.is_null() {
            return Ok(None);
        }

        let resolved = Self::substitute_body_templates(body, args);

        if let Some(text) = resolved.as_str() {
            return Ok(Some(text.to_string()));
        }

        if resolved.is_object() || resolved.is_array() {
            return Ok(Some(serde_json::to_string(&resolved)?));
        }

        anyhow::bail!("'body' must be a string, object, or array");
    }
}

#[async_trait]
impl Tool for SkillHttpTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        &self.tool_description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.build_parameters_schema()
    }

    fn is_skill_derived_tool(&self) -> bool {
        true
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let url = Self::substitute_template(&self.url_template, &args);

        // Validate URL scheme
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Only http:// and https:// URLs are allowed, got: {url}"
                )),
            });
        }

        let method = match self.parse_method(&args) {
            Ok(m) => m,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e.to_string()),
                });
            }
        };

        let headers = match self.parse_headers(&args) {
            Ok(h) => h,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e.to_string()),
                });
            }
        };

        let body = match self.parse_body(&args) {
            Ok(b) => b,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e.to_string()),
                });
            }
        };

        let args_log =
            serde_json::to_string(&args).unwrap_or_else(|e| format!("<serialize args: {e}>"));
        tracing::info!(
            tool = %self.tool_name,
            http_method = %method,
            url = %url,
            args = %args_log,
            has_request_body = body.is_some(),
            "skill_http: executing HTTP request"
        );

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build HTTP client: {e}"))?;

        let mut request = client.request(method, &url);
        for (key, value) in headers {
            request = request.header(&key, &value);
        }
        if let Some(body) = body {
            request = request.body(body);
        }

        let response = match request.send().await {
            Ok(resp) => resp,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("HTTP request failed: {e}")),
                });
            }
        };

        let status = response.status();
        let body = match response.bytes().await {
            Ok(bytes) => {
                let mut text = String::from_utf8_lossy(&bytes).to_string();
                if text.len() > MAX_RESPONSE_BYTES {
                    let mut b = MAX_RESPONSE_BYTES.min(text.len());
                    while b > 0 && !text.is_char_boundary(b) {
                        b -= 1;
                    }
                    text.truncate(b);
                    text.push_str("\n... [response truncated at 1MB]");
                }
                text
            }
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to read response body: {e}")),
                });
            }
        };

        Ok(ToolResult {
            success: status.is_success(),
            output: body,
            error: if status.is_success() {
                None
            } else {
                Some(format!("HTTP {}", status))
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::SkillTool;

    fn sample_http_tool() -> SkillTool {
        let mut args = HashMap::new();
        args.insert("city".to_string(), "City name to look up".to_string());

        SkillTool {
            name: "get_weather".to_string(),
            description: "Fetch weather for a city".to_string(),
            kind: "http".to_string(),
            command: "https://api.example.com/weather?city={{city}}".to_string(),
            args,
            method: None,
            headers: HashMap::new(),
            body: None,
        }
    }

    #[test]
    fn skill_http_tool_name_is_prefixed() {
        let tool = SkillHttpTool::new("weather_skill", &sample_http_tool());
        assert_eq!(tool.name(), "weather_skill.get_weather");
    }

    #[test]
    fn skill_http_tool_description() {
        let tool = SkillHttpTool::new("weather_skill", &sample_http_tool());
        assert_eq!(tool.description(), "Fetch weather for a city");
    }

    #[test]
    fn skill_http_tool_parameters_schema() {
        let tool = SkillHttpTool::new("weather_skill", &sample_http_tool());
        let schema = tool.parameters_schema();

        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["city"].is_object());
        assert_eq!(schema["properties"]["city"]["type"], "string");
    }

    #[test]
    fn skill_http_tool_substitute_args() {
        let tool = SkillHttpTool::new("weather_skill", &sample_http_tool());
        let result = SkillHttpTool::substitute_template(
            &tool.url_template,
            &serde_json::json!({"city": "London"}),
        );
        assert_eq!(result, "https://api.example.com/weather?city=London");
    }

    #[test]
    fn skill_http_tool_spec_roundtrip() {
        let tool = SkillHttpTool::new("weather_skill", &sample_http_tool());
        let spec = tool.spec();
        assert_eq!(spec.name, "weather_skill.get_weather");
        assert_eq!(spec.description, "Fetch weather for a city");
        assert_eq!(spec.parameters["type"], "object");
    }

    #[test]
    fn skill_http_tool_empty_args() {
        let st = SkillTool {
            name: "ping".to_string(),
            description: "Ping endpoint".to_string(),
            kind: "http".to_string(),
            command: "https://api.example.com/ping".to_string(),
            args: HashMap::new(),
            method: None,
            headers: HashMap::new(),
            body: None,
        };
        let tool = SkillHttpTool::new("s", &st);
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["method"].is_object());
        assert!(schema["properties"]["headers"].is_object());
        assert!(schema["properties"]["body"].is_object());
    }

    #[test]
    fn skill_http_tool_schema_includes_http_fields() {
        let tool = SkillHttpTool::new("weather_skill", &sample_http_tool());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["method"].is_object());
        assert!(schema["properties"]["headers"].is_object());
        assert!(schema["properties"]["body"].is_object());
    }

    #[test]
    fn parse_method_defaults_to_get() {
        let tool = SkillHttpTool::new("weather_skill", &sample_http_tool());
        let method = tool.parse_method(&serde_json::json!({})).unwrap();
        assert_eq!(method, reqwest::Method::GET);
    }

    #[test]
    fn parse_method_accepts_post() {
        let tool = SkillHttpTool::new("weather_skill", &sample_http_tool());
        let method = tool
            .parse_method(&serde_json::json!({"method": "post"}))
            .unwrap();
        assert_eq!(method, reqwest::Method::POST);
    }

    #[test]
    fn parse_method_rejects_invalid() {
        let tool = SkillHttpTool::new("weather_skill", &sample_http_tool());
        let err = tool
            .parse_method(&serde_json::json!({"method": "TRACE"}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("Unsupported HTTP method"));
    }

    #[test]
    fn parse_headers_accepts_string_values() {
        let tool = SkillHttpTool::new("weather_skill", &sample_http_tool());
        let headers = tool
            .parse_headers(&serde_json::json!({
                "headers": {
                    "Authorization": "Bearer token",
                    "Content-Type": "application/json"
                }
            }))
            .unwrap();
        assert_eq!(headers.len(), 2);
    }

    #[test]
    fn parse_headers_rejects_non_object() {
        let tool = SkillHttpTool::new("weather_skill", &sample_http_tool());
        let err = tool
            .parse_headers(&serde_json::json!({"headers": "x"}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("must be an object"));
    }

    #[test]
    fn parse_body_accepts_json_object() {
        let tool = SkillHttpTool::new("weather_skill", &sample_http_tool());
        let body = tool
            .parse_body(&serde_json::json!({
                "body": {"city": "London", "unit": "metric"}
            }))
            .unwrap()
            .unwrap();
        assert!(body.contains("\"city\":\"London\""));
    }

    #[test]
    fn defaults_support_placeholder_substitution() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer {{token}}".to_string());
        let st = SkillTool {
            name: "create".to_string(),
            description: "Create item".to_string(),
            kind: "http".to_string(),
            command: "https://api.example.com/{{tenant}}/items".to_string(),
            args: HashMap::new(),
            method: Some("{{verb}}".to_string()),
            headers,
            body: Some(serde_json::json!({
                "name": "{{name}}",
                "meta": {"source": "{{source}}"}
            })),
        };
        let tool = SkillHttpTool::new("s", &st);
        let args = serde_json::json!({
            "tenant": "acme",
            "token": "abc123",
            "verb": "post",
            "name": "demo",
            "source": "skill"
        });
        let url = SkillHttpTool::substitute_template(&tool.url_template, &args);
        assert_eq!(url, "https://api.example.com/acme/items");
        let method = tool.parse_method(&args).unwrap();
        assert_eq!(method, reqwest::Method::POST);
        let parsed_headers = tool.parse_headers(&args).unwrap();
        assert!(
            parsed_headers
                .iter()
                .any(|(k, v)| k == "Authorization" && v == "Bearer abc123")
        );
        let body = tool.parse_body(&args).unwrap().unwrap();
        assert!(body.contains("\"name\":\"demo\""));
        assert!(body.contains("\"source\":\"skill\""));
    }
}
