//! mylar3 service backend — Mylar3 comic book (CBR/CBZ) downloader & manager.
//!
//! Implements `ServiceBackend` so the generic `service.*` tools
//! (deploy/backup/restore/configure/status/connect/sync) drive mylar3. No
//! `#[orca_tool]`s — the only orca dep is `plugin-toolkit`. Modeled on the
//! nfs StorageBackend. See orca/docs/PLUGIN-PROGRAM.md.
#![allow(clippy::disallowed_types)]

use plugin_toolkit::service::{
    BoxFuture, Endpoint, Runtime, ServiceBackend, ServiceCapability, ServiceError, ServiceStatus,
    WorkloadSpec,
};

mod abi_export;

/// mylar3 backend. Holds only the provider name; per-instance endpoint/creds
/// come from the `Endpoint` the generic `service.*` tools hand each op.
#[derive(Debug, Clone)]
pub struct Mylar3Backend {
    provider: &'static str,
}

impl Mylar3Backend {
    pub fn new(provider: &'static str) -> Self {
        Self { provider }
    }
}

impl ServiceBackend for Mylar3Backend {
    fn provider(&self) -> &str { self.provider }

    /// Runtimes mylar3 can be placed on. `service.deploy` hands the
    /// `workload_spec` below to a matching deploy target — this backend never
    /// drives pct/docker itself (that mechanic lives in the deploy-target domain).
    fn runtimes(&self) -> Vec<Runtime> { vec![Runtime::Docker, Runtime::Podman, Runtime::Lxc] }

    fn capabilities(&self) -> Vec<ServiceCapability> { vec![ServiceCapability::Deploy, ServiceCapability::Backup, ServiceCapability::Restore, ServiceCapability::Configure, ServiceCapability::Status] }

    fn default_port(&self) -> u16 { 8090 }

    /// In-workload paths holding config/data. This is ALL mylar3 declares for
    /// backup — the generic pluggable backup (tar for containers/LXC, PBS for
    /// Proxmox guests when available) snapshots these. No backup/restore code
    /// here; those are inherited from ServiceBackend's defaults.
    fn data_paths(&self) -> Vec<String> { vec!["/config".to_string()] }

    fn workload_spec<'a>(&'a self, _runtime: Runtime, _ep: &'a Endpoint)
        -> BoxFuture<'a, Result<WorkloadSpec, ServiceError>>
    {
        // TODO: describe the mylar3 workload (image/template, ports, mounts,
        // env) for the chosen runtime. The deploy target turns this into a
        // compose service / LXC config / VM. See deploy-target::WorkloadSpec.
        Box::pin(async move { Err(ServiceError::unimplemented("mylar3.workload_spec")) })
    }

    fn configure<'a>(&'a self, _ep: &'a Endpoint, _config: &'a str)
        -> BoxFuture<'a, Result<(), ServiceError>>
    {
        // TODO: apply mylar3-specific config idempotently.
        Box::pin(async move { Err(ServiceError::unimplemented("mylar3.configure")) })
    }

    fn status<'a>(&'a self, _ep: &'a Endpoint)
        -> BoxFuture<'a, Result<ServiceStatus, ServiceError>>
    {
        // TODO: real health/diagnostics.
        Box::pin(async move { Err(ServiceError::unimplemented("mylar3.status")) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declares_provider() {
        let b = Mylar3Backend::new("mylar3");
        assert_eq!(b.provider(), "mylar3");
    }
}
