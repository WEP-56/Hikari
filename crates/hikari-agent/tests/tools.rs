use hikari_agent::ToolRegistry;
use serde_json::json;

#[test]
fn pi_edit_preserves_bom_and_crlf_and_rejects_ambiguous_edits() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("sample.txt");
    std::fs::write(&path, "\u{feff}first\r\nsecond\r\n").unwrap();
    let registry = ToolRegistry::workspace(temp.path());
    registry
        .call(
            "edit",
            &json!({"path":"sample.txt","old_text":"first\nsecond","new_text":"changed\nsecond"})
                .to_string(),
        )
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "\u{feff}changed\r\nsecond\r\n"
    );
    std::fs::write(&path, "duplicate duplicate").unwrap();
    assert!(registry
        .call(
            "edit",
            &json!({"path":"sample.txt","old_text":"duplicate","new_text":"new"}).to_string()
        )
        .is_err());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "duplicate duplicate"
    );
    registry
        .call(
            "write",
            &json!({"path":"new/nested/file.txt","content":"created"}).to_string(),
        )
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(temp.path().join("new/nested/file.txt")).unwrap(),
        "created"
    );
    assert!(registry
        .call(
            "write",
            &json!({"path":"../outside.txt","content":"bad"}).to_string()
        )
        .is_err());
}
