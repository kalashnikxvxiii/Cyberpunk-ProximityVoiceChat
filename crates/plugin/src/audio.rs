//! Audio engine.
//!
//! Capture: microphone → gain → echo cancellation / noise suppression →
//! push-to-talk or voice gate → Opus → server.
//! Playback: remote voices → per-speaker muffling and spatial gains (from the
//! acoustics the CET mod computes) → reverb of the surroundings → output.
//!
//! The cpal callbacks only downmix/upmix and move samples through lock-free
//! rings; everything else runs on this thread. The output ring is kept topped
//! up to a fixed depth, so the device clock paces the mix.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Host, SampleFormat, Stream};
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use vc_dsp::jitter::{Jitter, Next};
use vc_dsp::processor::{Processor, Suppression};
use vc_dsp::scene::{self, Scene};
use vc_dsp::binaural::Binaural;
use vc_dsp::{FRAME, HALF, LowPass, RATE, Resampler, Reverb, Smooth, db_to_gain, rms_db, spatial};
use vc_protocol::{ClientMsg, Env, PlayerId};

use crate::config::{Mode, Noise};
use crate::keys;
use crate::log_info;
use crate::net::ChannelSlot;
use crate::state::{Meters, Shared, Surroundings};

/// Mixed audio kept queued for the output device.
const OUTPUT_DEPTH_MS: u32 = 60;
const TICK: Duration = Duration::from_millis(5);
pub const OPUS_BITRATE: i32 = 24_000;
/// The open-mic gate stays open this long after the voice drops.
const GATE_HOLD: Duration = Duration::from_millis(400);
/// A speaker counts as talking (HUD) this long after its last audible frame.
const TALK_HOLD: Duration = Duration::from_millis(300);
/// Own-voice queues (monitor, self reverb) never hold more than this.
const SELF_QUEUE_MAX: usize = FRAME * 5;
/// Monitor loudness that, held this long, means it is feeding back.
const HOWL_DB: f32 = -12.0;
const HOWL_HALVES: u32 = 80;
/// Frames waiting for NightCoop's frame (a second).
const OUTBOX_MAX: usize = 50;
/// The game's sound is read this often.
const NOISE_WINDOW: Duration = Duration::from_millis(500);
/// While only this player's sounds play (pocket radio, calls), the last reading of the place
/// holds this long, then is dropped.
const PERSONAL_HOLD: Duration = Duration::from_secs(30);
/// Noise hides reverb tails: at full noise only this much of the reverb is left.
const NOISE_REVERB_LEFT: f32 = 0.15;

