//! Microphone clean-up: WebRTC AEC3 echo cancellation and noise suppression
//! (via the `aec3` port), or RNNoise for the "AI" setting. 10 ms frames.

use aec3::audio_processing::ns::ns_config::{NsConfig, SuppressionLevel};
use aec3::nodes::audio::AudioFormat;
use aec3::pipelines::linear::{self, LinearPipeline};
use nnnoiseless::DenoiseState;

use crate::HALF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suppression {
    Off,
    /// WebRTC noise suppression, 0 (light) ..= 3 (strongest).
    Classic(u8),
    /// RNNoise.
    Ai,
}

pub struct Processor {
    echo_cancel: bool,
    suppression: Suppression,
    pipeline: Option<LinearPipeline>,
    rnnoise: Option<Box<DenoiseState<'static>>>,
    scratch: [f32; HALF],
}

impl Processor {
    pub fn new(echo_cancel: bool, suppression: Suppression) -> Result<Self, String> {
        let mut p = Self {
            echo_cancel: !echo_cancel,
            suppression,
            pipeline: None,
            rnnoise: None,
            scratch: [0.0; HALF],
        };
        p.configure(echo_cancel, suppression)?;
        Ok(p)
    }

    /// Rebuilds the chain when the settings change.
    pub fn configure(&mut self, echo_cancel: bool, suppression: Suppression) -> Result<(), String> {
        if echo_cancel == self.echo_cancel && suppression == self.suppression {
            return Ok(());
        }
        self.echo_cancel = echo_cancel;
        self.suppression = suppression;
        let classic = match suppression {
            Suppression::Classic(level) => Some(match level {
                0 => SuppressionLevel::K6dB,
                1 => SuppressionLevel::K12dB,
                2 => SuppressionLevel::K18dB,
                _ => SuppressionLevel::K21dB,
            }),
            _ => None,
        };
        self.pipeline = if echo_cancel || classic.is_some() {
            let format = AudioFormat::ten_ms(crate::RATE, 1);
            let mut b = linear::builder(format, format)
                .enable_high_pass_filter(true)
                .enable_noise_suppression(classic.is_some());
            if let Some(level) = classic {
                b = b.noise_suppression_config(NsConfig { target_level: level, ..NsConfig::default() });
            }
            Some(b.build().map_err(|e| format!("audio processing: {e:?}"))?)
        } else {
            None
        };
        self.rnnoise = (suppression == Suppression::Ai).then(DenoiseState::new);
        Ok(())
    }

    /// Far-end reference: what the speakers are about to play.
    pub fn render(&mut self, frame: &[f32]) {
        if self.echo_cancel
            && let Some(p) = &mut self.pipeline
        {
            let _ = p.handle_render_frame(frame);
        }
    }

    /// Cleans one 10 ms microphone frame in place.
    pub fn capture(&mut self, frame: &mut [f32]) {
        debug_assert_eq!(frame.len(), HALF);
        if let Some(p) = &mut self.pipeline
            && let Ok(true) = p.process_capture_frame(frame, &mut self.scratch)
        {
            frame.copy_from_slice(&self.scratch);
        }
        if let Some(rnn) = &mut self.rnnoise {
            // RNNoise expects 16-bit sample scale.
            for (s, x) in self.scratch.iter_mut().zip(frame.iter()) {
                *s = x * 32_768.0;
            }
            let input = self.scratch;
            rnn.process_frame(&mut self.scratch, &input);
            for (x, s) in frame.iter_mut().zip(self.scratch.iter()) {
                *x = s / 32_768.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rms_db;

    fn noise(seed: &mut u32) -> f32 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (*seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
    }

    #[test]
    fn every_mode_keeps_frames_finite() {
        for mode in [Suppression::Off, Suppression::Classic(1), Suppression::Classic(3), Suppression::Ai] {
            let mut p = Processor::new(true, mode).unwrap();
            let mut seed = 1;
            for _ in 0..100 {
                let render: Vec<f32> = (0..HALF).map(|_| noise(&mut seed) * 0.2).collect();
                p.render(&render);
                let mut mic: Vec<f32> = (0..HALF).map(|_| noise(&mut seed) * 0.1).collect();
                p.capture(&mut mic);
                assert!(mic.iter().all(|s| s.is_finite()), "{mode:?}");
            }
        }
    }

    #[test]
    fn echo_is_reduced() {
        let mut p = Processor::new(true, Suppression::Off).unwrap();
        let mut seed = 7;
        let mut history: Vec<Vec<f32>> = Vec::new();
        let (mut before, mut after) = (0.0, 0.0);
        for i in 0..600 {
            let render: Vec<f32> = (0..HALF).map(|_| noise(&mut seed) * 0.3).collect();
            p.render(&render);
            history.push(render);
            // The microphone hears the speakers 40 ms later, quieter.
            let mut mic: Vec<f32> = match history.len().checked_sub(5) {
                Some(j) => history[j].iter().map(|s| s * 0.5).collect(),
                None => vec![0.0; HALF],
            };
            let level_in = rms_db(&mic);
            p.capture(&mut mic);
            if i >= 500 {
                before += level_in;
                after += rms_db(&mic);
            }
        }
        let reduction = (before - after) / 100.0;
        assert!(reduction > 10.0, "echo reduced by only {reduction:.1} dB");
    }
}
