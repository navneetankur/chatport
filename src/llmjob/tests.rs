use super::*;

fn parse(s: &str) -> Result<Option<Args>> {
    parse_args(s.split_whitespace().map(String::from).collect())
}

#[test]
fn run_args() {
    let a = parse("run -s a.md -m b.md -m - --headers -p cheap --effort low hello world")
        .unwrap()
        .unwrap();
    assert_eq!(a.cmd, Cmd::Run);
    assert_eq!(a.system, ["a.md"]);
    assert_eq!(a.msg, ["b.md", "-"]);
    assert_eq!(a.words, ["hello", "world"]);
    assert!(a.headers);
    assert_eq!(a.profile.as_deref(), Some("cheap"));
    assert_eq!(a.effort, Some(ReasoningEffort::Low));
    let a = parse("run -c last -- -not-a-flag").unwrap().unwrap();
    assert_eq!(a.cont.as_deref(), Some("last"));
    assert_eq!(a.words, ["-not-a-flag"]);
}

#[test]
fn options_before_command() {
    let a = parse("--config c.toml -p x run -m a hi").unwrap().unwrap();
    assert_eq!(a.cmd, Cmd::Run);
    assert_eq!(a.config, Some(PathBuf::from("c.toml")));
    assert_eq!(a.profile.as_deref(), Some("x"));
    assert_eq!(a.words, ["hi"]);
    assert!(parse("--config c.toml").is_err());
}

#[test]
fn help_and_errors() {
    assert!(parse("").unwrap().is_none());
    assert!(parse("run --help").unwrap().is_none());
    assert!(parse("frob").is_err());
    assert!(parse("run --bogus").is_err());
    assert!(parse("run -m").is_err());
    assert!(parse("run --effort huge").is_err());
    assert!(parse("run --key sk").is_err());
    // -s with -c, --dir on run, --save on batch, get without id.
    assert!(parse("run -c last -s x.md").is_err());
    assert!(parse("run --dir d").is_err());
    assert!(parse("batch --save").is_err());
    assert!(parse("get").is_err());
    assert!(parse("get a b").is_err());
    assert!(parse("get abc -m x").is_err());
    assert!(parse("rm abc --wait").is_err());
    assert!(parse("rm").is_err());
    assert!(parse("rm --all abc").is_err());
    assert!(parse("get abc --all").is_err());
    assert!(parse("get --all --wait").is_err());
    assert!(parse("ls --all").is_err());
    assert!(parse("run --force").is_err());
    assert!(parse("ls extra").is_err());
    assert!(parse("keep --save").is_err());
}

#[test]
fn rm_args() {
    let a = parse("rm k3f 52p").unwrap().unwrap();
    assert_eq!((a.cmd, a.words.len(), a.all), (Cmd::Rm, 2, false));
    let a = parse("rm --all --force").unwrap().unwrap();
    assert!(a.all && a.force && a.words.is_empty());
}

#[test]
fn server_base_from_flag_or_bind() {
    assert_eq!(server_base(None, "0.0.0.0:8787"), "http://127.0.0.1:8787");
    assert_eq!(server_base(None, "[::]:8787"), "http://[::1]:8787");
    assert_eq!(server_base(None, "192.168.1.5:80"), "http://192.168.1.5:80");
    assert_eq!(
        server_base(Some("localhost:4268"), "x"),
        "http://localhost:4268"
    );
    assert_eq!(server_base(Some("https://h/chat/"), "x"), "https://h/chat");
    let a = parse("notify --server h:1 k3f").unwrap().unwrap();
    assert_eq!((a.cmd, a.server.as_deref()), (Cmd::Notify, Some("h:1")));
    assert!(parse("notify --all").unwrap().unwrap().all);
    assert!(parse("notify").is_err());
    assert!(parse("notify --all k3f").is_err());
    assert!(parse("batch --server h:1 q").is_err());
}

#[test]
fn get_all_args() {
    let a = parse("get --all").unwrap().unwrap();
    assert!(a.all && a.words.is_empty());
}

#[test]
fn batch_and_get_args() {
    let a = parse("batch -s s.md --dir in q").unwrap().unwrap();
    assert_eq!(a.cmd, Cmd::Batch);
    assert_eq!(a.dir, Some(PathBuf::from("in")));
    assert_eq!(a.words, ["q"]);
    let a = parse("get k3f --dir out --wait").unwrap().unwrap();
    assert_eq!(
        (a.cmd, a.words, a.wait),
        (Cmd::Get, vec!["k3f".to_string()], true)
    );
}

#[test]
fn dir_files_sorted_skips_hidden_and_dirs() {
    let d = std::env::temp_dir().join(format!("llmjob-dir-{}", new_id()));
    std::fs::create_dir_all(d.join("sub")).unwrap();
    for f in ["b.txt", "a.txt", ".hidden"] {
        std::fs::write(d.join(f), "x").unwrap();
    }
    let names: Vec<String> = dir_files(&d).unwrap().into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["a.txt", "b.txt"]);
    std::fs::remove_dir_all(&d).unwrap();
    assert!(dir_files(&d).is_err());
}

#[test]
fn lookup_by_prefix_suffix_and_last() {
    let d = std::env::temp_dir().join(format!("llmjob-convs-{}", new_id()));
    let convs = Convs::open(&d).unwrap();
    let meta = |id: &str| Meta {
        id: id.into(),
        title: "t".into(),
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
    };
    convs.main.put_meta(&meta("aaaa1111")).unwrap();
    convs.scratch.put_meta(&meta("bbbb2222")).unwrap();
    assert_eq!(convs.lookup("aaaa").unwrap(), (true, "aaaa1111".into()));
    assert_eq!(convs.lookup("2222").unwrap(), (false, "bbbb2222".into()));
    assert!(convs.lookup("zz").is_err());
    assert!(convs.lookup("last").is_err());
    convs.set_last("bbbb2222").unwrap();
    assert_eq!(convs.lookup("last").unwrap(), (false, "bbbb2222".into()));
    convs.keep("bbbb2222").unwrap();
    assert_eq!(convs.lookup("last").unwrap(), (true, "bbbb2222".into()));
    assert_eq!(
        convs.main.group_of("bbbb2222").unwrap().as_deref(),
        Some(GROUP)
    );
    convs.scratch.put_meta(&meta("cccc3333")).unwrap();
    convs.clear_scratch().unwrap();
    assert!(convs.scratch.list().unwrap().is_empty());
    std::fs::remove_dir_all(&d).unwrap();
}
