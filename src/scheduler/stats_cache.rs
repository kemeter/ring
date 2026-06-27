//! Background cache of per-deployment runtime resource usage.
//!
//! Querying a runtime for instance stats (`get_instance_stats`) hits a socket
//! per deployment and is comparatively expensive, so we do NOT do it on the
//! `/metrics` request path: a Prometheus scrape every 15s across N deployments
//! would mean N runtime round-trips per scrape and could blow the scraper's
//! timeout. Instead a standalone worker refreshes the numbers on the scheduler
//! interval into an in-memory store, and `/metrics` reads that store
//! instantly. The cost of talking to the runtimes is decoupled from the scrape
//! frequency.
//!
//! The trade-off is freshness: values are at most one refresh interval stale.
//! For capacity/resource dashboards that is fine; for anything needing
//! per-request freshness, query `/deployments/{id}/metrics` directly.
//!
//! Fail-soft: a runtime that is slow or unreachable has its deployments omitted
//! from the snapshot (logged), never stalling the refresh or the scrape.

use crate::api::dto::stats::InstanceStatsOutput;
use crate::api::server::RuntimeMap;
use crate::models::deployments;
use futures::stream::{self, StreamExt};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::time::{sleep, timeout};

/// Per-deployment aggregate, ready to render as labelled Prometheus series.
/// `name`/`namespace`/`runtime` are the label set; the couple
/// (namespace, name) is unique, so no unstable `id` is needed as a label.
#[derive(Debug, Clone)]
pub(crate) struct DeploymentRuntimeStats {
    pub name: String,
    pub namespace: String,
    pub runtime: String,
    pub instance_count: u64,
    pub cpu_usage_percent: f64,
    pub memory_usage_bytes: u64,
    pub memory_limit_bytes: u64,
    pub network_rx_bytes: u64,
    pub network_tx_bytes: u64,
    pub disk_read_bytes: u64,
    pub disk_write_bytes: u64,
    pub pids: u64,
    pub restarts: u64,
}

/// In-memory snapshot read by `/metrics`. Replaced wholesale on each refresh so
/// readers never observe a half-updated set.
#[derive(Debug, Default)]
pub(crate) struct StatsSnapshot {
    /// Unix timestamp (seconds) of the last successful refresh. `0` if never.
    pub last_refresh_unix: u64,
    pub deployments: Vec<DeploymentRuntimeStats>,
}

/// Shared handle to the snapshot. Cheap to clone (an `Arc`); stored in
/// `AppState` and handed to the refresh worker.
pub(crate) type StatsCache = Arc<RwLock<StatsSnapshot>>;

pub(crate) fn new_cache() -> StatsCache {
    Arc::new(RwLock::new(StatsSnapshot::default()))
}

/// Per-deployment ceiling on the `get_instance_stats` call. A wedged runtime
/// must not hold the whole refresh; its deployment is dropped from this round.
const PER_DEPLOYMENT_TIMEOUT: Duration = Duration::from_secs(5);

/// Max number of `get_instance_stats` calls in flight at once. The calls are
/// independent socket round-trips, so running them concurrently makes the
/// refresh duration depend on the number of *waves* (deployments / this) rather
/// than the total deployment count. Bounded so a node with hundreds of
/// deployments doesn't open hundreds of runtime sockets at once.
const REFRESH_CONCURRENCY: usize = 24;

/// Run the refresh loop forever, polling on `interval_secs` (the scheduler
/// interval). Each tick recomputes the snapshot and swaps it in.
pub(crate) async fn run(
    cache: StatsCache,
    pool: SqlitePool,
    runtimes: RuntimeMap,
    interval_secs: u64,
    now_unix: impl Fn() -> u64 + Send,
) {
    let tick = Duration::from_secs(interval_secs.max(1));
    loop {
        refresh(&cache, &pool, &runtimes, now_unix()).await;
        sleep(tick).await;
    }
}

