//! Control-plane socket (AGENT.md §11 queue item ⑦) — the cheap handshake subset.
//!
//! Borrowed from AppOpt's shape: a **local socket** and a `token`/`version`/`pid`
//! handshake, with the daemon as the side that **connects out** (反连) to a socket its
//! peer owns. The peer there is an app; here it is whatever wants the daemon's data or
//! wants to drive it (a controller, a tool, a future scene app).
//!
//! This round implements only the cheap half, deliberately:
//!
//! * connect (abstract `@name` or a filesystem path),
//! * send `HELLO v=<n> pid=<pid> uid=<uid> token=<hex>`,
//! * require `OK v=<n>` back, and refuse a peer whose protocol version differs,
//! * keep the link alive with `PING` / `PONG` so a dropped peer is noticed.
//!
//! No command set yet — that is the expensive half and it needs a consumer to design it
//! against. Everything above the syscalls is pure and host-tested.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Protocol version. A peer that answers with a different one is refused, not guessed at.
pub const VERSION: u32 = 1;

/// One handshake message from the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub version: u32,
    pub pid: u32,
    pub uid: u32,
    pub token: String,
}

impl Hello {
    /// `HELLO v=1 pid=123 uid=0 token=<hex>` — one line, no spaces inside a value so the
    /// parser stays a split-and-match.
    pub fn line(&self) -> String {
        format!(
            "HELLO v={} pid={} uid={} token={}\n",
            self.version, self.pid, self.uid, self.token
        )
    }

    /// Parse a `HELLO` line. Unknown keys are ignored; a missing or malformed required
    /// field is a refusal, never a default.
    pub fn parse(line: &str) -> Option<Hello> {
        let line = line.trim();
        let mut it = line.split_whitespace();
        if it.next()? != "HELLO" {
            return None;
        }
        let (mut v, mut pid, mut uid) = (None, None, None);
        let mut token = None;
        for kv in it {
            let (k, val) = kv.split_once('=')?;
            match k {
                "v" => v = val.parse::<u32>().ok(),
                "pid" => pid = val.parse::<u32>().ok(),
                "uid" => uid = val.parse::<u32>().ok(),
                "token" => token = Some(val.to_string()),
                _ => {}
            }
        }
        Some(Hello {
            version: v?,
            pid: pid?,
            uid: uid?,
            token: token.filter(|t| !t.is_empty())?,
        })
    }
}

/// The peer's answer to a `HELLO` (or a `PING`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ack {
    Ok { version: u32 },
    Pong,
    Err,
}

/// Parse one reply line. Anything unrecognised is an error, because a control channel
/// that silently ignores its peer is worse than one that says so.
pub fn parse_reply(line: &str) -> Result<Ack, String> {
    let line = line.trim();
    if line == "PONG" {
        return Ok(Ack::Pong);
    }
    let mut it = line.split_whitespace();
    match it.next() {
        Some("OK") => {
            let v = it
                .next()
                .and_then(|kv| kv.strip_prefix("v="))
                .and_then(|s| s.parse::<u32>().ok())
                .ok_or_else(|| format!("malformed OK line: {line:?}"))?;
            Ok(Ack::Ok { version: v })
        }
        Some("ERR") => Ok(Ack::Err),
        _ => Err(format!("unexpected reply: {line:?}")),
    }
}

