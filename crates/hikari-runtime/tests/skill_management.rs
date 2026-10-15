use hikari_agent::ToolRegistry;
use hikari_runtime::SkillCatalog;
use serde_json::{json, Value};
use std::fs;

fn manifest(body: &str) -> String {
    format!("---\nname: sample\ndescription: Sample skill\n---\n{body}")
}
fn manage(registry: &ToolRegistry, value: Value) -> Value {
    serde_json::from_str(&registry.call("skill_manage", &value.to_string()).unwrap()).unwrap()
}

#[test]
fn complete_resources_install_update_delete_and_rollback_preserve_running_versions() {
    let temp = tempfile::tempdir().unwrap();
    let roots = vec![".hikari/skills".into()];
    let empty = SkillCatalog::discover(temp.path(), &roots, &[]);
    let mut manager = ToolRegistry::empty();
    empty
        .register_management(&mut manager, temp.path())
        .unwrap();
    let installed = manage(
        &manager,
        json!({"action":"install","name":"sample","files":[
            {"path":"SKILL.md","text":manifest("original")},
            {"path":"scripts/helper.py","text":"print('original')"},
            {"path":"assets/binary.dat","base64":"AAECA/8="}
        ]}),
    );
    assert_eq!(
        fs::read(temp.path().join(".hikari/skills/sample/assets/binary.dat")).unwrap(),
        [0, 1, 2, 3, 255]
    );
    let catalog = SkillCatalog::discover(temp.path(), &roots, &[]);
    let mut running = ToolRegistry::empty();
    catalog.register(&mut running).unwrap();
    let original = running.call("load_skill", r#"{"name":"sample"}"#).unwrap();
    let frozen_base = original
        .lines()
        .find_map(|line| line.strip_prefix("Base directory: "))
        .unwrap();
    assert_eq!(
        fs::read_to_string(std::path::Path::new(frozen_base).join("scripts/helper.py")).unwrap(),
        "print('original')"
    );
    manage(
        &manager,
        json!({"action":"update","name":"sample","files":[{"path":"SKILL.md","text":manifest("updated")}],"remove":["scripts/helper.py"]}),
    );
    assert!(running
        .call("load_skill", r#"{"name":"sample"}"#)
        .unwrap()
        .contains("original"));
    assert!(running
        .call(
            "load_skill",
            r#"{"name":"sample","resource":"scripts/helper.py"}"#
        )
        .unwrap()
        .contains("original"));
    assert!(!temp
        .path()
        .join(".hikari/skills/sample/scripts/helper.py")
        .exists());
    let refreshed = SkillCatalog::discover(temp.path(), &roots, &[]);
    let mut next = ToolRegistry::empty();
    refreshed.register(&mut next).unwrap();
    assert!(next
        .call("load_skill", r#"{"name":"sample"}"#)
        .unwrap()
        .contains("updated"));
    let history = manage(&manager, json!({"action":"history","name":"sample"}));
    assert!(history["revisions"]
        .as_array()
        .unwrap()
        .contains(&installed["revision"]));
    manage(&manager, json!({"action":"delete","name":"sample"}));
    assert!(SkillCatalog::discover(temp.path(), &roots, &[])
        .skills
        .is_empty());
    assert!(running
        .call("load_skill", r#"{"name":"sample"}"#)
        .unwrap()
        .contains("original"));
    manage(
        &manager,
        json!({"action":"rollback","name":"sample","revision":installed["revision"]}),
    );
    assert_eq!(
        fs::read_to_string(temp.path().join(".hikari/skills/sample/scripts/helper.py")).unwrap(),
        "print('original')"
    );
    drop(running);
    assert!(!std::path::Path::new(frozen_base).exists());
}

#[test]
fn invalid_paths_and_manifests_do_not_damage_the_existing_skill() {
    let temp = tempfile::tempdir().unwrap();
    let mut registry = ToolRegistry::empty();
    SkillCatalog::default()
        .register_management(&mut registry, temp.path())
        .unwrap();
    manage(
        &registry,
        json!({"action":"create","name":"sample","files":[{"path":"SKILL.md","text":manifest("intact")}]}),
    );
    for bad in [
        json!({"action":"update","name":"../escape","files":[]}),
        json!({"action":"update","name":"sample","files":[{"path":"../escape","text":"bad"}]}),
        json!({"action":"update","name":"sample","files":[{"path":"C:/escape","text":"bad"}]}),
        json!({"action":"update","name":"sample","files":[{"path":"SKILL.md","text":"invalid"}]}),
        json!({"action":"update","name":"sample","files":[{"path":"SKILL.md","text":manifest("bad"),"base64":"YQ=="}]}),
        json!({"action":"rollback","name":"sample","revision":"../escape"}),
    ] {
        assert!(registry.call("skill_manage", &bad.to_string()).is_err());
    }
    assert!(
        fs::read_to_string(temp.path().join(".hikari/skills/sample/SKILL.md"))
            .unwrap()
            .contains("intact")
    );
}

#[test]
fn skill_index_has_a_fixed_budget_with_many_long_multilingual_descriptions() {
    let temp = tempfile::tempdir().unwrap();
    for index in 0..100 {
        let root = temp.path().join(format!("sample-{index}"));
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("SKILL.md"),
            format!(
                "---\nname: sample-{index}\ndescription: {}\n---\nBody",
                "中文描述很长".repeat(100)
            ),
        )
        .unwrap();
    }
    let catalog = SkillCatalog::discover(temp.path(), &[temp.path().to_owned()], &[]);
    assert_eq!(catalog.skills.len(), 100);
    assert!(catalog.prompt().len() < 3200);
    assert!(catalog.prompt().contains("entries omitted"));
}

#[test]
fn management_requires_permission_and_cancelled_operations_do_not_write() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = SkillCatalog::default();
    let mut guarded = ToolRegistry::workspace(temp.path());
    catalog
        .register_management(&mut guarded, temp.path())
        .unwrap();
    let args = json!({"action":"create","name":"sample","files":[{"path":"SKILL.md","text":manifest("instructions")}]}).to_string();
    assert!(guarded
        .call("skill_manage", &args)
        .unwrap_err()
        .contains("审批"));
    let mut unguarded = ToolRegistry::empty();
    catalog
        .register_management(&mut unguarded, temp.path())
        .unwrap();
    assert!(unguarded
        .call_with_abort(
            "skill_manage",
            &args,
            &std::sync::atomic::AtomicBool::new(true)
        )
        .is_err());
    assert!(!temp.path().join(".hikari/skills/sample").exists());
}

#[test]
fn direct_frozen_resource_reads_retain_sensitive_permission_checks() {
    use hikari_agent::permission::{assess, Decision, PermissionMode};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join(".hikari/skills/sample");
    fs::create_dir_all(root.join("references")).unwrap();
    fs::write(root.join("SKILL.md"), manifest("instructions")).unwrap();
    fs::write(root.join("references/private.md"), "sensitive data").unwrap();
    let catalog = SkillCatalog::discover(temp.path(), &[".hikari/skills".into()], &[]);
    let mut loader = ToolRegistry::empty();
    catalog.register(&mut loader).unwrap();
    let loaded = loader.call("load_skill", r#"{"name":"sample"}"#).unwrap();
    let base = loaded
        .lines()
        .find_map(|line| line.strip_prefix("Base directory: "))
        .unwrap();
    let resource = std::path::Path::new(base).join("references/private.md");
    assert!(matches!(
        assess(
            PermissionMode::Careful,
            temp.path(),
            "read_file",
            &json!({"path":resource})
        )
        .unwrap()
        .0,
        Decision::Ask(_)
    ));
    assert_eq!(
        assess(
            PermissionMode::Autonomous,
            temp.path(),
            "read_file",
            &json!({"path":resource})
        )
        .unwrap()
        .0,
        Decision::Allow
    );
    let guarded = ToolRegistry::workspace(temp.path());
    assert!(guarded
        .call("read_file", &json!({"path":resource}).to_string())
        .unwrap_err()
        .contains("审批"));
}

#[test]
fn frozen_resources_live_until_the_final_child_registry_is_released() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sample");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("SKILL.md"), manifest("instructions")).unwrap();
    let catalog = SkillCatalog::discover(temp.path(), &[root], &[]);
    let mut parent = ToolRegistry::empty();
    catalog.register(&mut parent).unwrap();
    let loaded = parent.call("load_skill", r#"{"name":"sample"}"#).unwrap();
    let base = std::path::PathBuf::from(
        loaded
            .lines()
            .find_map(|line| line.strip_prefix("Base directory: "))
            .unwrap(),
    );
    let execution = hikari_agent::execution::Execution::new("talk-test".into(), Default::default());
    let child = parent.fork_for_execution(execution).unwrap();
    drop(parent);
    assert!(base.join("SKILL.md").exists());
    assert!(child
        .call("load_skill", r#"{"name":"sample"}"#)
        .unwrap()
        .contains("instructions"));
    drop(child);
    assert!(!base.exists());
}

#[test]
fn differently_cased_skill_names_use_distinct_frozen_directories() {
    let temp = tempfile::tempdir().unwrap();
    for (directory, name, resource) in [
        ("first", "sample", "lowercase"),
        ("second", "Sample", "uppercase"),
    ] {
        let root = temp.path().join(directory);
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {name}\n---\n{resource}"),
        )
        .unwrap();
        fs::write(root.join("resource.txt"), resource).unwrap();
    }
    let catalog = SkillCatalog::discover(temp.path(), &[temp.path().to_owned()], &[]);
    assert_eq!(catalog.skills.len(), 2);
    let mut registry = ToolRegistry::empty();
    catalog.register(&mut registry).unwrap();
    let mut bases = Vec::new();
    for (name, expected) in [("sample", "lowercase"), ("Sample", "uppercase")] {
        let loaded = registry
            .call("load_skill", &json!({"name":name}).to_string())
            .unwrap();
        let base = std::path::PathBuf::from(
            loaded
                .lines()
                .find_map(|line| line.strip_prefix("Base directory: "))
                .unwrap(),
        );
        assert_eq!(
            fs::read_to_string(base.join("resource.txt")).unwrap(),
            expected
        );
        assert!(registry
            .call(
                "load_skill",
                &json!({"name":name,"resource":"resource.txt"}).to_string()
            )
            .unwrap()
            .contains(expected));
        bases.push(base);
    }
    assert_ne!(bases[0], bases[1]);
    drop(registry);
    assert!(bases.iter().all(|path| !path.exists()));
}

#[cfg(windows)]
#[test]
fn junctions_cannot_redirect_management_or_load_external_resources() {
    use std::process::Command;
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".hikari")).unwrap();
    let target = temp.path().join(".hikari/skills");
    let status = Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(target.to_string_lossy().replace('/', "\\"))
        .arg(outside.path())
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let mut registry = ToolRegistry::empty();
    SkillCatalog::default()
        .register_management(&mut registry, temp.path())
        .unwrap();
    assert!(registry.call("skill_manage", &json!({"action":"create","name":"sample","files":[{"path":"SKILL.md","text":manifest("bad")}]}).to_string()).is_err());
    assert!(!outside.path().join("sample").exists());
    // Remove only the junction itself, preserving the external directory.
    fs::remove_dir(&target).unwrap();
    fs::create_dir_all(target.join("sample")).unwrap();
    fs::write(target.join("sample/SKILL.md"), manifest("instructions")).unwrap();
    let status = Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(
            target
                .join("sample/references")
                .to_string_lossy()
                .replace('/', "\\"),
        )
        .arg(outside.path())
        .output()
        .unwrap();
    assert!(status.status.success());
    let catalog = SkillCatalog::discover(temp.path(), &[".hikari/skills".into()], &[]);
    assert!(catalog.skills.is_empty());
    assert!(catalog.diagnostics.iter().any(|d| d.contains("junction")));
    fs::remove_dir(target.join("sample/references")).unwrap();
}
