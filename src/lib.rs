//! mylar3 service backend — Mylar3 comic book (CBR/CBZ) library manager.
//!
//! Implements `ServiceBackend` so the generic `service.*` tools
//! (deploy/backup/restore/configure/status/connect/sync) drive mylar3,
//! alongside the `mylar3.` detect + remediate tools in [`tools`]. The only orca
//! dep is `plugin-toolkit`. See orca/docs/PLUGIN-PROGRAM.md.
#![allow(clippy::disallowed_types)]

pub mod api;
mod definitions;
pub mod execute;
pub mod remediate;
pub mod secret;
pub mod status;
pub mod tools;

use plugin_toolkit::service::{
    BoxFuture, Routes, Runtime, ServiceBackend, ServiceCapability, ServiceError, ServiceStatus,
    WorkloadSpec,
};

/// mylar3 backend. Holds only the provider name; per-instance routes/creds
/// come from the `Routes` the generic `service.*` tools hand each op.
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
    fn provider(&self) -> &str {
        self.provider
    }

    /// Runtimes mylar3 can be placed on. `service.deploy` hands the
    /// `workload_spec` below to a matching deploy target — this backend never
    /// drives pct/docker itself (that mechanic lives in the deploy-target domain).
    fn runtimes(&self) -> Vec<Runtime> {
        vec![Runtime::Docker, Runtime::Podman, Runtime::Lxc]
    }

    fn capabilities(&self) -> Vec<ServiceCapability> {
        vec![
            ServiceCapability::Deploy,
            ServiceCapability::Backup,
            ServiceCapability::Restore,
            ServiceCapability::Configure,
            ServiceCapability::Status,
        ]
    }

    fn default_port(&self) -> u16 {
        8090
    }

    /// In-workload paths holding config/data. This is ALL mylar3 declares for
    /// backup — the generic pluggable backup (tar for containers/LXC, PBS for
    /// Proxmox guests when available) snapshots these. No backup/restore code
    /// here; those are inherited from ServiceBackend's defaults.
    fn data_paths(&self) -> Vec<String> {
        vec!["/config".to_string()]
    }

    fn workload_spec<'a>(
        &'a self,
        _runtime: Runtime,
        _instance: &'a str,
        _routes: &'a Routes,
    ) -> BoxFuture<'a, Result<WorkloadSpec, ServiceError>> {
        // TODO: describe the mylar3 workload (image/template, ports, mounts,
        // env) for the chosen runtime. The deploy target turns this into a
        // compose service / LXC config / VM. See deploy-target::WorkloadSpec.
        Box::pin(async move { Err(ServiceError::unimplemented("mylar3.workload_spec")) })
    }

    /// Settings changes go through `mylar3.configure`, which is admin-only and
    /// dry-run by default; this opaque-string entry point has neither.
    fn configure<'a>(
        &'a self,
        _instance: &'a str,
        _routes: &'a Routes,
        _config: &'a str,
    ) -> BoxFuture<'a, Result<(), ServiceError>> {
        Box::pin(async move {
            Err(ServiceError::Other(
                "mylar3 settings are planned by `mylar3.configure` (admin, dry run by default)"
                    .into(),
            ))
        })
    }

    /// The `mylar3.status` report for the endpoint named `instance`, reduced to
    /// health plus its findings; `ServiceInfo` has no mylar3 variant.
    fn status<'a>(
        &'a self,
        instance: &'a str,
        _routes: &'a Routes,
    ) -> BoxFuture<'a, Result<ServiceStatus, ServiceError>> {
        Box::pin(async move {
            let m = tools::connect(instance)
                .await
                .map_err(|e| ServiceError::Other(format!("{e:#}")))?;
            let s = status::status(instance, &m, tools::DEFAULT_STUCK_HOURS)
                .await
                .map_err(|e| ServiceError::Transport(format!("{e:#}")))?;
            let mut detail = s.findings.join("; ");
            if let Some(e) = &s.config_error {
                if !detail.is_empty() {
                    detail.push_str("; ");
                }
                detail.push_str(&format!("settings unknown: {e}"));
            }
            Ok(ServiceStatus {
                healthy: s.healthy,
                detail,
                ..Default::default()
            })
        })
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
