//! Audio engine: microphone → Opus → server while push-to-talk is held, and
//! remote speakers → spatial stereo mix → default output device.
//!
//! The cpal callbacks only downmix/upmix and move samples through lock-free
//! rings; resampling, Opus and mixing run on our own worker thread. The output
//! ring is kept topped up to a fixed depth, so the device clock paces the mix.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream};
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use vc_protocol::{ClientMsg, PlayerId, Vec3};

use crate::game::Pose;
use crate::log_info;
use crate::net::{FRAME, Sender, Shared};

/// Opus works at 48 kHz; 20 ms frames.
const OPUS_RATE: u32 = 48_000;
/// Mixed audio kept queued for the output device.
const OUTPUT_DEPTH_MS: u32 = 60;
const TICK: Duration = Duration::from_millis(5);
const OPUS_BITRATE: i32 = 24_000;
/// Within this distance speakers are at full volume.
const NEAR: f32 = 2.0;
/// Party members within proximity range never drop below this volume.
const PARTY_MIN_GAIN: f32 = 0.7;

fn key_down(vk: i32) -> bool {
    // High bit set = key currently down.
    unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(vk) < 0 }
}

pub fn run(shared: Arc<Shared>, mut tx: Sender) {
    if let Err(e) = run_inner(&shared, &mut tx) {
        log_info(format_args!("audio: {e}"));
    }
}

