use arc_swap::ArcSwap;
use std::sync::Arc;
use tempfile::tempdir;

#[tokio::test]
async fn test_sandbox_creation_in_initialize() {
    let workspace = tempdir().expect("create workspace tempdir");
    let instance_dir = tempdir().expect("create instance tempdir");
    let data_dir = tempdir().expect("create data tempdir");
    let config = Arc::new(ArcSwap::from_pointee(
        spacebot::sandbox::SandboxConfig::default(),
    ));

    let sandbox = spacebot::sandbox::Sandbox::new(
        config,
        workspace.path().to_path_buf(),
        instance_dir.path(),
        data_dir.path().to_path_buf(),
        Arc::from("test-agent"),
    )
    .await;

    let _backend = spacebot::sandbox::detect_backend().await;

    assert!(sandbox.mode_enabled());
}
