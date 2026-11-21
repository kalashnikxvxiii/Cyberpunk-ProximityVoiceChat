//! A thin, safe layer over Steam Audio's C API (phonon 4.8.1): only what we
//! use. Every handle is released on drop; Steam Audio counts references, so a
//! handle kept by another object (an HRTF inside an effect) stays alive.

use std::ffi::c_void;
use std::ptr::{null, null_mut};

use audionimbus_sys as ffi;

pub use ffi::{IPLHit as Hit, IPLMaterial as Material, IPLRay as Ray, IPLVector3 as V3};

/// Our audio runs at 48 kHz in 20 ms frames.
pub const RATE: i32 = 48_000;
pub const FRAME: usize = 960;
/// Ambisonics order of the rebounds: 1 = 4 channels, enough to tell where
/// the bulk of a room's sound comes from.
pub const ORDER: i32 = 1;
pub const AMBI_CHANNELS: usize = ((ORDER + 1) * (ORDER + 1)) as usize;
/// Longest rebound tail simulated (s).
pub const IR_SECONDS: f32 = 1.0;

fn ok(e: ffi::IPLerror, what: &str) -> Result<(), String> {
    match e {
        ffi::IPLerror::IPL_STATUS_SUCCESS => Ok(()),
        other => Err(format!("{what}: {other:?}")),
    }
}

fn audio_settings() -> ffi::IPLAudioSettings {
    ffi::IPLAudioSettings { samplingRate: RATE, frameSize: FRAME as i32 }
}

pub fn v3(x: f32, y: f32, z: f32) -> V3 {
    V3 { x, y, z }
}

/// Position and orientation, Steam Audio's axes (y up, right-handed).
#[derive(Debug, Clone, Copy)]
pub struct Space {
    pub origin: V3,
    pub ahead: V3,
    pub up: V3,
    pub right: V3,
}

impl Space {
    pub fn at(origin: V3) -> Self {
        Self { origin, ahead: v3(0.0, 0.0, -1.0), up: v3(0.0, 1.0, 0.0), right: v3(1.0, 0.0, 0.0) }
    }

    fn raw(&self) -> ffi::IPLCoordinateSpace3 {
        ffi::IPLCoordinateSpace3 { right: self.right, up: self.up, ahead: self.ahead, origin: self.origin }
    }
}

macro_rules! handle {
    ($name:ident, $raw:ty, $release:ident) => {
        pub struct $name($raw);
        // Steam Audio objects are reference counted and internally synchronised
        // where its API allows use from several threads (see each use).
        unsafe impl Send for $name {}
        unsafe impl Sync for $name {}
        impl Drop for $name {
            fn drop(&mut self) {
                unsafe { ffi::$release(&mut self.0) };
            }
        }
    };
}

handle!(Context, ffi::IPLContext, iplContextRelease);
handle!(Hrtf, ffi::IPLHRTF, iplHRTFRelease);
handle!(Simulator, ffi::IPLSimulator, iplSimulatorRelease);
handle!(Source, ffi::IPLSource, iplSourceRelease);
handle!(DirectEffect, ffi::IPLDirectEffect, iplDirectEffectRelease);
handle!(BinauralEffect, ffi::IPLBinauralEffect, iplBinauralEffectRelease);
handle!(ReflectionEffect, ffi::IPLReflectionEffect, iplReflectionEffectRelease);
handle!(DecodeEffect, ffi::IPLAmbisonicsDecodeEffect, iplAmbisonicsDecodeEffectRelease);

impl Context {
    pub fn new() -> Result<Self, String> {
        let mut settings = ffi::IPLContextSettings {
            version: ffi::STEAMAUDIO_VERSION,
            logCallback: None,
            allocateCallback: None,
            freeCallback: None,
            simdLevel: ffi::IPLSIMDLevel::IPL_SIMDLEVEL_AVX2,
            flags: ffi::IPLContextFlags(0),
        };
        let mut raw = null_mut();
        ok(unsafe { ffi::iplContextCreate(&mut settings, &mut raw) }, "context")?;
        Ok(Self(raw))
    }

    /// `source` as seen from a listener at `listener`: a unit vector in the
    /// listener's own axes (what HRTFs take).
    pub fn relative(&self, source: V3, listener: &Space) -> V3 {
        unsafe { ffi::iplCalculateRelativeDirection(self.0, source, listener.origin, listener.ahead, listener.up) }
    }
}