fn run_inner(shared: &Shared, tx: &mut Sender) -> Result<(), String> {
    let host = cpal::default_host();
    let input = host.default_input_device().ok_or("no input device")?;
    let output = host.default_output_device().ok_or("no output device")?;
    let in_cfg = input.default_input_config().map_err(|e| e.to_string())?;
    let out_cfg = output.default_output_config().map_err(|e| e.to_string())?;
    for (what, dev, cfg) in [("in ", &input, &in_cfg), ("out", &output, &out_cfg)] {
        log_info(format_args!(
            "audio: {what} '{}' {} Hz {} ch {:?}",
            dev.description().map(|d| d.name().to_owned()).unwrap_or_default(),
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
    let (mut cap_tx, mut cap_rx) = HeapRb::<f32>::new(in_rate as usize).split();
    let (mut play_tx, mut play_rx) = HeapRb::<f32>::new(out_rate as usize * 2).split();

    let in_ch = in_cfg.channels() as usize;
    let in_stream: Stream = input
        .build_input_stream::<f32, _, _>(
            in_cfg.config(),
            move |data, _| {
                cap_tx.push_iter(
                    data.chunks_exact(in_ch).map(|f| f.iter().sum::<f32>() / in_ch as f32),
                );
            },
            |e| log_info(format_args!("audio: input error: {e}")),
            None,
        )
        .map_err(|e| e.to_string())?;

    let out_ch = out_cfg.channels() as usize;
    let out_stream: Stream = output
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
            |e| log_info(format_args!("audio: output error: {e}")),
            None,
        )
        .map_err(|e| e.to_string())?;

    in_stream.play().map_err(|e| e.to_string())?;
    out_stream.play().map_err(|e| e.to_string())?;

    let mut encoder = opus::Encoder::new(OPUS_RATE, opus::Channels::Mono, opus::Application::Voip)
        .map_err(|e| e.to_string())?;
    encoder.set_bitrate(opus::Bitrate::Bits(OPUS_BITRATE)).map_err(|e| e.to_string())?;
    let mut mic_clip = opus::SoftClip::new(opus::Channels::Mono);
    let mut out_clip = opus::SoftClip::new(opus::Channels::Stereo);
    let mut to_opus = Resampler::new(in_rate, OPUS_RATE);
    let (mut to_out_l, mut to_out_r) =
        (Resampler::new(OPUS_RATE, out_rate), Resampler::new(OPUS_RATE, out_rate));

    let out_depth = (out_rate * OUTPUT_DEPTH_MS / 1000) as usize * 2;
    let mut raw = vec![0.0f32; in_rate as usize / 10];
    let mut pcm = Vec::<f32>::with_capacity(FRAME * 8);
    let mut frame = [0.0f32; FRAME];
    let mut packet = [0u8; 1000];
    let mut voice = [0.0f32; FRAME];
    let mut mix = vec![0.0f32; FRAME * 2];
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let (mut out_l, mut out_r) = (Vec::new(), Vec::new());
    let mut seq = 0u32;
    let mut talking = false;
    // Speakers currently playing, to log when one starts.
    let mut audible = HashSet::<PlayerId>::new();

    log_info(format_args!("audio: ready"));
    while !shared.stop.load(Ordering::Relaxed) {
        let pose = *shared.pose.lock().unwrap();
        let link = *shared.link.lock().unwrap();
        let live = *shared.live.lock().unwrap();

        // --- capture ---
        let n = cap_rx.pop_slice(&mut raw);
        to_opus.process(&raw[..n], &mut pcm);
        let whole = pcm.len() / FRAME * FRAME;
        let can_talk = key_down(live.ptt_vk) && link.is_some() && pose.is_some();
        if can_talk != talking {
            talking = can_talk;
            match (talking, pose) {
                (true, Some(p)) => log_info(format_args!("audio: talking at {:.1?}", p.pos())),
                _ => log_info(format_args!("audio: stopped")),
            }
        }
        if let (true, Some(p)) = (talking, pose) {
            for chunk in pcm[..whole].chunks_exact(FRAME) {
                for (o, s) in frame.iter_mut().zip(chunk) {
                    *o = s * live.mic_gain;
                }
                mic_clip.apply(&mut frame);
                let len = encoder.encode_float(&frame, &mut packet).map_err(|e| e.to_string())?;
                tx.send(&ClientMsg::Voice { seq, pos: p.pos(), opus: packet[..len].to_vec() });
                seq = seq.wrapping_add(1);
            }
        }
        pcm.drain(..whole);

        // --- playback ---
        while play_tx.occupied_len() < out_depth {
            mix.fill(0.0);
            {
                let mut speakers = shared.speakers.lock().unwrap();
                for (id, s) in speakers.iter_mut() {
                    if !s.next_frame(&mut voice) {
                        audible.remove(id);
                        continue;
                    }
                    let (gl, gr) = match (pose, link) {
                        (Some(p), Some(l)) => spatial(&p, s.pos, s.party, l.range),
                        _ => (0.0, 0.0),
                    };
                    if audible.insert(*id) {
                        let dist = pose.map(|p| dist(p.pos(), s.pos));
                        log_info(format_args!(
                            "audio: hearing #{id} at {dist:.1?} m, gain L {gl:.2} R {gr:.2}{}",
                            if s.party { " (party)" } else { "" }
                        ));
                    }
                    let (gl, gr) = (gl * live.volume, gr * live.volume);
                    for (m, v) in mix.chunks_exact_mut(2).zip(&voice) {
                        m[0] += v * gl;
                        m[1] += v * gr;
                    }
                }
            }
            out_clip.apply(&mut mix);
            left.clear();
            right.clear();
            left.extend(mix.iter().step_by(2));
            right.extend(mix.iter().skip(1).step_by(2));
            out_l.clear();
            out_r.clear();
            to_out_l.process(&left, &mut out_l);
            to_out_r.process(&right, &mut out_r);
            play_tx.push_iter(out_l.iter().zip(&out_r).flat_map(|(l, r)| [*l, *r]));
        }

        thread::sleep(TICK);
    }
    Ok(())
}

fn dist(a: Vec3, b: Vec3) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum::<f32>().sqrt()
}

/// Left/right gains for a speaker at `src` heard by the listener `p`.
fn spatial(p: &Pose, src: Vec3, party: bool, range: f32) -> (f32, f32) {
    let d = [src[0] - p.position.x, src[1] - p.position.y, src[2] - p.position.z];
    let dist = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    if party && dist > range {
        // Out of earshot: party members sound like a radio, centred.
        let g = std::f32::consts::FRAC_1_SQRT_2;
        return (g, g);
    }
    let mut gain = if dist <= NEAR {
        1.0
    } else {
        ((range - dist) / (range - NEAR)).clamp(0.0, 1.0)
    };
    if party {
        gain = gain.max(PARTY_MIN_GAIN);
    }
    // Pan from the horizontal direction relative to where the player faces.
    // Z is up, so the right-hand vector of forward (fx, fy) is (fy, -fx).
    let horiz = (d[0] * d[0] + d[1] * d[1]).sqrt();
    let pan = if horiz > 0.5 {
        let (fx, fy) = (p.forward.x, p.forward.y);
        let flen = (fx * fx + fy * fy).sqrt().max(1e-6);
        ((d[0] * fy - d[1] * fx) / (horiz * flen)).clamp(-1.0, 1.0)
    } else {
        0.0
    };
    // Equal-power panning.
    let angle = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
    (gain * angle.cos(), gain * angle.sin())
}

/// Streaming linear resampler; good enough for voice.
struct Resampler {
    step: f64,
    pos: f64,
    prev: f32,
}

impl Resampler {
    fn new(from: u32, to: u32) -> Self {
        Self { step: from as f64 / to as f64, pos: 0.0, prev: 0.0 }
    }

    /// Appends the resampled `input` to `out`. Positions index a virtual
    /// signal where 0 is the last sample of the previous call.
    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }
        if self.step == 1.0 {
            out.extend_from_slice(input);
            return;
        }
        let prev = self.prev;
        let at = |i: usize| if i == 0 { prev } else { input[i - 1] };
        let n = input.len() as f64;
        while self.pos < n {
            let i = self.pos as usize;
            let frac = (self.pos - i as f64) as f32;
            let (a, b) = (at(i), at(i + 1));
            out.push(a + (b - a) * frac);
            self.pos += self.step;
        }
        self.pos -= n;
        self.prev = input[input.len() - 1];
    }
}
