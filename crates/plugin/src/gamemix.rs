//! The game's own sound, taken inside the process on its way to the device:
//! only what the game engine itself plays (no other programs, no other mods'
//! players, not our voices), sample by sample, the same under Windows and
//! Proton.
//!
//! Every WASAPI stream in a process shares the vtables of one implementation.
//! We patch four slots:
//! - `IAudioClient::Initialize`: notes each stream's format;
//! - `IAudioClient::GetService`: ties the render client to that format, and
//!   to who asked for it: the game's executable (its Wwise, or XAudio2 on its
//!   behalf) or someone else (RadioExt's FMOD, Audioware, our cpal);
//! - `IAudioRenderClient::GetBuffer`/`ReleaseBuffer`: on release, the game's
//!   buffer is downmixed to mono into a ring for [`drain`].
//!
//! The hooks must be in before the game opens its stream, so [`install`] runs
//! when the plugin loads. A stream opened before that has no known format and
//! is left alone.

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, MMDeviceEnumerator, eConsole, eRender,
};
use windows_sys::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows_sys::Win32::System::Diagnostics::Debug::RtlCaptureStackBackTrace;
use windows_sys::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW,
    GetModuleHandleExW, GetModuleHandleW,
};
use windows_sys::Win32::System::Memory::{PAGE_PROTECTION_FLAGS, PAGE_READWRITE, VirtualProtect};
use windows_sys::core::GUID;

type Hresult = i32;
type Obj = *mut c_void;
type InitializeFn = unsafe extern "system" fn(Obj, i32, u32, i64, i64, *const u8, *const GUID) -> Hresult;
type GetServiceFn = unsafe extern "system" fn(Obj, *const GUID, *mut Obj) -> Hresult;
type GetBufferFn = unsafe extern "system" fn(Obj, u32, *mut *mut u8) -> Hresult;
type ReleaseBufferFn = unsafe extern "system" fn(Obj, u32, u32) -> Hresult;

const IID_DEVICE_ENUMERATOR: GUID = GUID::from_u128(0xa95664d2_9614_4f35_a746_de8db63617e6);
const IID_AUDIO_CLIENT: GUID = GUID::from_u128(0x1cb9ad4c_dbfa_4c32_b178_c2f568a703b2);
const IID_RENDER_CLIENT: GUID = GUID::from_u128(0xf294acfc_3146_4483_a7bf_addca7c260e2);

// Vtable slots (after IUnknown's QueryInterface, AddRef, Release).
const RELEASE: usize = 2;
const ENUM_DEFAULT_ENDPOINT: usize = 4;
const DEVICE_ACTIVATE: usize = 3;
const CLIENT_INITIALIZE: usize = 3;
const CLIENT_MIX_FORMAT: usize = 8;
const CLIENT_SERVICE: usize = 14;
const RENDER_GET: usize = 3;
const RENDER_RELEASE: usize = 4;

/// One second of mono at 48 kHz, and some.
const RING: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Sample {
    F32,
    I16,
}

#[derive(Debug, Clone, Copy)]
struct Format {
    channels: usize,
    rate: u32,
    sample: Sample,
}

/// Patched slots: (vtable slot address, original value).
static PATCHED: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
static ORIG_INIT: AtomicUsize = AtomicUsize::new(0);
static ORIG_SERVICE: AtomicUsize = AtomicUsize::new(0);
static ORIG_GET: AtomicUsize = AtomicUsize::new(0);
static ORIG_RELEASE: AtomicUsize = AtomicUsize::new(0);
/// Audio client → its stream's format.
static CLIENTS: Mutex<Option<HashMap<usize, Format>>> = Mutex::new(None);
/// Render client → (format, opened by the game). Read on the audio thread through `CACHE`.
static RENDERS: Mutex<Option<HashMap<usize, (Format, bool)>>> = Mutex::new(None);
/// The game's stream we follow (the first one that plays), and its rate.
static MAIN: AtomicUsize = AtomicUsize::new(0);
static MAIN_RATE: AtomicU32 = AtomicU32::new(0);
static TX: Mutex<Option<HeapProd<f32>>> = Mutex::new(None);
static RX: Mutex<Option<HeapCons<f32>>> = Mutex::new(None);