impl Hrtf {
    /// Steam Audio's built-in HRTF (measured on real heads).
    pub fn new(ctx: &Context) -> Result<Self, String> {
        let mut settings = ffi::IPLHRTFSettings {
            type_: ffi::IPLHRTFType::IPL_HRTFTYPE_DEFAULT,
            sofaFileName: null(),
            sofaData: null(),
            sofaDataSize: 0,
            volume: 1.0,
            normType: ffi::IPLHRTFNormType::IPL_HRTFNORMTYPE_NONE,
        };
        let mut raw = null_mut();
        ok(unsafe { ffi::iplHRTFCreate(ctx.0, &mut audio_settings(), &mut settings, &mut raw) }, "hrtf")?;
        Ok(Self(raw))
    }
}

/// Answers Steam Audio's rays with the world's geometry. Called from
/// whichever thread runs a simulation, possibly two at once (the direct
/// sound on one, the rebounds on another): no lock is held while it works,
/// so one can wait for the game without blocking the other.
pub trait Tracer: Send + Sync {
    /// For each ray: the nearest surface within `[min, max]`, or [`miss`].
    fn closest(&self, rays: &[Ray], min: &[f32], max: &[f32], hits: &mut [Hit]);

    /// It lets rays through leaky surfaces itself (see `passes`), all in one
    /// go: no second call for them.
    fn leaks_inside(&self) -> bool {
        false
    }
}

/// A ray that hit nothing.
pub fn miss() -> Hit {
    Hit {
        distance: f32::INFINITY,
        triangleIndex: -1,
        objectIndex: -1,
        materialIndex: -1,
        normal: v3(0.0, 1.0, 0.0),
        material: null_mut(),
    }
}

fn is_hit(h: &Hit, max: f32) -> bool {
    h.distance.is_finite() && h.distance <= max && !h.material.is_null()
}

/// Same for every run: which rays a leaky surface lets through (see `leaks`).
pub fn ray_hash(r: &Ray) -> f32 {
    let mut h: u32 = 2_166_136_261;
    for v in [r.origin.x, r.origin.y, r.origin.z, r.direction.x, r.direction.y, r.direction.z] {
        h = (h ^ v.to_bits()).wrapping_mul(16_777_619);
    }
    (h >> 8) as f32 / (1u32 << 24) as f32
}

/// Steam Audio's rebounds don't go through surfaces. A closed interior door
/// does let a room's sound out (gaps, thin panel): it lets this share of
/// the rays through, the same ones every time (no flicker).
fn passes(h: &Hit, r: &Ray) -> bool {
    let leak = crate::materials::leak(h.material);
    leak > 0.0 && ray_hash(r) < leak
}

/// Nearest hits, rays going on through leaky surfaces.
fn resolve(t: &dyn Tracer, rays: &[Ray], min: &[f32], max: &[f32], hits: &mut [Hit]) {
    t.closest(rays, min, max, hits);
    if t.leaks_inside() {
        return;
    }
    for _ in 0..3 {
        let again: Vec<usize> = (0..rays.len()).filter(|&i| is_hit(&hits[i], max[i]) && passes(&hits[i], &rays[i])).collect();
        if again.is_empty() {
            return;
        }
        let r: Vec<Ray> = again.iter().map(|&i| rays[i]).collect();
        let lo: Vec<f32> = again.iter().map(|&i| hits[i].distance + 0.02).collect();
        let hi: Vec<f32> = again.iter().map(|&i| max[i]).collect();
        let mut h = vec![miss(); again.len()];
        t.closest(&r, &lo, &hi, &mut h);
        for (k, &i) in again.iter().enumerate() {
            hits[i] = h[k];
        }
    }
}

