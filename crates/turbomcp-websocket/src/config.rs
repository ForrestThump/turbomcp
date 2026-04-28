//! Configuration types for WebSocket bidirectional transport
//!
//! This module provides configuration structures for WebSocket transport
//! including connection settings, reconnection policies, and elicitation limits.

use std::time::Duration;

/// Configuration for WebSocket bidirectional transport
#[derive(Clone, Debug)]
pub struct WebSocketBidirectionalConfig {
    /// WebSocket URL to connect to (client mode)
    pub url: Option<String>,

    /// Bind address for server mode
    pub bind_addr: Option<String>,

    /// Maximum message size (default: 16MB)
    pub max_message_size: usize,

    /// Keep-alive interval
    pub keep_alive_interval: Duration,

    /// Reconnection configuration
    pub reconnect: ReconnectConfig,

    /// Elicitation timeout
    pub elicitation_timeout: Duration,

    /// Maximum concurrent elicitations
    pub max_concurrent_elicitations: usize,
}

impl Default for WebSocketBidirectionalConfig {
    fn default() -> Self {
        Self {
            url: None,
            bind_addr: None,
            max_message_size: 16 * 1024 * 1024, // 16MB
            keep_alive_interval: Duration::from_secs(30),
            reconnect: ReconnectConfig::default(),
            elicitation_timeout: Duration::from_secs(30),
            max_concurrent_elicitations: 10,
        }
    }
}

impl WebSocketBidirectionalConfig {
    /// Create a new configuration with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Create client configuration with URL
    pub fn client(url: String) -> Self {
        Self {
            url: Some(url),
            ..Self::default()
        }
    }

    /// Create server configuration with bind address
    pub fn server(bind_addr: String) -> Self {
        Self {
            bind_addr: Some(bind_addr),
            ..Self::default()
        }
    }

    /// Set maximum message size
    pub fn with_max_message_size(mut self, size: usize) -> Self {
        self.max_message_size = size;
        self
    }

    /// Set keep-alive interval
    pub fn with_keep_alive_interval(mut self, interval: Duration) -> Self {
        self.keep_alive_interval = interval;
        self
    }

    /// Set reconnection configuration
    pub fn with_reconnect_config(mut self, config: ReconnectConfig) -> Self {
        self.reconnect = config;
        self
    }

    /// Set elicitation timeout
    pub fn with_elicitation_timeout(mut self, timeout: Duration) -> Self {
        self.elicitation_timeout = timeout;
        self
    }

    /// Set maximum concurrent elicitations
    pub fn with_max_concurrent_elicitations(mut self, max: usize) -> Self {
        self.max_concurrent_elicitations = max;
        self
    }
}

/// Reconnection configuration
#[derive(Clone, Debug)]
pub struct ReconnectConfig {
    /// Enable automatic reconnection
    pub enabled: bool,

    /// Initial retry delay
    pub initial_delay: Duration,

    /// Maximum retry delay
    pub max_delay: Duration,

    /// Exponential backoff factor
    pub backoff_factor: f64,

    /// Maximum number of retries
    pub max_retries: u32,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            initial_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
            backoff_factor: 2.0,
            max_retries: 10,
        }
    }
}

impl ReconnectConfig {
    /// Create new reconnection configuration
    pub fn new() -> Self {
        Self::default()
    }

    /// Set whether reconnection is enabled
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Set initial delay
    pub fn with_initial_delay(mut self, delay: Duration) -> Self {
        self.initial_delay = delay;
        self
    }

    /// Set maximum delay
    pub fn with_max_delay(mut self, delay: Duration) -> Self {
        self.max_delay = delay;
        self
    }

    /// Set backoff factor
    pub fn with_backoff_factor(mut self, factor: f64) -> Self {
        self.backoff_factor = factor;
        self
    }

    /// Set maximum retries
    pub fn with_max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_websocket_config_default() {
        let config = WebSocketBidirectionalConfig::default();
        assert_eq!(config.max_message_size, 16 * 1024 * 1024);
        assert_eq!(config.keep_alive_interval, Duration::from_secs(30));
        assert_eq!(config.max_concurrent_elicitations, 10);
    }

    #[test]
    fn test_websocket_config_client() {
        let config = WebSocketBidirectionalConfig::client("ws://example.com".to_string());
        assert_eq!(config.url, Some("ws://example.com".to_string()));
        assert_eq!(config.bind_addr, None);
    }

    #[test]
    fn test_websocket_config_server() {
        let config = WebSocketBidirectionalConfig::server("0.0.0.0:8080".to_string());
        assert_eq!(config.bind_addr, Some("0.0.0.0:8080".to_string()));
        assert_eq!(config.url, None);
    }

    #[test]
    fn test_websocket_config_builder() {
        let config = WebSocketBidirectionalConfig::new()
            .with_max_message_size(1024)
            .with_keep_alive_interval(Duration::from_secs(60))
            .with_max_concurrent_elicitations(5);

        assert_eq!(config.max_message_size, 1024);
        assert_eq!(config.keep_alive_interval, Duration::from_secs(60));
        assert_eq!(config.max_concurrent_elicitations, 5);
    }
}