/// A fresh token: 16 bytes from `/dev/urandom`, hex. `None` if there is no entropy to be
/// had — an invented token would be worse than none.
pub fn token_from_urandom() -> Option<String> {
    let mut buf = [0u8; 16];
    let f = std::fs::File::open("/dev/urandom").ok()?;
    use std::io::Read as _;
    let mut f = f;
    f.read_exact(&mut buf).ok()?;
    Some(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// The control token: the file if it holds one, otherwise a fresh token written there
/// (0600). A token we cannot persist is still returned — the link is one-way then, which
/// is fine for the handshake-only subset.
pub fn load_or_create_token(path: &Path) -> Option<String> {
    if let Ok(s) = std::fs::read_to_string(path) {
        let t = s.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    let t = token_from_urandom()?;
    write_private(path, &format!("{t}\n"));
    Some(t)
}

fn write_private(path: &Path, data: &str) {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
    {
        let _ = f.write_all(data.as_bytes());
    }
}

/// Where the token lives: `UPERF_CTL_TOKEN_FILE`, else `<config dir>/uperf.token`.
pub fn token_path(cfg_dir: Option<&Path>) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("UPERF_CTL_TOKEN_FILE") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    cfg_dir.map(|d| d.join("uperf.token"))
}

/// `@name` is an abstract socket (the Android `LocalSocket` idiom); anything else is a
/// filesystem path. The distinction is in the address, so both are built here.
fn sockaddr(addr: &str) -> Result<(libc::sockaddr_un, libc::socklen_t), String> {
    let mut sa: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    sa.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let path = sa.sun_path.len();
    let len = if let Some(name) = addr.strip_prefix('@') {
        let b = name.as_bytes();
        if b.is_empty() || b.len() + 1 > path {
            return Err(format!("abstract socket name too long: {addr:?}"));
        }
        sa.sun_path[0] = 0; // abstract: leading NUL, then the name
        for (i, c) in b.iter().enumerate() {
            sa.sun_path[i + 1] = *c as libc::c_char;
        }
        (std::mem::size_of::<libc::sa_family_t>() + 1 + b.len()) as libc::socklen_t
    } else {
        let b = addr.as_bytes();
        if b.len() + 1 > path {
            return Err(format!("socket path too long: {addr:?}"));
        }
        for (i, c) in b.iter().enumerate() {
            sa.sun_path[i] = *c as libc::c_char;
        }
        (std::mem::size_of::<libc::sa_family_t>() + b.len() + 1) as libc::socklen_t
    };
    Ok((sa, len))
}

/// Connect out to the peer's socket.
pub fn connect(addr: &str) -> Result<UnixStream, String> {
    let (sa, len) = sockaddr(addr)?;
    let fd: RawFd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(format!("socket: {}", std::io::Error::last_os_error()));
    }
    let rc = unsafe { libc::connect(fd, &sa as *const libc::sockaddr_un as *const libc::sockaddr, len) };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(format!("connect {addr}: {e}"));
    }
    Ok(unsafe { UnixStream::from_raw_fd(fd) })
}

/// Our uid, as the peer will see it on its end of the socket.
pub fn uid() -> u32 {
    unsafe { libc::getuid() }
}

/// Send the hello and demand a matching `OK`. Returns the peer's version.
pub fn handshake(stream: &mut UnixStream, token: &str) -> Result<u32, String> {
    let hello = Hello {
        version: VERSION,
        pid: std::process::id(),
        uid: uid(),
        token: token.to_string(),
    };
    stream
        .write_all(hello.line().as_bytes())
        .map_err(|e| format!("write hello: {e}"))?;
    stream.flush().ok();

    let mut line = String::new();
    let mut rd = BufReader::new(stream.try_clone().map_err(|e| format!("clone: {e}"))?);
    rd.read_line(&mut line).map_err(|e| format!("read ack: {e}"))?;
    match parse_reply(&line)? {
        Ack::Ok { version } if version == VERSION => Ok(version),
        Ack::Ok { version } => Err(format!("peer speaks v{version}, we speak v{VERSION}")),
        Ack::Err => Err("peer refused the handshake".into()),
        Ack::Pong => Err("peer answered a hello with a pong".into()),
    }
}

/// The control-plane task. Opt-in: `UPERF_CTL_SOCKET=@name` (or a path).
pub struct CtlTask {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl CtlTask {
    /// The configured socket address, or `None` when the feature is off.
    pub fn socket_addr() -> Option<String> {
        std::env::var("UPERF_CTL_SOCKET").ok().filter(|s| !s.is_empty())
    }

