use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::types::GraphQlResponse;

/// Base URLs for Rivian's GraphQL endpoints
pub const GATEWAY_URL: &str = "https://rivian.com/api/gql/gateway/graphql";
/// Vehicle state and other authenticated queries go through the same gateway
pub const API_URL: &str = "https://rivian.com/api/gql/gateway/graphql";
pub const CHARGING_URL: &str = "https://rivian.com/api/gql/chrg/user/graphql";
pub const ORDERS_URL: &str = "https://rivian.com/api/gql/orders/graphql";
pub const CONTENT_URL: &str = "https://rivian.com/api/gql/content/graphql";
pub const T2D_URL: &str = "https://rivian.com/api/gql/t2d/graphql";

/// A log entry emitted by the client for each request
#[derive(Debug, Clone)]
pub struct RequestLog {
    pub operation: String,
    pub status: Option<u16>,
    pub duration_ms: u128,
    pub error: Option<String>,
    /// Full request body (only populated in debug mode)
    pub request_body: Option<String>,
    /// Full response body (only populated in debug mode)
    pub response_body: Option<String>,
    /// Request headers (only populated in debug mode)
    pub request_headers: Option<String>,
}

/// HTTP client for Rivian's GraphQL API
#[derive(Debug, Clone)]
pub struct RivianClient {
    http: reqwest::Client,
    debug: bool,
    log_tx: Option<mpsc::UnboundedSender<RequestLog>>,
}

