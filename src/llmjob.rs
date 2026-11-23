//! `llmjob`: one-shot LLM calls and OpenRouter batches from the command line,
//! sharing chatport's config, keys and conversation storage.
//!
//! Conversations from `run` are chatport conversations:
//! - `--save`: `conversations/llmjob/` (shows up in the web UI)
//! - otherwise: a one-chat scratch store `data_dir/llmjob/scratch/`, replaced
//!   by the next unsaved run; `llmjob keep` moves it into `conversations/llmjob/`.
//!
//! `data_dir/llmjob/last` holds the id of the conversation `-c last` means.

mod compose;
pub(crate) mod jobs;

use crate::api::{context, record_model, title_from, with_data_collection_default};
use crate::config::{self, Config, Profile};
use crate::provider::openrouter::OpenRouter;
use crate::provider::{
    CachePolicy, ChatEvent, ChatRequest, Provider, ReasoningEffort, is_batch_model,
};
use crate::store::{Message, Meta, Store, new_id, now_ms};
use crate::tree;
use anyhow::{Context, Result, bail};
use compose::{Part, Reader};
use futures::StreamExt;
use std::io::Write;
use std::path::{Path, PathBuf};

const GROUP: &str = "llmjob";
/// Characters of a conversation id shown to the user (its random tail).
const SHOWN_ID_CHARS: usize = 6;

const HELP: &str = "\
usage:
  llmjob run   [opts] [-s FILE]... [-m FILE]... [TEXT...]   reply on stdout
  llmjob run   [opts] -c ID|last [-m FILE]... [TEXT...]     continue a conversation
  llmjob keep                                              save the last unsaved run
  llmjob batch [opts] [-s FILE]... [-m FILE]... [--dir DIR] [TEXT...]
                                                           prints a job id
  llmjob get   ID|last [--dir DIR] [--wait]                exit 2 while running
  llmjob get   --all                                       cache every finished job
  llmjob ls
  llmjob rm    ID|last...  |  --all [--force]          delete jobs (local + upstream)

options:
  -s, --system FILE   system prompt file (repeatable)
  -m, --msg FILE      message file, `-` = stdin (repeatable)
  -p, --profile NAME  [profile.NAME] from config (default: `default` if present)
      --model ID      override the model
      --effort LEVEL  max|xhigh|high|medium|low|minimal|none
      --headers       wrap message files in <file name=\"…\"> tags
      --save          run: keep the conversation (conversations/llmjob/)
  -c, --continue ID   run: continue conversation ID (prefix/suffix) or `last`
      --dir DIR       batch: one request per file in DIR; get: write outputs there
      --wait          get: poll every minute until finished
      --all           get: check every unfetched job once, cache finished ones
                      rm: every job (running ones are always skipped)
      --force         rm: also jobs finished upstream but never fetched
      --config PATH   default ~/.config/chatport/config.toml
      --keyfile PATH  key for the default key name (else OPENROUTER_API_KEY, config)

Parts of a message (profile msg files, -m files, the --dir file, TEXT) are
joined by a blank line; empty ones are skipped. A new conversation gets the
profile's system/msg files; a continued one keeps its system prompt.";

#[derive(Debug, PartialEq)]
enum Cmd {
    Run,
    Keep,
    Batch,
    Get,
    Ls,
    Rm,
}

#[derive(Debug)]
struct Args {
    cmd: Cmd,
    config: Option<PathBuf>,
    keyfile: Option<PathBuf>,
    profile: Option<String>,
    model: Option<String>,
    effort: Option<ReasoningEffort>,
    system: Vec<String>,
    msg: Vec<String>,
    /// Positional words (TEXT, or the job id of get/rm).
    words: Vec<String>,
    headers: bool,
    save: bool,
    cont: Option<String>,
    dir: Option<PathBuf>,
    wait: bool,
    all: bool,
    force: bool,
}

