use std::sync::Arc;

use async_trait::async_trait;
use visor_types::{
    BuildOutput, BuildRequest, BuildService, ExecRequest, ExecResult, ExecutionBackend, VmConfig,
    VmInfo,
};

/// Minimal stub backend for smoke-testing the router construction.
#[derive(Debug)]
struct StubBackend;

#[async_trait]
impl ExecutionBackend for StubBackend {
    async fn create(&self, _config: VmConfig) -> anyhow::Result<VmInfo> {
        unimplemented!()
    }
    async fn list(&self) -> anyhow::Result<Vec<VmInfo>> {
        Ok(Vec::new())
    }
    async fn get(&self, _id: &str) -> anyhow::Result<VmInfo> {
        unimplemented!()
    }
    async fn exec(&self, _id: &str, _req: ExecRequest) -> anyhow::Result<ExecResult> {
        unimplemented!()
    }
    async fn stop(&self, _id: &str, _timeout: u64) -> anyhow::Result<()> {
        unimplemented!()
    }
    async fn kill(&self, _id: &str) -> anyhow::Result<()> {
        unimplemented!()
    }
    async fn destroy(&self, _id: &str) -> anyhow::Result<()> {
        unimplemented!()
    }
    async fn console_output(&self, _id: &str) -> anyhow::Result<Vec<u8>> {
        unimplemented!()
    }
}

#[test]
fn docker_router_builds_without_panic() {
    // This test verifies that all route registrations are valid and don't
    // collide. axum 0.8 panics on duplicate routes, so successful construction
    // means no collisions.
    let backend: Arc<dyn ExecutionBackend> = Arc::new(StubBackend);
    let _router = super::docker_router(backend, None, None);
}

#[test]
fn docker_router_builds_with_build_service() {
    use visor_types::BuildProgress;

    struct StubBuildService;

    #[async_trait]
    impl BuildService for StubBuildService {
        async fn build_image(&self, _req: BuildRequest) -> anyhow::Result<BuildOutput> {
            Ok(BuildOutput::new(
                "sha256:stub".to_owned(),
                vec![BuildProgress::new(1, 1, "FROM scratch".to_owned())],
            ))
        }
    }

    let backend: Arc<dyn ExecutionBackend> = Arc::new(StubBackend);
    let build_service: Arc<dyn BuildService> = Arc::new(StubBuildService);
    let _router = super::docker_router(backend, Some(build_service), None);
}
