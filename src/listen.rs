//! Listener setup: systemd socket activation, falling back to `bind` from config.

use anyhow::{Context, Result};
use std::os::fd::FromRawFd;

/// First fd systemd passes (`SD_LISTEN_FDS_START`).
const FD_START: i32 = 3;

/// What the `LISTEN_*` environment says.
#[derive(Debug, PartialEq)]
pub enum Activation {
    /// Not socket-activated (vars absent, or meant for another process).
    None,
    /// Exactly one fd passed to us.
    One,
    /// Activation vars are present and meant for us, but unusable.
    Unusable(String),
}

fn parse_env(listen_pid: Option<&str>, listen_fds: Option<&str>, my_pid: u32) -> Activation {
    let (Some(pid), Some(fds)) = (listen_pid, listen_fds) else {
        return Activation::None;
    };
    if pid.trim().parse::<u32>().ok() != Some(my_pid) {
        return Activation::None;
    }
    return match fds.trim().parse::<u32>() {
        Ok(1) => Activation::One,
        Ok(n) => Activation::Unusable(format!("LISTEN_FDS={n}, expected 1")),
        Err(_) => Activation::Unusable(format!("LISTEN_FDS={fds:?} is not a number")),
    };
}

/// Take the systemd-passed socket at fd 3 as a tokio listener.
fn take_fd() -> Result<tokio::net::TcpListener> {
    // SAFETY: LISTEN_PID matched our pid and LISTEN_FDS=1, so systemd handed us
    // fd 3 and nothing else in this process owns it.
    let std_l = unsafe { std::net::TcpListener::from_raw_fd(FD_START) };
    // getsockname fails / mis-parses for non-inet sockets: our "is it TCP" check.
    std_l
        .local_addr()
        .context("passed fd 3 is not a TCP socket")?;
    std_l.set_nonblocking(true)?;
    return Ok(tokio::net::TcpListener::from_std(std_l)?);
}

/// Read and clear the `LISTEN_*` vars.
///
/// # Safety
/// The process must be single-threaded (call from `main` before the tokio
/// runtime is built): `remove_var` races with any concurrent env read,
/// including libc's own (`getaddrinfo`, `localtime`, ...).
pub unsafe fn take_activation() -> Activation {
    let act = parse_env(
        std::env::var("LISTEN_PID").ok().as_deref(),
        std::env::var("LISTEN_FDS").ok().as_deref(),
        std::process::id(),
    );
    // Don't leak activation vars to anything we spawn.
    // SAFETY: caller guarantees we are single-threaded.
    unsafe {
        std::env::remove_var("LISTEN_PID");
        std::env::remove_var("LISTEN_FDS");
        std::env::remove_var("LISTEN_FDNAMES");
    }
    return act;
}

/// Socket-activated listener if `act` says systemd passed one, else bind `bind`.
pub async fn listener(act: Activation, bind: &str) -> Result<tokio::net::TcpListener> {
    match act {
        Activation::One => match take_fd() {
            Ok(l) => {
                let addr = l.local_addr()?;
                log::info!(
                    "socket-activated, listening on http://{addr} (configured bind ignored)"
                );
                return Ok(l);
            }
            Err(e) => log::warn!("socket activation unusable ({e:#}); binding {bind}"),
        },
        Activation::Unusable(why) => {
            log::warn!("socket activation unusable ({why}); binding {bind}")
        }
        Activation::None => {}
    }
    let l = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    log::info!("listening on http://{bind}");
    return Ok(l);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_vars() {
        assert_eq!(parse_env(None, None, 42), Activation::None);
        assert_eq!(parse_env(Some("42"), None, 42), Activation::None);
    }

    #[test]
    fn other_pid() {
        assert_eq!(parse_env(Some("7"), Some("1"), 42), Activation::None);
        assert_eq!(parse_env(Some("x"), Some("1"), 42), Activation::None);
    }

    #[test]
    fn one_fd() {
        assert_eq!(parse_env(Some("42"), Some("1"), 42), Activation::One);
    }

    #[test]
    fn unusable() {
        assert!(matches!(
            parse_env(Some("42"), Some("2"), 42),
            Activation::Unusable(_)
        ));
        assert!(matches!(
            parse_env(Some("42"), Some("0"), 42),
            Activation::Unusable(_)
        ));
        assert!(matches!(
            parse_env(Some("42"), Some("z"), 42),
            Activation::Unusable(_)
        ));
    }
}
