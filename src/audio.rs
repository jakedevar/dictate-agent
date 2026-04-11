use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SampleRate, StreamConfig};
use std::sync::{Arc, Mutex};
use tracing::{error, info, warn};

pub struct AudioCapture {
    stream: Option<cpal::Stream>,
    buffer: Arc<Mutex<Vec<f32>>>,
    is_recording: bool,
}

impl AudioCapture {
    pub fn new() -> Result<Self> {
        Ok(Self {
            stream: None,
            buffer: Arc::new(Mutex::new(Vec::new())),
            is_recording: false,
        })
    }

    pub fn start(&mut self) -> Result<()> {
        if self.is_recording {
            anyhow::bail!("Already recording");
        }

        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| anyhow::anyhow!("No input device found"))?;

        info!("Using input device: {}", device.name().unwrap_or_default());

        // Request 16kHz mono — PipeWire/PulseAudio will resample if needed
        let desired_config = StreamConfig {
            channels: 1,
            sample_rate: SampleRate(16000),
            buffer_size: cpal::BufferSize::Default,
        };

        let buffer = self.buffer.clone();
        buffer.lock().unwrap().clear();

        // Check what sample formats the device supports
        let supported_configs: Vec<_> = device
            .supported_input_configs()
            .map(|cfgs| cfgs.collect())
            .unwrap_or_default();

        // Prefer f32, fall back to i16
        let uses_f32 = supported_configs.iter().any(|c| c.sample_format() == SampleFormat::F32);

        let stream = if uses_f32 {
            let buf = buffer.clone();
            device.build_input_stream(
                &desired_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    buf.lock().unwrap().extend_from_slice(data);
                },
                |err| error!("Audio stream error: {}", err),
                None,
            )?
        } else {
            // Fallback: build i16 stream and convert to f32
            let buf = buffer.clone();
            warn!("Device does not support f32 input, using i16 with conversion");
            device.build_input_stream(
                &desired_config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let converted: Vec<f32> = data.iter().map(|&s| s as f32 / 32768.0).collect();
                    buf.lock().unwrap().extend_from_slice(&converted);
                },
                |err| error!("Audio stream error: {}", err),
                None,
            )?
        };

        stream.play()?;
        self.stream = Some(stream);
        self.is_recording = true;
        info!("Recording started (16kHz mono f32)");
        Ok(())
    }

    /// Stops recording and returns the audio buffer as f32 samples at 16kHz.
    /// Adds a trailing 500ms capture delay (matching Python audio.py:60).
    pub async fn stop(&mut self) -> Option<Vec<f32>> {
        if !self.is_recording {
            return None;
        }

        // Trailing audio capture delay (matches audio.py:60 — time.sleep(0.5))
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Drop the stream to stop recording
        self.stream.take();
        self.is_recording = false;

        let samples = std::mem::take(&mut *self.buffer.lock().unwrap());
        let duration = Self::duration_secs(&samples);
        info!(
            "Recording stopped: {:.1}s ({} samples)",
            duration,
            samples.len()
        );

        if samples.is_empty() {
            None
        } else {
            Some(samples)
        }
    }

    /// Cancel recording without returning audio data
    pub fn cancel(&mut self) {
        self.stream.take();
        self.is_recording = false;
        self.buffer.lock().unwrap().clear();
    }

    pub fn is_recording(&self) -> bool {
        self.is_recording
    }

    /// Audio duration in seconds from a sample buffer (16kHz)
    pub fn duration_secs(samples: &[f32]) -> f64 {
        samples.len() as f64 / 16000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_duration_secs() {
        assert!((AudioCapture::duration_secs(&vec![0.0; 16000]) - 1.0).abs() < f64::EPSILON);
        assert!((AudioCapture::duration_secs(&vec![0.0; 32000]) - 2.0).abs() < f64::EPSILON);
        assert!((AudioCapture::duration_secs(&[]) - 0.0).abs() < f64::EPSILON);
        assert!((AudioCapture::duration_secs(&vec![0.0; 8000]) - 0.5).abs() < f64::EPSILON);
    }
}
