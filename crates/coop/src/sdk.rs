//! [`Coop`] over NightCoop's real table, and the C callbacks that reach
//! [`Hooks`]. Finding the table (`GetModuleHandleW` + `NightCoopSdkGet`) is
//! the plugin's job; this starts from the pointer it got.

use core::ffi::{CStr, c_char, c_void};
use std::cell::{Cell, RefCell};
use std::ffi::CString;
use std::mem::{offset_of, size_of_val, zeroed};
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::ffi::{self, NcSdkApi, NcSdkHooks, NcSdkMod, NcSdkModInfo, NcSdkPartnerState, NcSdkWhere};
use crate::{Coop, E_ARG, E_STATE, Hooks, ModInfo, PartnerState, Peer, Where};

/// The table entry if this NightCoop's table has it (`NCSDK_HAS`).
macro_rules! entry {
    ($api:expr, $f:ident) => {{
        let api: &NcSdkApi = $api;
        if api.size as usize >= offset_of!(NcSdkApi, $f) + size_of_val(&api.$f) { api.$f } else { None }
    }};
}

const MAX_PEERS: usize = 64;

/// Our side of the table: NULL once NightCoop has said `shutdown`.
pub struct Sdk {
    api: Cell<*const NcSdkApi>,
    handle: NcSdkMod,
}

impl Sdk {
    fn api(&self) -> Option<&NcSdkApi> {
        // SAFETY: the pointer came from NightCoopSdkGet and is cleared before NightCoop unloads.
        unsafe { self.api.get().as_ref() }
    }

    /// NightCoop's build stamp, for the diagnostics.
    pub fn build(&self) -> String {
        self.api()
            .filter(|a| !a.build.is_null())
            .map(|a| unsafe { CStr::from_ptr(a.build) }.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// The keys NightCoop reads itself: `name\tvk\tIK_name\taction\twhen\trebindable` per line.
    pub fn keys_used(&self) -> String {
        let Some(f) = self.api().and_then(|a| entry!(a, keysUsed)) else { return String::new() };
        let mut buf = vec![0u8; 4096];
        let n = unsafe { f(buf.as_mut_ptr().cast(), buf.len()) };
        buf.truncate(n.min(buf.len() - 1));
        String::from_utf8_lossy(&buf).into_owned()
    }
}

/// What NightCoop does with the key `vk`, from [`Sdk::keys_used`]'s text: `name (action)` of each line
/// that reads it.
pub fn key_users(keys_used: &str, vk: i32) -> Vec<String> {
    keys_used
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            let v = f.get(1)?.trim();
            let v = match v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
                Some(h) => i32::from_str_radix(h, 16).ok()?,
                None => v.parse().ok()?,
            };
            let action = f.get(3).map(|a| a.trim()).unwrap_or_default();
            covered(f[0].trim(), v, action).contains(&vk).then(|| match Some(action).filter(|a| !a.is_empty()) {
                Some(action) => format!("{} ({action})", f[0].trim()),
                None => f[0].trim().to_owned(),
            })
        })
        .collect()
}

/// The keys one line reads: its own, and the run of keys of the same kind
/// its action names ("1..8", "NumPad1..NumPad9", "F1-F4, F6-F8, F10"), by
/// counting from its own code ("2" is "1" + 1).
fn covered(name: &str, vk: i32, action: &str) -> Vec<i32> {
    let mut out = vec![vk];
    let prefix = name.trim_end_matches(|c: char| c.is_ascii_digit());
    let Ok(first) = name[prefix.len()..].parse::<i32>() else { return out };
    let b = action.as_bytes();
    // A number right after `prefix` at `i` (not inside a longer word): (number, end).
    let number_at = |i: usize| -> Option<(i32, usize)> {
        let start = i + prefix.len();
        if !action.get(i..)?.starts_with(prefix) || (i > 0 && b[i - 1].is_ascii_alphanumeric()) {
            return None;
        }
        let end = start + b[start..].iter().take_while(|c| c.is_ascii_digit()).count();
        Some((action.get(start..end)?.parse().ok()?, end))
    };
    let mut i = 0;
    while i < b.len() {
        let Some((a, end)) = number_at(i) else {
            i += 1;
            continue;
        };
        let sep = ["..", "-"].into_iter().find(|s| action[end..].starts_with(s));
        let (z, next) = match sep.and_then(|s| number_at(end + s.len())) {
            Some((z, e)) => (z, e),
            // A lone key counts only for a named kind ("F10"), not a bare number ("hold 1 s").
            None if !prefix.is_empty() => (a, end),
            None => (a - 1, end),
        };
        out.extend((a..=z.min(a + 24)).map(|n| vk + n - first));
        i = next;
    }
    out
}