/// Copies of `r` moved sideways and up/down by [`SEAM`].
fn shifted(r: &Ray) -> [Ray; 4] {
    let d = r.direction;
    let up = v3(0.0, 1.0, 0.0);
    let mut side = v3(d.y * up.z - d.z * up.y, d.z * up.x - d.x * up.z, d.x * up.y - d.y * up.x);
    let l = (side.x * side.x + side.y * side.y + side.z * side.z).sqrt();
    side = if l > 1e-3 { v3(side.x / l, side.y / l, side.z / l) } else { v3(1.0, 0.0, 0.0) };
    let vert = v3(side.y * d.z - side.z * d.y, side.z * d.x - side.x * d.z, side.x * d.y - side.y * d.x);
    let at = |v: V3, k: f32| Ray { origin: v3(r.origin.x + v.x * k, r.origin.y + v.y * k, r.origin.z + v.z * k), direction: d };
    [at(side, SEAM), at(side, -SEAM), at(vert, SEAM), at(vert, -SEAM)]
}

/// The game's collision has seams (a sliding door's edge, a window frame)
/// that a line slips through where no sound would.
const SEAM: f32 = 0.08;

/// One ray, seam-proof: if the line slips through where both copies on
/// either side of it are blocked, it counts as blocked there. Steam Audio
/// traces single rays for the direct sound (what hides the voice, what it
/// goes through), where one seam would let it all through.
fn single(t: &dyn Tracer, r: &Ray, min: f32, max: f32) -> Hit {
    let mut h = [miss()];
    resolve(t, std::slice::from_ref(r), &[min], &[max], &mut h);
    if is_hit(&h[0], max) {
        return h[0];
    }
    let copies = shifted(r);
    let mut ch = [miss(); 4];
    resolve(t, &copies, &[min; 4], &[max; 4], &mut ch);
    let pair = |a: usize, b: usize| (is_hit(&ch[a], max) && is_hit(&ch[b], max)).then(|| if ch[a].distance <= ch[b].distance { ch[a] } else { ch[b] });
    pair(0, 1).or_else(|| pair(2, 3)).unwrap_or(h[0])
}

type Shared = Box<dyn Tracer>;

unsafe fn tracer<'a>(user: *mut c_void) -> &'a dyn Tracer {
    unsafe { &**(user as *const Shared) }
}

unsafe extern "C" fn closest_one(ray: *const Ray, min: f32, max: f32, hit: *mut Hit, user: *mut c_void) {
    let h = single(unsafe { tracer(user) }, unsafe { &*ray }, min, max);
    unsafe { *hit = h };
}

// Steam Audio marks rays it doesn't want traced (one that escaped to the
// sky has no surface to shade) with max < min, and reads them as blocked:
// answered as clear, it shades a surface that isn't there and crashes.

unsafe extern "C" fn any_one(ray: *const Ray, min: f32, max: f32, occluded: *mut u8, user: *mut c_void) {
    let blocked = max < min || is_hit(&single(unsafe { tracer(user) }, unsafe { &*ray }, min, max), max);
    unsafe { *occluded = u8::from(blocked) };
}

unsafe fn slices<'a, T>(n: i32, rays: *const Ray, min: *const f32, max: *const f32, out: *mut T) -> (&'a [Ray], &'a [f32], &'a [f32], &'a mut [T]) {
    let n = n.max(0) as usize;
    if n == 0 {
        return (&[], &[], &[], &mut []);
    }
    unsafe {
        (
            std::slice::from_raw_parts(rays, n),
            std::slice::from_raw_parts(min, n),
            std::slice::from_raw_parts(max, n),
            std::slice::from_raw_parts_mut(out, n),
        )
    }
}

unsafe extern "C" fn closest_batch(n: i32, rays: *const Ray, min: *const f32, max: *const f32, hits: *mut Hit, user: *mut c_void) {
    let (rays, min, max, hits) = unsafe { slices(n, rays, min, max, hits) };
    if !rays.is_empty() {
        resolve(unsafe { tracer(user) }, rays, min, max, hits);
    }
}

unsafe extern "C" fn any_batch(n: i32, rays: *const Ray, min: *const f32, max: *const f32, occluded: *mut u8, user: *mut c_void) {
    let (rays, min, max, occ) = unsafe { slices(n, rays, min, max, occluded) };
    if rays.is_empty() {
        return;
    }
    let mut hits = vec![miss(); rays.len()];
    resolve(unsafe { tracer(user) }, rays, min, max, &mut hits);
    for i in 0..rays.len() {
        occ[i] = u8::from(max[i] < min[i] || is_hit(&hits[i], max[i]));
    }
}

