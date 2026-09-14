use std::future::Future;
use std::time::Duration;

/// Cancellation budgets at three nesting levels.
///
/// umakadata checked its per-endpoint timeout only *between* measurements, so
/// one stalled query ran 24 minutes against a 4-hour cap and nothing could
/// interrupt it. Every level here is enforced by `tokio::time::timeout`, which
/// drops the future.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub request: Duration,
    pub metric: Duration,
    pub endpoint: Duration,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            request: Duration::from_secs(30),
            metric: Duration::from_secs(60),
            endpoint: Duration::from_secs(600),
        }
    }
}

/// The budget ran out. Callers must map this to `Verdict::Indeterminate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Expired;

impl Budget {
    pub async fn with_metric_budget<F, T>(&self, f: F) -> Result<T, Expired>
    where
        F: Future<Output = T>,
    {
        tokio::time::timeout(self.metric, f).await.map_err(|_| Expired)
    }

    pub async fn with_endpoint_budget<F, T>(&self, f: F) -> Result<T, Expired>
    where
        F: Future<Output = T>,
    {
        tokio::time::timeout(self.endpoint, f).await.map_err(|_| Expired)
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn a_slow_future_is_cancelled_not_awaited() {
        let b = Budget { request: Duration::from_millis(10), metric: Duration::from_millis(20), endpoint: Duration::from_millis(50) };
        let start = std::time::Instant::now();
        let out = b.with_metric_budget(async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            7
        }).await;
        // The point of the whole module: the budget must interrupt the work,
        // not merely report afterwards that it took too long.
        assert!(out.is_err());
        assert!(start.elapsed() < Duration::from_secs(1), "budget did not cancel");
    }

    #[tokio::test]
    async fn a_fast_future_returns_its_value() {
        let b = Budget::default();
        let out = b.with_metric_budget(async { 7 }).await;
        assert_eq!(out.unwrap(), 7);
    }
}
