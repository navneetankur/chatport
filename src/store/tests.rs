use super::*;
use crate::provider::CachePolicy;

/// Fresh data dir under the system temp dir, removed on drop.
struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("chatport-test-{}", new_id()));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn meta(title: &str) -> Meta {
    Meta {
        id: new_id(),
        title: title.into(),
        created_at: 0,
        updated_at: 0,
        model: "m".into(),
        provider: None,
        system: None,
        cache: CachePolicy::default(),
        reasoning_effort: None,
        key_name: None,
        file_root: None,
        tools: false,
        web_tools: false,
        leaf: None,
        pending_batch: None,
    }
}

fn conv_root(t: &TempDir) -> PathBuf {
    t.0.join("conversations")
}

#[test]
fn new_chat_goes_to_ungrouped_with_label() {
    let t = TempDir::new();
    let s = Store::open(t.0.clone()).unwrap();
    let m = meta("a/b - c");
    s.put_meta(&m).unwrap();
    let dir = conv_root(&t)
        .join(UNGROUPED)
        .join(format!("{}-a∕b - c", m.id));
    assert!(dir.join("meta.json").is_file());
    let l = s.list().unwrap();
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].id, m.id);
    assert_eq!(l[0].group, None);
    assert_eq!(l[0].title, "a∕b - c");
    assert_eq!(s.group_of(&m.id).unwrap(), None);
}

#[test]
fn title_change_renames_folder_and_keeps_messages() {
    let t = TempDir::new();
    let s = Store::open(t.0.clone()).unwrap();
    let mut m = meta("old");
    s.put_meta(&m).unwrap();
    s.append(&m.id, &Message::user(None, "hi".into(), vec![]))
        .unwrap();
    m.title = "new".into();
    s.put_meta(&m).unwrap();
    assert_eq!(s.list().unwrap()[0].title, "new");
    assert_eq!(s.messages(&m.id).unwrap().len(), 1);
    assert_eq!(s.meta(&m.id).unwrap().unwrap().title, "new");
}

#[test]
fn move_between_groups_and_cleanup() {
    let t = TempDir::new();
    let s = Store::open(t.0.clone()).unwrap();
    let m = meta("x");
    s.put_meta(&m).unwrap();
    s.move_to(&m.id, Some("work-stuff")).unwrap();
    assert_eq!(s.group_of(&m.id).unwrap().as_deref(), Some("work-stuff"));
    // Appends still land after the move.
    s.append(&m.id, &Message::user(None, "hi".into(), vec![]))
        .unwrap();
    assert_eq!(s.messages(&m.id).unwrap().len(), 1);
    s.move_to(&m.id, Some("other")).unwrap();
    assert!(
        !conv_root(&t).join("work-stuff").exists(),
        "empty group removed"
    );
    s.move_to(&m.id, None).unwrap();
    assert!(!conv_root(&t).join("other").exists());
    assert!(conv_root(&t).join(UNGROUPED).is_dir(), "ungrouped kept");
    assert_eq!(s.list().unwrap()[0].group, None);
}

#[test]
fn group_rename_and_delete() {
    let t = TempDir::new();
    let s = Store::open(t.0.clone()).unwrap();
    let (a, b) = (meta("a"), meta("b"));
    s.put_meta(&a).unwrap();
    s.put_meta(&b).unwrap();
    s.move_to(&a.id, Some("g")).unwrap();
    s.move_to(&b.id, Some("g")).unwrap();
    s.rename_group("g", "h").unwrap();
    assert_eq!(s.group_of(&a.id).unwrap().as_deref(), Some("h"));
    s.move_to(&b.id, Some("k")).unwrap();
    assert!(s.rename_group("h", "k").is_err(), "target exists");
    s.delete_group("h", false).unwrap();
    assert_eq!(s.group_of(&a.id).unwrap(), None);
    s.delete_group("k", true).unwrap();
    assert!(s.meta(&b.id).unwrap().is_none());
    assert_eq!(s.list().unwrap().len(), 1);
}

#[test]
fn delete_removes_empty_group() {
    let t = TempDir::new();
    let s = Store::open(t.0.clone()).unwrap();
    let m = meta("x");
    s.put_meta(&m).unwrap();
    s.move_to(&m.id, Some("g")).unwrap();
    assert!(s.delete(&m.id).unwrap());
    assert!(!conv_root(&t).join("g").exists());
    assert!(!s.delete(&m.id).unwrap());
}

#[test]
fn group_names_checked() {
    for bad in ["", " a", ".x", "a/b", UNGROUPED, "a\nb"] {
        assert!(check_group(bad).is_err(), "{bad:?}");
    }
    for ok in ["work", "a-b", "Ünï code", "_other"] {
        assert!(check_group(ok).is_ok(), "{ok:?}");
    }
}

#[test]
fn label_cut_on_char_boundary() {
    let l = label_for(&"é".repeat(200));
    assert!(l.len() <= LABEL_MAX_BYTES);
    assert_eq!(l.chars().count(), LABEL_MAX_BYTES / 2);
}

#[test]
fn bad_ids_rejected() {
    let t = TempDir::new();
    let s = Store::open(t.0.clone()).unwrap();
    assert!(s.find("../x").is_err());
    assert!(s.find("a-b").is_err());
}

#[test]
fn adopt_moves_chat_between_stores() {
    let t = TempDir::new();
    let main = Store::open(t.0.clone()).unwrap();
    let scratch = Store::open(t.0.join("scratch")).unwrap();
    let m = meta("hello");
    scratch.put_meta(&m).unwrap();
    let dir = scratch.path_of(&m.id).unwrap().unwrap();
    main.adopt(&dir, Some("llmjob")).unwrap();
    assert!(scratch.meta(&m.id).unwrap().is_none());
    assert_eq!(main.group_of(&m.id).unwrap().as_deref(), Some("llmjob"));
    assert_eq!(main.meta(&m.id).unwrap().unwrap().title, "hello");
    // Same id again: refused.
    scratch.put_meta(&m).unwrap();
    let dir = scratch.path_of(&m.id).unwrap().unwrap();
    assert!(main.adopt(&dir, None).is_err());
}