/// A scene whose geometry is whatever `Tracer` answers.
pub struct Scene {
    raw: ffi::IPLScene,
    // Steam Audio holds a pointer to it: boxed, and dropped after the scene.
    _tracer: Box<Shared>,
}

unsafe impl Send for Scene {}
unsafe impl Sync for Scene {}

impl Drop for Scene {
    fn drop(&mut self) {
        unsafe { ffi::iplSceneRelease(&mut self.raw) };
    }
}

impl Scene {
    pub fn new(ctx: &Context, tracer: Box<dyn Tracer>) -> Result<Self, String> {
        let tracer: Box<Shared> = Box::new(tracer);
        let mut settings = ffi::IPLSceneSettings {
            type_: ffi::IPLSceneType::IPL_SCENETYPE_CUSTOM,
            closestHitCallback: Some(closest_one),
            anyHitCallback: Some(any_one),
            batchedClosestHitCallback: Some(closest_batch),
            batchedAnyHitCallback: Some(any_batch),
            userData: &*tracer as *const Shared as *mut c_void,
            embreeDevice: null_mut(),
            radeonRaysDevice: null_mut(),
        };
        let mut raw = null_mut();
        ok(unsafe { ffi::iplSceneCreate(ctx.0, &mut settings, &mut raw) }, "scene")?;
        unsafe { ffi::iplSceneCommit(raw) };
        Ok(Self { raw, _tracer: tracer })
    }
}

/// How much of the simulation to do per run.
#[derive(Debug, Clone, Copy)]
pub struct Quality {
    /// Rays shot from the listener for rebounds.
    pub rays: i32,
    pub bounces: i32,
    /// Points over the source's sphere tested for occlusion.
    pub occlusion_samples: i32,
    /// Surfaces a blocked voice may go through.
    pub transmission_rays: i32,
}

pub const MAX_RAYS: i32 = 4096;
pub const MAX_SOURCES: i32 = 32;
pub const MAX_OCCLUSION_SAMPLES: i32 = 64;

const SIM_FLAGS: ffi::IPLSimulationFlags =
    ffi::IPLSimulationFlags(ffi::IPLSimulationFlags::IPL_SIMULATIONFLAGS_DIRECT.0 | ffi::IPLSimulationFlags::IPL_SIMULATIONFLAGS_REFLECTIONS.0);
pub type Flags = ffi::IPLSimulationFlags;
pub const DIRECT: Flags = ffi::IPLSimulationFlags::IPL_SIMULATIONFLAGS_DIRECT;
pub const REFLECTIONS: Flags = ffi::IPLSimulationFlags::IPL_SIMULATIONFLAGS_REFLECTIONS;

impl Simulator {
    pub fn new(ctx: &Context, scene: &Scene) -> Result<Self, String> {
        let mut settings = ffi::IPLSimulationSettings {
            flags: SIM_FLAGS,
            sceneType: ffi::IPLSceneType::IPL_SCENETYPE_CUSTOM,
            reflectionType: ffi::IPLReflectionEffectType::IPL_REFLECTIONEFFECTTYPE_CONVOLUTION,
            maxNumOcclusionSamples: MAX_OCCLUSION_SAMPLES,
            maxNumRays: MAX_RAYS,
            numDiffuseSamples: 32,
            maxDuration: IR_SECONDS,
            maxOrder: ORDER,
            maxNumSources: MAX_SOURCES,
            numThreads: 1,
            // Rays go to the game a batch at a time, each batch a frame or more:
            // every bounce's rays in one (1024 took 0.7-1.1 s a run in a
            // diner, too slow walking out of it, 2026-11-20).
            rayBatchSize: 2048,
            numVisSamples: 1,
            samplingRate: RATE,
            frameSize: FRAME as i32,
            openCLDevice: null_mut(),
            radeonRaysDevice: null_mut(),
            tanDevice: null_mut(),
        };
        let mut raw = null_mut();
        ok(unsafe { ffi::iplSimulatorCreate(ctx.0, &mut settings, &mut raw) }, "simulator")?;
        unsafe {
            ffi::iplSimulatorSetScene(raw, scene.raw);
            ffi::iplSimulatorCommit(raw);
        }
        Ok(Self(raw))
    }

