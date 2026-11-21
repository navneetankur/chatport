//! Message tree helpers. Messages are in file (append) order; siblings share
//! `parent_id`; "latest" means last appended.

use crate::store::Message;
use std::collections::HashMap;

/// Root..=leaf. Empty if leaf unknown.
pub fn path<'a>(msgs: &'a [Message], leaf: &str) -> Vec<&'a Message> {
    let by_id: HashMap<&str, &Message> = msgs.iter().map(|m| (m.id.as_str(), m)).collect();
    let mut out = Vec::new();
    let mut cur = Some(leaf);
    while let Some(id) = cur {
        let Some(m) = by_id.get(id) else { break };
        out.push(*m);
        cur = m.parent_id.as_deref();
        if out.len() > msgs.len() {
            break; // cycle guard
        }
    }
    out.reverse();
    out
}

/// Follow latest child repeatedly from `id` down to a tip.
pub fn latest_descendant(msgs: &[Message], id: &str) -> String {
    let mut cur = id.to_string();
    for _ in 0..=msgs.len() {
        match msgs
            .iter()
            .rev()
            .find(|m| m.parent_id.as_deref() == Some(cur.as_str()))
        {
            Some(c) => cur = c.id.clone(),
            None => break,
        }
    }
    cur
}

pub fn default_leaf(msgs: &[Message]) -> Option<String> {
    msgs.last().map(|m| m.id.clone())
}

/// Leaf to show: stored one if valid, else last appended.
pub fn resolve_leaf(msgs: &[Message], stored: Option<&str>) -> Option<String> {
    match stored {
        Some(l) if msgs.iter().any(|m| m.id == l) => Some(l.to_string()),
        _ => default_leaf(msgs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Role;

    fn m(id: &str, parent: Option<&str>, role: Role) -> Message {
        let mut x = Message::user(parent.map(String::from), id.into(), vec![]);
        x.id = id.into();
        x.role = role;
        x
    }

    //  u1 ─ a1 ─ u2 ─ a2
    //            └ u2b ─ a2b
    //       └ a1b              (regenerated a1)
    fn sample() -> Vec<Message> {
        use Role::*;
        vec![
            m("u1", None, User),
            m("a1", Some("u1"), Assistant),
            m("u2", Some("a1"), User),
            m("a2", Some("u2"), Assistant),
            m("u2b", Some("a1"), User),
            m("a2b", Some("u2b"), Assistant),
            m("a1b", Some("u1"), Assistant),
        ]
    }

    #[test]
    fn path_follows_parents() {
        let s = sample();
        let ids: Vec<_> = path(&s, "a2b").iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["u1", "a1", "u2b", "a2b"]);
        assert!(path(&s, "nope").is_empty());
    }

    #[test]
    fn latest_descendant_picks_last_appended() {
        let s = sample();
        assert_eq!(latest_descendant(&s, "a1"), "a2b");
        assert_eq!(latest_descendant(&s, "u2"), "a2");
        assert_eq!(latest_descendant(&s, "u1"), "a1b");
    }

    #[test]
    fn resolve_leaf_falls_back() {
        let s = sample();
        assert_eq!(resolve_leaf(&s, Some("a2")).as_deref(), Some("a2"));
        assert_eq!(resolve_leaf(&s, Some("gone")).as_deref(), Some("a1b"));
        assert_eq!(resolve_leaf(&[], None), None);
    }
}
