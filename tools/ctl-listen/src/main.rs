//! ctl-listen — the peer side of ⑦'s handshake, for device verification.
//!
//! It owns the socket (abstract `@name`, the Android `LocalSocket` idiom, or a
//! filesystem path), accepts one connection, prints the daemon's `HELLO`
//! (token/version/pid/uid), answers `OK v=1`, and then answers `PING` with `PONG` so the
//! keep-alive is exercised too. `--reject` answers `ERR` instead, which is how the
//! daemon's refusal path is checked.
//!
//! usage: ctl-listen <@name|path> [--reject] [--seconds N]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::io::FromRawFd;

fn sockaddr(addr: &str) -> Result<(libc::sockaddr_un, libc::socklen_t), String> {
    let mut sa: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    sa.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let cap = sa.sun_path.len();
    let len = if let Some(name) = addr.strip_prefix('@') {
        let b = name.as_bytes();
        if b.is_empty() || b.len() + 1 > cap {
            return Err(format!("bad abstract name {addr:?}"));
        }
        sa.sun_path[0] = 0;
        for (i, c) in b.iter().enumerate() {
            sa.sun_path[i + 1] = *c as libc::c_char;
        }
        (std::mem::size_of::<libc::sa_family_t>() + 1 + b.len()) as libc::socklen_t
    } else {
        let b = addr.as_bytes();
        if b.len() + 1 > cap {
            return Err(format!("path too long {addr:?}"));
        }
        for (i, c) in b.iter().enumerate() {
            sa.sun_path[i] = *c as libc::c_char;
        }
        (std::mem::size_of::<libc::sa_family_t>() + b.len() + 1) as libc::socklen_t
    };
    Ok((sa, len))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let addr = args.iter().find(|a| !a.starts_with("--")).cloned().unwrap_or_else(|| "@uperf-ctl".into());
    let reject = args.iter().any(|a| a == "--reject");
    let seconds: u64 = args
        .iter()
        .position(|a| a == "--seconds")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);

    let (sa, len) = match sockaddr(&addr) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        eprintln!("socket: {}", std::io::Error::last_os_error());
        std::process::exit(2);
    }
    if unsafe { libc::bind(fd, &sa as *const libc::sockaddr_un as *const libc::sockaddr, len) } != 0 {
        eprintln!("bind {addr}: {}", std::io::Error::last_os_error());
        std::process::exit(2);
    }
    if unsafe { libc::listen(fd, 4) } != 0 {
        eprintln!("listen: {}", std::io::Error::last_os_error());
        std::process::exit(2);
    }
    println!("ctl-listen: bound {addr} (reject={reject}, {seconds}s)");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    while std::time::Instant::now() < deadline {
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        let pr = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, 1000) };
        if pr <= 0 || (pfd.revents & libc::POLLIN) == 0 {
            continue;
        }
        let cfd = unsafe { libc::accept(fd, std::ptr::null_mut(), std::ptr::null_mut()) };
        if cfd < 0 {
            continue;
        }
        let mut conn = unsafe { std::os::unix::net::UnixStream::from_raw_fd(cfd) };
        // peer credentials: the daemon's real uid/pid as the kernel sees them
        let mut ucred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut l = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let got = unsafe {
            libc::getsockopt(
                cfd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut ucred as *mut libc::ucred as *mut libc::c_void,
                &mut l,
            ) == 0
        };
        if got {
            println!("peercred: uid={} pid={}", ucred.uid, ucred.pid);
        }

        let mut rd = BufReader::new(conn.try_clone().unwrap());
        let mut line = String::new();
        if rd.read_line(&mut line).unwrap_or(0) == 0 {
            println!("peer closed before the handshake");
            continue;
        }
        let l = line.trim();
        println!("hello: {l}");
        let ok = l.starts_with("HELLO ") && l.contains("v=1") && l.contains("token=");
        println!("hello_well_formed={ok}");
        let reply = if reject || !ok { "ERR refused by ctl-listen\n" } else { "OK v=1\n" };
        let _ = conn.write_all(reply.as_bytes());
        let _ = conn.flush();
        println!("replied: {}", reply.trim());

        if reject || !ok {
            continue;
        }
        let _ = conn.set_read_timeout(Some(std::time::Duration::from_millis(1000)));
        while std::time::Instant::now() < deadline {
            line.clear();
            match rd.read_line(&mut line) {
                Ok(0) => {
                    println!("peer closed");
                    break;
                }
                Ok(_) => {
                    let t = line.trim();
                    if t.eq_ignore_ascii_case("PING") {
                        let _ = conn.write_all(b"PONG\n");
                        let _ = conn.flush();
                        println!("pong");
                    } else if !t.is_empty() {
                        println!("line: {t}");
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
                    continue;
                }
                Err(e) => {
                    println!("read: {e}");
                    break;
                }
            }
        }
        break;
    }
    println!("ctl-listen: done");
}
