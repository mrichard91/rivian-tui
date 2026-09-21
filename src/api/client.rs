use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::types::{GraphQlError, GraphQlResponse};

fn build_http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .gzip(true)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .context("failed to build HTTP client")
}

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
    /// Per-field GraphQL errors that came back alongside usable `data`
    /// (partial response). Not fatal; surfaced so unsupported fields are
    /// visible in the activity log.
    pub warnings: Vec<String>,
}

/// The saved session is no longer accepted by the API (HTTP 401 or a
/// GraphQL `UNAUTHENTICATED` error). Callers downcast to this to distinguish
/// "log in again" from a transient failure.
#[derive(Debug)]
pub struct SessionExpired(pub String);

impl std::fmt::Display for SessionExpired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "session expired: {}", self.0)
    }
}

impl std::error::Error for SessionExpired {}

/// A successfully parsed GraphQL body: the deserialized `data` plus any
/// non-fatal per-field errors the server reported alongside it.
#[derive(Debug)]
pub struct ParsedResponse<T> {
    pub data: T,
    pub warnings: Vec<String>,
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
        Ok(Self {
            http: build_http_client()?,
            debug: false,
            log_tx: None,
        })
    }

    /// Wrap an existing reqwest::Client. Used by the app to share a single
    /// connection pool across all spawned requests instead of building a
    /// fresh client (and a fresh pool) per task.
    pub fn from_http(http: reqwest::Client) -> Self {
        Self {
            http,
            debug: false,
            log_tx: None,
        }
    }

    /// Build a reqwest::Client configured the way Rivian's API expects
    /// (gzip, generous timeouts). Exposed so the app can construct one
    /// shared client and clone it cheaply into per-request `RivianClient`s.
    pub fn build_http() -> Result<reqwest::Client> {
        build_http_client()
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
        // Slice by chars, not bytes — multi-byte values would otherwise panic
        // on a string-boundary error.
        let chars: Vec<char> = value.chars().collect();
        if chars.len() <= 8 {
            return "<redacted>".into();
        }
        let head: String = chars[..4].iter().collect();
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("{head}...{tail}")
    }

    fn is_sensitive_key(key: &str) -> bool {
        matches!(
            key.to_ascii_lowercase().as_str(),
            "authorization"
                | "csrf-token"
                | "csrftoken"
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
            warnings: Vec::new(),
        });

        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(SessionExpired(format!("HTTP {status}")).into());
        }

        if !status.is_success() {
            // Redact the response body before surfacing it as an error — it
            // can flow into the activity log and (for JSON bodies) may echo
            // sensitive request fields back to the caller.
            let body = match serde_json::from_str::<Value>(&text) {
                Ok(_) => Self::redact_json_text(&text),
                Err(_) => text.chars().take(256).collect::<String>(),
            };
            bail!("HTTP {status}: {body}");
        }

        let parsed = Self::parse_graphql_body::<T>(&text)?;
        if !parsed.warnings.is_empty() {
            self.emit_log(RequestLog {
                operation: operation_name.to_string(),
                status: Some(status.as_u16()),
                duration_ms,
                error: None,
                request_body: None,
                response_body: None,
                request_headers: None,
                warnings: parsed.warnings,
            });
        }
        Ok(parsed.data)
    }

    /// Parse a GraphQL response body.
    ///
    /// GraphQL servers return `{ data: {...partial...}, errors: [...] }` when
    /// a single field's resolver fails — Rivian does this for fields a given
    /// model lacks (third-row seats on an R1T, hitch status, ...). A response
    /// is only a failure when `data` is absent/null; otherwise the errors are
    /// returned as non-fatal warnings alongside the deserialized data.
    pub fn parse_graphql_body<T: DeserializeOwned>(text: &str) -> Result<ParsedResponse<T>> {
        let gql_resp: GraphQlResponse<Value> = serde_json::from_str(text)
            .with_context(|| format!("failed to parse response: {text}"))?;

        let messages: Vec<String> = gql_resp
            .errors
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|e| e.display_message())
            .collect();

        let data = match gql_resp.data {
            Some(data) if !data.is_null() => data,
            _ => {
                if messages.is_empty() {
                    bail!("GraphQL response contained no data");
                }
                let unauthenticated = gql_resp
                    .errors
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .any(GraphQlError::is_unauthenticated);
                if unauthenticated {
                    return Err(SessionExpired(messages.join("; ")).into());
                }
                bail!("GraphQL errors: {}", messages.join("; "));
            }
        };

        let data = serde_json::from_value(data).context("failed to parse GraphQL data payload")?;
        Ok(ParsedResponse {
            data,
            warnings: messages,
        })
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
                },
                "createCsrfToken": { "csrfToken": "csrf-abcdef123456" }
            }
        });

        RivianClient::redact_json_value(&mut value);

        assert_ne!(value["variables"]["email"], "driver@example.com");
        assert_ne!(
            value["data"]["createCsrfToken"]["csrfToken"], "csrf-abcdef123456",
            "csrfToken from the CSRF response must be redacted"
        );
        assert_ne!(value["variables"]["password"], "supersecret");
        assert_ne!(value["variables"]["otpCode"], "123456");
        assert_ne!(value["data"]["login"]["accessToken"], "access-abcdef123456");
        assert_ne!(
            value["data"]["login"]["refreshToken"],
            "refresh-abcdef123456"
        );
    }

    #[test]
    fn partial_response_with_field_errors_still_yields_data() {
        // Rivian returns per-field resolver errors (e.g. a field a given model
        // lacks) alongside the rest of `data`. That must not discard the poll.
        let body = r#"{
            "data": { "vehicleState": { "batteryLevel": { "value": 55 }, "rearHitchStatus": null } },
            "errors": [ { "message": "Cannot resolve rearHitchStatus", "path": ["vehicleState","rearHitchStatus"] } ]
        }"#;
        let parsed = RivianClient::parse_graphql_body::<serde_json::Value>(body).unwrap();
        assert_eq!(parsed.data["vehicleState"]["batteryLevel"]["value"], 55);
        assert_eq!(parsed.warnings.len(), 1);
        assert!(parsed.warnings[0].contains("rearHitchStatus"));
    }

    #[test]
    fn response_with_errors_and_no_data_is_an_error() {
        let body = r#"{ "data": null, "errors": [ { "message": "Not authorized", "extensions": { "code": "UNAUTHENTICATED" } } ] }"#;
        let err = RivianClient::parse_graphql_body::<serde_json::Value>(body).unwrap_err();
        assert!(err.to_string().contains("UNAUTHENTICATED"), "{err}");
    }

    #[test]
    fn unauthenticated_error_code_is_typed_session_expiry() {
        let body = r#"{ "data": null, "errors": [ { "message": "Not authorized", "extensions": { "code": "UNAUTHENTICATED" } } ] }"#;
        let err = RivianClient::parse_graphql_body::<serde_json::Value>(body).unwrap_err();
        assert!(
            err.downcast_ref::<SessionExpired>().is_some(),
            "UNAUTHENTICATED must surface as SessionExpired, got {err}"
        );
    }
}
