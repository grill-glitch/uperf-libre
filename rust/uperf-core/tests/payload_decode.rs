//! Host-side smoke test for the topic-dispatch payload parsing (AGENT.md §10.1).
//!
//! Builds without NDK; just decodes every payload variant we expect to receive
//! over `uperf_rs_on_event`. Run with `cargo test -p uperf-core`.
//!
//! We re-implement `Event::parse` here to avoid linking the libc/FFI surface
//! (host tests can't link `uperf_core`'s staticlib without an Android target).

#[derive(Debug, PartialEq, Eq, Clone)]
enum Event {
    Touch(bool),
    Btn(bool),
    InputState { hold: bool, swipe: bool, gesture: bool },
    Topapp(String),
    Offscreen(bool),
    CgroupList { topic: Topic, pids: Vec<i32> },
    CgroupUpdate(Topic),
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Hash)]
enum Topic {
    InputTouch,
    InputBtn,
    InputState,
    TopappPkgName,
    OffscreenState,
    CgroupTaList,
    CgroupFgList,
    CgroupBgList,
    CgroupReList,
    CgroupTaUpdate,
    CgroupFgUpdate,
    CgroupBgUpdate,
    CgroupReUpdate,
}

impl Topic {
    fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "input.touch" => Self::InputTouch,
            "input.btn" => Self::InputBtn,
            "input.state" => Self::InputState,
            "topapp.pkgName" => Self::TopappPkgName,
            "offscreen.state" => Self::OffscreenState,
            "cgroup.ta.list" => Self::CgroupTaList,
            "cgroup.fg.list" => Self::CgroupFgList,
            "cgroup.bg.list" => Self::CgroupBgList,
            "cgroup.re.list" => Self::CgroupReList,
            "cgroup.ta.update" => Self::CgroupTaUpdate,
            "cgroup.fg.update" => Self::CgroupFgUpdate,
            "cgroup.bg.update" => Self::CgroupBgUpdate,
            "cgroup.re.update" => Self::CgroupReUpdate,
            _ => return None,
        })
    }
}

impl Event {
    fn parse(topic: &str, payload: &[u8]) -> Option<Self> {
        use Topic::*;
        let t = Topic::from_str(topic)?;
        match t {
            InputTouch => Some(Event::Touch(decode_bool(payload))),
            InputBtn => Some(Event::Btn(decode_bool(payload))),
            InputState => decode_input_state(payload)
                .map(|(hold, swipe, gesture)| Event::InputState { hold, swipe, gesture }),
            TopappPkgName => {
                let s = std::str::from_utf8(payload).ok()?;
                Some(Event::Topapp(s.trim_end_matches('\0').to_owned()))
            }
            OffscreenState => Some(Event::Offscreen(decode_bool(payload))),
            CgroupTaList | CgroupFgList | CgroupBgList | CgroupReList => {
                let pids = decode_pid_list(payload).unwrap_or_default();
                Some(Event::CgroupList { topic: t, pids })
            }
            CgroupTaUpdate | CgroupFgUpdate | CgroupBgUpdate | CgroupReUpdate => {
                Some(Event::CgroupUpdate(t))
            }
        }
    }
}

fn decode_bool(payload: &[u8]) -> bool {
    if payload.len() < 4 {
        return false;
    }
    i32::from_ne_bytes(payload[0..4].try_into().unwrap()) != 0
}

fn decode_input_state(payload: &[u8]) -> Option<(bool, bool, bool)> {
    if payload.len() < 12 {
        return None;
    }
    let hold = i32::from_ne_bytes(payload[0..4].try_into().ok()?) != 0;
    let swipe = i32::from_ne_bytes(payload[4..8].try_into().ok()?) != 0;
    let gesture = i32::from_ne_bytes(payload[8..12].try_into().ok()?) != 0;
    Some((hold, swipe, gesture))
}

