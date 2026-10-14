use hikari_runtime::{AppConfig, ConfigStore, PersonaStore, SessionStore};

#[test]
fn role_card_migrates_name_is_shared_and_external_edits_change_next_prompt() {
    let temp = tempfile::tempdir().unwrap();
    let store = PersonaStore::new(temp.path());
    let initial = store.initialize("月").unwrap();
    assert!(initial.contains("你是月"));
    store
        .save("# 角色\n你是星，幽默而认真。\n# 示例\n用户：你好\n星：来了。")
        .unwrap();
    assert!(store.prompt().unwrap().contains("你是星"));
    assert!(store.path().with_extension("md.bak").exists());
    std::fs::write(store.path(), "# 角色\n你是海。").unwrap();
    assert!(store.prompt().unwrap().contains("你是海"));
    assert!(store.save("").is_err());
    assert!(store.load().unwrap().contains("海"));
}

#[test]
fn legacy_sessions_enable_future_memory_without_backfill() {
    let temp = tempfile::tempdir().unwrap();
    let config = ConfigStore::new(temp.path());
    config.save(&AppConfig::default()).unwrap();
    assert!(config.load().unwrap().memory.enabled);
    std::fs::write(temp.path().join("sessions.json"),r#"[{"id":1,"title":"old","transcript":[{"role":"user","text":"old"}],"context":[{"role":"user","text":"old"}],"retained_until":0,"usage":{"input":0,"output":0}}]"#).unwrap();
    let sessions = SessionStore::new(temp.path()).load().unwrap();
    assert!(sessions[0].memory_enabled);
    assert_eq!(sessions[0].retained_until, 1);
}

#[test]
fn crash_checkpoint_is_recovered_once_and_disabled_history_is_skipped() {
    use hikari_agent::{Message, Usage};
    use hikari_runtime::{memory::recover_memory, MemoryCore, MemoryModels, Session};
    let temp = tempfile::tempdir().unwrap();
    let config = AppConfig::default();
    let models = MemoryModels::new(&config);
    let (core, _) = MemoryCore::open(&temp.path().join("m.db"), true).unwrap();
    let mut sessions = vec![Session {
        id: 2,
        title: "crash".into(),
        workspace: None,
        pinned: false,
        transcript: vec![],
        context: vec![Message::user("待提交")],
        retained_until: 0,
        context_base: 20,
        memory_enabled: true,
        usage: Usage::default(),
        execution: Default::default(),
    }];
    assert!(recover_memory(&core, &models, &config, &mut sessions).is_empty());
    assert_eq!(sessions[0].retained_until, 1);
    assert_eq!(core.status().unwrap()["documents"], 1);
    assert!(recover_memory(&core, &models, &config, &mut sessions).is_empty());
    assert_eq!(core.status().unwrap()["documents"], 1);
    sessions[0].memory_enabled = false;
    sessions[0].context.push(Message::user("不记忆"));
    recover_memory(&core, &models, &config, &mut sessions);
    sessions[0].memory_enabled = true;
    recover_memory(&core, &models, &config, &mut sessions);
    assert_eq!(core.status().unwrap()["documents"], 1);
}