thread_local! {
    /// (render client, buffer, frames) from the last GetBuffer on this thread.
    static PENDING: Cell<(usize, usize, u32)> = const { Cell::new((0, 0, 0)) };
    /// (render client, channels, sample is f32, follow it) last looked up on this thread.
    static CACHE: Cell<(usize, usize, bool, bool)> = const { Cell::new((0, 0, false, false)) };
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Moves the game's mono sound since the last call to `out`; its rate, or `None` if nothing
/// of the game's has played yet.
pub fn drain(out: &mut Vec<f32>) -> Option<u32> {
    let rate = MAIN_RATE.load(Ordering::Acquire);
    if let Some(rx) = lock(&RX).as_mut() {
        let mut chunk = [0.0f32; 1024];
        loop {
            let n = rx.pop_slice(&mut chunk);
            out.extend_from_slice(&chunk[..n]);
            if n < chunk.len() {
                break;
            }
        }
    }
    (rate != 0).then_some(rate)
}

/// Format from a WAVEFORMATEX(TENSIBLE), when we can read its samples.
unsafe fn parse(fmt: *const u8) -> Option<Format> {
    if fmt.is_null() {
        return None;
    }
    // SAFETY: a WAVEFORMATEX is at least 18 bytes; the SubFormat at 24 only with tag 0xFFFE.
    unsafe {
        let u16_at = |o: usize| u16::from_le_bytes([*fmt.add(o), *fmt.add(o + 1)]);
        let tag = u16_at(0);
        let channels = usize::from(u16_at(2));
        let rate = u32::from_le_bytes([*fmt.add(4), *fmt.add(5), *fmt.add(6), *fmt.add(7)]);
        let bits = u16_at(14);
        let sub = if tag == 0xFFFE { u16::from(*fmt.add(24)) } else { tag };
        let sample = match (sub, bits) {
            (3, 32) => Sample::F32,
            (1, 16) => Sample::I16,
            _ => return None,
        };
        (channels > 0 && rate > 0).then_some(Format { channels, rate, sample })
    }
}

#[inline(never)]
/// Whether the code that called our hook belongs to the game: its executable, or XAudio2.
fn called_by_game() -> bool {
    let mut frames = [std::ptr::null_mut::<c_void>(); 2];
    // SAFETY: room for 2 frames; frame 0 is this hook, frame 1 its caller.
    let got = unsafe { RtlCaptureStackBackTrace(1, 2, frames.as_mut_ptr(), std::ptr::null_mut()) };
    if got == 0 {
        return false;
    }
    let mut module: HMODULE = std::ptr::null_mut();
    // SAFETY: an address inside a loaded module; no reference taken.
    let ok = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            frames[1] as *const u16,
            &mut module,
        )
    };
    if ok == 0 {
        return false;
    }
    // SAFETY: the executable's own handle.
    if module == unsafe { GetModuleHandleW(std::ptr::null()) } {
        return true;
    }
    let mut name = [0u16; 260];
    // SAFETY: a buffer of 260.
    let n = unsafe { GetModuleFileNameW(module, name.as_mut_ptr(), name.len() as u32) } as usize;
    let name = String::from_utf16_lossy(&name[..n.min(name.len())]).to_lowercase();
    name.rsplit(['\\', '/']).next().is_some_and(|f| f.starts_with("xaudio2"))
}

unsafe extern "system" fn initialize(
    this: Obj,
    mode: i32,
    flags: u32,
    duration: i64,
    period: i64,
    fmt: *const u8,
    session: *const GUID,
) -> Hresult {
    // SAFETY: the original slot, saved before ours went in; the caller's arguments.
    let orig: InitializeFn = unsafe { std::mem::transmute(ORIG_INIT.load(Ordering::Acquire)) };
    let hr = unsafe { orig(this, mode, flags, duration, period, fmt, session) };
    // SAFETY: Initialize reads the same format.
    if hr >= 0
        && let Some(f) = unsafe { parse(fmt) }
    {
        lock(&CLIENTS).get_or_insert_with(HashMap::new).insert(this as usize, f);
    }
    hr
}

unsafe extern "system" fn get_service(this: Obj, riid: *const GUID, out: *mut Obj) -> Hresult {
    // SAFETY: as in `initialize`.
    let orig: GetServiceFn = unsafe { std::mem::transmute(ORIG_SERVICE.load(Ordering::Acquire)) };
    let hr = unsafe { orig(this, riid, out) };
    // SAFETY: a GUID pointer from the caller; `out` written on success.
    let render = hr >= 0 && !riid.is_null() && !out.is_null() && unsafe { guid_eq(&*riid, &IID_RENDER_CLIENT) };
    if render && let Some(f) = lock(&CLIENTS).as_ref().and_then(|m| m.get(&(this as usize)).copied()) {
        let game = called_by_game();
        lock(&RENDERS).get_or_insert_with(HashMap::new).insert(unsafe { *out } as usize, (f, game));
    }
    hr
}

