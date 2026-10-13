use hikari_agent::{Message, Usage};
use hikari_runtime::workspace::{resolve_workspace, WorkspaceStore};
use hikari_runtime::{AppConfig, ConfigStore, ProviderProfile, Session, SessionStore};
use std::fs;

#[test]
fn workspace_history_survives_restart_and_default_directory_does_not_follow_selection() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let first = temp.path().join("项目 A & B");
    let second = temp.path().join("project-2");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();
    let first = resolve_workspace(&first).unwrap();
    let second = resolve_workspace(&second).unwrap();
    let store = WorkspaceStore::new(&data);
    let history = store.remember(&[], &first).unwrap();
    let history = store.remember(&history, &second).unwrap();
    store.remember(&history, &first).unwrap();
    let restarted = WorkspaceStore::new(&data);
    assert_eq!(restarted.load().unwrap(), vec![first, second]);
    assert_eq!(restarted.default_directory(), data.join("chat"));
    assert!(resolve_workspace(&temp.path().join("missing")).is_err());
    let file = temp.path().join("file");
    fs::write(&file, "text").unwrap();
    assert!(resolve_workspace(&file).is_err());
}

#[test]
fn old_sessions_load_without_workspace_or_pin_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let session = Session {
        id: 8,
        title: "旧会话".into(),
        workspace: None,
        pinned: false,
        transcript: vec![Message::user("你好")],
        context: vec![],
        retained_until: 0,
        usage: Usage::default(),
    };
    let mut old = serde_json::to_value(session).unwrap();
    old.as_object_mut().unwrap().remove("workspace");
    old.as_object_mut().unwrap().remove("pinned");
    fs::write(
        temp.path().join("sessions.json"),
        serde_json::to_vec(&vec![old]).unwrap(),
    )
    .unwrap();
    let loaded = SessionStore::new(temp.path()).load().unwrap();
    assert!(loaded[0].workspace.is_none());
    assert!(!loaded[0].pinned);
    assert_eq!(loaded[0].transcript, vec![Message::user("你好")]);
}

#[test]
fn provider_keys_persist_separately_and_empty_keys_remain_empty() {
    let temp = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(temp.path());
    let mut config = AppConfig::default();
    config.providers[0].api_key = "fixture-key-a".into();
    let mut second = ProviderProfile::new("second".into(), "第二个".into(), Default::default());
    second.api_key = "fixture-key-b".into();
    config.providers.push(second);
    store.save(&config).unwrap();
    let mut loaded = store.load().unwrap();
    assert_eq!(loaded.providers[0].api_key, "fixture-key-a");
    assert_eq!(loaded.providers[1].api_key, "fixture-key-b");
    loaded.activate("second").unwrap();
    loaded.providers[1].api_key.clear();
    store.save(&loaded).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded.active_provider, "second");
    assert_eq!(loaded.providers[0].api_key, "fixture-key-a");
    assert!(loaded.providers[1].api_key.is_empty());
    let mut legacy = serde_json::to_value(loaded).unwrap();
    for profile in legacy["providers"].as_array_mut().unwrap() {
        profile.as_object_mut().unwrap().remove("api_key");
    }
    fs::write(store.path(), serde_json::to_vec(&legacy).unwrap()).unwrap();
    assert!(store
        .load()
        .unwrap()
        .providers
        .iter()
        .all(|p| p.api_key.is_empty()));
}
