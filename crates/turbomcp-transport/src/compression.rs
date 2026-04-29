//! Message compression support

use std::io::{Read, Write};

use crate::core::{TransportError, TransportResult};
use serde_json::Value;

/// Default cap on decompressed payload size: 16 MiB.
///
/// Decompression bombs (small compressed input that expands to GiB) are a known
/// DoS vector. Callers handling untrusted input should override this via
/// [`MessageCompressor::with_max_decompressed_size`] to match their
/// `LimitsConfig::max_request_size` / `max_response_size`.
pub const DEFAULT_MAX_DECOMPRESSED_SIZE: usize = 16 * 1024 * 1024;

/// Compression algorithm
#[derive(Debug, Clone, Copy)]
pub enum CompressionType {
    /// No compression
    None,
    /// Gzip compression
    #[cfg(feature = "flate2")]
    Gzip,
    /// Brotli compression
    #[cfg(feature = "brotli")]
    Brotli,
    /// LZ4 compression
    #[cfg(feature = "lz4_flex")]
    Lz4,
}

/// Message compressor/decompressor.
///
/// Decompression is bounded by `max_decompressed_size` to prevent decompression
/// bombs. The default cap is [`DEFAULT_MAX_DECOMPRESSED_SIZE`] (16 MiB);
/// override with [`MessageCompressor::with_max_decompressed_size`].
#[derive(Debug)]
pub struct MessageCompressor {
    compression_type: CompressionType,
    max_decompressed_size: usize,
}

impl MessageCompressor {
    /// Create a new message compressor with the default decompression cap
    /// ([`DEFAULT_MAX_DECOMPRESSED_SIZE`]).
    #[must_use]
    pub const fn new(compression_type: CompressionType) -> Self {
        Self {
            compression_type,
            max_decompressed_size: DEFAULT_MAX_DECOMPRESSED_SIZE,
        }
    }

    /// Override the maximum decompressed payload size, in bytes.
    ///
    /// Decompression that would exceed this cap returns
    /// [`TransportError::ResponseTooLarge`] without allocating the bomb.
    #[must_use]
    pub const fn with_max_decompressed_size(mut self, max: usize) -> Self {
        self.max_decompressed_size = max;
        self
    }

    /// Currently configured decompression cap, in bytes.
    #[must_use]
    pub const fn max_decompressed_size(&self) -> usize {
        self.max_decompressed_size
    }

    /// Compress a JSON message
    pub fn compress(&self, message: &Value) -> TransportResult<Vec<u8>> {
        let json_bytes = serde_json::to_vec(message)
            .map_err(|e| TransportError::SerializationFailed(e.to_string()))?;

        match self.compression_type {
            CompressionType::None => Ok(json_bytes),

            #[cfg(feature = "flate2")]
            CompressionType::Gzip => {
                use flate2::{Compression, write::GzEncoder};

                let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
                encoder
                    .write_all(&json_bytes)
                    .map_err(|e| TransportError::Internal(e.to_string()))?;
                encoder
                    .finish()
                    .map_err(|e| TransportError::Internal(e.to_string()))
            }

            #[cfg(feature = "brotli")]
            CompressionType::Brotli => {
                use brotli::enc::BrotliEncoderParams;

                let params = BrotliEncoderParams::default();
                let mut compressed = Vec::new();
                brotli::BrotliCompress(&mut json_bytes.as_slice(), &mut compressed, &params)
                    .map_err(|e| {
                        TransportError::Internal(format!("Brotli compression failed: {e}"))
                    })?;
                Ok(compressed)
            }

            #[cfg(feature = "lz4_flex")]
            CompressionType::Lz4 => {
                use lz4_flex::compress_prepend_size;
                Ok(compress_prepend_size(&json_bytes))
            }
        }
    }

    /// Decompress a message back to JSON.
    ///
    /// Output is bounded by [`Self::max_decompressed_size`]; payloads that
    /// would exceed the cap return [`TransportError::ResponseTooLarge`]
    /// without allocating the full bomb. The LZ4 size-prefix is validated
    /// against the cap before any allocation, defending against
    /// attacker-supplied size headers.
    pub fn decompress(&self, compressed: &[u8]) -> TransportResult<Value> {
        let cap = self.max_decompressed_size;
        let json_bytes = match self.compression_type {
            CompressionType::None => {
                if compressed.len() > cap {
                    return Err(TransportError::ResponseTooLarge {
                        size: compressed.len(),
                        max: cap,
                    });
                }
                compressed.to_vec()
            }

            #[cfg(feature = "flate2")]
            CompressionType::Gzip => {
                use flate2::read::GzDecoder;

                let decoder = GzDecoder::new(compressed);
                read_bounded(decoder, cap)?
            }

            #[cfg(feature = "brotli")]
            CompressionType::Brotli => {
                let decoder = brotli::Decompressor::new(compressed, 4096);
                read_bounded(decoder, cap)?
            }

            #[cfg(feature = "lz4_flex")]
            CompressionType::Lz4 => {
                use lz4_flex::decompress_size_prepended;

                // Validate the attacker-controllable size prefix before
                // allocating: the first 4 LE bytes are the uncompressed size.
                if compressed.len() < 4 {
                    return Err(TransportError::SerializationFailed(
                        "LZ4 frame missing size prefix".into(),
                    ));
                }
                let prefix = u32::from_le_bytes([
                    compressed[0],
                    compressed[1],
                    compressed[2],
                    compressed[3],
                ]) as usize;
                if prefix > cap {
                    return Err(TransportError::ResponseTooLarge {
                        size: prefix,
                        max: cap,
                    });
                }
                decompress_size_prepended(compressed)
                    .map_err(|e| TransportError::Internal(e.to_string()))?
            }
        };

        serde_json::from_slice(&json_bytes)
            .map_err(|e| TransportError::SerializationFailed(e.to_string()))
    }
}

