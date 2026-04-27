//! TurboTransport - Enhanced transport with fault tolerance features
//!
//! This module provides the TurboTransport wrapper that combines:
//! - Retry mechanisms with exponential backoff
//! - Circuit breaker pattern for fast failure
//! - Health checking and monitoring
//! - Message deduplication
//! - Comprehensive metrics collection

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, RwLock};
use tokio::time::sleep;

use crate::core::{
    Transport, TransportConfig, TransportError, TransportMessage, TransportMetrics,
    TransportResult, TransportState, TransportType,
};

use super::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitBreakerStats};
use super::deduplication::DeduplicationCache;
use super::health::{HealthCheckConfig, HealthChecker, HealthInfo, HealthStatus};
use super::metrics::TurboTransportMetrics;
use super::retry::RetryConfig;

/// TurboTransport - Enhanced transport with retry, circuit breaker, and health checking
#[derive(Debug)]
pub struct TurboTransport {
    /// Underlying transport
    inner: Arc<Mutex<Box<dyn Transport>>>,
    /// Cached transport type (immutable for the lifetime of this wrapper).
    /// Pre-3.2.0 the `transport_type()` accessor fell back to `try_lock` and
    /// returned `Stdio` on contention — a fabricated value masquerading as
    /// truth in metrics. We snapshot the value at construction so the
    /// accessor is lock-free and accurate.
    cached_transport_type: TransportType,
    /// Cached endpoint (immutable for the lifetime of this wrapper). Same
    /// rationale as `cached_transport_type` — the previous `try_lock`-based
    /// accessor returned `None` on contention.
    cached_endpoint: Option<String>,
    /// Retry configuration
    retry_config: RetryConfig,
    /// Circuit breaker
    circuit_breaker: Arc<Mutex<CircuitBreaker>>,
    /// Health checker
    health_checker: Arc<Mutex<HealthChecker>>,
    /// Transport metrics
    metrics: Arc<TurboTransportMetrics>,
    /// Message deduplication cache
    dedup_cache: Arc<RwLock<DeduplicationCache>>,
}

impl TurboTransport {
    /// Create a new TurboTransport wrapper
    pub fn new(
        transport: Box<dyn Transport>,
        retry_config: RetryConfig,
        circuit_config: CircuitBreakerConfig,
        health_config: HealthCheckConfig,
    ) -> Self {
        let circuit_breaker = Arc::new(Mutex::new(CircuitBreaker::new(circuit_config)));
        let health_checker = Arc::new(Mutex::new(HealthChecker::new(health_config)));
        let metrics = Arc::new(TurboTransportMetrics::default());
        let dedup_cache = Arc::new(RwLock::new(DeduplicationCache::new(
            1000,
            Duration::from_secs(300),
        )));

        // Snapshot type/endpoint up front — the inner transport is `Box<dyn>`
        // so we can call its sync accessors before wrapping it in the Mutex.
        let cached_transport_type = transport.transport_type();
        let cached_endpoint = transport.endpoint();

        Self {
            inner: Arc::new(Mutex::new(transport)),
            cached_transport_type,
            cached_endpoint,
            retry_config,
            circuit_breaker,
            health_checker,
            metrics,
            dedup_cache,
        }
    }

    /// Create TurboTransport with default configurations
    pub fn with_defaults(transport: Box<dyn Transport>) -> Self {
        Self::new(
            transport,
            RetryConfig::default(),
            CircuitBreakerConfig::default(),
            HealthCheckConfig::default(),
        )
    }

    /// Execute operation with retry logic
    async fn execute_with_retry<F, Fut, T>(&self, mut operation: F) -> TransportResult<T>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = TransportResult<T>>,
    {
        let mut attempt = 0;
        let mut last_error = None;

        while attempt < self.retry_config.max_attempts {
            // Check circuit breaker
            {
                let mut breaker = self.circuit_breaker.lock().await;
                if !breaker.should_allow_operation() {
                    self.metrics
                        .circuit_breaker_trips
                        .fetch_add(1, Ordering::Relaxed);
                    return Err(TransportError::Internal(
                        "Circuit breaker is open".to_string(),
                    ));
                }
            }

            let start_time = Instant::now();
            let result = operation().await;
            let duration = start_time.elapsed();

            // Update latency metrics
            self.metrics.update_latency(duration.as_micros() as u64);

            // Record circuit breaker result
            {
                let mut breaker = self.circuit_breaker.lock().await;
                breaker.record_result(result.is_ok(), duration);
                self.metrics.update_circuit_state(breaker.state()).await;
            }

            match result {
                Ok(value) => {
                    if attempt > 0 {
                        self.metrics.record_successful_retry();
                    }
                    return Ok(value);
                }
                Err(error) => {
                    if !self.should_retry(&error, attempt) {
                        return Err(error);
                    }

                    last_error = Some(error);
                    attempt += 1;

                    if attempt < self.retry_config.max_attempts {
                        self.metrics.record_retry_attempt();
                        let delay = self.retry_config.calculate_delay(attempt);
                        sleep(delay).await;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            TransportError::Internal("Maximum retry attempts exceeded".to_string())
        }))
    }

    /// Check if error should trigger a retry
    fn should_retry(&self, error: &TransportError, attempt: u32) -> bool {
        if attempt >= self.retry_config.max_attempts {
            return false;
        }

        // Use the retry configuration to determine if we should retry
        let error_str = error.to_string();
        self.retry_config.should_retry(&error_str, attempt)
    }

    /// Start background health monitoring
    pub async fn start_health_monitoring(&self) {
        let health_checker = self.health_checker.clone();
        let metrics = self.metrics.clone();
        let transport = self.inner.clone();
        // Honor the configured interval rather than a hardcoded value.
        let interval = {
            let checker = self.health_checker.lock().await;
            checker.config().interval
        };

        tokio::spawn(async move {
            let mut interval_timer = tokio::time::interval(interval);

            loop {
                interval_timer.tick().await;

                // Perform health check on the transport
                let health_status = {
                    let mut checker = health_checker.lock().await;
                    let transport_guard = transport.lock().await;
                    let is_healthy = checker.check_health(&**transport_guard).await;

                    if is_healthy {
                        HealthStatus::Healthy
                    } else {
                        metrics.record_health_check_failure();
                        HealthStatus::Unhealthy
                    }
                };

                metrics.update_health_status(health_status).await;
            }
        });
    }

    /// Get TurboTransport metrics snapshot
    pub async fn get_metrics_snapshot(&self) -> super::metrics::MetricsSnapshot {
        self.metrics.snapshot().await
    }

    /// Get circuit breaker statistics
    pub async fn get_circuit_breaker_stats(&self) -> CircuitBreakerStats {
        let breaker = self.circuit_breaker.lock().await;
        breaker.statistics()
    }

    /// Get health information
    pub async fn get_health_info(&self) -> HealthInfo {
        let checker = self.health_checker.lock().await;
        checker.health_info().clone()
    }

    /// Check if the transport is performing well overall
    pub async fn is_performing_well(&self) -> bool {
        self.metrics.is_performing_well().await
    }

    /// Reset all metrics and states
    pub async fn reset(&self) {
        self.metrics.reset().await;

        let mut breaker = self.circuit_breaker.lock().await;
        breaker.reset();

        let mut checker = self.health_checker.lock().await;
        checker.reset();

        let mut dedup = self.dedup_cache.write().await;
        dedup.clear();
    }
}

impl Transport for TurboTransport {
    fn transport_type(&self) -> TransportType {
        // Lock-free: returned from the construction-time snapshot. Previously
        // this fell back to `try_lock` + `Stdio` on contention which produced
        // wrong metrics under load.
        self.cached_transport_type
    }

