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
use std::os::unix::net::{SocketAddr, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

// The platform-specific address constructors. Both paths exist and were verified by
// compile probe on this repo's two real targets (`UNSAFE_AUDIT_REPORT.md` §R-1):
// the host test target is Linux, the device is Android.
#[cfg(target_os = "android")]
use std::os::android::net::SocketAddrExt;
#[cfg(target_os = "linux")]
use std::os::linux::net::SocketAddrExt;

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
///
/// The bytes are std's, not ours: `from_abstract_name` writes the leading NUL, the
/// name, and an address length of `sizeof(sa_family_t) + 1 + len`; `from_pathname`
/// writes the path plus the trailing NUL the kernel expects. The length and byte
/// boundaries are the same ones the previous hand-built version used (name ≤ 107
/// bytes, path ≤ 107 bytes), so no caller-visible limit moved.
///
/// The error strings are the previous ones verbatim. Two deliberate deviations from
/// "just call std":
///
/// * an empty abstract name (`@` — reachable as `UPERF_CTL_SOCKET=@`) is still
///   refused. std accepts it and builds the *anonymous* abstract address, which
///   would turn a config mistake into an endless `ECONNREFUSED` retry loop instead
///   of the previous one-line validation error;
/// * a pathname containing an interior NUL is refused by std and reported with the
///   "too long" message. That cannot be reached from `UPERF_CTL_SOCKET`, because an
///   environment value cannot contain a NUL byte.
fn sockaddr(addr: &str) -> Result<SocketAddr, String> {
    match addr.strip_prefix('@') {
        Some(name) if name.is_empty() => {
            Err(format!("abstract socket name too long: {addr:?}"))
        }
        Some(name) => SocketAddr::from_abstract_name(name.as_bytes())
            .map_err(|_| format!("abstract socket name too long: {addr:?}")),
        None => SocketAddr::from_pathname(addr)
            .map_err(|_| format!("socket path too long: {addr:?}")),
    }
}

/// Connect out to the peer's socket.
///
/// std creates the socket (with `SOCK_CLOEXEC`) and connects it in one call, so the
/// previous `socket(2)` / `connect(2)` branches are merged into one error. See
/// [`is_socket_creation_errno`] for how the two are still told apart in the message.
pub fn connect(addr: &str) -> Result<UnixStream, String> {
    let sa = sockaddr(addr)?;
    UnixStream::connect_addr(&sa).map_err(|e| {
        if is_socket_creation_errno(e.raw_os_error()) {
            format!("socket: {e}")
        } else {
            format!("connect {addr}: {e}")
        }
    })
}

/// Did this errno come from `socket(2)` rather than `connect(2)`?
///
/// `UnixStream::connect_addr` performs both syscalls and hands back one
/// `io::Error`, where the previous implementation logged `socket: <e>` for the
/// first and `connect <addr>: <e>` for the second. These are the errnos only
/// `socket(2)` can produce: resource exhaustion (`EMFILE`, `ENFILE`, `ENOMEM`,
/// `ENOBUFS`) and the address-family/type pair being refused
/// (`EPROTONOSUPPORT`, `EAFNOSUPPORT`). `connect(2)` on an already-valid
/// `AF_UNIX`/`SOCK_STREAM` fd cannot return any of them, so for every errno that
/// can actually occur the original distinction is preserved exactly.
///
/// `EACCES` is deliberately **not** in the set: `connect(2)` does return it for a
/// filesystem path, and the old code reported that as a `connect` failure.
fn is_socket_creation_errno(errno: Option<i32>) -> bool {
    match errno {
        Some(e) => matches!(
            e,
            libc::EMFILE
                | libc::ENFILE
                | libc::ENOMEM
                | libc::ENOBUFS
                | libc::EPROTONOSUPPORT
                | libc::EAFNOSUPPORT
        ),
        None => false,
    }
}

/// Our uid, as the peer will see it on its end of the socket.
pub fn uid() -> u32 {
    // SAFETY: `getuid` takes no arguments and cannot fail.
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

    /// The address bytes are the observable contract with the peer: an abstract
    /// address must hold the name *exactly* — leading NUL, no terminator, no prefix —
    /// because the independent peer used on device (`tools/ctl-listen`) builds the
    /// same layout by hand and the kernel compares bytes up to the address length.
    ///
    /// This replaces the old `sockaddr_un`-layout assertions, which pinned an
    /// internal representation instead of the observable address.
    #[test]
    fn abstract_addresses_keep_the_exact_name_bytes() {
        assert!(sockaddr("@uperf-ctl").is_ok());
        assert!(sockaddr("/data/local/tmp/x.sock").is_ok());

        let sa = sockaddr("@abc").unwrap();
        assert_eq!(sa.as_abstract_name(), Some(&b"abc"[..]), "name bytes changed");
        assert!(sa.as_pathname().is_none(), "an abstract address is not a pathname");

        let name = "uperf-e2e-0123456789";
        let sa = sockaddr(&format!("@{name}")).unwrap();
        assert_eq!(sa.as_abstract_name(), Some(name.as_bytes()), "name bytes changed");

        let sa = sockaddr("/data/local/tmp/x.sock").unwrap();
        assert_eq!(sa.as_pathname(), Some(Path::new("/data/local/tmp/x.sock")));
        assert!(sa.as_abstract_name().is_none(), "a pathname is not abstract");
    }

    /// The length limits must not move with the refactor: the hand-built version
    /// accepted a 107-byte abstract name (which is `SUN_LEN` with the leading NUL)
    /// and refused 108, and the same for a pathname. Now that the boundary comes
    /// from std it is pinned here rather than assumed.
    #[test]
    fn address_length_limits_are_unchanged() {
        assert!(sockaddr(&format!("@{}", "x".repeat(107))).is_ok());
        assert!(sockaddr(&format!("@{}", "x".repeat(108))).is_err());
        let path_ok = format!("/{}", "y".repeat(106));
        assert_eq!(path_ok.len(), 107);
        assert!(sockaddr(&path_ok).is_ok());
        let path_bad = format!("/{}", "y".repeat(107));
        assert_eq!(path_bad.len(), 108);
        assert!(sockaddr(&path_bad).is_err());
        assert!(sockaddr("@").is_err(), "an empty abstract name is refused");
        // ...and it is refused with the message the previous implementation used, not
        // with a "connect failed" line from the anonymous abstract address std builds
        // for an empty name.
        assert_eq!(
            sockaddr("@").unwrap_err(),
            "abstract socket name too long: \"@\""
        );
    }

    /// The `socket(2)`-vs-`connect(2)` distinction in the log line is preserved for
    /// every errno that can actually occur: a missing endpoint is a `connect`.
    #[test]
    fn a_missing_endpoint_reports_a_connect_failure() {
        let dir = std::env::temp_dir().join(format!("uperf_ctl_miss_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("nobody-listening");
        let addr = sock.to_str().unwrap().to_string();

        let e = connect(&addr).unwrap_err();
        assert!(e.starts_with(&format!("connect {addr}: ")), "got {e:?}");

        let e = connect("@uperf-nobody-listening-at-all").unwrap_err();
        assert!(e.starts_with("connect @uperf-nobody-listening-at-all: "), "got {e:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A real connect over an **abstract** address, checking the two properties the
    /// removed `unsafe` used to provide by hand: `SOCK_CLOEXEC` on the fd (or it
    /// leaks into every child the daemon spawns) and a link that carries bytes both
    /// ways. The byte-level agreement with the independent hand-built peer
    /// (`tools/ctl-listen`) is what the alioth e2e run demonstrates — see §R-1 of
    /// `UNSAFE_AUDIT_REPORT.md`; this is the host-side half.
    #[test]
    fn abstract_connect_works_and_the_fd_is_cloexec() {
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixListener;

        let addr = format!("@uperf-cloexec-{}", std::process::id());
        let listener = UnixListener::bind_addr(&sockaddr(&addr).unwrap()).unwrap();
        let mut stream = connect(&addr).expect("connect to the abstract listener");
        let (mut peer, _) = listener.accept().expect("accept");

        let fd = stream.as_raw_fd();
        let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{fd}")).unwrap();
        let flags = info
            .lines()
            .find_map(|l| l.strip_prefix("flags:"))
            .expect("fdinfo has a flags line")
            .trim();
        let flags = u32::from_str_radix(flags, 8).expect("fdinfo flags are octal");
        assert_eq!(flags & 0o2000000, 0o2000000, "O_CLOEXEC missing (flags={flags:o})");

        stream.write_all(b"PING\n").unwrap();
        stream.flush().unwrap();
        let mut line = String::new();
        BufReader::new(peer.try_clone().unwrap()).read_line(&mut line).unwrap();
        assert_eq!(line, "PING\n");

        peer.write_all(b"PONG\n").unwrap();
        peer.flush().unwrap();
        let mut back = String::new();
        BufReader::new(stream.try_clone().unwrap()).read_line(&mut back).unwrap();
        assert_eq!(back, "PONG\n");
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