fn cstring(s: &str) -> Option<CString> {
    CString::new(s).ok()
}

/// Up to the first NUL: what a C string of these bytes would be.
fn c_bytes(b: &[u8]) -> CString {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    CString::new(&b[..end]).unwrap_or_default()
}

impl Coop for Sdk {
    fn where_(&self) -> Where {
        let Some(f) = self.api().and_then(|a| entry!(a, where_)) else { return Where::default() };
        let mut w: NcSdkWhere = unsafe { zeroed() };
        w.size = size_of_val(&w) as u32;
        unsafe { f(&mut w) };
        Where {
            in_session: w.inSession != 0,
            is_host: w.isHost != 0,
            in_world: w.inWorld != 0,
            in_host_world: w.inHostWorld != 0,
            local_peer: w.localPeer,
            peer_count: w.peerCount,
            shared_time_ms: w.sharedTimeMs,
        }
    }

    fn peers(&self) -> Vec<Peer> {
        let Some(f) = self.api().and_then(|a| entry!(a, peers)) else { return Vec::new() };
        let mut out = vec![0; MAX_PEERS];
        let n = unsafe { f(out.as_mut_ptr(), out.len()) };
        out.truncate(n.min(MAX_PEERS));
        out
    }

    fn peer_name(&self, peer: Peer) -> String {
        let Some(f) = self.api().and_then(|a| entry!(a, peerName)) else { return String::new() };
        let mut buf = [0u8; 128];
        unsafe { f(peer, buf.as_mut_ptr().cast(), buf.len()) };
        self.clean_text(&buf, 0)
    }

    fn peer_has_mod(&self, peer: Peer) -> i32 {
        match self.api().and_then(|a| entry!(a, peerHasMod)) {
            Some(f) => unsafe { f(self.handle, peer) },
            None => E_STATE,
        }
    }

    fn send(&self, to: Peer, channel: u16, data: &[u8], flags: u32) -> i32 {
        match self.api().and_then(|a| entry!(a, send)) {
            Some(f) => unsafe { f(self.handle, to, channel, data.as_ptr().cast(), data.len(), flags) },
            None => E_STATE,
        }
    }

    fn state_set(&self, key: &str, data: &[u8]) -> i32 {
        let Some(f) = self.api().and_then(|a| entry!(a, stateSet)) else { return E_STATE };
        let Some(key) = cstring(key) else { return E_ARG };
        unsafe { f(self.handle, key.as_ptr(), data.as_ptr().cast(), data.len()) }
    }