    fn capabilities(&self) -> &crate::core::TransportCapabilities {
        // Use a static default since capabilities are typically the same for all transports
        // of the same type and this is a sync method that can't access the inner transport
        static DEFAULT_CAPABILITIES: std::sync::LazyLock<crate::core::TransportCapabilities> =
            std::sync::LazyLock::new(crate::core::TransportCapabilities::default);
        &DEFAULT_CAPABILITIES
    }

    fn state(&self) -> Pin<Box<dyn Future<Output = TransportState> + Send + '_>> {
        Box::pin(async move {
            let inner = self.inner.lock().await;
            inner.state().await
        })
    }

    fn connect(&self) -> Pin<Box<dyn Future<Output = TransportResult<()>> + Send + '_>> {
        Box::pin(async move {
            let inner = self.inner.clone();
            self.execute_with_retry(move || {
                let inner = inner.clone();
                async move {
                    let transport = inner.lock().await;
                    transport.connect().await
                }
            })
            .await
        })
    }

    fn disconnect(&self) -> Pin<Box<dyn Future<Output = TransportResult<()>> + Send + '_>> {
        Box::pin(async move {
            let inner = self.inner.lock().await;
            inner.disconnect().await
        })
    }

    fn send(
        &self,
        message: TransportMessage,
    ) -> Pin<Box<dyn Future<Output = TransportResult<()>> + Send + '_>> {
        Box::pin(async move {
            // SECURITY/CORRECTNESS: This is **not** an idempotency guarantee.
            //
            // We track recently observed message IDs purely to drop accidental
            // sender-side duplicates (e.g. an upstream layer fanning out the
            // same payload). Dropping is silent and treated as success because
            // the dedup window is best-effort and the caller has *already*
            // expressed intent to send this id once.
            //
            // Callers that need at-least-once redelivery semantics MUST mint a
            // fresh `MessageId` per attempt — reusing the previous id will be
            // suppressed here.
            {
                let mut dedup = self.dedup_cache.write().await;
                if dedup.is_duplicate(&message.id.to_string()) {
                    self.metrics.record_duplicate_filtered();
                    tracing::debug!(
                        message_id = %message.id,
                        "Suppressed duplicate send (sender-side dedup window hit)"
                    );
                    return Ok(());
                }
            }

            let inner = self.inner.clone();
            let msg = message.clone();
            self.execute_with_retry(move || {
                let inner = inner.clone();
                let msg = msg.clone();
                async move {
                    let transport = inner.lock().await;
                    transport.send(msg).await
                }
            })
            .await
        })
    }

    fn receive(
        &self,
    ) -> Pin<Box<dyn Future<Output = TransportResult<Option<TransportMessage>>> + Send + '_>> {
        Box::pin(async move {
            let inner = self.inner.clone();
            self.execute_with_retry(move || {
                let inner = inner.clone();
                async move {
                    let transport = inner.lock().await;
                    transport.receive().await
                }
            })
            .await
        })
    }

    fn metrics(&self) -> Pin<Box<dyn Future<Output = TransportMetrics> + Send + '_>> {
        Box::pin(async move {
            let inner = self.inner.lock().await;
            inner.metrics().await
        })
    }

    fn endpoint(&self) -> Option<String> {
        // Lock-free: returned from the construction-time snapshot. Previously
        // this fell back to `try_lock` + `None` on contention which silently
        // hid the endpoint when callers needed it most (high traffic / debug).
        self.cached_endpoint.clone()
    }

    fn configure(
        &self,
        config: TransportConfig,
    ) -> Pin<Box<dyn Future<Output = TransportResult<()>> + Send + '_>> {
        Box::pin(async move {
            let inner = self.inner.lock().await;
            inner.configure(config).await
        })
    }
}