/// Read from `reader` into a fresh `Vec`, refusing to grow past `cap` bytes.
///
/// Streaming decoders (gzip, brotli) read until EOF; limiting the underlying
/// `Read` via `take(cap)` would silently truncate the stream and produce a
/// "valid but wrong" byte slice. Instead, we let the decoder run one extra
/// byte past the cap so we can *detect* overflow and surface an explicit
/// `ResponseTooLarge` error.
#[cfg(any(feature = "flate2", feature = "brotli"))]
fn read_bounded<R: Read>(reader: R, cap: usize) -> TransportResult<Vec<u8>> {
    let probe_cap = cap.saturating_add(1);
    let mut bounded = reader.take(probe_cap as u64);
    let mut out = Vec::new();
    bounded
        .read_to_end(&mut out)
        .map_err(|e| TransportError::Internal(e.to_string()))?;
    if out.len() > cap {
        return Err(TransportError::ResponseTooLarge {
            size: out.len(),
            max: cap,
        });
    }
    Ok(out)
}

impl Default for MessageCompressor {
    fn default() -> Self {
        Self::new(CompressionType::None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_no_compression() {
        let compressor = MessageCompressor::new(CompressionType::None);
        let message = json!({"test": "data", "number": 42});

        let compressed = compressor.compress(&message).unwrap();
        let decompressed = compressor.decompress(&compressed).unwrap();

        assert_eq!(message, decompressed);
    }

    #[cfg(feature = "lz4_flex")]
    #[test]
    fn test_lz4_compression() {
        let compressor = MessageCompressor::new(CompressionType::Lz4);
        let message = json!({
            "large_data": "x".repeat(1000),
            "numbers": (0..100).collect::<Vec<i32>>()
        });

        let compressed = compressor.compress(&message).unwrap();
        let decompressed = compressor.decompress(&compressed).unwrap();

        assert_eq!(message, decompressed);

        // Verify compression actually reduces size for large data
        let original_size = serde_json::to_vec(&message).unwrap().len();
        assert!(compressed.len() < original_size);
    }

    #[test]
    fn test_none_rejects_oversized_payload() {
        let compressor =
            MessageCompressor::new(CompressionType::None).with_max_decompressed_size(16);
        let oversized = vec![b'a'; 32];
        match compressor.decompress(&oversized) {
            Err(TransportError::ResponseTooLarge { size, max }) => {
                assert_eq!(size, 32);
                assert_eq!(max, 16);
            }
            other => panic!("expected ResponseTooLarge, got {other:?}"),
        }
    }

    #[cfg(feature = "flate2")]
    #[test]
    fn test_gzip_decompression_bomb_is_rejected() {
        use flate2::{Compression, write::GzEncoder};
        // 1 MiB of zeros gzip'd compresses to ~1 KiB — a classic small-input,
        // big-output bomb shape.
        let payload = vec![0u8; 1024 * 1024];
        let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&payload).unwrap();
        let bomb = encoder.finish().unwrap();
        assert!(bomb.len() < 4096, "expected high compression ratio");

        let compressor =
            MessageCompressor::new(CompressionType::Gzip).with_max_decompressed_size(64 * 1024);
        match compressor.decompress(&bomb) {
            Err(TransportError::ResponseTooLarge { max, .. }) => {
                assert_eq!(max, 64 * 1024);
            }
            other => panic!("expected ResponseTooLarge, got {other:?}"),
        }
    }

    #[cfg(feature = "lz4_flex")]
    #[test]
    fn test_lz4_rejects_attacker_supplied_size_prefix() {
        // A frame that *claims* (via its size prefix) to decompress to 1 GiB.
        // We must reject before allocating anything close to that.
        let mut bomb = (1024u32 * 1024 * 1024).to_le_bytes().to_vec();
        bomb.extend_from_slice(&[0u8; 8]);
        let compressor =
            MessageCompressor::new(CompressionType::Lz4).with_max_decompressed_size(64 * 1024);
        match compressor.decompress(&bomb) {
            Err(TransportError::ResponseTooLarge { size, max }) => {
                assert_eq!(size, 1024 * 1024 * 1024);
                assert_eq!(max, 64 * 1024);
            }
            other => panic!("expected ResponseTooLarge, got {other:?}"),
        }
    }
}