    fn state_get(&self, key: &str) -> Option<Vec<u8>> {
        let f = self.api().and_then(|a| entry!(a, stateGet))?;
        let key = cstring(key)?;
        let mut buf = vec![0u8; crate::STATE_VALUE_MAX as usize];
        let n = unsafe { f(self.handle, key.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
        buf.truncate(usize::try_from(n).ok()?.min(buf.len()));
        Some(buf)
    }

    fn partner_body(&self, peer: Peer) -> u64 {
        match self.api().and_then(|a| entry!(a, partnerBody)) {
            Some(f) => unsafe { f(peer) },
            None => 0,
        }
    }

    fn partner_state(&self, peer: Peer) -> Option<PartnerState> {
        let f = self.api().and_then(|a| entry!(a, partnerState))?;
        let mut s: NcSdkPartnerState = unsafe { zeroed() };
        s.size = size_of_val(&s) as u32;
        (unsafe { f(peer, &mut s) } == crate::OK).then_some(PartnerState { flags: s.flags, place: s.where_ })
    }

    fn clean_text(&self, text: &[u8], max: usize) -> String {
        let Some(f) = self.api().and_then(|a| entry!(a, cleanText)) else { return String::new() };
        let input = c_bytes(text);
        let cap = if max == 0 { ffi::NCSDK_TEXT_MAX as usize } else { max } + 1;
        let mut out = vec![0u8; cap];
        let n = unsafe { f(input.as_ptr(), max, out.as_mut_ptr().cast(), cap) };
        out.truncate(n.min(cap - 1));
        String::from_utf8_lossy(&out).into_owned()
    }

    fn log(&self, level: i32, text: &str) {
        if let Some(f) = self.api().and_then(|a| entry!(a, log)) {
            let text = c_bytes(text.as_bytes());
            unsafe { f(self.handle, level, text.as_ptr()) };
        }
    }
}

struct Inner<H> {
    sdk: Sdk,
    hooks: RefCell<H>,
    c_hooks: NcSdkHooks,
}

/// A registered mod. Dropping it unregisters (unless NightCoop is already gone).
pub struct Registration<H: Hooks> {
    inner: Box<Inner<H>>,
}

impl<H: Hooks> Registration<H> {
    /// Calls into our hooks from outside a callback (the plugin's own game-thread tick).
    /// `None` if NightCoop is gone.
    pub fn with<R>(&self, f: impl FnOnce(&mut H, &Sdk) -> R) -> Option<R> {
        let i = &self.inner;
        if i.sdk.api.get().is_null() {
            return None;
        }
        let mut h = i.hooks.try_borrow_mut().ok()?;
        catch_unwind(AssertUnwindSafe(|| f(&mut h, &i.sdk))).ok()
    }

    pub fn alive(&self) -> bool {
        !self.inner.sdk.api.get().is_null()
    }
}

impl<H: Hooks> Drop for Registration<H> {
    fn drop(&mut self) {
        let sdk = &self.inner.sdk;
        if let Some(f) = sdk.api().and_then(|a| entry!(a, unregisterMod)) {
            unsafe { f(sdk.handle) };
        }
        sdk.api.set(std::ptr::null());
    }
}

/// Registers the mod with `hooks`. `Err` gives the hooks back when NightCoop refuses
/// (bad id, blocked by the player with `sdk_block`, too many mods; its log says which).
///
/// # Safety
/// `api` is what `NightCoopSdkGet(NCSDK_VERSION)` returned (not NULL); call on the game thread.
pub unsafe fn register<H: Hooks>(api: *const NcSdkApi, info: &ModInfo, hooks: H) -> Result<Registration<H>, H> {
    let Some(reg) = (unsafe { api.as_ref() }).and_then(|a| entry!(a, registerMod)) else { return Err(hooks) };
    let (Some(id), Some(name), Some(version)) = (cstring(info.id), cstring(info.name), cstring(info.version)) else {
        return Err(hooks);
    };
    let mut inner = Box::new(Inner {
        sdk: Sdk { api: Cell::new(api), handle: 0 },
        hooks: RefCell::new(hooks),
        c_hooks: unsafe { zeroed() },
    });
    let ctx: *mut c_void = (&raw mut *inner).cast();
    let c = &mut inner.c_hooks;
    c.size = size_of_val(c) as u32;
    c.context = ctx;
    c.frame = Some(on_frame::<H>);
    c.event = Some(on_event::<H>);
    c.message = Some(on_message::<H>);
    c.stateChanged = Some(on_state_changed::<H>);
    c.stateRequest = Some(on_state_request::<H>);
    c.shutdown = Some(on_shutdown::<H>);
    let mut m: NcSdkModInfo = unsafe { zeroed() };
    m.size = size_of_val(&m) as u32;
    m.flags = info.flags;
    m.id = id.as_ptr();
    m.name = name.as_ptr();
    m.version = version.as_ptr();
    m.protocol = info.protocol;
    m.hooks = &inner.c_hooks;
    // NightCoop copies the strings and the hooks; no hook runs inside this call.
    let handle = unsafe { reg(&m) };
    if handle == 0 {
        return Err(inner.hooks.into_inner());
    }
    inner.sdk.handle = handle;
    Ok(Registration { inner })
}

/// Runs a hook: never after shutdown, never re-entered, and a panic stays on our side of the C boundary.
fn hook<H: Hooks, R>(ctx: *mut c_void, f: impl FnOnce(&mut H, &dyn Coop) -> R) -> Option<R> {
    // SAFETY: ctx is the Box<Inner<H>> of a live Registration (unregistered before it is freed).
    let inner = unsafe { ctx.cast::<Inner<H>>().as_ref() }?;
    if inner.sdk.api.get().is_null() {
        return None;
    }
    let mut h = inner.hooks.try_borrow_mut().ok()?;
    catch_unwind(AssertUnwindSafe(|| f(&mut h, &inner.sdk))).ok()
}

/// A byte buffer from C: NULL means empty.
unsafe fn bytes<'a>(data: *const u8, size: usize) -> &'a [u8] {
    if data.is_null() || size == 0 { &[] } else { unsafe { std::slice::from_raw_parts(data, size) } }
}

unsafe fn key<'a>(k: *const c_char) -> Option<&'a str> {
    if k.is_null() { None } else { unsafe { CStr::from_ptr(k) }.to_str().ok() }
}

unsafe extern "C" fn on_frame<H: Hooks>(ctx: *mut c_void, dt: f32) {
    hook::<H, _>(ctx, |h, c| h.frame(c, dt));
}

unsafe extern "C" fn on_event<H: Hooks>(ctx: *mut c_void, event: i32, peer: Peer) {
    hook::<H, _>(ctx, |h, c| h.event(c, event, peer));
}

unsafe extern "C" fn on_message<H: Hooks>(ctx: *mut c_void, from: Peer, channel: u16, data: *const u8, size: usize) {
    let data = unsafe { bytes(data, size) };
    hook::<H, _>(ctx, |h, c| h.message(c, from, channel, data));
}

unsafe extern "C" fn on_state_changed<H: Hooks>(
    ctx: *mut c_void,
    k: *const c_char,
    data: *const u8,
    size: usize,
    by: Peer,
) {
    let Some(k) = (unsafe { key(k) }) else { return };
    let data = (!data.is_null()).then(|| unsafe { bytes(data, size) });
    hook::<H, _>(ctx, |h, c| h.state_changed(c, k, data, by));
}

unsafe extern "C" fn on_state_request<H: Hooks>(
    ctx: *mut c_void,
    from: Peer,
    k: *const c_char,
    data: *const u8,
    size: usize,
) -> i32 {
    let Some(k) = (unsafe { key(k) }) else { return 0 };
    let data = unsafe { bytes(data, size) };
    hook::<H, _>(ctx, |h, c| h.state_request(c, from, k, data)).unwrap_or(false) as i32
}

unsafe extern "C" fn on_shutdown<H: Hooks>(ctx: *mut c_void) {
    // SAFETY: as in `hook`.
    let Some(inner) = (unsafe { ctx.cast::<Inner<H>>().as_ref() }) else { return };
    inner.sdk.api.set(std::ptr::null());
    if let Ok(mut h) = inner.hooks.try_borrow_mut() {
        let _ = catch_unwind(AssertUnwindSafe(|| h.shutdown()));
    }
}

#[cfg(test)]
mod tests {
    //! A stub table in Rust: checks the struct sizes we pass, the callbacks, and that
    //! nothing reaches the table after `shutdown`.
    use super::*;
    use std::sync::Mutex;

    static CALLS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static HOOKS: Mutex<usize> = Mutex::new(0);

    fn note(s: String) {
        CALLS.lock().unwrap().push(s);
    }

    unsafe extern "C" fn register_mod(info: *const NcSdkModInfo) -> NcSdkMod {
        let i = unsafe { &*info };
        assert_eq!(i.size as usize, size_of::<NcSdkModInfo>());
        let hooks = unsafe { &*i.hooks };
        assert_eq!(hooks.size as usize, size_of::<NcSdkHooks>());
        *HOOKS.lock().unwrap() = i.hooks as usize;
        note(format!("register {} p{} f{}", unsafe { CStr::from_ptr(i.id) }.to_str().unwrap(), i.protocol, i.flags));
        7
    }
    unsafe extern "C" fn unregister_mod(m: NcSdkMod) {
        note(format!("unregister {m}"));
    }
    unsafe extern "C" fn where_(w: *mut NcSdkWhere) {
        let w = unsafe { &mut *w };
        assert_eq!(w.size as usize, size_of::<NcSdkWhere>());
        (w.inSession, w.isHost, w.localPeer, w.peerCount, w.sharedTimeMs) = (1, 1, 0, 2, 1234);
    }
    unsafe extern "C" fn send(m: NcSdkMod, to: Peer, ch: u16, _d: *const c_void, n: usize, f: u32) -> i32 {
        note(format!("send m{m} to{to} ch{ch} n{n} f{f}"));
        1
    }

    #[derive(Default)]
    struct Rec(Vec<String>);
    impl Hooks for Rec {
        fn frame(&mut self, c: &dyn Coop, _dt: f32) {
            self.0.push(format!("frame {}", c.where_().shared_time_ms));
            c.send(crate::TO_ALL, 3, b"abc", 0);
        }
        fn event(&mut self, _: &dyn Coop, e: i32, p: Peer) {
            self.0.push(format!("event {e} {p}"));
        }
        fn message(&mut self, _: &dyn Coop, from: Peer, ch: u16, d: &[u8]) {
            self.0.push(format!("msg {from} {ch} {d:?}"));
        }
        fn state_changed(&mut self, _: &dyn Coop, k: &str, d: Option<&[u8]>, by: Peer) {
            self.0.push(format!("state {k} {d:?} {by}"));
        }
        fn state_request(&mut self, _: &dyn Coop, _: Peer, k: &str, _: &[u8]) -> bool {
            k == "ok"
        }
        fn shutdown(&mut self) {
            self.0.push("shutdown".into());
        }
    }

    #[test]
    fn registers_calls_hooks_and_stops_after_shutdown() {
        let mut api: NcSdkApi = unsafe { zeroed() };
        api.size = size_of::<NcSdkApi>() as u32;
        api.version = 1;
        api.registerMod = Some(register_mod);
        api.unregisterMod = Some(unregister_mod);
        api.where_ = Some(where_);
        api.send = Some(send);
        let info = ModInfo { id: "test.mod", name: "T", version: "1", protocol: 3, flags: 0x300 };
        let reg = unsafe { register(&api, &info, Rec::default()) }.ok().unwrap();
        let hooks = unsafe { &*(*HOOKS.lock().unwrap() as *const NcSdkHooks) };
        unsafe {
            hooks.frame.unwrap()(hooks.context, 0.016);
            hooks.event.unwrap()(hooks.context, crate::ev::PEER_MOD, 2);
            hooks.message.unwrap()(hooks.context, 2, 5, b"hi".as_ptr(), 2);
            hooks.message.unwrap()(hooks.context, 2, 6, std::ptr::null(), 0);
            hooks.stateChanged.unwrap()(hooks.context, c"room".as_ptr(), std::ptr::null(), 0, 0);
            assert_eq!(hooks.stateRequest.unwrap()(hooks.context, 1, c"ok".as_ptr(), std::ptr::null(), 0), 1);
            assert_eq!(hooks.stateRequest.unwrap()(hooks.context, 1, c"no".as_ptr(), std::ptr::null(), 0), 0);
            hooks.shutdown.unwrap()(hooks.context);
            hooks.frame.unwrap()(hooks.context, 0.016);
        }
        assert!(!reg.alive());
        assert!(reg.with(|_, _| ()).is_none());
        let seen = reg.inner.hooks.borrow().0.clone();
        assert_eq!(seen, [
            "frame 1234",
            "event 7 2",
            "msg 2 5 [104, 105]",
            "msg 2 6 []",
            "state room None 0",
            "shutdown",
        ]);
        drop(reg);
        // No unregister: NightCoop is gone.
        assert_eq!(*CALLS.lock().unwrap(), ["register test.mod p3 f768", "send m7 to4294967294 ch3 n3 f0"]);
    }

    #[test]
    fn key_users_counts_the_runs_an_action_names() {
        let text = "1\t0x31\tIK_1\tapply quickhack 1 of the wheel (1..8: rows 1..8, the default set)\n\
                    F1\t0x70\tIK_F1\tapply quickhack 1 (the F-keys set: F1-F4, F6-F8, F10)\n\
                    NumPad1\t0x61\tIK_NumPad1\tvote for row 1 (NumPad1..NumPad9: rows 1..9)\n\
                    T\t0x54\tIK_T\thold 2 s: pass out";
        assert_eq!(key_users(text, 0x32).len(), 1, "2");
        assert!(key_users(text, 0x38).len() == 1 && key_users(text, 0x39).is_empty(), "8 yes, 9 no");
        assert!(key_users(text, 0x73).len() == 1 && key_users(text, 0x74).is_empty() && key_users(text, 0x79).len() == 1, "F4, not F5, F10");
        assert_eq!(key_users(text, 0x69).len(), 1, "NumPad9");
        assert!(key_users(text, 0x55).is_empty(), "T's \"2 s\" is no key");
    }

    #[test]
    fn key_users_reads_decimal_and_hex_and_skips_junk() {
        let text = "F6\t117\tIK_F6\tcoop menu\talways\t1\nV\t0x56\tIK_V\tvoice\tin session\t0\nbroken\n\tx\n";
        assert_eq!(key_users(text, 117), ["F6 (coop menu)"]);
        assert_eq!(key_users(text, 0x56), ["V (voice)"]);
        assert!(key_users(text, 5).is_empty());
        assert!(key_users("", 5).is_empty());
    }

    #[test]
    fn a_short_table_has_no_late_entries() {
        let mut api: NcSdkApi = unsafe { zeroed() };
        api.keysUsed = Some(keys);
        api.size = offset_of!(NcSdkApi, keysUsed) as u32;
        assert!(entry!(&api, keysUsed).is_none());
        api.size = size_of::<NcSdkApi>() as u32;
        assert!(entry!(&api, keysUsed).is_some());
        unsafe extern "C" fn keys(_: *mut c_char, _: usize) -> usize {
            0
        }
    }
}