/// Recompute the snapshot once and swap it into the cache. Extracted from the
/// loop so it is directly testable with a mock runtime.
pub(crate) async fn refresh(
    cache: &StatsCache,
    pool: &SqlitePool,
    runtimes: &RuntimeMap,
    now_unix: u64,
) {
    let mut filters: HashMap<String, Vec<String>> = HashMap::new();
    filters.insert("status".to_string(), vec!["running".to_string()]);

    let active = match deployments::find_all(pool, filters).await {
        Ok(d) => d,
        Err(e) => {
            warn!("stats cache: listing active deployments failed: {}", e);
            return;
        }
    };

    // Query the runtimes concurrently: each `get_instance_stats` is an
    // independent socket round-trip, and doing them in series made the refresh
    // duration grow linearly with the deployment count (hundreds of
    // deployments → minutes per cycle). `buffer_unordered` keeps at most
    // `REFRESH_CONCURRENCY` in flight, so the cycle scales with the number of
    // waves instead. Order is irrelevant: the snapshot is an unordered set.
    let out: Vec<DeploymentRuntimeStats> = stream::iter(active)
        .map(|deployment| async move {
            let runtime = runtimes.get(&deployment.runtime)?;

            let stats = match timeout(
                PER_DEPLOYMENT_TIMEOUT,
                runtime.get_instance_stats(&deployment.id),
            )
            .await
            {
                Ok(stats) => stats,
                Err(_) => {
                    warn!(
                        "stats cache: get_instance_stats timed out for {} ({}), skipping this round",
                        deployment.id, deployment.runtime
                    );
                    return None;
                }
            };

            if stats.is_empty() {
                // No live instances (or the runtime reports none) — omit rather
                // than emit an all-zero series for a deployment with no data.
                return None;
            }

            Some(aggregate(
                &deployment.name,
                &deployment.namespace,
                &deployment.runtime,
                &stats,
            ))
        })
        .buffer_unordered(REFRESH_CONCURRENCY)
        .filter_map(|row| async move { row })
        .collect()
        .await;

    match cache.write() {
        Ok(mut guard) => {
            guard.last_refresh_unix = now_unix;
            guard.deployments = out;
        }
        Err(e) => warn!("stats cache: snapshot lock poisoned: {}", e),
    }
}

