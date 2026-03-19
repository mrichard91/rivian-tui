use anyhow::{Context, Result, bail};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::types::GraphQlResponse;

/// Base URLs for Rivian's GraphQL endpoints
pub const GATEWAY_URL: &str = "https://rivian.com/api/gql/gateway/graphql";
/// Vehicle state and other authenticated queries go through the same gateway
pub const API_URL: &str = "https://rivian.com/api/gql/gateway/graphql";
pub const CHARGING_URL: &str = "https://rivian.com/api/gql/chrg/user/graphql";

/// A log entry emitted by the client for each request
#[derive(Debug, Clone)]
pub struct RequestLog {
    pub operation: String,
    pub url: String,
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
            HeaderValue::from_static(
                "RivianApp/1304 CFNetwork/1404.0.5 Darwin/22.3.0",
            ),
        );
        headers.insert("Accept", HeaderValue::from_static("application/json"));
        headers.insert(
            "Content-Type",
            HeaderValue::from_static("application/json"),
        );
        headers.insert(
            "Accept-Language",
            HeaderValue::from_static("en-US"),
        );
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
                {
                    if val.len() > 20 {
                        format!("{}...{}", &val[..10], &val[val.len() - 4..])
                    } else {
                        val.to_string()
                    }
                } else {
                    val.to_string()
                };
                format!("  {}: {}", k, masked)
            })
            .collect::<Vec<_>>()
            .join("\n")
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
            url: url.to_string(),
            status: Some(status.as_u16()),
            duration_ms,
            error: if !status.is_success() {
                Some(format!("HTTP {status}"))
            } else {
                None
            },
            request_body: if self.debug {
                Some(serde_json::to_string_pretty(&body).unwrap_or_default())
            } else {
                None
            },
            response_body: if self.debug { Some(text.clone()) } else { None },
            request_headers: if self.debug {
                Some(Self::format_headers(&headers))
            } else {
                None
            },
        });

        if !status.is_success() {
            bail!("HTTP {status}: {text}");
        }

        let gql_resp: GraphQlResponse<T> = serde_json::from_str(&text)
            .with_context(|| format!("failed to parse response: {text}"))?;

        if let Some(errors) = gql_resp.errors {
            let msgs: Vec<_> = errors.iter().map(|e| e.message.as_str()).collect();
            bail!("GraphQL errors: {}", msgs.join("; "));
        }

        gql_resp
            .data
            .context("GraphQL response contained no data")
    }

    /// Execute a raw GraphQL query and return the JSON value
    pub async fn graphql_raw(
        &self,
        url: &str,
        operation_name: &str,
        query: &str,
        variables: Option<Value>,
        extra_headers: Option<Vec<(&str, String)>>,
    ) -> Result<Value> {
        self.graphql(url, operation_name, query, variables, extra_headers)
            .await
    }
}
