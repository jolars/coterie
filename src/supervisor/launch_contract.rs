//! Operator evidence for the admitted worker contract, independent of discovery.

use std::fs::File;
use std::io::Read;
use std::sync::OnceLock;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::config::{ConfigLock, EffectiveConfig, PermissionProfile};
use crate::providers::ProviderProbe;

use super::session::{AgentLaunch, required_capabilities};

#[derive(Serialize)]
pub(super) struct LaunchContract<'a> {
    schema_version: u8,
    supervisor: &'static Build,
    provider: ProviderContract,
    bridge: &'static BridgeCatalogue,
    role: &'a str,
    permission_profile: PermissionProfile,
    configuration_fingerprint: String,
}

#[derive(Serialize)]
struct Build {
    version: &'static str,
    protocol_version: u16,
    sha256: Option<String>,
}

#[derive(Serialize)]
struct ProviderContract {
    name: String,
    version: String,
    capabilities: Vec<String>,
    required_capabilities: Vec<String>,
}

#[derive(Serialize)]
struct BridgeCatalogue {
    catalog_sha256: String,
    tools: Vec<String>,
}

pub(super) fn snapshot<'a>(
    probe: &ProviderProbe,
    launch: &'a AgentLaunch,
    configuration: &EffectiveConfig,
) -> LaunchContract<'a> {
    static BUILD: OnceLock<Build> = OnceLock::new();
    static BRIDGE: OnceLock<BridgeCatalogue> = OnceLock::new();
    LaunchContract {
        schema_version: 1,
        supervisor: BUILD.get_or_init(|| Build {
            version: env!("CARGO_PKG_VERSION"),
            protocol_version: crate::protocol::PROTOCOL_VERSION,
            // The installed path may have been replaced since this process started.
            sha256: executable_digest().ok(),
        }),
        provider: ProviderContract {
            name: probe.name.clone(),
            version: probe.version.to_string(),
            capabilities: probe
                .capabilities
                .iter()
                .map(ToString::to_string)
                .collect(),
            required_capabilities: required_capabilities(launch)
                .map(|capability| capability.to_string())
                .collect(),
        },
        bridge: BRIDGE.get_or_init(|| {
            let catalog = crate::mcp::catalog();
            BridgeCatalogue {
                catalog_sha256: hex(&Sha256::digest(
                    serde_json::to_vec(&catalog)
                        .expect("the generated catalogue serializes"),
                )),
                tools: catalog
                    .iter()
                    .map(|t| {
                        t["name"].as_str().expect("tools have names").to_owned()
                    })
                    .collect(),
            }
        }),
        role: &launch.role,
        permission_profile: launch.permission_profile,
        configuration_fingerprint: ConfigLock::for_config(configuration)
            .fingerprint,
    }
}

fn executable_digest() -> std::io::Result<String> {
    let mut file = File::open("/proc/self/exe")?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let length = file.read(&mut buffer)?;
        if length == 0 {
            return Ok(hex(&hash.finalize()));
        }
        hash.update(&buffer[..length]);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
