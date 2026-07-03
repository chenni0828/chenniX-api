use std::time::Duration;

use axum::http::{Request, Response};
use tower_http::trace::{
    DefaultOnBodyChunk, DefaultOnEos, DefaultOnFailure, DefaultOnRequest,
    HttpMakeClassifier, MakeSpan, OnResponse, TraceLayer,
};
use tracing::Span;

/// A `MakeSpan` implementation that creates a span with `method`, `uri`,
/// `status`, and `latency_ms` fields for every request.
#[derive(Clone, Default)]
pub struct RequestSpan;

impl<B> MakeSpan<B> for RequestSpan {
    fn make_span(&mut self, request: &Request<B>) -> Span {
        tracing::info_span!(
            "http_request",
            method = %request.method(),
            uri = %request.uri().path_and_query().map(|pq| pq.as_str()).unwrap_or(request.uri().path()),
            status = tracing::field::Empty,
            latency_ms = tracing::field::Empty,
        )
    }
}

/// An `OnResponse` implementation that records status and latency into the
/// span and emits a single `tracing::info!` log line per request.
#[derive(Clone, Default)]
pub struct LogOnResponse;

impl<B> OnResponse<B> for LogOnResponse {
    fn on_response(self, response: &Response<B>, latency: Duration, span: &Span) {
        let status = response.status().as_u16();
        let latency_ms = latency.as_secs_f64() * 1000.0;
        span.record("status", &status);
        span.record("latency_ms", &format!("{:.1}", latency_ms));
        tracing::info!("{} {:.1}ms", status, latency_ms);
    }
}

/// The concrete [`TraceLayer`] type returned by [`request_log_layer`].
pub type LogTraceLayer = TraceLayer<
    HttpMakeClassifier,
    RequestSpan,
    DefaultOnRequest,
    LogOnResponse,
    DefaultOnBodyChunk,
    DefaultOnEos,
    DefaultOnFailure,
>;

/// Build a [`TraceLayer`] that logs method, path, status, and latency for
/// every HTTP request.
///
/// # Example
/// ```ignore
/// use chennix_server::middleware::logging::request_log_layer;
///
/// Router::new()
///     .route("/api", get(handler))
///     .layer(request_log_layer())
///     .with_state(state)
/// ```
pub fn request_log_layer() -> LogTraceLayer {
    TraceLayer::new_for_http()
        .make_span_with(RequestSpan)
        .on_response(LogOnResponse)
}