/// Sum a deployment's instance stats into a single labelled row. CPU and byte
/// totals add across instances; memory percentage is intentionally not summed
/// (it is derived from usage/limit by the consumer when needed).
fn aggregate(
    name: &str,
    namespace: &str,
    runtime: &str,
    instances: &[InstanceStatsOutput],
) -> DeploymentRuntimeStats {
    DeploymentRuntimeStats {
        name: name.to_string(),
        namespace: namespace.to_string(),
        runtime: runtime.to_string(),
        instance_count: instances.len() as u64,
        cpu_usage_percent: instances.iter().map(|i| i.cpu_usage_percent).sum(),
        memory_usage_bytes: instances.iter().map(|i| i.memory.usage_bytes).sum(),
        memory_limit_bytes: instances.iter().map(|i| i.memory.limit_bytes).sum(),
        network_rx_bytes: instances.iter().map(|i| i.network.rx_bytes).sum(),
        network_tx_bytes: instances.iter().map(|i| i.network.tx_bytes).sum(),
        disk_read_bytes: instances.iter().map(|i| i.disk_io.read_bytes).sum(),
        disk_write_bytes: instances.iter().map(|i| i.disk_io.write_bytes).sum(),
        pids: instances.iter().map(|i| i.pids.current).sum(),
        restarts: instances.iter().map(|i| i.restart_count).sum(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::dto::stats::{DiskIoStats, MemoryStats, NetworkStats, PidStats};

    fn instance(cpu: f64, mem: u64, rx: u64) -> InstanceStatsOutput {
        InstanceStatsOutput {
            instance_id: "i".to_string(),
            instance_name: "i".to_string(),
            cpu_usage_percent: cpu,
            memory: MemoryStats {
                usage_bytes: mem,
                limit_bytes: mem * 2,
                usage_percent: 50.0,
            },
            network: NetworkStats {
                rx_bytes: rx,
                tx_bytes: 0,
                rx_packets: 0,
                tx_packets: 0,
            },
            disk_io: DiskIoStats {
                read_bytes: 0,
                write_bytes: 0,
            },
            pids: PidStats {
                current: 1,
                limit: 100,
            },
            restart_count: 2,
        }
    }

    #[test]
    fn aggregate_sums_across_instances() {
        let stats = vec![instance(10.0, 100, 5), instance(20.0, 200, 7)];
        let agg = aggregate("web", "prod", "docker", &stats);
        assert_eq!(agg.name, "web");
        assert_eq!(agg.namespace, "prod");
        assert_eq!(agg.runtime, "docker");
        assert_eq!(agg.instance_count, 2);
        assert_eq!(agg.cpu_usage_percent, 30.0);
        assert_eq!(agg.memory_usage_bytes, 300);
        assert_eq!(agg.memory_limit_bytes, 600);
        assert_eq!(agg.network_rx_bytes, 12);
        assert_eq!(agg.pids, 2);
        assert_eq!(agg.restarts, 4);
    }

    #[test]
    fn new_cache_starts_empty() {
        let cache = new_cache();
        let guard = cache.read().unwrap();
        assert_eq!(guard.last_refresh_unix, 0);
        assert!(guard.deployments.is_empty());
    }

    use crate::hypervisor::lifecycle_trait::RuntimeLifecycle;
    use crate::hypervisor::mock::MockRuntime;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    async fn insert_deployment(pool: &SqlitePool, id: &str, name: &str, status: &str) {
        sqlx::query(
            "INSERT INTO deployment (id, created_at, status, namespace, runtime, kind, name) \
             VALUES (?, '2024-01-01', ?, 'prod', 'docker', 'worker', ?)",
        )
        .bind(id)
        .bind(status)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn refresh_caches_stats_for_running_deployments_only() {
        let pool = test_pool().await;
        insert_deployment(&pool, "d-run", "web", "running").await;
        // A non-running deployment must be ignored even though the runtime
        // would happily return stats for it.
        insert_deployment(&pool, "d-stop", "old", "completed").await;

        let runtime: Arc<dyn RuntimeLifecycle> =
            Arc::new(MockRuntime::healthy().with_instance_stats(vec![instance(10.0, 100, 5)]));
        let mut map: HashMap<String, Arc<dyn RuntimeLifecycle>> = HashMap::new();
        map.insert("docker".to_string(), runtime);
        let runtimes: RuntimeMap = Arc::new(map);

        let cache = new_cache();
        refresh(&cache, &pool, &runtimes, 1_700_000_000).await;

        let guard = cache.read().unwrap();
        assert_eq!(guard.last_refresh_unix, 1_700_000_000);
        assert_eq!(guard.deployments.len(), 1, "only the running one is cached");
        let row = &guard.deployments[0];
        assert_eq!(row.name, "web");
        assert_eq!(row.namespace, "prod");
        assert_eq!(row.runtime, "docker");
        assert_eq!(row.cpu_usage_percent, 10.0);
        assert_eq!(row.memory_usage_bytes, 100);
    }

    #[tokio::test]
    async fn refresh_queries_deployments_concurrently() {
        // With a per-call delay and many deployments, a sequential refresh would
        // take roughly count * delay. The concurrent refresh finishes in about
        // one wave (ceil(count / REFRESH_CONCURRENCY) * delay). We use a short
        // real delay and assert the wall-clock time stays far below the
        // sequential cost, which proves the calls overlap.
        let pool = test_pool().await;
        let count = REFRESH_CONCURRENCY + 5;
        for i in 0..count {
            insert_deployment(&pool, &format!("d-{i}"), &format!("web-{i}"), "running").await;
        }

        let delay = Duration::from_millis(50);
        let runtime: Arc<dyn RuntimeLifecycle> = Arc::new(
            MockRuntime::healthy()
                .with_instance_stats(vec![instance(10.0, 100, 5)])
                .with_stats_delay(delay),
        );
        let mut map: HashMap<String, Arc<dyn RuntimeLifecycle>> = HashMap::new();
        map.insert("docker".to_string(), runtime);
        let runtimes: RuntimeMap = Arc::new(map);

        let cache = new_cache();
        let start = std::time::Instant::now();
        refresh(&cache, &pool, &runtimes, 1_700_000_000).await;
        let elapsed = start.elapsed();

        // Sequential would be count * delay (~1450ms); two waves are ~100ms.
        // A generous ceiling keeps the test robust on slow CI while still
        // failing loudly if the refresh ever goes back to serial.
        let sequential = delay * count as u32;
        assert!(
            elapsed < sequential / 3,
            "refresh took {elapsed:?}; sequential would be ~{sequential:?}, so the calls are not overlapping",
        );

        let guard = cache.read().unwrap();
        assert_eq!(guard.deployments.len(), count, "every deployment is cached");
    }

    #[tokio::test]
    async fn refresh_omits_deployments_with_no_instance_stats() {
        let pool = test_pool().await;
        insert_deployment(&pool, "d-run", "web", "running").await;

        // Runtime returns no stats (e.g. no live instances) → no series.
        let runtime: Arc<dyn RuntimeLifecycle> = Arc::new(MockRuntime::healthy());
        let mut map: HashMap<String, Arc<dyn RuntimeLifecycle>> = HashMap::new();
        map.insert("docker".to_string(), runtime);
        let runtimes: RuntimeMap = Arc::new(map);

        let cache = new_cache();
        refresh(&cache, &pool, &runtimes, 1).await;

        let guard = cache.read().unwrap();
        assert!(guard.deployments.is_empty());
        // The refresh still ran, so the timestamp advances.
        assert_eq!(guard.last_refresh_unix, 1);
    }
}
