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
            // M17: network accounting summed across the container's
            // interfaces (the workload integrations' §4
            // ResourceUsage.network_bytes derivation). A container
            // with no reported networks (e.g. host-network mode
            // before veth accounting) reports None, not zero —
            // "unknown" and "nothing" stay distinguishable.
            network_rx_bytes: sample.networks.as_ref().map(sum_rx),
            network_tx_bytes: sample.networks.as_ref().map(sum_tx),
        }),
        Some(Err(e)) => Err(map_error("container stats", e)),
        // An empty stats stream means the container is gone; a
        // missing container has no usage to report.
        None => Ok(ResourceUsage::default()),
    }
}

/// Sum the receive counters across every interface.
fn sum_rx(
    networks: &std::collections::HashMap<String, bollard::models::ContainerNetworkStats>,
) -> u64 {
    networks
        .values()
        .filter_map(|n| n.rx_bytes)
        .fold(0u64, u64::saturating_add)
}

/// Sum the transmit counters across every interface.
fn sum_tx(
    networks: &std::collections::HashMap<String, bollard::models::ContainerNetworkStats>,
) -> u64 {
    networks
        .values()
        .filter_map(|n| n.tx_bytes)
        .fold(0u64, u64::saturating_add)
}
