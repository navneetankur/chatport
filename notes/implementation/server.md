# server, config, build (`src/main.rs`, `src/config.rs`, `src/auth.rs`, `src/listen.rs`)

- `chatport [--config PATH] [--keyfile PATH] [--bind ADDR]`, `chatport gc [--delete]`.
  Config default `~/.config/chatport/config.toml` (optional when not given).
- config: `bind`, `data_dir`, `[[keys]] {name, key}` + `default_key` (default:
  first), `default_model`, `[default_provider]`, `default_file_root`, `[auth]`,
  `[batch] poll_interval`, `[profile.*]` (llmjob.md). Unknown keys logged via
  `serde_ignored`. Example: `config.example.toml`.
- keys: `--keyfile` > `OPENROUTER_API_KEY` > config, for the default key only
  (creates "default" if none). `Config::resolve_keys` at startup, `key_for(name)`
  per request; `meta.key_name` None = default; missing key = reply error.
- auth: optional HTTP Basic (`[auth] user/password`), middleware precomputes the
  header; off with a startup warning when absent. Plain HTTP, LAN only.
- systemd socket activation (`src/listen.rs`, units in `contrib/`); unusable
  activation → warn + bind from config. `--bind` overrides config, activation
  wins over both. No idle exit.
- build: `tools/build.sh [--release] [--force]` (npm install if needed, web build
  only if `web/` is newer than `dist/index.html`, then cargo);
  `tools/run.sh [--release] [args]`. `web/dist` must exist for `cargo build`.