/// Options that take a value (needed to find the command among leading options).
const VALUE_OPTS: [&str; 13] = [
    "-s",
    "--system",
    "-m",
    "--msg",
    "-p",
    "--profile",
    "--model",
    "--effort",
    "-c",
    "--continue",
    "--dir",
    "--config",
    "--keyfile",
];

/// Moves the command word to the front: options may come before it.
fn command_first(mut argv: Vec<String>) -> Vec<String> {
    let mut i = 0;
    while i < argv.len() && argv[i].starts_with('-') && argv[i] != "-" && argv[i] != "--" {
        i += if VALUE_OPTS.contains(&argv[i].as_str()) {
            2
        } else {
            1
        };
    }
    if i > 0 && i < argv.len() {
        let cmd = argv.remove(i);
        argv.insert(0, cmd);
    }
    return argv;
}

fn parse_args(argv: Vec<String>) -> Result<Option<Args>> {
    let mut it = command_first(argv).into_iter();
    let cmd = match it.next().as_deref() {
        Some("run") => Cmd::Run,
        Some("keep") => Cmd::Keep,
        Some("batch") => Cmd::Batch,
        Some("get") => Cmd::Get,
        Some("ls") => Cmd::Ls,
        Some("rm") => Cmd::Rm,
        None | Some("-h" | "--help" | "help") => return Ok(None),
        Some(other) if other.starts_with('-') => bail!("no command given (see llmjob --help)"),
        Some(other) => bail!("unknown command {other:?} (see llmjob --help)"),
    };
    let mut a = Args {
        cmd,
        config: None,
        keyfile: None,
        profile: None,
        model: None,
        effort: None,
        system: vec![],
        msg: vec![],
        words: vec![],
        headers: false,
        save: false,
        cont: None,
        dir: None,
        wait: false,
        all: false,
        force: false,
    };
    let mut flags_done = false;
    while let Some(arg) = it.next() {
        if flags_done || arg == "-" || !arg.starts_with('-') {
            a.words.push(arg);
            continue;
        }
        let mut val = || it.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--" => flags_done = true,
            "-s" | "--system" => a.system.push(val()?),
            "-m" | "--msg" => a.msg.push(val()?),
            "-p" | "--profile" => a.profile = Some(val()?),
            "--model" => a.model = Some(val()?),
            "--effort" => {
                let v = val()?;
                a.effort = Some(
                    serde_json::from_value(serde_json::Value::String(v.clone()))
                        .with_context(|| format!("bad --effort {v:?}"))?,
                );
            }
            "--headers" => a.headers = true,
            "--save" => a.save = true,
            "-c" | "--continue" => a.cont = Some(val()?),
            "--dir" => a.dir = Some(PathBuf::from(val()?)),
            "--wait" => a.wait = true,
            "--all" => a.all = true,
            "--force" => a.force = true,
            "--config" => a.config = Some(PathBuf::from(val()?)),
            "--keyfile" => a.keyfile = Some(config::expand_tilde(&val()?)),
            "--key" => bail!("--key is not supported (visible in ps); use --keyfile PATH"),
            "-h" | "--help" => return Ok(None),
            other => bail!("unknown option {other:?} (see llmjob --help)"),
        }
    }
    check_args(&a)?;
    return Ok(Some(a));
}

