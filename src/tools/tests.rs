use super::*;

struct TmpDir(PathBuf);
impl TmpDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("chatport-tools-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        TmpDir(d)
    }
    fn root(&self) -> &str {
        self.0.to_str().unwrap()
    }
}
impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn call(root: &str, name: &str, args: Value) -> ToolResult {
    run(
        root,
        &ToolCall {
            id: "c1".into(),
            name: name.into(),
            arguments: args.to_string(),
        },
    )
}

#[test]
fn jail_rejects_escapes() {
    let t = TmpDir::new("jail");
    let root = Path::new(t.root());
    assert!(jail(root, "a/b.md").is_ok());
    assert!(jail(root, "./x").is_ok());
    assert!(jail(root, "../x").is_err());
    assert!(jail(root, "a/../../x").is_err());
    assert!(jail(root, "/etc/passwd").is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/tmp", t.0.join("out")).unwrap();
        assert!(jail(root, "out/x").is_err());
        std::os::unix::fs::symlink(&t.0, t.0.join("self")).unwrap();
        assert!(jail(root, "self/x").is_ok());
    }
}

#[test]
fn write_read_edit_list() {
    let t = TmpDir::new("rw");
    let r = call(
        t.root(),
        "write_file",
        json!({"path": "d/n.md", "content": "a\nb\nc\n"}),
    );
    assert!(!r.error, "{}", r.output);
    assert!(r.output.starts_with("created"));
    assert!(r.diff.as_deref().unwrap().contains("+b"));

    let r = call(
        t.root(),
        "read_file",
        json!({"path": "d/n.md", "start_line": 2, "end_line": 3}),
    );
    assert_eq!(r.output, "b\nc");

    let r = call(
        t.root(),
        "edit_file",
        json!({"path": "d/n.md", "old_string": "b", "new_string": "B"}),
    );
    assert!(!r.error, "{}", r.output);
    let d = r.diff.unwrap();
    assert!(d.contains("-b") && d.contains("+B"), "{d}");
    assert_eq!(
        std::fs::read_to_string(t.0.join("d/n.md")).unwrap(),
        "a\nB\nc\n"
    );

    let r = call(
        t.root(),
        "write_file",
        json!({"path": "d/n.md", "content": "z\n"}),
    );
    assert!(r.output.starts_with("updated"));

    let r = call(t.root(), "list_dir", json!({}));
    assert_eq!(r.output, "d/");
    let r = call(t.root(), "list_dir", json!({"path": "d"}));
    assert_eq!(r.output, "n.md");
}

#[test]
fn errors_go_back_to_the_model() {
    let t = TmpDir::new("err");
    std::fs::write(t.0.join("x.md"), "aa aa").unwrap();
    let cases = [
        (
            "edit_file",
            json!({"path": "x.md", "old_string": "aa", "new_string": "b"}),
        ),
        (
            "edit_file",
            json!({"path": "x.md", "old_string": "zz", "new_string": "b"}),
        ),
        (
            "edit_file",
            json!({"path": "missing.md", "old_string": "a", "new_string": "b"}),
        ),
        ("read_file", json!({"path": "missing.md"})),
        ("read_file", json!({})),
        ("write_file", json!({"path": "../x", "content": ""})),
        ("nope", json!({})),
    ];
    for (name, args) in cases {
        let r = call(t.root(), name, args.clone());
        assert!(r.error, "{name} {args} should fail: {}", r.output);
        assert!(r.output.starts_with("error: "));
    }
    let bad = run(
        t.root(),
        &ToolCall {
            id: "c".into(),
            name: "list_dir".into(),
            arguments: "{oops".into(),
        },
    );
    assert!(bad.error);
    // Untouched by the failed edits.
    assert_eq!(std::fs::read_to_string(t.0.join("x.md")).unwrap(), "aa aa");
}
