//! The `pterodactyl0` instance network (Wings's
//! `environment/docker.go` `ConfigureDocker`/`createDockerNetwork`).

use bollard::models::{NetworkCreateRequest, NetworkInspect};
use bollard::Docker;

use zim_engine_application::supervision::RuntimeError;

use crate::{map_error, DockerSettings};

/// Ensure the instance network exists (creating it with Wings's
/// bridge options when missing) and return its IPv4 gateway (the
/// `SERVER_IP=127.0.0.1` rewrite target).
///
/// Wings's options that carry over verbatim: a non-default bridge
/// named `pterodactyl0`, inter-container communication per
/// `enable_icc`, IP masquerading on, `host_binding_ipv4 =
/// 0.0.0.0`, the bridge interface name, and the MTU. See the crate
/// docs for the two recorded deviations (auto-allocated IPAM subnet,
/// IPv6 default-off).
pub(crate) async fn ensure_network(
    docker: &Docker,
    settings: &DockerSettings,
) -> Result<Option<std::net::Ipv4Addr>, RuntimeError> {
    match docker
        .inspect_network(
            settings.network_name.as_str(),
            None::<bollard::query_parameters::InspectNetworkOptions>,
        )
        .await
    {
        Ok(inspect) => Ok(gateway_of(&inspect)),
        Err(e) if crate::is_not_found(&e) => {
            create_network(docker, settings).await?;
            // Read it back for the gateway (auto-allocated).
            let inspect = docker
                .inspect_network(
                    settings.network_name.as_str(),
                    None::<bollard::query_parameters::InspectNetworkOptions>,
                )
                .await
                .map_err(|e| map_error("network inspect (post-create)", e))?;
            Ok(gateway_of(&inspect))
        }
        Err(e) => Err(map_error("network inspect", e)),
    }
}

/// The network's first IPv4 gateway from its IPAM config.
fn gateway_of(inspect: &NetworkInspect) -> Option<std::net::Ipv4Addr> {
    inspect
        .ipam
        .as_ref()?
        .config
        .as_ref()?
        .iter()
        .filter_map(|entry| entry.gateway.as_deref())
        .find_map(|gateway| gateway.parse().ok())
}

async fn create_network(docker: &Docker, settings: &DockerSettings) -> Result<(), RuntimeError> {
    let options = std::collections::HashMap::from([
        ("encryption".to_owned(), "false".to_owned()),
        (
            "com.docker.network.bridge.default_bridge".to_owned(),
            "false".to_owned(),
        ),
        (
            "com.docker.network.bridge.enable_icc".to_owned(),
            settings.enable_icc.to_string(),
        ),
        (
            "com.docker.network.bridge.enable_ip_masquerade".to_owned(),
            "true".to_owned(),
        ),
        (
            "com.docker.network.bridge.host_binding_ipv4".to_owned(),
            "0.0.0.0".to_owned(),
        ),
        (
            "com.docker.network.bridge.name".to_owned(),
            settings.network_name.clone(),
        ),
        (
            "com.docker.network.driver.mtu".to_owned(),
            settings.network_mtu.to_string(),
        ),
    ]);

    let request = NetworkCreateRequest {
        name: settings.network_name.clone(),
        driver: Some(settings.network_driver.clone()),
        // No IPAM config: Docker auto-allocates the subnet (the
        // documented deviation from Wings's fixed ranges).
        ipam: None,
        enable_ipv6: Some(settings.enable_ipv6),
        internal: Some(false),
        attachable: Some(false),
        ingress: Some(false),
        options: Some(options),
        labels: Some(std::collections::HashMap::from([(
            "Service".to_owned(),
            "ZIM".to_owned(),
        )])),
        ..Default::default()
    };

    match docker.create_network(request).await {
        Ok(_) => Ok(()),
        // 409 conflict: the network appeared between our inspect and
        // create (a concurrent engine boot or test peer). That peer
        // created exactly what we wanted; treat as success.
        Err(e)
            if matches!(
                &e,
                bollard::errors::Error::DockerResponseServerError {
                    status_code: 409,
                    ..
                }
            ) =>
        {
            Ok(())
        }
        Err(e) => Err(map_error("network create", e)),
    }
}
