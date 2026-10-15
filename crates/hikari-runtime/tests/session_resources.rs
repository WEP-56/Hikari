use hikari_agent::{AttachmentStore, Message, ToolOutput, Usage};
use hikari_runtime::{Session, SessionStore};
use sha2::{Digest, Sha256};
use std::fs;

fn session(messages: Vec<Message>) -> Session {
    Session {
        id: 1,
        title: "附件会话".into(),
        workspace: None,
        pinned: false,
        transcript: messages.clone(),
        context: messages,
        retained_until: 0,
        context_base: 0,
        memory_enabled: false,
        usage: Usage::default(),
        execution: Default::default(),
    }
}

#[test]
fn session_roundtrip_preserves_attachment_references_and_structured_tool_results() {
    let dir = tempfile::tempdir().unwrap();
    let resources = AttachmentStore::new(dir.path().join("artifacts/talk-1"));
    let image = resources
        .import("输入图.png", "image/png", b"\x89PNG\r\n\x1a\nfixture")
        .unwrap();
    let file = resources
        .import("report.pdf", "application/pdf", b"%PDF-report")
        .unwrap();
    let mut user = Message::user("分析附件");
    user.set_attachments(vec![image.clone()]);
    let result = Message::with_output(
        "call-1",
        "image_gen",
        ToolOutput {
            text: "已生成".into(),
            attachments: vec![image.clone(), file.clone()],
            structured: Some(serde_json::json!({"model":"test","count":1})),
            ..Default::default()
        },
    );
    let record = session(vec![user, result]);
    let expected = record.context.clone();
    let store = SessionStore::new(dir.path());
    store.save(&[record]).unwrap();
    let raw = fs::read_to_string(store.path()).unwrap();
    assert!(!raw.contains("data:image"));
    assert!(!raw.contains("base64"));
    let loaded = store.load().unwrap();
    assert_eq!(loaded[0].context, expected);
    assert_eq!(loaded[0].transcript, expected);
    assert!(loaded[0].context[0].attachments()[0]
        .data_url()
        .unwrap()
        .starts_with("data:image/png;base64,"));
    assert_eq!(loaded[0].context[1].attachments()[1], file);
    // A missing resource preserves the original reference and produces a visible read failure.
    fs::remove_file(&image.path).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded[0].context, expected);
    assert!(loaded[0].context[0].attachments()[0].data_url().is_err());
}

#[test]
fn cleanup_removes_only_matching_hash_resources_from_the_selected_session() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("artifacts/talk-1");
    let store = AttachmentStore::new(target.clone());
    let owned = [
        store
            .import("image.png", "image/png", b"\x89PNG\r\n\x1a\npng")
            .unwrap(),
        store
            .import("image.jpg", "image/jpeg", b"\xff\xd8\xffjpeg")
            .unwrap(),
        store
            .import("image.webp", "image/webp", b"RIFF0000WEBPimage")
            .unwrap(),
        store
            .import("image.gif", "image/gif", b"GIF89aimage")
            .unwrap(),
        store
            .import("file.bin", "application/octet-stream", b"binary")
            .unwrap(),
    ];
    let long_output = "Long tool output with 中文";
    let artifact = target.join(format!("{:x}.txt", Sha256::digest(long_output.as_bytes())));
    fs::write(&artifact, long_output).unwrap();
    let other = AttachmentStore::new(dir.path().join("artifacts/talk-2"))
        .import("image.png", "image/png", b"\x89PNG\r\n\x1a\npng")
        .unwrap();
    let unmanaged = target.join("notes.txt");
    fs::write(&unmanaged, "keep").unwrap();
    let hash_mismatch = target.join(format!("{}.png", "0".repeat(64)));
    fs::write(&hash_mismatch, "keep changed content").unwrap();
    let unknown_extension = target.join(format!("{:x}.exe", Sha256::digest(b"keep executable")));
    fs::write(&unknown_extension, "keep executable").unwrap();
    let nested = target.join("nested");
    fs::create_dir(&nested).unwrap();
    fs::write(nested.join("keep.txt"), "keep").unwrap();
    SessionStore::new(dir.path()).remove_artifacts(1).unwrap();
    assert!(owned.iter().all(|a| !a.path.exists()));
    assert!(!artifact.exists());
    for path in [
        &other.path,
        &unmanaged,
        &hash_mismatch,
        &unknown_extension,
        &nested,
    ] {
        assert!(path.exists(), "{}", path.display());
    }
    // Cleanup uses the session's storage directory, never arbitrary serialized attachment paths.
    let outside = tempfile::NamedTempFile::new().unwrap();
    assert!(outside.path().exists());
    SessionStore::new(dir.path()).remove_artifacts(999).unwrap();
    assert!(outside.path().exists());
}

