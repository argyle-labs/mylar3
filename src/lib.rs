#![allow(clippy::disallowed_types)]

pub mod acquisition;
pub mod api;
mod definitions;
pub mod execute;
pub mod ini;
pub mod logs;
pub mod reconcile;
pub mod remediate;
pub mod sab;
pub mod secret;
pub mod status;
pub mod tools;

#[cfg(test)]
mod mock;

use plugin_toolkit::service::{
    BoxFuture, Routes, Runtime, ServiceBackend, ServiceCapability, ServiceError, ServiceStatus,
    WorkloadSpec,
};

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

    fn data_paths(&self) -> Vec<String> {
        vec!["/config".to_string()]
    }

    fn workload_spec<'a>(
        &'a self,
        _runtime: Runtime,
        _instance: &'a str,
        _routes: &'a Routes,
    ) -> BoxFuture<'a, Result<WorkloadSpec, ServiceError>> {
        Box::pin(async move { Err(ServiceError::unimplemented("mylar3.workload_spec")) })
    }

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