    /// Adds or removes sources: takes effect at the next run.
    pub fn commit(&self) {
        unsafe { ffi::iplSimulatorCommit(self.0) };
    }

    /// The listener, for one kind of simulation (`DIRECT` or `REFLECTIONS`):
    /// each kind runs on its own thread and sets its own inputs.
    pub fn set_listener(&self, flags: Flags, listener: &Space, q: &Quality) {
        let mut shared = ffi::IPLSimulationSharedInputs {
            listener: listener.raw(),
            numRays: q.rays.clamp(1, MAX_RAYS),
            numBounces: q.bounces.max(1),
            duration: IR_SECONDS,
            order: ORDER,
            irradianceMinDistance: 1.0,
            pathingVisCallback: None,
            pathingUserData: null_mut(),
        };
        unsafe { ffi::iplSimulatorSetSharedInputs(self.0, flags, &mut shared) };
    }

    /// Occlusion and transmission for every source: cheap.
    pub fn run_direct(&self) {
        unsafe { ffi::iplSimulatorRunDirect(self.0) };
    }

    /// Rebounds for every source: many rays, run on its own thread.
    pub fn run_reflections(&self) {
        unsafe { ffi::iplSimulatorRunReflections(self.0) };
    }
}

/// What the direct simulation found between a source and the listener.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Direct {
    /// Share of the source seen from the listener (0 = hidden, 1 = in plain sight).
    pub occlusion: f32,
    /// What passes through what hides it, low/mid/high bands.
    pub transmission: [f32; 3],
}

impl Source {
    pub fn new(sim: &Simulator) -> Result<Self, String> {
        let mut settings = ffi::IPLSourceSettings { flags: SIM_FLAGS };
        let mut raw = null_mut();
        ok(unsafe { ffi::iplSourceCreate(sim.0, &mut settings, &mut raw) }, "source")?;
        unsafe { ffi::iplSourceAdd(raw, sim.0) };
        Ok(Self(raw))
    }

    pub fn remove(&self, sim: &Simulator) {
        unsafe { ffi::iplSourceRemove(self.0, sim.0) };
    }

    pub fn set(&self, flags: Flags, at: V3, q: &Quality, radius: f32) {
        let mut inputs = ffi::IPLSimulationInputs {
            flags,
            directFlags: ffi::IPLDirectSimulationFlags(
                ffi::IPLDirectSimulationFlags::IPL_DIRECTSIMULATIONFLAGS_OCCLUSION.0
                    | ffi::IPLDirectSimulationFlags::IPL_DIRECTSIMULATIONFLAGS_TRANSMISSION.0,
            ),
            source: Space::at(at).raw(),
            distanceAttenuationModel: ffi::IPLDistanceAttenuationModel {
                type_: ffi::IPLDistanceAttenuationModelType::IPL_DISTANCEATTENUATIONTYPE_DEFAULT,
                minDistance: 1.0,
                callback: None,
                userData: null_mut(),
                dirty: ffi::IPLbool::IPL_FALSE,
            },
            airAbsorptionModel: ffi::IPLAirAbsorptionModel {
                type_: ffi::IPLAirAbsorptionModelType::IPL_AIRABSORPTIONTYPE_DEFAULT,
                coefficients: [0.0; 3],
                callback: None,
                userData: null_mut(),
                dirty: ffi::IPLbool::IPL_FALSE,
            },
            directivity: ffi::IPLDirectivity { dipoleWeight: 0.0, dipolePower: 0.0, callback: None, userData: null_mut() },
            // A sphere, not a point: a seam in the geometry hides a few of its
            // samples, not the whole voice.
            occlusionType: ffi::IPLOcclusionType::IPL_OCCLUSIONTYPE_VOLUMETRIC,
            occlusionRadius: radius,
            numOcclusionSamples: q.occlusion_samples.clamp(1, MAX_OCCLUSION_SAMPLES),
            reverbScale: [1.0; 3],
            hybridReverbTransitionTime: 1.0,
            hybridReverbOverlapPercent: 0.25,
            baked: ffi::IPLbool::IPL_FALSE,
            bakedDataIdentifier: unsafe { std::mem::zeroed() },
            pathingProbes: null_mut(),
            visRadius: 0.0,
            visThreshold: 0.0,
            visRange: 0.0,
            pathingOrder: 0,
            enableValidation: ffi::IPLbool::IPL_FALSE,
            findAlternatePaths: ffi::IPLbool::IPL_FALSE,
            numTransmissionRays: q.transmission_rays.max(1),
            deviationModel: null_mut(),
        };
        unsafe { ffi::iplSourceSetInputs(self.0, flags, &mut inputs) };
    }

