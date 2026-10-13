//! The one-shot `stats` capability.

use bollard::query_parameters::StatsOptionsBuilder;
use bollard::Docker;
use futures_util::StreamExt;

use zim_engine_application::supervision::{ResourceUsage, RuntimeError};

use crate::map_error;

/// A single resource-usage sample (one-shot stats; the streaming
/// metrics surface is M7's).
pub(crate) async fn stats(docker: &Docker, container: &str) -> Result<ResourceUsage, RuntimeError> {
    let options = StatsOptionsBuilder::default()
        .stream(false)
        .one_shot(true)
        .build();
    let mut stream = docker.stats(container, Some(options));

    match stream.next().await {
        Some(Ok(sample)) => Ok(ResourceUsage {
            cpu_usage_total_ns: sample
                .cpu_stats
                .as_ref()
                .and_then(|c| c.cpu_usage.as_ref())
                .and_then(|u| u.total_usage),
            system_cpu_total_ns: sample.cpu_stats.as_ref().and_then(|c| c.system_cpu_usage),
            online_cpus: sample
                .cpu_stats
                .as_ref()
                .and_then(|c| c.online_cpus)
                .map(u64::from),
            memory_usage_bytes: sample.memory_stats.as_ref().and_then(|m| m.usage),
            memory_limit_bytes: sample.memory_stats.as_ref().and_then(|m| m.limit),
            pids_current: sample.pids_stats.as_ref().and_then(|p| p.current),
        }),
        Some(Err(e)) => Err(map_error("container stats", e)),
        // An empty stats stream means the container is gone; a
        // missing container has no usage to report.
        None => Ok(ResourceUsage::default()),
    }
}