pub fn run(shared: Arc<Shared>, slot: Arc<ChannelSlot>) {
    let mut last_error = String::new();
    while !shared.stop.load(Ordering::Relaxed) {
        match Engine::open(&shared).and_then(|mut e| e.run(&shared, &slot)) {
            Ok(()) => {}
            Err(e) => {
                if e != last_error {
                    let code = if e.contains("f32") { "audio_format" } else { "audio_device" };
                    crate::events::report(crate::events::Level::Error, "Audio", code, e.clone());
                    last_error = e.clone();
                }
                *shared.lock(&shared.audio_status) = e;
                // Retry soon, or as soon as the devices are changed.
                let generation = shared.settings_gen();
                for _ in 0..60 {
                    if shared.stop.load(Ordering::Relaxed) || shared.settings_gen() != generation {
                        break;
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
}

fn device_name(d: &Device) -> String {
    d.description().map(|d| d.name().to_owned()).unwrap_or_default()
}

fn scan_devices(host: &Host, shared: &Shared) {
    let names = |it: Option<Vec<Device>>| -> Vec<String> {
        let mut v: Vec<String> = it.unwrap_or_default().iter().map(device_name).filter(|n| !n.is_empty()).collect();
        v.dedup();
        v
    };
    let inputs = names(host.input_devices().ok().map(Iterator::collect));
    let outputs = names(host.output_devices().ok().map(Iterator::collect));
    *shared.lock(&shared.devices) = (inputs, outputs);
}

fn pick(host: &Host, wanted: &str, input: bool) -> Result<Device, String> {
    let default = || {
        if input { host.default_input_device() } else { host.default_output_device() }
            .ok_or_else(|| format!("no {} device", if input { "input" } else { "output" }))
    };
    if wanted.is_empty() {
        return default();
    }
    let found = if input { host.input_devices().ok() } else { host.output_devices().ok() }
        .and_then(|mut it| it.find(|d| device_name(d) == wanted));
    match found {
        Some(d) => Ok(d),
        None => {
            log_info(format_args!("audio: device '{wanted}' not found, using the default"));
            default()
        }
    }
}

fn suppression(n: Noise) -> Suppression {
    match n {
        Noise::Off => Suppression::Off,
        Noise::Low => Suppression::Classic(0),
        Noise::Medium => Suppression::Classic(1),
        Noise::High => Suppression::Classic(2),
        Noise::Max => Suppression::Classic(3),
        Noise::Ai => Suppression::Ai,
    }
}

/// One way a voice reaches us (the loudest, or a second one from another
/// side), with its own muffling and place in our head.
struct Way {
    filter: LowPass,
    cutoff: Smooth,
    head: Binaural,
    /// Where it seems to come from, gliding (see `glide`).
    from: Option<[f32; 3]>,
    /// Its loudness, followed (see `follow`).
    level: Smooth,
}

impl Way {
    fn new() -> Self {
        Self { filter: LowPass::new(), cutoff: Smooth::new(LowPass::OPEN, 4.0), head: Binaural::new(), from: None, level: Smooth::new(0.0, 4.0) }
    }

    /// Follows a way in that comes from `to` (None: gone) at `gain`, heard by
    /// `listener`: where it seems to come from and its gain now. It glides as
    /// the opening moves; when the way leaves for another opening it fades
    /// out and comes back in there (sound doesn't sweep through the walls in
    /// between: a voice gliding from one doorway to a window, back and forth,
    /// fought over where it came from, in game 2026-11-20).
    fn follow(&mut self, listener: [f32; 3], to: Option<[f32; 3]>, gain: f32) -> Option<([f32; 3], f32)> {
        let aim = |p: [f32; 3]| {
            let d: [f32; 3] = std::array::from_fn(|i| p[i] - listener[i]);
            let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-6);
            d.map(|v| v / l)
        };
        let jump = matches!((self.from, to), (Some(f), Some(t)) if {
            let (a, b) = (aim(f), aim(t));
            a[0] * b[0] + a[1] * b[1] + a[2] * b[2] < JUMP_COS
        });
        let g = self.level.step(if jump || to.is_none() { 0.0 } else { gain });
        match (self.from, to) {
            (Some(_), Some(t)) if jump && g < FADED => self.from = Some(t),
            (_, Some(t)) if !jump => _ = self.glide(t),
            (_, None) if g < FADED => self.from = None,
            _ => {}
        }
        self.from.map(|f| (f, g))
    }

    fn glide(&mut self, to: [f32; 3]) -> [f32; 3] {
        let at = match self.from {
            Some(f) if spatial::distance(f, to) < GLIDE_MAX_M => std::array::from_fn(|i| f[i] + (to[i] - f[i]) * GLIDE),
            _ => to,
        };
        self.from = Some(at);
        at
    }

    /// Adds `voice` heard as `h` into `mix`, and `send` of it into our room's
    /// reverb (`here`) and theirs (`there`).
    fn play(&mut self, voice: &[f32; FRAME], h: &spatial::Heard, gain: f32, send: [f32; 2], mix: &mut [f32], here: &mut [f32], there: &mut [f32]) {
        let mut x = *voice;
        self.filter.set_cutoff(self.cutoff.step(h.cutoff));
        self.filter.process(&mut x);
        // Our 3D: each ear's delay and head shadow; gain ramps across the frame.
        self.head.process(&x, h.dir, gain * HEAD_LEVEL, mix);
        for (rev, send) in [here, there].into_iter().zip(send) {
            if send > 0.0 {
                for (r, s) in rev.iter_mut().zip(&x) {
                    *r += s * send;
                }
            }
        }
    }
}

/// Playback state of one remote speaker, owned by the audio thread.
struct Voice {
    jitter: Jitter<Vec<u8>>,
    decoder: opus::Decoder,
    ways: [Way; 2],
    /// How much over their usual voice they talk, held (see `LOUD_FALL`).
    loud: f32,
    /// Heard through the party radio last frame.
    radio: bool,
    far: FarRoom,
    /// Steam Audio's effects for this voice, once used (see `sound3d`).
    sa: Option<vc_acoustics::render::Voice>,
    /// Last Steam Audio frame and until when its rebounds ring on in silence.
    sa_tail: Option<(vc_acoustics::render::Heard, Instant)>,
    /// Steam Audio's direct result, followed smoothly (see [`SLIDE`]).
    sa_direct: Option<vc_acoustics::sa::Direct>,
    /// Where the voice seems to come from (world, Steam Audio's axes, unit),
    /// followed smoothly.
    sa_dir: Option<vc_acoustics::sa::V3>,
    /// How much their place and ours ring (see [`ring_of`]).
    ring: Smooth,
}

/// The ring of the room they talk in, heard from where their voice comes in
/// (a tunnel's mouth): each speaker's own, so it rings on as we walk out.
struct FarRoom {
    reverb: Reverb,
    head: Binaural,
    dir: spatial::Dir,
    /// What goes in this frame (see `Way::play`).
    send: Vec<f32>,
    wet: Vec<f32>,
    mono: Vec<f32>,
}

/// Share of a far room's ring heard all around rather than from its direction.
const FAR_DIFFUSE: f32 = 0.25;

impl FarRoom {
    fn new() -> Self {
        Self {
            reverb: Reverb::new(),
            head: Binaural::new(),
            dir: spatial::AHEAD,
            send: vec![0.0; FRAME],
            wet: vec![0.0; FRAME * 2],
            mono: vec![0.0; FRAME],
        }
    }

    /// Rings this frame's `send` (and the tail of earlier ones) into `mix`.
    fn ring(&mut self, mix: &mut [f32]) {
        self.wet.fill(0.0);
        self.reverb.process(&self.send, &mut self.wet);
        self.send.fill(0.0);
        // Mostly from their direction, a little all around (it spills out wide).
        for ((m, lr), x) in self.mono.iter_mut().zip(self.wet.chunks_exact(2)).zip(mix.chunks_exact_mut(2)) {
            *m = (lr[0] + lr[1]) * 0.5;
            x[0] += lr[0] * FAR_DIFFUSE;
            x[1] += lr[1] * FAR_DIFFUSE;
        }
        self.head.process(&self.mono, self.dir, 1.0, mix);
    }
}

/// A way in that moves farther than this (60°) left for another opening (see `Way::follow`).
const JUMP_COS: f32 = 0.5;
/// A way faded under this has gone: it may come back elsewhere.
const FADED: f32 = 0.02;
/// Each 20 ms the apparent source covers this much of the way to a new
/// opening: about 0.5 s, so a switch of doorway moves the voice, not jumps it.
const GLIDE: f32 = 0.04;
/// A shout's reach is held, then falls back this much per 20 ms (3 dB/s):
/// the gaps between shouted words don't cut a far voice into pieces.
const LOUD_FALL: f32 = 0.06;
/// The noise the range follows changes over about 3 s (frames of 20 ms):
/// music and traffic don't make far voices come and go.
const NOISE_FRAMES: f32 = 150.0;
/// Steam Audio's rebounds against the voice, once on our scale (see
/// [`steam_to_ours`]): half, the rooms ring without covering the voice.
/// (Unscaled at 0.4, a voice behind a door or round a corner was almost
/// inaudible; scaled 3x at every distance, too loud right next to it, in
/// game 2026-11-20.)
const REBOUND_GAIN: f32 = 0.5;

const _: () = assert!(vc_dsp::spatial::FADE_TAIL == vc_protocol::FADE_TAIL);
/// Share of the way to Steam Audio's latest answer covered each 20 ms (about
/// a fifth of a second): a doorway's edge, a step out in front of a window,
/// change the voice smoothly instead of at once. (Half a second, 0.04, was
/// heard lagging at a doorway, in game 2026-11-20.)
const SLIDE: f32 = 0.1;

/// Most a voice rebounds against itself in the open air at its distance, as
/// `rebound_level` counts: -6 dB at our ears. Walking away from it, the room's ring swelled
/// over it, louder and boomier from 2 to 5 m (in game, 2026-11-20): at a
/// "-6 dB" that didn't count the decoding, the rebounds came 8 dB over the
/// voice at the ears (tests/scenes.rs). The way a voice sounds is mostly its
/// own; the room is around it. (That ring, louder than the voice, was also
/// what made voices close by too loud: they're no longer turned down.)
/// Out of sight too: uncapped there, a step from a doorway out of its sight
/// made the voice 7 dB louder, the room's ring pouring out over it
/// (tests/scenes.rs, in game 2026-11-20); the way round the edge carries it.
const WET_MAX: f32 = 0.5 / vc_acoustics::render::EAR;
/// How much a place rings all round, from its surroundings (CET's):
/// reverberant (`wet`) and big (`size`: a tunnel's walls far
/// along it, a furnished diner's close by), followed over a few seconds.
/// Where it does, WET_MAX, the rebounds' low shelf and the close voices'
/// softening fade out: a bare tunnel sounds as before the diner's fixes, its
/// voice carried down its length (in game, 2026-11-20). The thresholds sit
/// between the two, measured along a tunnel (wet 0.23 to 0.45, mostly over
/// 0.3) and in Tom's Diner (0.13 to 0.27).
pub(crate) fn ring_of(wet: f32, size: f32) -> f32 {
    smoothstep(0.22, 0.32, wet) * smoothstep(0.08, 0.16, size)
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// How a voice falls off along a bare tunnel, against the open air's
/// (spread to this power: 0.5, 3 dB a doubling instead of 6).
const TUNNEL_LAW: f32 = 0.5;

/// [`ring_of`] followed over about 3 s (frames of 20 ms): CET's surroundings
/// jump about as we walk, and a tunnel rang now fully, now hardly at all.
const RING_FRAMES: f32 = 150.0;

/// A voice's level softened the louder it comes (full, 1: 60%): close voices
/// in a tunnel at the level they were heard right at (in game, 2026-11-20).
/// Only where the place rings: in a room, the boom that made them too loud is gone.
fn soften(gain: f32) -> f32 {
    1.0 / (1.0 + 0.667 * gain.max(0.0))
}

/// Our scale against Steam Audio's at `dist` m: our voice is full within
/// [`spatial::NEAR`] (3 m) and falls from there, Steam Audio's falls from 1 m.
fn steam_to_ours(dist: f32) -> f32 {
    (spatial::NEAR / dist.max(spatial::NEAR)) * dist.max(1.0)
}

/// After a voice stops, its rebounds ring this long.
const REBOUND_TAIL: Duration = Duration::from_millis(1200);
/// Our own voice's share of the reverb, against another player's.
const SELF_REVERB: f32 = 0.4;
/// Past this the voice jumped for real (teleport, respawn): no glide.
const GLIDE_MAX_M: f32 = 30.0;
/// Both ears at full is 3 dB above the old centred pan; keep the level players set.
const HEAD_LEVEL: f32 = std::f32::consts::FRAC_1_SQRT_2;

impl Voice {
    fn new() -> Option<Self> {
        Some(Self {
            jitter: Jitter::default(),
            decoder: opus::Decoder::new(RATE, opus::Channels::Mono).ok()?,
            ways: [Way::new(), Way::new()],
            loud: 0.0,
            radio: false,
            far: FarRoom::new(),
            sa: None,
            sa_tail: None,
            sa_direct: None,
            sa_dir: None,
            ring: Smooth::new(0.0, RING_FRAMES),
        })
    }

    /// Next 20 ms of this speaker, or `None` while silent.
    fn next(&mut self, out: &mut [f32; FRAME]) -> Option<()> {
        let n = match self.jitter.pop() {
            Next::Silent => return None,
            Next::Packet(pkt) => self.decoder.decode_float(&pkt, out, false),
            // Packet loss concealment.
            Next::Lost => self.decoder.decode_float(&[], out, false),
        };
        (n.ok()? == FRAME).then_some(())
    }
}

struct Engine {
    _input: Stream,
    _output: Stream,
    failed: Arc<AtomicBool>,
    cap_rx: ringbuf::HeapCons<f32>,
    play_tx: ringbuf::HeapProd<f32>,
    in_rate: u32,
    out_rate: u32,
    devices: (String, String),
}

impl Engine {
    fn open(shared: &Shared) -> Result<Self, String> {
        let s = shared.settings();
        let host = cpal::default_host();
        scan_devices(&host, shared);
        let input = pick(&host, &s.input_device, true)?;
        let output = pick(&host, &s.output_device, false)?;
        let in_cfg = input.default_input_config().map_err(|e| e.to_string())?;
        let out_cfg = output.default_output_config().map_err(|e| e.to_string())?;
        for (what, dev, cfg) in [("in ", &input, &in_cfg), ("out", &output, &out_cfg)] {
            log_info(format_args!(
                "audio: {what} '{}' {} Hz {} ch {:?}",
                device_name(dev),
                cfg.sample_rate(),
                cfg.channels(),
                cfg.sample_format()
            ));
        }
        if in_cfg.sample_format() != SampleFormat::F32 || out_cfg.sample_format() != SampleFormat::F32 {
            return Err("only f32 devices are supported for now".into());
        }
        let (in_rate, out_rate) = (in_cfg.sample_rate(), out_cfg.sample_rate());
        // Capture: mono at the input rate. Playback: interleaved L/R at the output rate.
        let (mut cap_tx, cap_rx) = HeapRb::<f32>::new(in_rate as usize).split();
        let (play_tx, mut play_rx) = HeapRb::<f32>::new(out_rate as usize * 2).split();
        let failed = Arc::new(AtomicBool::new(false));

        let in_ch = in_cfg.channels() as usize;
        let fail = failed.clone();
        let input_stream = input
            .build_input_stream::<f32, _, _>(
                in_cfg.config(),
                move |data, _| {
                    cap_tx.push_iter(data.chunks_exact(in_ch).map(|f| f.iter().sum::<f32>() / in_ch as f32));
                },
                move |e| {
                    log_info(format_args!("audio: input error: {e}"));
                    if matches!(e.kind(), cpal::ErrorKind::DeviceNotAvailable) {
                        fail.store(true, Ordering::Relaxed);
                    }
                },
                None,
            )
            .map_err(|e| e.to_string())?;

        let out_ch = out_cfg.channels() as usize;
        let fail = failed.clone();
        let output_stream = output
            .build_output_stream::<f32, _, _>(
                out_cfg.config(),
                move |data: &mut [f32], _| {
                    for frame in data.chunks_exact_mut(out_ch) {
                        let (l, r) = match (play_rx.try_pop(), play_rx.try_pop()) {
                            (Some(l), Some(r)) => (l, r),
                            _ => (0.0, 0.0),
                        };
                        if out_ch == 1 {
                            frame[0] = (l + r) * 0.5;
                        } else {
                            frame[0] = l;
                            frame[1] = r;
                        }
                    }
                },
                move |e| {
                    log_info(format_args!("audio: output error: {e}"));
                    if matches!(e.kind(), cpal::ErrorKind::DeviceNotAvailable) {
                        fail.store(true, Ordering::Relaxed);
                    }
                },
                None,
            )
            .map_err(|e| e.to_string())?;
        input_stream.play().map_err(|e| e.to_string())?;
        output_stream.play().map_err(|e| e.to_string())?;
        *shared.lock(&shared.audio_status) = "ok".into();
        crate::events::resolve("audio_device");
        crate::events::resolve("audio_format");
        Ok(Self {
            _input: input_stream,
            _output: output_stream,
            failed,
            cap_rx,
            play_tx,
            in_rate,
            out_rate,
            devices: (s.input_device, s.output_device),
        })
    }

    /// Runs until stop, a device change in the settings, or a lost device.
    fn run(&mut self, shared: &Shared, slot: &ChannelSlot) -> Result<(), String> {
        let mut s = shared.settings();
        let mut generation = shared.settings_gen();
        let mut processor = Processor::new(s.echo_cancel, suppression(s.noise))?;
        let mut encoder = opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip)
            .map_err(|e| e.to_string())?;
        encoder.set_bitrate(opus::Bitrate::Bits(OPUS_BITRATE)).map_err(|e| e.to_string())?;
        let mut mic_clip = opus::SoftClip::new(opus::Channels::Mono);
        let mut out_clip = opus::SoftClip::new(opus::Channels::Stereo);
        let mut to_48k = Resampler::new(self.in_rate, RATE);
        let (mut to_out_l, mut to_out_r) = (Resampler::new(RATE, self.out_rate), Resampler::new(RATE, self.out_rate));
        // Our room's ring (theirs is in each `Voice`).
        let mut reverb = Reverb::new();
        let mut voices: HashMap<PlayerId, Voice> = HashMap::new();
        let mut decoder: Option<vc_acoustics::render::Decoder> = None;
        let silence = [0.0f32; FRAME];

        let out_depth = (self.out_rate * OUTPUT_DEPTH_MS / 1000) as usize * 2;
        let mut raw = vec![0.0f32; self.in_rate as usize / 10];
        let mut pcm = Vec::<f32>::with_capacity(FRAME * 8);
        let mut tx_frame = Vec::<f32>::with_capacity(FRAME);
        let mut packet = [0u8; 1000];
        let mut voice = [0.0f32; FRAME];
        let mut mix = vec![0.0f32; FRAME * 2];
        let mut rev_in = vec![0.0f32; FRAME];
        let (mut left, mut right, mut out_l, mut out_r) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut monitor_q = VecDeque::<f32>::new();
        let mut self_rev_q = VecDeque::<f32>::new();
        let mut gate_until: Option<Instant> = None;
        let mut reference = vec![0.0f32; FRAME];
        let mut hot_halves = 0u32;
        let mut seq = 0u32;
        let mut transmitting = false;
        let mut shout = spatial::Shout::default();
        let mut scene: Option<(u32, Scene)> = None;
        let mut game_pcm = Vec::<f32>::new();
        let mut window_at = Instant::now();
        let mut reading: Option<scene::Reading> = None;
        let mut personal_since: Option<Instant> = None;
        let mut noise_heard = Smooth::new(0.0, NOISE_FRAMES);

        log_info(format_args!("audio: ready"));
        while !shared.stop.load(Ordering::Relaxed) {
            if self.failed.load(Ordering::Relaxed) {
                return Err("audio device lost".into());
            }
            if shared.settings_gen() != generation {
                generation = shared.settings_gen();
                s = shared.settings();
                if (&s.input_device, &s.output_device) != (&self.devices.0, &self.devices.1) {
                    log_info(format_args!("audio: switching devices"));
                    return Ok(());
                }
                processor.configure(s.echo_cancel, suppression(s.noise))?;
            }
            if shared.rescan_devices.swap(false, Ordering::Relaxed) {
                scan_devices(&cpal::default_host(), shared);
            }
            let pose = *shared.lock(&shared.pose);
            let link = shared.lock(&shared.link).joined();
            // --- the place, from the game's own sound (see gamemix and vc_dsp::scene) ---
            // Sounds only this player hears (pocket radio, calls, menus) say nothing about it.
            let personal = shared.personal().is_some();
            personal_since = if personal { personal_since.or(Some(Instant::now())) } else { None };
            game_pcm.clear();
            if let Some(rate) = crate::gamemix::drain(&mut game_pcm) {
                if scene.as_ref().is_none_or(|(r, _)| *r != rate) {
                    scene = Some((rate, Scene::new(rate)));
                }
                if !personal && let Some((_, sc)) = &mut scene {
                    sc.push(&game_pcm);
                }
            }
            if window_at.elapsed() >= NOISE_WINDOW {
                window_at = Instant::now();
                if !personal {
                    reading = scene.as_ref().and_then(|(_, sc)| sc.reading());
                } else if personal_since.is_some_and(|t| t.elapsed() > PERSONAL_HOLD) {
                    reading = None;
                }
            }
            let noise = reading.map_or(0.0, |r| spatial::noise(r.noise_db));
            let reverb_left = 1.0 - (1.0 - NOISE_REVERB_LEFT) * noise;
            // Music, crowds and traffic cover the reverb, here and in what we send.
            let surroundings = if s.reverb { shared.surroundings() } else { None }
                .map(|e| Surroundings { wet: e.wet * reverb_left, ..e });
            let listener_wet = surroundings.map_or(0.0, |e| e.wet);
            let my_ear = shared.ear();

            // --- capture, in 10 ms steps ---
            let n = self.cap_rx.pop_slice(&mut raw);
            to_48k.process(&raw[..n], &mut pcm);
            let whole = pcm.len() / HALF * HALF;
            let input_gain = db_to_gain(s.input_gain_db);
            let mut meters = Meters::default();
            for half in pcm[..whole].chunks_exact_mut(HALF) {
                for x in half.iter_mut() {
                    *x *= input_gain;
                }
                meters.input_db = rms_db(half);
                processor.capture(half);
                meters.processed_db = rms_db(half);
                let gate = match s.mode {
                    Mode::PushToTalk => keys::down(s.ptt_vk),
                    Mode::Open => {
                        if meters.processed_db > s.vad_threshold_db {
                            gate_until = Some(Instant::now() + GATE_HOLD);
                        }
                        gate_until.is_some_and(|t| Instant::now() < t)
                    }
                };
                meters.gate_open = gate;
                // Muting the chat mutes the microphone too: talking to people you
                // can't hear makes no sense.
                // No pose is fine: menus and the co-op lobby, heard without distance.
                let now_tx = gate && !s.mic_muted && !s.deafened && link.is_some();
                if now_tx != transmitting {
                    transmitting = now_tx;
                    tx_frame.clear();
                }
                meters.transmitting = transmitting;
                for x in half.iter_mut() {
                    *x *= s.mic_volume;
                }
                if s.monitor && !s.deafened {
                    // Speakers instead of headphones: the mic hears itself and howls.
                    hot_halves = if meters.processed_db > HOWL_DB { hot_halves + 1 } else { 0 };
                    if hot_halves >= HOWL_HALVES {
                        hot_halves = 0;
                        monitor_q.clear();
                        crate::events::report(
                            crate::events::Level::Warn,
                            "Audio",
                            "monitor_feedback",
                            crate::tr!("l'ascolto del microfono fischiava ed è stato spento", "microphone listening was howling and has been turned off"),
                        );
                        let _ = shared.update(|set| set.set("monitor", "off"));
                        s.monitor = false;
                    } else {
                        monitor_q.extend(half.iter());
                    }
                }
                if transmitting && s.self_reverb && listener_wet > 0.0 && !s.deafened {
                    // Softer than others' voices: our own echo right in our ears is confusing.
                    let send = s.self_reverb_level * listener_wet * SELF_REVERB;
                    self_rev_q.extend(half.iter().map(|x| x * send));
                }
                if !transmitting {
                    continue;
                }
                tx_frame.extend_from_slice(half);
                if tx_frame.len() == FRAME {
                    let loud_db = shout.push(rms_db(&tx_frame));
                    mic_clip.apply(&mut tx_frame);
                    let len = encoder.encode_float(&tx_frame, &mut packet).map_err(|e| e.to_string())?;
                    tx_frame.clear();
                    let env = Env {
                        loud: loud_db.round().min(255.0) as u8,
                        ear: Env::ear_cm(shared.ear()),
                        ..surroundings.map_or(Env::default(), |e| e.to_env())
                    };
                    let pos = pose.map(|p| p.pos());
                    if shared.coop_voice.load(Ordering::Relaxed) {
                        // NightCoop's frame takes them (and seals them); a stalled game doesn't pile them up.
                        let mut out = shared.lock(&shared.outbox);
                        out.push_back(vc_coop::chat::Frame { seq, opus: packet[..len].to_vec(), env });
                        let extra = out.len().saturating_sub(OUTBOX_MAX);
                        out.drain(..extra);
                    } else if let (Some(key), Some((me, _))) = (shared.lock(&shared.voice_key).clone(), link) {
                        // Sealed end to end: servers route it without hearing it.
                        let opus = key.seal(me, seq, &packet[..len]);
                        slot.send(&ClientMsg::Voice { seq, pos, env, opus });
                    }
                    seq = seq.wrapping_add(1);
                }
            }
            pcm.drain(..whole);
            if whole > 0 {
                *shared.lock(&shared.meters) = meters;
            }
            for q in [&mut monitor_q, &mut self_rev_q] {
                let extra = q.len().saturating_sub(SELF_QUEUE_MAX);
                q.drain(..extra);
            }

            // --- incoming voice ---
            for (from, seq, opus, reset) in std::mem::take(&mut *shared.lock(&shared.inbox)) {
                if reset {
                    voices.remove(&from);
                }
                if !voices.contains_key(&from) {
                    let Some(v) = Voice::new() else { continue };
                    voices.insert(from, v);
                }
                voices.get_mut(&from).unwrap().jitter.push(seq, opus);
            }
            voices.retain(|id, _| shared.lock(&shared.speakers).contains_key(id));

            // --- playback, 20 ms at a time ---
            let open_range = link.map_or(vc_protocol::RANGE, |(_, r)| r);
            while self.play_tx.occupied_len() < out_depth {
                let range = open_range * spatial::noise_reach(noise_heard.step(noise));
                mix.fill(0.0);
                rev_in.fill(0.0);
                let now = Instant::now();
                // Steam Audio, when on and in the world (else the classic paths).
                let s3d = crate::sound3d::active(shared).filter(|_| pose.is_some());
                let space = pose.map(|p| vc_acoustics::world::listener(spatial::ear(p.pos(), my_ear), p.forward_vec()));
                if let Some(s3d) = s3d
                    && decoder.is_none()
                {
                    decoder = vc_acoustics::render::Decoder::new(&s3d.engine).map_err(|e| log_info(format_args!("acoustics: {e}"))).ok();
                }
                for (id, v) in voices.iter_mut() {
                    if v.next(&mut voice).is_none() {
                        v.far.ring(&mut mix);
                        // Its rebounds ring on a little after the last word.
                        if let (Some(s3d), Some(dec), Some(sa), Some((h, until))) = (s3d, decoder.as_mut(), v.sa.as_mut(), v.sa_tail)
                            && now < until
                        {
                            let src = s3d.engine.source(u32::from(*id));
                            sa.render(&s3d.engine, src.as_deref(), &h, &silence, &mut mix, &mut dec.ambi);
                        }
                        continue;
                    }
                    let level = rms_db(&voice);
                    let (pos, party, env) = {
                        let mut speakers = shared.lock(&shared.speakers);
                        let Some(info) = speakers.get_mut(id) else { continue };
                        info.level_db = level;
                        if level > -55.0 {
                            info.audible_until = Some(now + TALK_HOLD);
                        }
                        (info.pos, info.party, info.env)
                    };
                    let pref = shared.name_of(*id).map(|n| s.player(&n)).unwrap_or_default();
                    v.loud = (v.loud - LOUD_FALL).max(f32::from(env.loud));
                    let reach = Env { loud: v.loud.round() as u8, ..env }.reach();
                    // The voice rings in both places: theirs and ours.
                    // Half rings here, half where they are.
                    let (wet_here, wet_there) = (listener_wet * 0.5, env.wet as f32 / 255.0 * 0.5 * reverb_left);
                    let carry = spatial::carry(wet_here + wet_there);
                    // Each way it reaches us, kept on the same slot as it moves.
                    let mut heard = [None; 2];
                    let speaker_ear = pos.map(|pos| spatial::ear(pos, env.ear_m().unwrap_or(spatial::EAR_HEIGHT)));
                    let sa_direct = s3d.and_then(|s| s.engine.direct_of(u32::from(*id)));
                    match (pose, pos) {
                        (Some(p), Some(_)) if s3d.is_some() && sa_direct.is_some() && speaker_ear.is_some() => {
                            let (s3d, at, space) = (s3d.unwrap(), speaker_ear.unwrap(), space.unwrap());
                            let d = {
                                let (to, was) = (sa_direct.unwrap(), v.sa_direct.unwrap_or(sa_direct.unwrap()));
                                let mix = |a: f32, b: f32| a + (b - a) * SLIDE;
                                vc_acoustics::sa::Direct {
                                    occlusion: mix(was.occlusion, to.occlusion),
                                    transmission: std::array::from_fn(|i| mix(was.transmission[i], to.transmission[i])),
                                }
                            };
                            v.sa_direct = Some(d);
                            let listener = spatial::ear(p.pos(), my_ear);
                            let dist = spatial::distance(listener, at);
                            // How much their place rings (see `ring_of`): where they talk, as
                            // it is, not as the noise here masks it. From both ends and after
                            // the noise, a tunnel stopped ringing at its mouth, the traffic
                            // outside, and stayed so until we came 30 m back in (2026-11-21).
                            let ring = v.ring.step(ring_of(f32::from(env.wet) / 255.0, f32::from(env.size) / 255.0));
                            // Where it rings all round it carries as in a bare tunnel (`carry`).
                            let carry = carry.max(ring);
                            let spread = spatial::NEAR / (dist / (1.0 + carry)).max(spatial::NEAR);
                            let guided = 1.0 + ring * (spread.powf(TUNNEL_LAW - 1.0) - 1.0);
                            // What goes through the walls isn't guided down the tunnel: out
                            // past its far mouth, the straight line through the hill (-37 to
                            // -47 dB) came 12 dB louder with it, the voice swelling 20 m out
                            // in the open (in game, 2026-11-21).
                            let d = vc_acoustics::sa::Direct { transmission: d.transmission.map(|t| t / guided), ..d };
                            let sa = match v.sa.as_mut() {
                                Some(sa) => sa,
                                None => match vc_acoustics::render::Voice::new(&s3d.engine) {
                                    Ok(sa) => v.sa.insert(sa),
                                    Err(e) => {
                                        log_info(format_args!("acoustics: {e}"));
                                        continue;
                                    }
                                },
                            };
                            // How loud it reaches us, against the open air at this distance:
                            // what's in plain sight or through the walls, plus the rebounds.
                            let through = d.occlusion + (1.0 - d.occlusion) * d.transmission.iter().sum::<f32>() / 3.0;
                            // The voice keeps its own sound: its rebounds stay under it in the
                            // open air (WET_MAX), however far, in sight or not. Where the place
                            // rings all round (a tunnel) they carry it, as far as they go.
                            sa.set_ring(ring);
                            let wet_cap = |r: f32| r.min(WET_MAX + (r - WET_MAX).max(0.0) * ring);
                            let raw_gain = REBOUND_GAIN * s.rebounds * steam_to_ours(dist);
                            let raw = raw_gain * sa.rebound_level() / spread;
                            let rebounds = wet_cap(raw);
                            let rebound_gain = if raw > 0.0 { raw_gain * rebounds / raw } else { raw_gain };
                            // Where it rings, the simulation's spikes taken out (a tunnel's
                            // rebounds jumped up run to run, 2026-11-21).
                            let rebound_gain = rebound_gain * sa.spike().powf(ring);
                            // Round the edges of openings (a doorway, a corner): sound bends
                            // there, Steam Audio's rays don't. The classic acoustics find those
                            // ways (CET's paths, the loudest and one from elsewhere), each heard
                            // from its own opening, all at once; their loudness on our scale
                            // here (it was worked out for the length of the way). A way from
                            // the speaker is the straight line: Steam Audio's.
                            let (main, alt) = shared.path(*id, at, s.occlusion);
                            let bent = |p: Option<spatial::Path>| p.filter(|p| spatial::distance(p.from, at) > 0.5);
                            let (main, alt) = (bent(Some(main)), bent(alt));
                            // Each on the slot nearest where it was, as in the classic acoustics.
                            let swap = matches!((alt, v.ways[0].from), (Some(a), Some(f))
                                if main.is_none_or(|m| spatial::distance(a.from, f) < spatial::distance(m.from, f)));
                            let slots = if swap { [alt, main] } else { [main, alt] };
                            let mut bends = [None; 2];
                            for (k, (way, p)) in v.ways.iter_mut().zip(slots).enumerate() {
                                // Along the way's length, kept together as the voice is (`carry`:
                                // left out, a tunnel's voice round a bend came 6 dB weaker than
                                // when it took the classic way whole, 2026-11-20).
                                let gain = p.map_or(0.0, |p| {
                                    let len = spatial::distance(listener, p.from) / (1.0 + carry);
                                    p.gain * (spatial::NEAR / len.max(spatial::NEAR)) / spread
                                });
                                if let Some((from, g)) = way.follow(listener, p.map(|p| p.from), gain) {
                                    let c = p.map_or(LowPass::OPEN, |p| p.cutoff);
                                    bends[k] = Some((from, g, [1.0, (c / 3000.0).min(1.0), (c / 10_000.0).min(1.0)]));
                                }
                            }
                            let bend2: f32 = bends.iter().flatten().map(|b| b.1 * b.1).sum();
                            let factor = (through * through + rebounds * rebounds + bend2).sqrt();
                            // Within NEAR our voice stays as loud (our distance law), but
                            // Steam Audio's rebounds against it grow with the distance (the
                            // voice falls off, the room's ring doesn't): the whole would
                            // swell walking away to 3 m (in game, 2026-11-20). Hold the
                            // whole at what it is at NEAR, the rebounds' share as is.
                            let hold = if dist < spatial::NEAR && factor > 0.0 {
                                let at_near = wet_cap(raw * spatial::NEAR / dist.max(1.0));
                                (through * through + at_near * at_near + bend2).sqrt() / factor
                            } else {
                                1.0
                            };
                            let from = at;
                            let sa_vec = |p: [f32; 3]| {
                                let q = vc_acoustics::world::to_sa(p);
                                let (x, y, z) = (q.x - space.origin.x, q.y - space.origin.y, q.z - space.origin.z);
                                let l = (x * x + y * y + z * z).sqrt().max(1e-6);
                                [x / l, y / l, z / l]
                            };
                            let seen_from = |p: [f32; 3]| vc_acoustics::render::direction(&s3d.engine, vc_acoustics::world::to_sa(p), &space);
                            let sa_bends = bends.map(|b| {
                                b.map(|(from, g, eq)| vc_acoustics::render::Bend {
                                    gain: g,
                                    eq,
                                    dir: seen_from(from),
                                    air: vc_acoustics::render::air(spatial::distance(listener, from)),
                                })
                            });
                            // Where their room rings from: the speaker for what goes straight
                            // through, the openings for what bends round, where the rebounds
                            // pour in from (a doorway) for the rest, weighed by how much of each.
                            let st = sa_vec(at);
                            let mut aim: [f32; 3] = st.map(|v| v * through * through);
                            for (from, g, _) in bends.iter().flatten() {
                                let o = sa_vec(*from);
                                aim = std::array::from_fn(|i| aim[i] + o[i] * g * g);
                            }
                            if let Some(a) = sa.arrival() {
                                let wr = rebounds * rebounds;
                                aim = [aim[0] + a.x * wr, aim[1] + a.y * wr, aim[2] + a.z * wr];
                            }
                            if aim.iter().all(|v| v.abs() < 1e-9) {
                                aim = st;
                            }
                            let was = v.sa_dir.map_or(aim, |w| [w.x, w.y, w.z]);
                            let g: [f32; 3] = std::array::from_fn(|i| was[i] + (aim[i] - was[i]) * SLIDE);
                            let gl = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt().max(1e-6);
                            let seems = vc_acoustics::sa::v3(g[0] / gl, g[1] / gl, g[2] / gl);
                            v.sa_dir = Some(seems);
                            let seems_dir = vc_acoustics::render::direction(
                                &s3d.engine,
                                vc_acoustics::sa::v3(space.origin.x + seems.x, space.origin.y + seems.y, space.origin.z + seems.z),
                                &space,
                            );
                            // A bare tunnel guides the voice along it: past NEAR it falls
                            // 3 dB a doubling, not 6 as in the open (TUNNEL_LAW), the whole of
                            // it, rebounds and all. Spread over the open air's law it fell
                            // 16 dB by 40 m and went silent before the mouth, 100 m off,
                            // where it used to carry (in game, 2026-11-20).
                            // What our range law judges: how loud it all reaches our ears,
                            // the rebounds as they come out there (render::EAR over what
                            // `rebound_level` says). Counted as `rebound_level`, a voice
                            // carried by its rebounds (down a tunnel, round its bend) went
                            // silent 10 dB too soon, there to stay until we came 30 m back
                            // (in game, 2026-11-21).
                            let at_ears = {
                                let r = rebounds * vc_acoustics::render::EAR;
                                (through * through + r * r + bend2).sqrt()
                            };
                            let path = spatial::Path { from, gain: at_ears * guided, cutoff: LowPass::OPEN };
                            let h = spatial::heard(listener, p.forward_vec(), path, party, range * reach, carry, v.radio);
                            v.radio = h.radio;
                            let volume = if s.deafened || pref.muted { 0.0 } else { pref.volume };
                            if h.radio {
                                v.ways[0].play(&voice, &h, h.gain * volume, [0.0; 2], &mut mix, &mut rev_in, &mut v.far.send);
                                v.sa_tail = None;
                            } else {
                                // Where the place rings all round, close voices turned down as
                                // before the diner's fixes (see `soften`).
                                let volume = volume * hold * (1.0 + (soften(h.gain / guided * hold) - 1.0) * ring);
                                // Our range law fades the whole voice out at the edge.
                                let fade = if spread * at_ears > 0.0 { h.gain / (spread * at_ears * guided) } else { 0.0 };
                                let (volume, tail_volume) = (volume * guided, volume);
                                let sh = vc_acoustics::render::Heard {
                                    gain: spread * fade * volume,
                                    direct: d,
                                    dir: seen_from(at),
                                    air: vc_acoustics::render::air(dist),
                                    rebounds: if s.reverb { rebound_gain * fade * volume } else { 0.0 },
                                    bends: sa_bends,
                                };
                                if let Some(dec) = decoder.as_mut() {
                                    let src = s3d.engine.source(u32::from(*id));
                                    sa.render(&s3d.engine, src.as_deref(), &sh, &voice, &mut mix, &mut dec.ambi);
                                    v.sa_tail = Some((sh, now + REBOUND_TAIL));
                                }
                                // Steam Audio's few bounces give the early echoes, not the
                                // long tail a tunnel or a garage rings with: our rooms'
                                // reverb adds it, as for the classic acoustics (S4, 2026-11-20).
                                let send = if s.reverb { h.reverb * tail_volume } else { 0.0 };
                                v.ways[1].play(&voice, &h, 0.0, [send * wet_here, send * wet_there], &mut mix, &mut rev_in, &mut v.far.send);
                                // Their room rings from where the voice seems to come from
                                // (Steam Audio's listener axes: x right, y up, -z ahead).
                                v.far.dir = [seems_dir.x, -seems_dir.z, seems_dir.y];
                            }
                            v.far.reverb.set_room(f32::from(env.size) / 255.0, f32::from(env.damp) / 255.0);
                            v.far.ring(&mut mix);
                            continue;
                        }
                        (Some(p), Some(pos)) => {
                            // The acoustics' paths run ear to ear; so must the straight line.
                            let (main, alt) = shared.path(*id, spatial::ear(pos, env.ear_m().unwrap_or(spatial::EAR_HEIGHT)), s.occlusion);
                            let swap = matches!((alt, v.ways[0].from), (Some(a), Some(f))
                                if spatial::distance(a.from, f) < spatial::distance(main.from, f));
                            let slots = if swap { [alt, Some(main)] } else { [Some(main), alt] };
                            let listener = spatial::ear(p.pos(), my_ear);
                            for (k, path) in slots.into_iter().enumerate() {
                                let Some(mut path) = path else { continue };
                                path.from = v.ways[k].glide(path.from);
                                // Only the loudest way decides the party radio.
                                let is_main = k == usize::from(swap);
                                heard[k] = Some(spatial::heard(listener, p.forward_vec(), path, party && is_main, range * reach, carry, v.radio));
                            }
                            if let Some(r) = heard.iter().flatten().find(|h| h.radio).copied() {
                                heard = [Some(r), None];
                            }
                        }
                        // Either of us outside the world: plain, like a call.
                        _ => heard[0] = Some(spatial::Heard { gain: 1.0, dir: spatial::AHEAD, cutoff: LowPass::OPEN, radio: true, reverb: 0.0 }),
                    }
                    v.radio = heard[0].is_some_and(|h| h.radio);
                    let volume = if s.deafened || pref.muted { 0.0 } else { pref.volume };
                    let silent = spatial::Heard { gain: 0.0, dir: spatial::AHEAD, cutoff: LowPass::OPEN, radio: false, reverb: 0.0 };
                    let mut loudest = 0.0;
                    for (way, h) in v.ways.iter_mut().zip(heard) {
                        let h = h.unwrap_or(silent);
                        let send = if s.reverb && !h.radio { h.reverb * volume } else { 0.0 };
                        // Their room rings out where most of it reaches us.
                        if send > loudest {
                            (loudest, v.far.dir) = (send, h.dir);
                        }
                        way.play(&voice, &h, h.gain * volume, [send * wet_here, send * wet_there], &mut mix, &mut rev_in, &mut v.far.send);
                    }
                    v.far.reverb.set_room(f32::from(env.size) / 255.0, f32::from(env.damp) / 255.0);
                    v.far.ring(&mut mix);
                }
                if let (Some(s3d), Some(dec), Some(space)) = (s3d, decoder.as_mut(), space.as_ref()) {
                    dec.decode(&s3d.engine, space, &mut mix);
                }
                // Echo canceller reference: only the other players. Our own
                // voice (monitor, our reverb) must not be in it, or the
                // canceller fights the microphone: it eats sibilants and the
                // loop through it can howl.
                for (r, lr) in reference.iter_mut().zip(mix.chunks_exact(2)) {
                    *r = (lr[0] + lr[1]) * 0.5 * s.voice_volume;
                }
                for (m, x) in mix.chunks_exact_mut(2).zip(drain_up_to(&mut monitor_q, FRAME)) {
                    m[0] += x * 0.6;
                    m[1] += x * 0.6;
                }
                for (r, x) in rev_in.iter_mut().zip(drain_up_to(&mut self_rev_q, FRAME)) {
                    *r += x;
                }
                if let Some(e) = surroundings {
                    reverb.set_room(e.size, e.damp);
                }
                reverb.process(&rev_in, &mut mix);
                for x in mix.iter_mut() {
                    *x *= s.voice_volume;
                }
                out_clip.apply(&mut mix);
                for half in reference.chunks_exact(HALF) {
                    processor.render(half);
                }
                left.clear();
                right.clear();
                left.extend(mix.iter().step_by(2));
                right.extend(mix.iter().skip(1).step_by(2));
                out_l.clear();
                out_r.clear();
                to_out_l.process(&left, &mut out_l);
                to_out_r.process(&right, &mut out_r);
                self.play_tx.push_iter(out_l.iter().zip(&out_r).flat_map(|(l, r)| [*l, *r]));
            }
            thread::sleep(TICK);
        }
        Ok(())
    }
}

fn drain_up_to(q: &mut VecDeque<f32>, n: usize) -> std::collections::vec_deque::Drain<'_, f32> {
    let n = n.min(q.len());
    q.drain(..n)
}