fn decode_pid_list(payload: &[u8]) -> Option<Vec<i32>> {
    if payload.len() < 16 {
        return None;
    }
    let pids_ptr = u64::from_ne_bytes(payload[0..8].try_into().unwrap()) as *const i32;
    let len = u64::from_ne_bytes(payload[8..16].try_into().unwrap()) as usize;
    if pids_ptr.is_null() || len == 0 {
        return Some(Vec::new());
    }
    // SAFETY: same contract as the real bridge (caller-owned valid i32 slice).
    let pids = unsafe { std::slice::from_raw_parts(pids_ptr, len) };
    Some(pids.to_vec())
}

// ---- tests -----------------------------------------------------------------

fn bool_payload(v: bool) -> Vec<u8> {
    let n: i32 = if v { 1 } else { 0 };
    n.to_ne_bytes().to_vec()
}

fn input_state_payload(hold: bool, swipe: bool, gesture: bool) -> Vec<u8> {
    let mut v = Vec::with_capacity(12);
    for b in [hold, swipe, gesture] {
        v.extend_from_slice(&(if b { 1i32 } else { 0i32 }).to_ne_bytes());
    }
    v
}

fn pid_list_payload(pids: &[i32]) -> Vec<u8> {
    let mut v = Vec::with_capacity(16);
    let ptr = pids.as_ptr() as usize as u64;
    v.extend_from_slice(&ptr.to_ne_bytes());
    v.extend_from_slice(&(pids.len() as u64).to_ne_bytes());
    v
}

#[test]
fn parse_input_touch_true() {
    let e = Event::parse("input.touch", &bool_payload(true)).unwrap();
    assert!(matches!(e, Event::Touch(true)));
}

#[test]
fn parse_input_btn_false() {
    let e = Event::parse("input.btn", &bool_payload(false)).unwrap();
    assert!(matches!(e, Event::Btn(false)));
}

#[test]
fn parse_input_state() {
    let e = Event::parse("input.state", &input_state_payload(true, true, false)).unwrap();
    match e {
        Event::InputState { hold, swipe, gesture } => {
            assert!(hold);
            assert!(swipe);
            assert!(!gesture);
        }
        _ => panic!("wrong event"),
    }
}

#[test]
fn parse_topapp() {
    let e = Event::parse("topapp.pkgName", b"org.librelab.messaging").unwrap();
    match e {
        Event::Topapp(s) => assert_eq!(s, "org.librelab.messaging"),
        _ => panic!("wrong event"),
    }
}

#[test]
fn parse_offscreen() {
    let e = Event::parse("offscreen.state", &bool_payload(true)).unwrap();
    assert!(matches!(e, Event::Offscreen(true)));
}

#[test]
fn parse_cgroup_list() {
    let pids = vec![1i32, 2, 3, 4, 5];
    let e = Event::parse("cgroup.ta.list", &pid_list_payload(&pids)).unwrap();
    match e {
        Event::CgroupList { pids: got, .. } => assert_eq!(got, pids),
        _ => panic!("wrong event"),
    }
}

#[test]
fn parse_cgroup_update() {
    let e = Event::parse("cgroup.fg.update", &[]).unwrap();
    assert!(matches!(e, Event::CgroupUpdate(_)));
}

#[test]
fn unknown_topic_returns_none() {
    assert!(Event::parse("does.not.exist", &[]).is_none());
}

#[test]
fn short_pid_list_returns_empty() {
    // 8 bytes parses as {ptr, len} where len=0 → empty pid Vec (a valid
    // "no pids" event per the bridge contract).
    let pids: Vec<u8> = vec![0u8; 8];
    let e = Event::parse("cgroup.ta.list", &pids).unwrap();
    match e {
        Event::CgroupList { pids, .. } => assert!(pids.is_empty()),
        _ => panic!("wrong event"),
    }
}

#[test]
fn empty_pid_list_returns_empty_vec() {
    // uperf_pid_list_t with len=0 is a valid "no pids" event.
    let pids: Vec<i32> = vec![];
    let e = Event::parse("cgroup.bg.list", &pid_list_payload(&pids)).unwrap();
    match e {
        Event::CgroupList { pids: got, .. } => assert!(got.is_empty()),
        _ => panic!("wrong event"),
    }
}