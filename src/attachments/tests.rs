use super::*;
use crate::store::{Message, Meta};

fn tmp() -> PathBuf {
    let d = std::env::temp_dir().join(format!("chatport-att-{}", crate::store::new_id()));
    std::fs::create_dir_all(&d).unwrap();
    return d;
}

fn att(name: &str, content: &str) -> Attachment {
    Attachment {
        name: name.into(),
        content: content.into(),
        ..Default::default()
    }
}

fn names(es: &[Entry]) -> Vec<&str> {
    es.iter().map(|e| e.name.as_str()).collect()
}

#[test]
fn save_dedupes_and_orders_by_last_use() {
    let d = tmp();
    let s = Attachments::open(&d).unwrap();
    let mut a = att("a.md", "AAA");
    let mut b = att("b.md", "BBB");
    s.save(&mut a, "c1", "Chat 1").unwrap();
    s.save(&mut b, "c1", "Chat 1").unwrap();
    let mut a2 = att("a.md", "AAA");
    s.save(&mut a2, "c2", "Chat 2").unwrap();
    assert_eq!(a.hash, a2.hash);
    assert_eq!(std::fs::read_dir(d.join("attachments")).unwrap().count(), 2);
    let p = s.picker().unwrap();
    assert_eq!(names(&p.recent), ["a.md", "b.md"]);
    assert_eq!(p.recent[0].chat_name.as_deref(), Some("Chat 2"));
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn hash_only_attachment_is_filled_from_blob() {
    let d = tmp();
    let s = Attachments::open(&d).unwrap();
    let mut a = att("a.md", "hello");
    s.save(&mut a, "c", "C").unwrap();
    let mut picked = Attachment {
        content: String::new(),
        ..a.clone()
    };
    s.save(&mut picked, "c", "C").unwrap();
    assert_eq!(picked.content, "hello");
    let mut stored = Attachment {
        content: String::new(),
        ..a.clone()
    };
    s.hydrate(&mut stored);
    assert_eq!(stored.content, "hello");
    std::fs::remove_dir_all(d.join("attachments")).unwrap();
    let mut gone = Attachment {
        content: String::new(),
        ..a
    };
    s.hydrate(&mut gone);
    assert!(gone.missing);
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn pin_and_hide() {
    let d = tmp();
    let s = Attachments::open(&d).unwrap();
    let (mut a, mut b) = (att("a.md", "A"), att("b.md", "B"));
    s.save(&mut a, "c", "C").unwrap();
    s.save(&mut b, "c", "C").unwrap();
    let (ha, hb) = (a.hash.clone().unwrap(), b.hash.clone().unwrap());
    s.pin(&ha).unwrap();
    s.pin(&ha).unwrap();
    let p = s.picker().unwrap();
    assert_eq!(names(&p.pinned), ["a.md"]);
    assert_eq!(names(&p.recent), ["b.md"]);
    s.hide(&hb).unwrap();
    assert!(s.picker().unwrap().recent.is_empty());
    // Attaching again un-hides.
    s.save(&mut b, "c", "C").unwrap();
    assert_eq!(names(&s.picker().unwrap().recent), ["b.md"]);
    // Hiding a pinned one unpins it.
    s.hide(&ha).unwrap();
    let p = s.picker().unwrap();
    assert!(p.pinned.is_empty());
    assert_eq!(names(&p.recent), ["b.md"]);
    assert!(s.pin("0".repeat(64).as_str()).is_err());
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn reattach_keeps_pin_and_pin_is_not_a_use() {
    let d = tmp();
    let s = Attachments::open(&d).unwrap();
    let (mut a, mut b) = (att("a.md", "A"), att("b.md", "B"));
    s.save(&mut a, "c", "C").unwrap();
    s.save(&mut b, "c", "C").unwrap();
    let ha = a.hash.clone().unwrap();
    s.pin(&ha).unwrap();
    // Re-attached directly (content, no hash): still pinned, newer chat name.
    let mut again = att("a.md", "A");
    s.save(&mut again, "c2", "Other").unwrap();
    let p = s.picker().unwrap();
    assert_eq!(names(&p.pinned), ["a.md"]);
    assert_eq!(p.pinned[0].chat_name.as_deref(), Some("Other"));
    // Unpin keeps `at`: b was not used since, so a (re-attached later) stays first.
    s.unpin(&ha).unwrap();
    assert_eq!(names(&s.picker().unwrap().recent), ["a.md", "b.md"]);
    // No-op restates append nothing.
    let n = s.lines().unwrap().len();
    s.unpin(&ha).unwrap();
    assert_eq!(s.lines().unwrap().len(), n);
    assert!(s.unpin(&"0".repeat(64)).is_err());
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn compact_keeps_newest_line_per_kept_hash() {
    let e = |h: &str, n: &str| Entry {
        hash: h.into(),
        name: n.into(),
        size: 0,
        at: 0,
        chat: None,
        chat_name: None,
        path: None,
        pinned: false,
        hidden: false,
    };
    let lines = [e("x", "1"), e("y", "2"), e("x", "3"), e("z", "4")];
    let keep: HashSet<String> = ["x".to_string(), "y".to_string()].into();
    assert_eq!(names(&compact(&lines, &keep)), ["2", "3"]);
}

#[test]
fn gc_keeps_referenced_pinned_and_recent_blobs() {
    let d = tmp();
    let store = Store::open(d.clone()).unwrap();
    let s = Attachments::open(&d).unwrap();
    let (mut used, mut pinned, mut orphan, mut fresh) =
        (att("u", "U"), att("p", "P"), att("o", "O"), att("f", "F"));
    for a in [&mut used, &mut pinned, &mut orphan, &mut fresh] {
        s.save(a, "c", "C").unwrap();
    }
    s.pin(pinned.hash.as_ref().unwrap()).unwrap();
    let meta: Meta = serde_json::from_value(serde_json::json!({
        "id": "abc", "title": "t", "created_at": 0, "updated_at": 0, "model": "m"
    }))
    .unwrap();
    store.put_meta(&meta).unwrap();
    store
        .append("abc", &Message::user(None, "hi".into(), vec![used.clone()]))
        .unwrap();
    let old = SystemTime::now() - GC_GRACE - Duration::from_secs(60);
    for a in [&used, &pinned, &orphan] {
        std::fs::File::options()
            .write(true)
            .open(s.blob(a.hash.as_ref().unwrap()))
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    s.gc(&store, false).unwrap();
    assert!(s.blob(orphan.hash.as_ref().unwrap()).exists());
    s.gc(&store, true).unwrap();
    assert!(!s.blob(orphan.hash.as_ref().unwrap()).exists());
    for a in [&used, &pinned, &fresh] {
        assert!(s.blob(a.hash.as_ref().unwrap()).exists(), "{}", a.name);
    }
    assert_eq!(s.lines().unwrap().len(), 3);
    std::fs::remove_dir_all(d).unwrap();
}