fn guid_eq(a: &GUID, b: &GUID) -> bool {
    (a.data1, a.data2, a.data3, a.data4) == (b.data1, b.data2, b.data3, b.data4)
}

unsafe extern "system" fn get_buffer(this: Obj, frames: u32, data: *mut *mut u8) -> Hresult {
    // SAFETY: as in `initialize`.
    let orig: GetBufferFn = unsafe { std::mem::transmute(ORIG_GET.load(Ordering::Acquire)) };
    let hr = unsafe { orig(this, frames, data) };
    if hr >= 0 && !data.is_null() {
        // SAFETY: GetBuffer succeeded, so it wrote the pointer.
        PENDING.set((this as usize, unsafe { *data } as usize, frames));
    }
    hr
}

unsafe extern "system" fn release_buffer(this: Obj, frames: u32, flags: u32) -> Hresult {
    let (who, at, got) = PENDING.replace((0, 0, 0));
    if who == this as usize && at != 0 && frames <= got {
        // SAFETY: the buffer GetBuffer gave this client, `got` frames of its format.
        unsafe { take(this as usize, at as *const u8, frames, flags) };
    }
    // SAFETY: as in `initialize`.
    let orig: ReleaseBufferFn = unsafe { std::mem::transmute(ORIG_RELEASE.load(Ordering::Acquire)) };
    unsafe { orig(this, frames, flags) }
}

unsafe fn take(render: usize, at: *const u8, frames: u32, flags: u32) {
    let (mut who, mut channels, mut float, mut follow) = CACHE.get();
    if who != render {
        let Some((f, game)) = lock(&RENDERS).as_ref().and_then(|m| m.get(&render).copied()) else {
            CACHE.set((render, 0, false, false));
            return;
        };
        // The first of the game's streams to play is the one we follow.
        follow = game
            && (MAIN.compare_exchange(0, render, Ordering::AcqRel, Ordering::Acquire).is_ok()
                || MAIN.load(Ordering::Acquire) == render);
        if follow {
            MAIN_RATE.store(f.rate, Ordering::Release);
        }
        (who, channels, float) = (render, f.channels, f.sample == Sample::F32);
        CACHE.set((who, channels, float, follow));
    }
    if !follow || channels == 0 {
        return;
    }
    // Never block the game's audio thread: a missed buffer doesn't matter.
    let Ok(mut tx) = TX.try_lock() else { return };
    let Some(tx) = tx.as_mut() else { return };
    let n = frames as usize;
    if flags & AUDCLNT_BUFFERFLAGS_SILENT as u32 != 0 {
        tx.push_iter(std::iter::repeat_n(0.0, n));
        return;
    }
    let k = 1.0 / channels as f32;
    // SAFETY: `n` frames of `channels` samples of the stream's format.
    unsafe {
        if float {
            let s = std::slice::from_raw_parts(at as *const f32, n * channels);
            tx.push_iter(s.chunks_exact(channels).map(|f| f.iter().sum::<f32>() * k));
        } else {
            let s = std::slice::from_raw_parts(at as *const i16, n * channels);
            tx.push_iter(s.chunks_exact(channels).map(|f| f.iter().map(|&x| f32::from(x)).sum::<f32>() * k / 32_768.0));
        }
    }
}

/// Slot `i` of a COM object's vtable.
unsafe fn slot_addr(obj: Obj, i: usize) -> *mut usize {
    // SAFETY: a live COM object starts with its vtable pointer.
    unsafe { (*(obj as *const *mut usize)).add(i) }
}

unsafe fn call_slot<F: Copy>(obj: Obj, i: usize) -> F {
    // SAFETY: the caller names the slot's real signature.
    unsafe { std::mem::transmute_copy(&*slot_addr(obj, i)) }
}

unsafe fn release(obj: Obj) {
    if !obj.is_null() {
        // SAFETY: IUnknown::Release.
        unsafe { call_slot::<unsafe extern "system" fn(Obj) -> u32>(obj, RELEASE)(obj) };
    }
}

fn check(hr: Hresult, what: &str) -> Result<(), String> {
    if hr < 0 { Err(format!("{what}: 0x{:08x}", hr as u32)) } else { Ok(()) }
}