/// Rejects options that mean nothing for the command.
fn check_args(a: &Args) -> Result<()> {
    let prompt = !a.system.is_empty() || !a.msg.is_empty() || a.headers;
    let model = a.profile.is_some() || a.model.is_some() || a.effort.is_some();
    let bad = |what: &str| bail!("{what} does not apply to `{:?}`", a.cmd);
    if a.force && a.cmd != Cmd::Rm {
        return bad("--force");
    }
    if a.all && !matches!(a.cmd, Cmd::Rm | Cmd::Get) {
        return bad("--all");
    }
    match a.cmd {
        Cmd::Run => {
            if a.dir.is_some() || a.wait {
                return bad("--dir/--wait");
            }
            if a.cont.is_some() && !a.system.is_empty() {
                bail!("-s with -c: a continued conversation keeps its system prompt");
            }
        }
        Cmd::Batch => {
            if a.save || a.cont.is_some() || a.wait {
                return bad("--save/-c/--wait");
            }
        }
        Cmd::Get => {
            if prompt || model || a.save || a.cont.is_some() {
                return bad("prompt/model options");
            }
            if a.all && (!a.words.is_empty() || a.dir.is_some() || a.wait) {
                bail!("get --all takes no job id, --dir or --wait");
            }
            if !a.all && a.words.len() != 1 {
                bail!("expected one job id (or `last`)");
            }
        }
        Cmd::Rm => {
            if prompt || model || a.save || a.cont.is_some() || a.dir.is_some() || a.wait {
                return bad("options");
            }
            match (a.all, a.words.is_empty()) {
                (true, false) => bail!("--all takes no job ids"),
                (false, true) => bail!("expected job ids (or --all)"),
                _ => {}
            }
        }
        Cmd::Keep | Cmd::Ls => {
            if prompt || model || a.save || a.cont.is_some() || a.dir.is_some() || a.wait {
                return bad("options");
            }
            if !a.words.is_empty() {
                bail!("unexpected argument {:?}", a.words[0]);
            }
        }
    }
    return Ok(());
}

/// What every command needs.
pub struct Ctx {
    pub config: Config,
    /// Folder of the config file: profile paths resolve against it.
    config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub provider: OpenRouter,
    /// Key name of the resolved profile (None = default key).
    pub key_name: Option<String>,
}

/// Entry point of the `llmjob` binary. Returns the exit code.
pub fn main() -> i32 {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args = match parse_args(std::env::args().skip(1).collect()) {
        Ok(Some(a)) => a,
        Ok(None) => {
            println!("{HELP}");
            return 0;
        }
        Err(e) => {
            eprintln!("llmjob: {e:#}");
            return 64;
        }
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("llmjob: {e}");
            return 1;
        }
    };
    match rt.block_on(dispatch(args)) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("llmjob: {e:#}");
            1
        }
    }
}

async fn dispatch(a: Args) -> Result<i32> {
    let (mut cfg, path) = config::load_with_path(a.config.clone())?;
    let override_key = match &a.keyfile {
        Some(p) => Some(config::read_keyfile(p)?),
        None => std::env::var("OPENROUTER_API_KEY").ok(),
    };
    cfg.resolve_keys(override_key)?;
    let profile = resolve_profile(&cfg, a.profile.as_deref())?;
    let ctx = Ctx {
        config_dir: path.parent().map(Path::to_path_buf).unwrap_or_default(),
        data_dir: config::expand_tilde(&cfg.data_dir),
        provider: OpenRouter::new(cfg.key_for(None)?.to_string())?,
        key_name: profile.key.clone(),
        config: cfg,
    };
    return match a.cmd {
        Cmd::Run => run(&ctx, &a, &profile).await,
        Cmd::Keep => keep(&ctx),
        Cmd::Batch => batch(&ctx, &a, &profile).await,
        Cmd::Get if a.all => jobs::get_all(&ctx).await,
        Cmd::Get => jobs::get(&ctx, &a.words[0], a.dir.as_deref(), a.wait).await,
        Cmd::Ls => jobs::ls(&ctx),
        Cmd::Rm => jobs::rm(&ctx, &a.words, a.all, a.force).await,
    };
}

/// Named profile (must exist), else `default` if configured, else empty.
fn resolve_profile(cfg: &Config, name: Option<&str>) -> Result<Profile> {
    return match name {
        Some(n) => cfg
            .profile
            .get(n)
            .cloned()
            .with_context(|| format!("no [profile.{n}] in config")),
        None => Ok(cfg.profile.get("default").cloned().unwrap_or_default()),
    };
}

/// Model and effort: command line over profile over config default.
fn model_of(ctx: &Ctx, a: &Args, p: &Profile) -> (String, Option<ReasoningEffort>) {
    let model = a
        .model
        .clone()
        .or_else(|| p.model.clone())
        .unwrap_or_else(|| ctx.config.default_model.clone());
    return (model, a.effort.or(p.effort));
}