    pub fn direct(&self) -> Direct {
        let mut out: ffi::IPLSimulationOutputs = unsafe { std::mem::zeroed() };
        unsafe { ffi::iplSourceGetOutputs(self.0, DIRECT, &mut out) };
        Direct { occlusion: out.direct.occlusion, transmission: out.direct.transmission }
    }

    /// The rebounds' impulse response, for [`ReflectionEffect::apply`]. Steam
    /// Audio's integrations read it on the audio thread while the simulation
    /// thread runs: it double-buffers it.
    fn reflections(&self) -> ffi::IPLReflectionEffectParams {
        let mut out: ffi::IPLSimulationOutputs = unsafe { std::mem::zeroed() };
        unsafe { ffi::iplSourceGetOutputs(self.0, REFLECTIONS, &mut out) };
        out.reflections
    }
}

/// Audio in Steam Audio's planar layout, owned by us.
pub struct Buffer {
    data: Vec<Vec<f32>>,
    ptrs: Vec<*mut f32>,
}

unsafe impl Send for Buffer {}

impl Buffer {
    pub fn new(channels: usize) -> Self {
        let mut data: Vec<Vec<f32>> = (0..channels).map(|_| vec![0.0; FRAME]).collect();
        let ptrs = data.iter_mut().map(|c| c.as_mut_ptr()).collect();
        Self { data, ptrs }
    }

    fn raw(&mut self) -> ffi::IPLAudioBuffer {
        ffi::IPLAudioBuffer { numChannels: self.ptrs.len() as i32, numSamples: FRAME as i32, data: self.ptrs.as_mut_ptr() }
    }

    pub fn channel(&self, i: usize) -> &[f32] {
        &self.data[i]
    }

    pub fn channel_mut(&mut self, i: usize) -> &mut [f32] {
        &mut self.data[i]
    }

    pub fn clear(&mut self) {
        for c in &mut self.data {
            c.fill(0.0);
        }
    }

    /// Adds `other` (same channel count) into this buffer.
    pub fn add(&mut self, other: &Buffer, gain: f32) {
        for (d, s) in self.data.iter_mut().zip(&other.data) {
            for (a, b) in d.iter_mut().zip(s) {
                *a += b * gain;
            }
        }
    }
}

impl DirectEffect {
    pub fn new(ctx: &Context) -> Result<Self, String> {
        let mut settings = ffi::IPLDirectEffectSettings { numChannels: 1 };
        let mut raw = null_mut();
        ok(unsafe { ffi::iplDirectEffectCreate(ctx.0, &mut audio_settings(), &mut settings, &mut raw) }, "direct effect")?;
        Ok(Self(raw))
    }

    /// `gain` (our distance law), `air` (highs lost over the distance,
    /// low/mid/high), then what blocks the way: the share in plain sight as
    /// is, the hidden share through the walls' materials.
    pub fn apply(&self, d: &Direct, gain: f32, air: [f32; 3], input: &mut Buffer, out: &mut Buffer) {
        let mut params = ffi::IPLDirectEffectParams {
            flags: ffi::IPLDirectEffectFlags(
                ffi::IPLDirectEffectFlags::IPL_DIRECTEFFECTFLAGS_APPLYDISTANCEATTENUATION.0
                    | ffi::IPLDirectEffectFlags::IPL_DIRECTEFFECTFLAGS_APPLYAIRABSORPTION.0
                    | ffi::IPLDirectEffectFlags::IPL_DIRECTEFFECTFLAGS_APPLYOCCLUSION.0
                    | ffi::IPLDirectEffectFlags::IPL_DIRECTEFFECTFLAGS_APPLYTRANSMISSION.0,
            ),
            transmissionType: ffi::IPLTransmissionType::IPL_TRANSMISSIONTYPE_FREQDEPENDENT,
            distanceAttenuation: gain,
            airAbsorption: air,
            directivity: 1.0,
            occlusion: d.occlusion,
            transmission: d.transmission,
        };
        let (mut i, mut o) = (input.raw(), out.raw());
        unsafe { ffi::iplDirectEffectApply(self.0, &mut params, &mut i, &mut o) };
    }
}

