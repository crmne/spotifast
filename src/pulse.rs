//! PulseAudio output with its server/device backlog exposed to the playback clock.
//! Uses the same S16 stream as librespot's backend; no extra audio conversion.
use std::{sync::Arc, time::Duration};

use librespot_playback::{
    NUM_CHANNELS, SAMPLE_RATE,
    audio_backend::{Sink, SinkError, SinkResult},
    convert::Converter,
    decoder::AudioPacket,
};

pub(crate) struct PulseSink {
    stream: Option<libpulse_simple_binding::Simple>,
    device: Option<String>,
    control: Arc<crate::sink::AudioControl>,
}

impl PulseSink {
    pub(crate) fn new(device: Option<String>, control: Arc<crate::sink::AudioControl>) -> Self {
        Self {
            stream: None,
            device,
            control,
        }
    }
}

impl PulseSink {
    fn open(&mut self) -> SinkResult<()> {
        if self.stream.is_none() {
            let spec = libpulse_binding::sample::Spec {
                format: libpulse_binding::sample::Format::S16NE,
                channels: NUM_CHANNELS,
                rate: SAMPLE_RATE,
            };
            self.stream = Some(
                libpulse_simple_binding::Simple::new(
                    None,
                    "Spotifast",
                    libpulse_binding::stream::Direction::Playback,
                    self.device.as_deref(),
                    "Music",
                    &spec,
                    None,
                    None,
                )
                .map_err(|e| SinkError::ConnectionRefused(format!("{e}")))?,
            );
        }
        Ok(())
    }
}

impl Drop for PulseSink {
    fn drop(&mut self) {
        self.control.clock.freeze();
    }
}

impl Sink for PulseSink {
    fn start(&mut self) -> SinkResult<()> {
        // librespot's start-error path can terminate its process. Defer an
        // unavailable output to write, as the native backend does.
        if let Err(error) = self.open() {
            log::debug!("PulseAudio not open at start: {error}");
        }
        Ok(())
    }

    fn stop(&mut self) -> SinkResult<()> {
        if let Some(stream) = self.stream.take() {
            stream
                .drain()
                .map_err(|e| SinkError::OnWrite(format!("{e}")))?;
            self.control.clock.drained();
        }
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        self.open()?;
        if self.control.take_reset()
            && let Some(stream) = &self.stream
        {
            stream
                .flush()
                .map_err(|e| SinkError::OnWrite(format!("{e}")))?;
        }
        let samples = packet
            .samples()
            .map_err(|e| SinkError::OnWrite(format!("{e}")))?;
        let samples = converter.f64_to_s16(samples);
        // Convert native-endian samples without unsafe casts or alignment assumptions.
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_ne_bytes()).collect();
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| SinkError::NotConnected("PulseAudio is closed".into()))?;
        stream
            .write(&bytes)
            .map_err(|e| SinkError::OnWrite(format!("{e}")))?;
        let latency = stream
            .get_latency()
            .map_err(|e| SinkError::OnWrite(format!("{e}")))?;
        self.control
            .clock
            .submit_latency(Duration::from_micros(latency.0));
        Ok(())
    }
}