/// System prompt of a new conversation: profile files, then `-s` files.
fn system_of(ctx: &Ctx, a: &Args, p: &Profile, r: &mut Reader) -> Result<Option<String>> {
    let mut parts = Vec::new();
    for f in &p.system {
        parts.push(r.read(f, Some(&ctx.config_dir))?);
    }
    for f in &a.system {
        parts.push(r.read(f, None)?);
    }
    return Ok(compose::join(&parts, false));
}

/// Message parts before the per-request ones: profile msg files (new
/// conversations only), then `-m` files.
fn msg_parts(ctx: &Ctx, a: &Args, p: Option<&Profile>, r: &mut Reader) -> Result<Vec<Part>> {
    let mut parts = Vec::new();
    for f in p.map(|p| p.msg.as_slice()).unwrap_or_default() {
        parts.push(r.read(f, Some(&ctx.config_dir))?);
    }
    for f in &a.msg {
        parts.push(r.read(f, None)?);
    }
    return Ok(parts);
}

fn text_part(a: &Args) -> Option<Part> {
    return (!a.words.is_empty()).then(|| Part::text(a.words.join(" ")));
}

/// Main store, scratch store and the `last` pointer.
struct Convs {
    main: Store,
    scratch: Store,
    last: PathBuf,
}

impl Convs {
    fn open(data_dir: &Path) -> Result<Self> {
        let dir = data_dir.join("llmjob");
        return Ok(Self {
            main: Store::open(data_dir.to_path_buf())?,
            scratch: Store::open(dir.join("scratch"))?,
            last: dir.join("last"),
        });
    }

    fn store(&self, saved: bool) -> &Store {
        if saved { &self.main } else { &self.scratch }
    }

    fn set_last(&self, id: &str) -> Result<()> {
        let tmp = self.last.with_extension("tmp");
        std::fs::write(&tmp, id)?;
        std::fs::rename(&tmp, &self.last)?;
        return Ok(());
    }

    fn last_id(&self) -> Result<String> {
        let id = std::fs::read_to_string(&self.last).context("no last conversation yet")?;
        return Ok(id.trim().to_string());
    }

    /// `last`, or an id prefix/suffix. Returns (saved?, full id).
    fn lookup(&self, q: &str) -> Result<(bool, String)> {
        let id = if q == "last" {
            self.last_id()?
        } else {
            let mut hits = Vec::new();
            for s in [&self.main, &self.scratch] {
                for c in s.list()? {
                    if c.id.starts_with(q) || c.id.ends_with(q) {
                        hits.push(c.id);
                    }
                }
            }
            match hits.len() {
                0 => bail!("no conversation matches {q:?}"),
                1 => hits.remove(0),
                n => bail!("{n} conversations match {q:?}"),
            }
        };
        if self.main.meta(&id)?.is_some() {
            return Ok((true, id));
        }
        if self.scratch.meta(&id)?.is_some() {
            return Ok((false, id));
        }
        bail!("conversation {} is gone", shown(&id));
    }

    /// Empties the scratch store (it holds one conversation at most).
    fn clear_scratch(&self) -> Result<()> {
        for c in self.scratch.list()? {
            self.scratch.delete(&c.id)?;
        }
        return Ok(());
    }

    /// Moves a scratch conversation into `conversations/llmjob/`.
    fn keep(&self, id: &str) -> Result<()> {
        let dir = self
            .scratch
            .path_of(id)?
            .with_context(|| format!("conversation {} is not in scratch", shown(id)))?;
        return self.main.adopt(&dir, Some(GROUP));
    }
}

fn shown(id: &str) -> &str {
    &id[id.len().saturating_sub(SHOWN_ID_CHARS)..]
}