impl BinauralEffect {
    pub fn new(ctx: &Context, hrtf: &Hrtf) -> Result<Self, String> {
        let mut settings = ffi::IPLBinauralEffectSettings { hrtf: hrtf.0 };
        let mut raw = null_mut();
        ok(unsafe { ffi::iplBinauralEffectCreate(ctx.0, &mut audio_settings(), &mut settings, &mut raw) }, "binaural effect")?;
        Ok(Self(raw))
    }

    /// Mono `input` from `dir` (listener's axes) into stereo `out`.
    pub fn apply(&self, hrtf: &Hrtf, dir: V3, input: &mut Buffer, out: &mut Buffer) {
        let mut params = ffi::IPLBinauralEffectParams {
            direction: dir,
            interpolation: ffi::IPLHRTFInterpolation::IPL_HRTFINTERPOLATION_BILINEAR,
            spatialBlend: 1.0,
            hrtf: hrtf.0,
            peakDelays: null_mut(),
        };
        let (mut i, mut o) = (input.raw(), out.raw());
        unsafe { ffi::iplBinauralEffectApply(self.0, &mut params, &mut i, &mut o) };
    }
}

impl ReflectionEffect {
    pub fn new(ctx: &Context) -> Result<Self, String> {
        let mut settings = ffi::IPLReflectionEffectSettings {
            type_: ffi::IPLReflectionEffectType::IPL_REFLECTIONEFFECTTYPE_CONVOLUTION,
            irSize: (IR_SECONDS * RATE as f32) as i32,
            numChannels: AMBI_CHANNELS as i32,
        };
        let mut raw = null_mut();
        ok(unsafe { ffi::iplReflectionEffectCreate(ctx.0, &mut audio_settings(), &mut settings, &mut raw) }, "reflection effect")?;
        Ok(Self(raw))
    }

    /// Mono `input` through `source`'s rebounds into ambisonics `out`.
    /// False: the simulation has no rebounds for it yet (silence out).
    pub fn apply(&self, source: &Source, input: &mut Buffer, out: &mut Buffer) -> bool {
        let mut params = source.reflections();
        if params.ir.is_null() {
            out.clear();
            return false;
        }
        params.numChannels = AMBI_CHANNELS as i32;
        params.irSize = (IR_SECONDS * RATE as f32) as i32;
        let (mut i, mut o) = (input.raw(), out.raw());
        unsafe { ffi::iplReflectionEffectApply(self.0, &mut params, &mut i, &mut o, null_mut()) };
        true
    }
}

impl DecodeEffect {
    pub fn new(ctx: &Context, hrtf: &Hrtf) -> Result<Self, String> {
        let mut settings = ffi::IPLAmbisonicsDecodeEffectSettings {
            speakerLayout: ffi::IPLSpeakerLayout {
                type_: ffi::IPLSpeakerLayoutType::IPL_SPEAKERLAYOUTTYPE_STEREO,
                numSpeakers: 2,
                speakers: null_mut(),
            },
            hrtf: hrtf.0,
            maxOrder: ORDER,
        };
        let mut raw = null_mut();
        ok(unsafe { ffi::iplAmbisonicsDecodeEffectCreate(ctx.0, &mut audio_settings(), &mut settings, &mut raw) }, "decode effect")?;
        Ok(Self(raw))
    }

    /// Ambisonics `input` (world axes) to stereo `out` for headphones, turned to `listener`.
    pub fn apply(&self, hrtf: &Hrtf, listener: &Space, input: &mut Buffer, out: &mut Buffer) {
        let mut params = ffi::IPLAmbisonicsDecodeEffectParams {
            order: ORDER,
            hrtf: hrtf.0,
            orientation: listener.raw(),
            binaural: ffi::IPLbool::IPL_TRUE,
        };
        let (mut i, mut o) = (input.raw(), out.raw());
        unsafe { ffi::iplAmbisonicsDecodeEffectApply(self.0, &mut params, &mut i, &mut o) };
    }
}
