use spacebot::sandbox::{SandboxBackend, detect_backend};

#[tokio::test]
async fn test_public_sandbox_detection_api_is_accessible() {
    let _backend = detect_backend().await;
    let public_variants = [
        SandboxBackend::Bubblewrap,
        SandboxBackend::SandboxExec,
        SandboxBackend::None,
    ];

    assert_eq!(public_variants.len(), 3);
}