impl RivianClient {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .gzip(true)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self {
            http,
            debug: false,
            log_tx: None,
        })
    }

    /// Enable debug mode (logs full request/response bodies)
    pub fn with_debug(mut self, debug: bool) -> Self {
        self.debug = debug;
        self
    }

    /// Attach a log channel for request tracing
    pub fn with_logger(mut self, tx: mpsc::UnboundedSender<RequestLog>) -> Self {
        self.log_tx = Some(tx);
        self
    }

    /// Default headers that mimic the iOS Rivian app
    fn default_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            "User-Agent",
            HeaderValue::from_static("RivianApp/1304 CFNetwork/1404.0.5 Darwin/22.3.0"),
        );
        headers.insert("Accept", HeaderValue::from_static("application/json"));
        headers.insert("Content-Type", HeaderValue::from_static("application/json"));
        headers.insert("Accept-Language", HeaderValue::from_static("en-US"));
        headers.insert(
            "Apollographql-Client-Name",
            HeaderValue::from_static("com.rivian.ios.consumer-apollo-ios"),
        );
        headers
    }

    fn emit_log(&self, log: RequestLog) {
        if let Some(tx) = &self.log_tx {
            let _ = tx.send(log);
        }
    }

    fn format_headers(headers: &HeaderMap) -> String {
        headers
            .iter()
            .map(|(k, v)| {
                let val = v.to_str().unwrap_or("<binary>");
                // Mask auth tokens in header values
                let masked = if k.as_str().eq_ignore_ascii_case("authorization")
                    || k.as_str().eq_ignore_ascii_case("csrf-token")
                    || k.as_str().eq_ignore_ascii_case("a-sess")
                    || k.as_str().eq_ignore_ascii_case("u-sess")
                {
                    Self::redact_secret_str(val)
                } else {
                    val.to_string()
                };
                format!("  {}: {}", k, masked)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn redact_secret_str(value: &str) -> String {
        if value.len() <= 8 {
            "<redacted>".into()
        } else {
            format!("{}...{}", &value[..4], &value[value.len() - 4..])
        }
    }

    fn is_sensitive_key(key: &str) -> bool {
        matches!(
            key.to_ascii_lowercase().as_str(),
            "authorization"
                | "csrf-token"
                | "csrtftoken"
                | "csrf"
                | "access_token"
                | "accesstoken"
                | "refresh_token"
                | "refreshtoken"
                | "user_session_token"
                | "usersessiontoken"
                | "app_session_token"
                | "appsessiontoken"
                | "otp_token"
                | "otptoken"
                | "otp_code"
                | "otpcode"
                | "password"
                | "email"
                | "a-sess"
                | "u-sess"
        )
    }

    fn redact_json_value(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, child) in map.iter_mut() {
                    if Self::is_sensitive_key(key) {
                        *child = Value::String(Self::redact_secret_str(
                            child.as_str().unwrap_or("<redacted>"),
                        ));
                    } else {
                        Self::redact_json_value(child);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    Self::redact_json_value(item);
                }
            }
            _ => {}
        }
    }

    fn redact_json_text(text: &str) -> String {
        match serde_json::from_str::<Value>(text) {
            Ok(mut value) => {
                Self::redact_json_value(&mut value);
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| "<redacted>".into())
            }
            Err(_) => "<non-json body omitted>".into(),
        }
    }

    /// Execute a GraphQL query against the given URL.
    pub async fn graphql<T: DeserializeOwned>(
        &self,
        url: &str,
        operation_name: &str,
        query: &str,
        variables: Option<Value>,
        extra_headers: Option<Vec<(&str, String)>>,
    ) -> Result<T> {
        let body = json!({
            "operationName": operation_name,
            "query": query,
            "variables": variables.unwrap_or(Value::Null),
        });

        let mut headers = Self::default_headers();
        if let Some(extra) = extra_headers {
            for (key, val) in extra {
                headers.insert(
                    reqwest::header::HeaderName::from_bytes(key.as_bytes())
                        .context("invalid header name")?,
                    HeaderValue::from_str(&val).context("invalid header value")?,
                );
            }
        }

        let start = std::time::Instant::now();

        let resp = self
            .http
            .post(url)
            .headers(headers.clone())
            .json(&body)
            .send()
            .await
            .context("request failed")?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let duration_ms = start.elapsed().as_millis();

        // Emit log entry
        self.emit_log(RequestLog {
            operation: operation_name.to_string(),
            status: Some(status.as_u16()),
            duration_ms,
            error: if !status.is_success() {
                Some(format!("HTTP {status}"))
            } else {
                None
            },
            request_body: if self.debug {
                let mut redacted = body.clone();
                Self::redact_json_value(&mut redacted);
                Some(serde_json::to_string_pretty(&redacted).unwrap_or_default())
            } else {
                None
            },
            response_body: if self.debug {
                Some(Self::redact_json_text(&text))
            } else {
                None
            },
            request_headers: if self.debug {
                Some(Self::format_headers(&headers))
            } else {
                None
            },
        });

        if !status.is_success() {
            bail!("HTTP {status}: {text}");
        }

        let gql_resp: GraphQlResponse<Value> = serde_json::from_str(&text)
            .with_context(|| format!("failed to parse response: {text}"))?;

        if let Some(errors) = gql_resp.errors {
            let msgs: Vec<_> = errors.iter().map(|e| e.display_message()).collect();
            bail!("GraphQL errors: {}", msgs.join("; "));
        }

        let data = gql_resp
            .data
            .context("GraphQL response contained no data")?;

        serde_json::from_value(data).context("failed to parse GraphQL data payload")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_sensitive_json_fields() {
        let mut value = serde_json::json!({
            "variables": {
                "email": "driver@example.com",
                "password": "supersecret",
                "otpCode": "123456"
            },
            "data": {
                "login": {
                    "accessToken": "access-abcdef123456",
                    "refreshToken": "refresh-abcdef123456"
                }
            }
        });

        RivianClient::redact_json_value(&mut value);

        assert_ne!(value["variables"]["email"], "driver@example.com");
        assert_ne!(value["variables"]["password"], "supersecret");
        assert_ne!(value["variables"]["otpCode"], "123456");
        assert_ne!(value["data"]["login"]["accessToken"], "access-abcdef123456");
        assert_ne!(
            value["data"]["login"]["refreshToken"],
            "refresh-abcdef123456"
        );
    }
}
