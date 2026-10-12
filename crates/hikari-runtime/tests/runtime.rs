use hikari_agent::{Message, ToolRegistry, Usage};
use hikari_runtime::{AppConfig, ConfigStore, Session, SessionStore, SkillCatalog};
use std::fs;
use std::path::PathBuf;

#[test]
fn configuration_and_session_replace_previous_files_without_losing_transcript() {
    let temp = tempfile::tempdir().unwrap();
    let config_store = ConfigStore::new(temp.path());
    let mut config = AppConfig::default();
    config_store.save(&config).unwrap();
    config.persona_name = "Hikari".into();
    config_store.save(&config).unwrap();
    assert_eq!(config_store.load().unwrap().persona_name, "Hikari");
    let store = SessionStore::new(temp.path());
    let transcript = vec![
        Message::user("original"),
        Message::assistant("done"),
        Message::tool_result("a", "read", "evidence", false),
    ];
    let context = vec![
        Message::ContextSummary {
            text: "summary".into(),
        },
        Message::user("current"),
    ];
    let session = Session {
        id: 1,
        title: "title".into(),
        transcript: transcript.clone(),
        context: context.clone(),
        retained_until: 1,
        usage: Usage {
            input: 10,
            output: 5,
        },
    };
    store.save(&[session.clone()]).unwrap();
    store.save(&[session]).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded[0].transcript, transcript);
    assert_eq!(loaded[0].context, context);
    assert_eq!(loaded[0].usage.total(), 15);
    fs::write(config_store.path(), b"invalid json").unwrap();
    assert!(config_store.load().is_err());
}

#[test]
fn skill_discovery_uses_yaml_and_loads_only_enabled_skills_and_contained_resources() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("skills");
    let path = root.join("sample");
    fs::create_dir_all(path.join("references")).unwrap();
    fs::write(path.join("SKILL.md"),"---\nname: sample\ndescription: >-\n  This is a multiline\n  description.\n---\n\nSkill instructions.").unwrap();
    fs::write(path.join("references/helper.md"), "helper data").unwrap();
    fs::write(root.join("outside.md"), "outside").unwrap();
    let roots = vec![PathBuf::from("skills")];
    let catalog = SkillCatalog::discover(temp.path(), &roots, &[]);
    assert_eq!(catalog.skills.len(), 1);
    assert_eq!(
        catalog.skills[0].description,
        "This is a multiline description."
    );
    assert!(!catalog.prompt().contains("Skill instructions"));
    let mut tools = ToolRegistry::empty();
    catalog.register(&mut tools).unwrap();
    assert!(tools
        .call("load_skill", r#"{"name":"sample"}"#)
        .unwrap()
        .contains("Skill instructions"));
    assert!(tools
        .call(
            "load_skill",
            r#"{"name":"sample","resource":"references/helper.md"}"#
        )
        .unwrap()
        .contains("helper data"));
    assert!(tools
        .call(
            "load_skill",
            r#"{"name":"sample","resource":"../outside.md"}"#
        )
        .is_err());
    let disabled = SkillCatalog::discover(temp.path(), &roots, &[catalog.skills[0].path.clone()]);
    let mut registry = ToolRegistry::empty();
    disabled.register(&mut registry).unwrap();
    assert!(registry.defs().is_empty());
    assert!(disabled.prompt().is_empty());
}