    pub fn spawn<L>(cfg_dir: Option<PathBuf>, log: L) -> Option<CtlTask>
    where
        L: Fn(&str) + Send + 'static,
    {
        let addr = Self::socket_addr()?;
        let retry_ms = std::env::var("UPERF_CTL_RETRY_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(5000);
        let token = token_path(cfg_dir.as_deref())
            .as_deref()
            .and_then(load_or_create_token);

        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let handle = std::thread::Builder::new()
            .name("uperf-ctl".into())
            .spawn(move || {
                let Some(token) = token else {
                    log("Rust: ctl-socket: no token available, not connecting");
                    return;
                };
                // Log each state change, not each retry: a peer that is simply not
                // there must not turn the log into a loop.
                let mut prev_failed = false;
                while !stop_thread.load(Ordering::SeqCst) {
                    match connect(&addr) {
                        Ok(mut s) => {
                            if prev_failed {
                                log(&format!("Rust: ctl-socket: connected to {addr} (v{VERSION})"));
                                prev_failed = false;
                            }
                            match handshake(&mut s, &token) {
                                Ok(v) => {
                                    log(&format!("Rust: ctl-socket: handshake ok, peer v{v}"));
                                    if let Err(e) = serve(&mut s, &stop_thread, &log) {
                                        log(&format!("Rust: ctl-socket: link ended: {e}"));
                                    }
                                }
                                Err(e) => log(&format!("Rust: ctl-socket: handshake failed: {e}")),
                            }
                        }
                        Err(e) => {
                            if !prev_failed {
                                log(&format!("Rust: ctl-socket: {e} (retrying)"));
                                prev_failed = true;
                            }
                        }
                    }
                    let mut slept = 0u64;
                    while slept < retry_ms && !stop_thread.load(Ordering::SeqCst) {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        slept += 50;
                    }
                }
                log("Rust: ctl-socket stopped");
            })
            .ok()?;
        Some(CtlTask { stop, handle: Some(handle) })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// After the handshake, keep the link alive: the daemon *drives* the exchange by sending
/// `PING` and requiring `PONG`, and it also answers a `PING` that arrives from the peer.
/// That is the whole command set for now — enough to prove the channel is two-way and
/// that a peer which stops answering is noticed instead of assumed healthy.
fn serve(stream: &mut UnixStream, stop: &Arc<AtomicBool>, log: &impl Fn(&str)) -> Result<(), String> {
    let ping_every = std::env::var("UPERF_CTL_PING_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(2000);
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(500)));
    let mut rd = BufReader::new(stream.try_clone().map_err(|e| format!("clone: {e}"))?);
    let mut pongs = 0u64;
    let mut unanswered = 0u32;
    let mut last_ping = std::time::Instant::now();
    let mut line = String::new();
    while !stop.load(Ordering::SeqCst) {
        if last_ping.elapsed() >= std::time::Duration::from_millis(ping_every) {
            if unanswered >= 3 {
                return Err(format!("peer stopped answering ({unanswered} pings)"));
            }
            stream.write_all(b"PING\n").map_err(|e| format!("write ping: {e}"))?;
            stream.flush().ok();
            unanswered += 1;
            last_ping = std::time::Instant::now();
        }
        line.clear();
        match rd.read_line(&mut line) {
            Ok(0) => return Err("peer closed".into()),
            Ok(_) => {
                let l = line.trim();
                if l == "PONG" {
                    unanswered = 0;
                    pongs += 1;
                    if pongs == 1 || pongs % 30 == 0 {
                        log(&format!("Rust: ctl-socket: pong #{pongs}"));
                    }
                } else if l.eq_ignore_ascii_case("PING") {
                    stream.write_all(b"PONG\n").map_err(|e| format!("write pong: {e}"))?;
                    stream.flush().ok();
                } else if !l.is_empty() {
                    log(&format!("Rust: ctl-socket: ignoring unsupported line {l:?}"));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
                continue;
            }
            Err(e) => return Err(format!("read: {e}")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_round_trips() {
        let h = Hello { version: VERSION, pid: 4242, uid: 0, token: "deadbeef".into() };
        let line = h.line();
        assert!(line.ends_with('\n'));
        assert_eq!(Hello::parse(&line), Some(h));
    }

    #[test]
    fn hello_rejects_missing_or_empty_fields() {
        assert_eq!(Hello::parse("HELLO v=1 pid=1 uid=0"), None, "no token");
        assert_eq!(Hello::parse("HELLO v=1 pid=1 uid=0 token="), None, "empty token");
        assert_eq!(Hello::parse("HELLO v=1 pid=1 token=ab"), None, "no uid");
        assert_eq!(Hello::parse("HELLO v=x pid=1 uid=0 token=ab"), None, "bad version");
        assert_eq!(Hello::parse("PONG"), None);
        // unknown keys are ignored, not fatal
        let h = Hello::parse("HELLO v=1 pid=1 uid=0 token=ab extra=1").unwrap();
        assert_eq!(h.token, "ab");
    }

    #[test]
    fn replies_are_parsed_strictly() {
        assert_eq!(parse_reply("OK v=1\n"), Ok(Ack::Ok { version: 1 }));
        assert_eq!(parse_reply("PONG\n"), Ok(Ack::Pong));
        assert_eq!(parse_reply("ERR no token\n"), Ok(Ack::Err));
        assert!(parse_reply("OK\n").is_err(), "OK without a version is malformed");
        assert!(parse_reply("what\n").is_err());
    }

    #[test]
    fn tokens_are_random_hex_and_persisted() {
        let dir = std::env::temp_dir().join(format!("uperf_ctl_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("uperf.token");
        let a = load_or_create_token(&path).expect("a token");
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        // the second call reads the same one back
        assert_eq!(load_or_create_token(&path).as_deref(), Some(a.as_str()));
        // and it is not world-readable
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token file mode {mode:o}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn abstract_and_path_addresses_are_both_accepted() {
        assert!(sockaddr("@uperf-ctl").is_ok());
        assert!(sockaddr("/data/local/tmp/x.sock").is_ok());
        let (sa, len) = sockaddr("@abc").unwrap();
        assert_eq!(sa.sun_path[0], 0, "abstract sockets start with a NUL");
        assert_eq!(sa.sun_path[1] as u8, b'a');
        // len counts the leading NUL
        assert_eq!(
            len as usize,
            std::mem::size_of::<libc::sa_family_t>() + 1 + 3
        );
        let long = format!("@{}", "x".repeat(300));
        assert!(sockaddr(&long).is_err(), "an over-long name must be refused");
    }

    /// The handshake against a listener we own, so the framing is proven end to end on
    /// the host (the device run uses the same code).
    #[test]
    fn handshake_accepts_a_matching_peer_and_refuses_a_mismatched_version() {
        for (reply, want_ok) in [("OK v=1\n", true), ("OK v=2\n", false), ("ERR nope\n", false)] {
            let dir = std::env::temp_dir().join(format!(
                "uperf_ctl_hs_{}_{}",
                std::process::id(),
                reply.len()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let sock = dir.join("s");
            let addr = sock.to_str().unwrap().to_string();

            let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
            let peer = std::thread::spawn(move || {
                let (mut c, _) = listener.accept().unwrap();
                let mut rd = BufReader::new(c.try_clone().unwrap());
                let mut line = String::new();
                rd.read_line(&mut line).unwrap();
                let h = Hello::parse(&line).expect("a well-formed hello");
                assert_eq!(h.version, VERSION);
                assert_eq!(h.pid, std::process::id());
                assert_eq!(h.token, "abc123");
                c.write_all(reply.as_bytes()).unwrap();
                c.flush().unwrap();
                h
            });

            let mut s = connect(&addr).expect("connect");
            let got = handshake(&mut s, "abc123");
            let h = peer.join().unwrap();
            assert_eq!(h.uid, uid());
            assert_eq!(got.is_ok(), want_ok, "reply {reply:?} -> {got:?}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
