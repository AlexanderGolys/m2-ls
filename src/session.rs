//! The lifetime of the client session. The server process ends as soon as the
//! client sends `exit` or closes its end of the connection.
//!
//! tower-lsp alone never ends the process after either: once `exit` arrives it
//! keeps waiting for further input that no client sends, and when the input
//! closes it still waits for every in-flight handler, including any awaiting a
//! reply from the departed client. Each editor restart therefore left a server
//! behind.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, ReadBuf};
use tokio::sync::Notify;
use tower::Service;
use tower_lsp::jsonrpc::{Request, Response};

/// The process exit code when the client ends the session after requesting
/// `shutdown`, as the LSP specification requires.
const EXIT_AFTER_SHUTDOWN: i32 = 0;

/// The process exit code when the session ends without a prior `shutdown`.
const EXIT_WITHOUT_SHUTDOWN: i32 = 1;

/// Signals the end of the client session, shared by everything that can
/// observe it.
#[derive(Debug, Clone, Default)]
pub struct SessionEnd {
    /// Notification used to wake the transport when the session ends.
    ended: Arc<Notify>,
    /// Whether the client completed the shutdown-request phase.
    shutdown_requested: Arc<AtomicBool>,
}

impl SessionEnd {
    /// Resolves once the client has ended the session.
    pub async fn ended(&self) {
        self.ended.notified().await;
    }

    /// The exit code the process ends with.
    pub fn exit_code(&self) -> i32 {
        if self.shutdown_requested.load(Ordering::Acquire) {
            EXIT_AFTER_SHUTDOWN
        } else {
            EXIT_WITHOUT_SHUTDOWN
        }
    }

    /// Notifies the protocol transport that the session has ended.
    fn end(&self) {
        self.ended.notify_one();
    }

    /// Records the shutdown-request phase before the client sends exit.
    fn record_shutdown_request(&self) {
        self.shutdown_requested.store(true, Ordering::Release);
    }
}

/// The client's input stream, ending the session when the client closes it.
pub struct ClientInput<Input> {
    /// Underlying input stream supplying protocol messages.
    input: Input,
    /// Shared shutdown state for the client session.
    session_end: SessionEnd,
}

impl<Input> ClientInput<Input> {
    /// Wraps the supplied transport component with the shared session-shutdown state.
    pub fn new(input: Input, session_end: SessionEnd) -> Self {
        Self { input, session_end }
    }
}

impl<Input: AsyncRead + Unpin> AsyncRead for ClientInput<Input> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let requested_bytes = buf.remaining() > 0;
        let filled_before = buf.filled().len();
        let poll = Pin::new(&mut self.input).poll_read(cx, buf);
        let reached_end = matches!(poll, Poll::Ready(Ok(())))
            && requested_bytes
            && buf.filled().len() == filled_before;
        if reached_end {
            self.session_end.end();
        }
        poll
    }
}

/// Wraps the language service, ending the session when the client sends the
/// `exit` notification and recording whether `shutdown` preceded it.
pub struct ExitNotificationService<Inner> {
    /// Wrapped service receiving requests after interception.
    inner: Inner,
    /// Shared shutdown state for the client session.
    session_end: SessionEnd,
}

impl<Inner> ExitNotificationService<Inner> {
    /// Wraps the supplied transport component with the shared session-shutdown state.
    pub fn new(inner: Inner, session_end: SessionEnd) -> Self {
        Self { inner, session_end }
    }
}

impl<Inner> Service<Request> for ExitNotificationService<Inner>
where
    Inner: Service<Request, Response = Option<Response>>,
    Inner::Future: Send + 'static,
{
    type Response = Option<Response>;
    type Error = Inner::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        match request.method() {
            "shutdown" => self.session_end.record_shutdown_request(),
            "exit" => self.session_end.end(),
            _ => {}
        }
        let mut timing = crate::telemetry::Timing::start(
            "rpc",
            serde_json::json!({
                "method": request.method(), "request_id": request.id(),
                "uri": request.params().and_then(|params| params.pointer("/textDocument/uri")),
                "version": request.params().and_then(|params| params.pointer("/textDocument/version")),
                "status": "cancelled",
            }),
        );
        let future = self.inner.call(request);
        Box::pin(async move {
            let response = future.await;
            timing.record("status", "completed");
            match &response {
                Ok(Some(response)) => {
                    if let Some(error) = response.error() {
                        timing.error(
                            serde_json::json!({"code": error.code, "message": error.message}),
                        );
                    }
                }
                Err(_) => timing.error(serde_json::json!({"kind": "transport"})),
                _ => {}
            }
            response
        })
    }
}