#[test]
fn deleting_a_session_releases_its_resources_after_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path());
    let image = AttachmentStore::new(dir.path().join("artifacts/talk-1"))
        .import("image.gif", "image/gif", b"GIF89aimage")
        .unwrap();
    let mut user = Message::user("old image");
    user.set_attachments(vec![image.clone()]);
    let mut record = session(vec![user]);
    record.context = vec![Message::ContextSummary {
        text: "compacted".into(),
    }];
    store.save(&[record]).unwrap();
    assert_eq!(
        store.load().unwrap()[0].transcript[0].attachments(),
        &[image.clone()]
    );
    store.save(&[]).unwrap();
    store.remove_artifacts(1).unwrap();
    assert!(store.load().unwrap().is_empty());
    assert!(!image.path.exists());
    assert!(!dir.path().join("artifacts/talk-1").exists());
}

#[test]
fn startup_draft_cleanup_releases_abandoned_attachments_without_touching_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path());
    // A first startup with no prior drafts is harmless.
    store.remove_draft_artifacts().unwrap();
    let draft = AttachmentStore::new(dir.path().join("artifacts/draft"))
        .import("unsent.png", "image/png", b"\x89PNG\r\n\x1a\nunsent")
        .unwrap();
    let sent = AttachmentStore::new(dir.path().join("artifacts/talk-1"))
        .import("sent.png", "image/png", b"\x89PNG\r\n\x1a\nsent")
        .unwrap();
    store.remove_draft_artifacts().unwrap();
    assert!(!draft.path.exists());
    assert!(!dir.path().join("artifacts/draft").exists());
    assert!(sent.path.exists());
    let fresh = AttachmentStore::new(dir.path().join("artifacts/draft"))
        .import("fresh.gif", "image/gif", b"GIF89afresh")
        .unwrap();
    let unknown = dir.path().join("artifacts/draft/notes.txt");
    fs::write(&unknown, "keep user file").unwrap();
    store.remove_draft_artifacts().unwrap();
    assert!(!fresh.path.exists());
    assert!(unknown.exists());
    assert!(sent.path.exists());
}

#[cfg(windows)]
#[test]
fn cleanup_rejects_junction_roots_and_preserves_nested_junction_targets() {
    use std::process::Command;
    fn junction(path: &std::path::Path, target: &std::path::Path) {
        let output = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(path.to_string_lossy().replace('/', "\\"))
            .arg(target)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("artifacts")).unwrap();
    let resource = AttachmentStore::new(outside.path().to_owned())
        .import("file", "text/plain", b"do not remove")
        .unwrap();
    let target = dir.path().join("artifacts/talk-1");
    junction(&target, outside.path());
    let store = SessionStore::new(dir.path());
    assert!(store.remove_artifacts(1).is_err());
    assert!(resource.path.exists());
    fs::remove_dir(&target).unwrap();
    fs::create_dir(&target).unwrap();
    let nested = target.join("nested");
    junction(&nested, outside.path());
    store.remove_artifacts(1).unwrap();
    assert!(resource.path.exists());
    assert!(nested.exists());
    fs::remove_dir(nested).unwrap();
}
