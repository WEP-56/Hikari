use hikari_agent::{AttachmentStore, Context, Message, ProtocolProvider, Provider, ProviderConfig};
use std::sync::Arc;

#[test]
fn concurrent_imports_are_content_addressed_and_tampering_is_detected() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(AttachmentStore::new(temp.path().into()));
    let bytes = b"\x89PNG\r\n\x1a\nfixture";
    let threads: Vec<_> = (0..12)
        .map(|_| {
            let store = store.clone();
            std::thread::spawn(move || store.import("fixture.png", "image/png", bytes).unwrap())
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(results.iter().all(|a| a == &results[0]));
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    assert!(results[0]
        .data_url()
        .unwrap()
        .starts_with("data:image/png;base64,"));
    std::fs::write(&results[0].path, b"changed").unwrap();
    assert!(results[0].data_url().is_err());
    assert!(store.get_image(&results[0].id).is_err());
    assert!(store.get_image("../outside").is_err());
}

#[test]
fn vision_request_budget_counts_images_without_tokenizing_base64() {
    let temp = tempfile::tempdir().unwrap();
    let store = AttachmentStore::new(temp.path().into());
    let mut bytes = vec![0; 1024 * 1024];
    bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    let image = store.import("photo.png", "image/png", &bytes).unwrap();
    let provider = ProtocolProvider::new(ProviderConfig::default(), "").unwrap();
    let plain = [Message::user("look")];
    let base = provider.input_tokens(&Context {
        system_prompt: "system",
        messages: &plain,
        tools: &[],
    });
    let mut with_image = plain.clone();
    with_image[0].set_attachments(vec![image]);
    let estimated = provider.input_tokens(&Context {
        system_prompt: "system",
        messages: &with_image,
        tools: &[],
    });
    assert_eq!(estimated - base, 32_000);
    assert!(
        estimated < 128_000,
        "one large encoded image should fit a normal window"
    );
}