/// `llmjob run`: new or continued conversation, reply streamed to stdout.
async fn run(ctx: &Ctx, a: &Args, p: &Profile) -> Result<i32> {
    let convs = Convs::open(&ctx.data_dir)?;
    let mut r = Reader::default();
    let headers = a.headers || p.headers;
    let (saved, mut meta, user) = match &a.cont {
        Some(q) => {
            let (saved, id) = convs.lookup(q)?;
            let store = convs.store(saved);
            let mut meta = store.meta(&id)?.context("conversation has no meta")?;
            let msgs = store.messages(&id)?;
            if meta.pending_batch.is_some() {
                bail!("conversation is waiting on a batch reply (see the web UI)");
            }
            // Only what was asked for explicitly changes the conversation.
            if a.profile.is_some() {
                if let Some(m) = &p.model {
                    meta.model = m.clone();
                }
                meta.reasoning_effort = p.effort.or(meta.reasoning_effort);
                if p.key.is_some() {
                    meta.key_name = p.key.clone();
                }
            }
            if let Some(m) = &a.model {
                meta.model = m.clone();
            }
            meta.reasoning_effort = a.effort.or(meta.reasoning_effort);
            let mut parts = msg_parts(ctx, a, None, &mut r)?;
            parts.extend(text_part(a));
            let text = compose::join(&parts, headers).context("empty message")?;
            let leaf = tree::resolve_leaf(&msgs, meta.leaf.as_deref());
            (saved, meta, Message::user(leaf, text, vec![]))
        }
        None => {
            let (model, effort) = model_of(ctx, a, p);
            let system = system_of(ctx, a, p, &mut r)?;
            let mut parts = msg_parts(ctx, a, Some(p), &mut r)?;
            parts.extend(text_part(a));
            let text = compose::join(&parts, headers).context("empty message")?;
            let user = Message::user(None, text, vec![]);
            let id = new_id();
            let now = now_ms();
            let meta = Meta {
                title: title_from(&user, &id),
                id,
                created_at: now,
                updated_at: now,
                model,
                provider: Some(with_data_collection_default(
                    p.provider
                        .clone()
                        .or_else(|| ctx.config.default_provider.clone()),
                )),
                system,
                cache: CachePolicy::default(),
                reasoning_effort: effort,
                key_name: p.key.clone(),
                file_root: None,
                tools: false,
                web_tools: false,
                leaf: None,
                pending_batch: None,
            };
            (a.save, meta, user)
        }
    };
    if is_batch_model(&meta.model) {
        bail!("{} is a batch model: use `llmjob batch`", meta.model);
    }
    let store = convs.store(saved);
    if a.cont.is_none() {
        if !saved {
            convs.clear_scratch()?;
        }
        store.put_meta(&meta)?;
        if saved {
            store.move_to(&meta.id, Some(GROUP))?;
        }
    }
    store.append(&meta.id, &user)?;
    convs.set_last(&meta.id)?;

    let msgs = store.messages(&meta.id)?;
    let req = ChatRequest {
        key: ctx.config.key_for(meta.key_name.as_deref())?.to_string(),
        model: meta.model.clone(),
        system: meta.system.clone(),
        messages: context(&tree::path(&msgs, &user.id), false),
        options: meta.provider.clone(),
        cache: meta.cache,
        reasoning_effort: meta.reasoning_effort,
        tools: vec![],
        web_tools: false,
        session_id: meta.id.clone(),
    };
    let asst = generate(ctx, req, &user).await;
    store.append(&meta.id, &asst)?;
    meta.leaf = Some(asst.id.clone());
    meta.updated_at = now_ms();
    store.put_meta(&meta)?;

    if a.save && !saved {
        convs.keep(&meta.id)?;
    }
    if a.save {
        eprintln!("id: {}", shown(&meta.id));
    }
    if let Some(e) = &asst.error {
        eprintln!("llmjob: {e}");
        return Ok(1);
    }
    return Ok(0);
}

