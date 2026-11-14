//! The protocol's error shape (`overview.md`, "Error model") and the JSON body extractor that never answers 500 for
//! malformed input.

use axum::body::Bytes;
use axum::extract::{FromRequest, Request};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub field: Option<String>,
    pub retry_after_s: Option<i64>,
    pub details: Option<Box<Value>>,
    /// 1.1.5: the same as `message` for the player, as localized text (optional; launchers that know it show it).
    pub text: Option<Box<Value>>,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            status,
            code,
            message: message.into(),
            field: None,
            retry_after_s: None,
            details: None,
            text: None,
        }
    }
    /// Adds the player's text in English and French.
    pub fn text(mut self, en: &str, fr: &str) -> Self {
        self.text = Some(Box::new(json!({"en": en, "fr": fr})));
        self
    }
    pub fn field(mut self, f: &str) -> Self {
        self.field = Some(f.to_string());
        self
    }
    pub fn retry_after(mut self, s: i64) -> Self {
        self.retry_after_s = Some(s.max(1));
        self
    }
    pub fn details(mut self, d: Value) -> Self {
        self.details = Some(Box::new(d));
        self
    }
    pub fn bad_request(code: &'static str, m: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, m)
    }
    pub fn invalid(field: &str, m: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, "validation_failed", m).field(field)
    }
    pub fn unauthorized(code: &'static str, m: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, code, m)
    }
    pub fn forbidden(code: &'static str, m: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, m)
    }
    pub fn not_found(m: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", m)
    }
    pub fn feature_disabled() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "feature_disabled",
            "this area is not offered by this server",
        )
    }
    pub fn conflict(code: &'static str, m: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, m)
    }
    pub fn rate_limited(retry: i64) -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "too many requests").retry_after(retry)
    }
    pub fn too_large() -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "the request body is too large",
        )
    }
    pub fn internal(m: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", m)
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        tracing::error!(error = %e, "database error");
        ApiError::internal("database error")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({"error": {
            "code": self.code,
            "message": self.message,
            "field": self.field,
            "retry_after_s": self.retry_after_s,
            "details": self.details.as_deref(),
        }});
        if let Some(t) = self.text {
            body["error"]["text"] = *t;
        }
        let mut resp = (self.status, axum::Json(body)).into_response();
        if let Some(s) = self
            .retry_after_s
            .filter(|_| self.status == StatusCode::TOO_MANY_REQUESTS)
            && let Ok(v) = HeaderValue::from_str(&s.to_string())
        {
            resp.headers_mut().insert(header::RETRY_AFTER, v);
        }
        resp
    }
}

/// A JSON body: `400 invalid_json` when it is not JSON (or not the expected JSON type), `422 validation_failed`
/// when a member has the wrong type, `413 payload_too_large` above the route's limit. An empty body reads as `{}`.
pub struct JsonBody<T>(pub T);

/// The raw JSON value of a body (result and status uploads), with the same rules.
pub struct RawJson(pub Value);

/// Security review L-S2: how long a request body may take to arrive (a result file is at most 64 KiB).
pub const BODY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

async fn read_value(req: Request, limit: usize) -> ApiResult<Value> {
    let bytes: Bytes = tokio::time::timeout(BODY_TIMEOUT, axum::body::to_bytes(req.into_body(), limit))
        .await
        .map_err(|_| {
            ApiError::new(
                axum::http::StatusCode::REQUEST_TIMEOUT,
                "bad_request",
                "the request body did not arrive in time",
            )
        })?
        .map_err(|_| ApiError::too_large())?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(json!({}));
    }
    serde_json::from_slice::<Value>(&bytes).map_err(|_| ApiError::bad_request("invalid_json", "the body is not JSON"))
}

/// Body limits by route, set by the router as a request extension.
#[derive(Clone, Copy)]
pub struct BodyLimit(pub usize);

fn limit_of(req: &Request) -> usize {
    req.extensions().get::<BodyLimit>().map(|l| l.0).unwrap_or(16 * 1024)
}

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for JsonBody<T> {
    type Rejection = ApiError;
    async fn from_request(req: Request, _: &S) -> ApiResult<Self> {
        let limit = limit_of(&req);
        let v = read_value(req, limit).await?;
        if !v.is_object() {
            return Err(ApiError::bad_request("invalid_json", "the body must be a JSON object"));
        }
        serde_json::from_value::<T>(v).map(JsonBody).map_err(|e| {
            let msg = e.to_string();
            let field = msg
                .split('`')
                .nth(1)
                .map(|f| format!("/{f}"))
                .unwrap_or_else(|| "/".into());
            ApiError::invalid(&field, format!("bad request body: {msg}"))
        })
    }
}

impl<S: Send + Sync> FromRequest<S> for RawJson {
    type Rejection = ApiError;
    async fn from_request(req: Request, _: &S) -> ApiResult<Self> {
        let limit = limit_of(&req);
        read_value(req, limit).await.map(RawJson)
    }
}
