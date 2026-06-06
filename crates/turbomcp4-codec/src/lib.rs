//! # turbomcp4-codec
//!
//! The wire codec layer: `bytes` ↔ typed values. The transport hands the codec
//! *complete* messages (line-delimited for stdio, SSE-event-framed for HTTP);
//! streaming/framing is the transport's job, not the codec's.
//!
//! The seam is a single [`Codec`] trait with two impls:
//!
//! - [`SerdeJsonCodec`] — the portable baseline. Always available, including on
//!   `wasm32`.
//! - [`SonicRsCodec`] — SIMD-accelerated JSON (behind the `simd` feature, native
//!   `x86_64`/`aarch64` only). [`DefaultCodec`] resolves to this where supported.
//!
//! All codecs are version-independent: they serialize whatever
//! [`serde::Serialize`] value they are given. The per-version typed modules and
//! the [`turbomcp4_core::JsonRpcMessage`] envelope sit above this layer.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use bytes::Bytes;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Errors produced while encoding to or decoding from the wire.
///
/// Decoding failures map to JSON-RPC parse errors (`-32700`) one layer up
/// (`turbomcp4-service`'s `ProtocolError::Parse`).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CodecError {
    /// The value could not be serialized to the wire format.
    #[error("encode failed: {0}")]
    Encode(String),
    /// The bytes could not be deserialized into the target type.
    #[error("decode failed: {0}")]
    Decode(String),
}

/// Bytes ↔ typed value, in a single wire format.
///
/// Object-unsafe by design (generic methods); the codec is chosen at builder
/// time as a concrete type and transports are generic over `C: Codec`. This
/// keeps the hot path monomorphized — no `dyn` dispatch per message.
pub trait Codec: Send + Sync + 'static {
    /// The name of the wire format, for diagnostics (`"json"`, …).
    fn format(&self) -> &'static str;

    /// Serialize `value` into a freshly allocated [`Bytes`] buffer.
    ///
    /// # Errors
    /// Returns [`CodecError::Encode`] if `value` cannot be represented.
    fn encode<T: Serialize>(&self, value: &T) -> Result<Bytes, CodecError>;

    /// Deserialize a complete message from `bytes`.
    ///
    /// # Errors
    /// Returns [`CodecError::Decode`] on malformed input or type mismatch.
    fn decode<T: DeserializeOwned>(&self, bytes: &[u8]) -> Result<T, CodecError>;
}

/// `serde_json`-backed codec. The portable baseline: available on every target,
/// including `wasm32`, with no SIMD requirement.
#[derive(Debug, Clone, Copy, Default)]
pub struct SerdeJsonCodec;

impl Codec for SerdeJsonCodec {
    fn format(&self) -> &'static str {
        "json"
    }

    fn encode<T: Serialize>(&self, value: &T) -> Result<Bytes, CodecError> {
        serde_json::to_vec(value)
            .map(Bytes::from)
            .map_err(|e| CodecError::Encode(e.to_string()))
    }

    fn decode<T: DeserializeOwned>(&self, bytes: &[u8]) -> Result<T, CodecError> {
        serde_json::from_slice(bytes).map_err(|e| CodecError::Decode(e.to_string()))
    }
}

/// SIMD-accelerated JSON codec (`sonic-rs`). Native `x86_64`/`aarch64` only;
/// gated behind the `simd` feature.
#[cfg(all(feature = "simd", any(target_arch = "x86_64", target_arch = "aarch64")))]
#[derive(Debug, Clone, Copy, Default)]
pub struct SonicRsCodec;

#[cfg(all(feature = "simd", any(target_arch = "x86_64", target_arch = "aarch64")))]
impl Codec for SonicRsCodec {
    fn format(&self) -> &'static str {
        "json"
    }

    fn encode<T: Serialize>(&self, value: &T) -> Result<Bytes, CodecError> {
        sonic_rs::to_vec(value)
            .map(Bytes::from)
            .map_err(|e| CodecError::Encode(e.to_string()))
    }

    fn decode<T: DeserializeOwned>(&self, bytes: &[u8]) -> Result<T, CodecError> {
        sonic_rs::from_slice(bytes).map_err(|e| CodecError::Decode(e.to_string()))
    }
}

/// The codec selected by default for the current build: [`SonicRsCodec`] where
/// SIMD is available, otherwise [`SerdeJsonCodec`]. Both encode byte-compatible
/// JSON, so this choice is transparent to peers.
#[cfg(all(feature = "simd", any(target_arch = "x86_64", target_arch = "aarch64")))]
pub type DefaultCodec = SonicRsCodec;

/// The codec selected by default for the current build. See the SIMD variant
/// for the rationale; this is the portable fallback.
#[cfg(not(all(feature = "simd", any(target_arch = "x86_64", target_arch = "aarch64"))))]
pub type DefaultCodec = SerdeJsonCodec;

#[cfg(test)]
mod tests {
    use super::*;
    use turbomcp4_core::{JsonRpcMessage, JsonRpcRequest};

    fn sample() -> JsonRpcMessage {
        JsonRpcRequest::new(1, "tools/list", Some(serde_json::json!({"cursor": "abc"}))).into()
    }

    fn roundtrip<C: Codec>(codec: C) {
        let msg = sample();
        let bytes = codec.encode(&msg).expect("encode");
        let back: JsonRpcMessage = codec.decode(&bytes).expect("decode");
        assert_eq!(msg, back);
    }

    #[test]
    fn serde_json_roundtrips() {
        roundtrip(SerdeJsonCodec);
    }

    #[cfg(all(feature = "simd", any(target_arch = "x86_64", target_arch = "aarch64")))]
    #[test]
    fn sonic_rs_roundtrips() {
        roundtrip(SonicRsCodec);
    }

    #[cfg(all(feature = "simd", any(target_arch = "x86_64", target_arch = "aarch64")))]
    #[test]
    fn codecs_are_byte_compatible() {
        let msg = sample();
        let a = SerdeJsonCodec.encode(&msg).unwrap();
        // Cross-decode: serde_json output parses under sonic-rs.
        let via_sonic: JsonRpcMessage = SonicRsCodec.decode(&a).unwrap();
        assert_eq!(msg, via_sonic);
    }

    #[test]
    fn decode_error_is_reported() {
        let err = SerdeJsonCodec
            .decode::<JsonRpcMessage>(b"{not json")
            .unwrap_err();
        assert!(matches!(err, CodecError::Decode(_)));
    }
}