/// Streams the reply to stdout and returns it as a message (errors inside).
async fn generate(ctx: &Ctx, req: ChatRequest, user: &Message) -> Message {
    let requested = req.model.clone();
    let mut asst = Message::assistant(user.id.clone(), requested.clone());
    let mut out = std::io::stdout();
    let result: Result<()> = async {
        let mut stream = ctx.provider.stream_chat(req).await?;
        while let Some(ev) = stream.next().await {
            match ev? {
                ChatEvent::Delta(t) => {
                    asst.content.push_str(&t);
                    out.write_all(t.as_bytes())?;
                    out.flush()?;
                }
                ChatEvent::Reasoning(t) => asst.reasoning.push_str(&t),
                ChatEvent::Usage(u) => asst.usage = Some(u),
                ChatEvent::Model(m) => record_model(&mut asst, &requested, m),
                ChatEvent::Provider(p) => asst.provider_name = Some(p),
                ChatEvent::ToolCall { .. } | ChatEvent::Raw(_) => {}
                ChatEvent::Done => break,
            }
        }
        return Ok(());
    }
    .await;
    if !asst.content.is_empty() && !asst.content.ends_with('\n') {
        let _ = writeln!(out);
    }
    if let Err(e) = result {
        asst.error = Some(format!("{e:#}"));
        asst.partial = true;
    }
    return asst;
}

/// `llmjob keep`: saves the last conversation if it is still in scratch.
fn keep(ctx: &Ctx) -> Result<i32> {
    let convs = Convs::open(&ctx.data_dir)?;
    let (saved, id) = convs.lookup("last")?;
    if saved {
        eprintln!("already saved: {}", shown(&id));
    } else {
        convs.keep(&id)?;
        eprintln!("id: {}", shown(&id));
    }
    return Ok(0);
}

/// `llmjob batch`: one request, or one per file in `--dir`.
async fn batch(ctx: &Ctx, a: &Args, p: &Profile) -> Result<i32> {
    let (mut model, effort) = model_of(ctx, a, p);
    if !is_batch_model(&model) {
        model.push_str(":batch");
    }
    let mut r = Reader::default();
    let headers = a.headers || p.headers;
    let system = system_of(ctx, a, p, &mut r)?;
    let common = msg_parts(ctx, a, Some(p), &mut r)?;
    let inputs: Vec<(String, Option<Part>)> = match &a.dir {
        None => vec![(jobs::single_name(), None)],
        Some(dir) => dir_files(dir)?
            .into_iter()
            .map(|(name, path)| {
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("reading {}", path.display()))?;
                let part = Part {
                    name: Some(name.clone()),
                    text,
                };
                Ok((name, Some(part)))
            })
            .collect::<Result<_>>()?,
    };
    let key = ctx.config.key_for(ctx.key_name.as_deref())?.to_string();
    let provider = p
        .provider
        .clone()
        .or_else(|| ctx.config.default_provider.clone());
    let mut reqs = Vec::new();
    for (name, file) in inputs {
        let mut parts = common.clone();
        parts.extend(file);
        parts.extend(text_part(a));
        let text =
            compose::join(&parts, headers).with_context(|| format!("{name}: empty request"))?;
        let req = ChatRequest {
            key: key.clone(),
            model: model.clone(),
            system: system.clone(),
            messages: vec![crate::provider::ChatMessage::text(
                crate::provider::Role::User,
                text,
            )],
            options: provider.clone(),
            cache: CachePolicy::default(),
            reasoning_effort: effort,
            tools: vec![],
            web_tools: false,
            session_id: format!("llmjob-{name}"),
        };
        reqs.push((name, req));
    }
    return jobs::submit(ctx, reqs).await;
}

/// Regular, non-hidden files directly in `dir`, sorted by name.
fn dir_files(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let e = e?;
        let Ok(name) = e.file_name().into_string() else {
            bail!("{}: file name is not UTF-8", e.path().display());
        };
        if name.starts_with('.') || !e.path().is_file() {
            continue;
        }
        out.push((name, e.path()));
    }
    if out.is_empty() {
        bail!("no files in {}", dir.display());
    }
    out.sort();
    return Ok(out);
}

#[cfg(test)]
mod tests;
