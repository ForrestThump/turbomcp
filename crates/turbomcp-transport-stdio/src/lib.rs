//! # turbomcp-transport-stdio
//!
//! The STDIO transport: newline-delimited JSON frames over a process's stdin
//! (inbound) and stdout (outbound) — the transport Claude Desktop and most local
//! MCP launchers speak. Framing (split on `\n`) is this crate's job; turning a
//! frame's bytes into a value is the [`Codec`]'s.
//!
//! The framing lives in [`LineTransport`], generic over any async byte streams,
//! so it is unit-testable over an in-memory pipe (and reusable by future
//! socket transports). [`StdioTransport`]/[`stdio`] specialize it to
//! stdin/stdout; [`serve_stdio`] pairs it with a service (the dispatcher).
//!
//! Phase 2 reads and replies one frame at a time. The concurrent writer-actor
//! (so a slow handler can't head-of-line-block reads, with writes kept ordered)
//! lands in Phase 4.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader, Stdin, Stdout,
};
use turbomcp_codec::{Codec, CodecError, DefaultCodec};
use turbomcp_core::JsonRpcMessage;
use turbomcp_service::{McpService, ServeConfig, Transport};

/// Failures from the line transport.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StdioError {
    /// An I/O error on the underlying stream.
    #[error("stdio i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// A frame could not be encoded/decoded.
    #[error("codec error: {0}")]
    Codec(#[from] CodecError),
}

/// Newline-delimited JSON-RPC over any async reader/writer pair.
///
/// Each inbound line is one complete frame (blank lines are skipped); each
/// outbound frame is written followed by `\n` and flushed. Stdio is the
/// `R = BufReader<Stdin>`, `W = Stdout` specialization ([`StdioTransport`]).
pub struct LineTransport<R, W, C = DefaultCodec> {
    reader: R,
    writer: W,
    codec: C,
    line: String,
}

impl<R, W, C: Codec> LineTransport<R, W, C> {
    /// Build a transport over `reader`/`writer` with the given codec.
    pub fn new(reader: R, writer: W, codec: C) -> Self {
        Self {
            reader,
            writer,
            codec,
            line: String::new(),
        }
    }
}

impl<R, W, C> Transport for LineTransport<R, W, C>
where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
    C: Codec,
{
    type Error = StdioError;

    async fn send(&mut self, msg: JsonRpcMessage) -> Result<(), Self::Error> {
        let bytes = self.codec.encode(&msg)?;
        self.writer.write_all(bytes.as_ref()).await?;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<JsonRpcMessage>, Self::Error> {
        loop {
            self.line.clear();
            let n = self.reader.read_line(&mut self.line).await?;
            if n == 0 {
                return Ok(None); // clean EOF
            }
            let trimmed = self.line.trim();
            if trimmed.is_empty() {
                continue; // tolerate blank keep-alive lines
            }
            return Ok(Some(self.codec.decode(trimmed.as_bytes())?));
        }
    }

    async fn close(mut self) -> Result<(), Self::Error> {
        self.writer.flush().await?;
        Ok(())
    }
}

/// Newline-delimited JSON over the process's stdin/stdout.
pub type StdioTransport<C = DefaultCodec> = LineTransport<BufReader<Stdin>, Stdout, C>;

/// A [`StdioTransport`] over the process's stdin/stdout with the [`DefaultCodec`].
#[must_use]
pub fn stdio() -> StdioTransport {
    LineTransport::new(
        BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
        DefaultCodec::default(),
    )
}

/// Serve `service` over stdin/stdout until the peer closes stdin. The common
/// entry point for a stdio MCP server.
///
/// # Errors
/// Propagates transport and service errors from the driver loop.
pub async fn serve_stdio<S>(service: S) -> Result<(), turbomcp_service::ProtocolError>
where
    S: McpService + Clone,
    S::Future: Send + 'static,
{
    turbomcp_service::serve(stdio(), service).await
}

/// Serve `service` over stdin/stdout with explicit [`ServeConfig`] — the entry
/// point when you need a shutdown token, drain timeout, or concurrency bound.
///
/// # Errors
/// Propagates transport and service errors from the driver loop.
pub async fn serve_stdio_with<S>(
    service: S,
    config: ServeConfig,
) -> Result<(), turbomcp_service::ProtocolError>
where
    S: McpService + Clone,
    S::Future: Send + 'static,
{
    turbomcp_service::serve_with(stdio(), service, config).await
}