/// Points `slot` at `ours`, keeping the original in `orig`.
unsafe fn patch(slot: *mut usize, ours: usize, orig: &AtomicUsize) -> Result<(), String> {
    // SAFETY: a vtable slot; writable only for the swap.
    unsafe {
        if *slot == ours {
            return Ok(());
        }
        let mut old: PAGE_PROTECTION_FLAGS = 0;
        if VirtualProtect(slot as *const c_void, size_of::<usize>(), PAGE_READWRITE, &mut old) == 0 {
            return Err("can't write an audio vtable".into());
        }
        orig.store(*slot, Ordering::Release);
        lock(&PATCHED).push((slot as usize, *slot));
        // An aligned word store: a thread calling through sees the old or the new slot.
        slot.write_volatile(ours);
        VirtualProtect(slot as *const c_void, size_of::<usize>(), old, &mut old);
    }
    Ok(())
}

/// Installs the hooks; idempotent. Initialises COM on the calling thread, so
/// call it from a thread of ours.
pub fn install() -> Result<(), String> {
    if !lock(&PATCHED).is_empty() {
        return Ok(());
    }
    {
        let (tx, rx) = HeapRb::<f32>::new(RING).split();
        *lock(&TX) = Some(tx);
        *lock(&RX) = Some(rx);
    }
    // SAFETY: COM on our own thread, every object released before returning.
    unsafe {
        let inited = CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32) >= 0;
        let (mut en, mut dev, mut client, mut render): (Obj, Obj, Obj, Obj) =
            (std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
        let mut fmt: *mut u8 = std::ptr::null_mut();
        let r = (|| {
            check(CoCreateInstance(&MMDeviceEnumerator, std::ptr::null_mut(), CLSCTX_ALL, &IID_DEVICE_ENUMERATOR, &mut en), "enumerator")?;
            let f: unsafe extern "system" fn(Obj, i32, i32, *mut Obj) -> Hresult = call_slot(en, ENUM_DEFAULT_ENDPOINT);
            check(f(en, eRender, eConsole, &mut dev), "default output")?;
            let f: unsafe extern "system" fn(Obj, *const GUID, u32, *const c_void, *mut Obj) -> Hresult = call_slot(dev, DEVICE_ACTIVATE);
            check(f(dev, &IID_AUDIO_CLIENT, CLSCTX_ALL, std::ptr::null(), &mut client), "audio client")?;
            let f: unsafe extern "system" fn(Obj, *mut *mut u8) -> Hresult = call_slot(client, CLIENT_MIX_FORMAT);
            check(f(client, &mut fmt), "mix format")?;
            let f: InitializeFn = call_slot(client, CLIENT_INITIALIZE);
            check(f(client, AUDCLNT_SHAREMODE_SHARED, 0, 10_000_000, 0, fmt, std::ptr::null()), "initialize")?;
            let f: GetServiceFn = call_slot(client, CLIENT_SERVICE);
            check(f(client, &IID_RENDER_CLIENT, &mut render), "render client")?;
            patch(slot_addr(client, CLIENT_INITIALIZE), initialize as InitializeFn as usize, &ORIG_INIT)?;
            patch(slot_addr(client, CLIENT_SERVICE), get_service as GetServiceFn as usize, &ORIG_SERVICE)?;
            patch(slot_addr(render, RENDER_GET), get_buffer as GetBufferFn as usize, &ORIG_GET)?;
            patch(slot_addr(render, RENDER_RELEASE), release_buffer as ReleaseBufferFn as usize, &ORIG_RELEASE)
        })();
        release(render);
        release(client);
        release(dev);
        release(en);
        if !fmt.is_null() {
            CoTaskMemFree(fmt as *const c_void);
        }
        if inited {
            CoUninitialize();
        }
        if r.is_err() {
            uninstall();
        }
        r
    }
}

/// Puts the original slots back (the plugin is unloading: our code is about to go).
pub fn uninstall() {
    for (slot, orig) in lock(&PATCHED).drain(..).rev() {
        let slot = slot as *mut usize;
        // SAFETY: a slot we patched, restored the same way.
        unsafe {
            let mut old: PAGE_PROTECTION_FLAGS = 0;
            if VirtualProtect(slot as *const c_void, size_of::<usize>(), PAGE_READWRITE, &mut old) != 0 {
                slot.write_volatile(orig);
                VirtualProtect(slot as *const c_void, size_of::<usize>(), old, &mut old);
            }
        }
    }
}
