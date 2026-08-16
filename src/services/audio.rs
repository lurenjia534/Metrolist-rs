use std::{
    collections::{HashSet, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use http_client::HttpClient;
use reqwest_client::ReqwestClient;
use rodio::{
    ChannelCount, Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, SampleRate, Source,
    cpal::{self, traits::DeviceTrait as _, traits::HostTrait as _},
    source::SeekError,
};

use crate::services::equalizer::BiquadCoefficients;
use crate::services::playback::{PlaybackReadFailure, RANGE_CHUNK_SIZE};
use crate::services::{
    AudioCache, AudioDeviceOperation, AudioDeviceSnapshot, AudioOutputDevice, AudioPlayer,
    DownloadedAudioStore, HttpRangeMediaSource, PlaybackSnapshot, PlaybackSource,
    PlaybackSourceAccess, PlaybackState,
};
use crate::{
    AppError, AppSettings, EQUALIZER_FREQUENCIES_HZ, EqualizerSettings, LoudnessLevel,
    ParametricEqualizerBand, PlaybackParameters, Result, services::build_http_client,
};

pub const DEFAULT_AUDIO_CACHE_BYTES: u64 = 512 * 1024 * 1024;
const MIN_NORMALIZATION_GAIN_MB: i32 = -1_500;
const MAX_NORMALIZATION_GAIN_MB: i32 = 300;
const TIME_STRETCH_CHUNK_FRAMES: usize = 4_096;
const SILENCE_THRESHOLD: f32 = 256.0 / 32_768.0;
const SILENCE_COMPRESSION_START_MS: u64 = 150;
const INSTANT_SILENCE_SKIP_START_MS: u64 = 2_000;
const SILENCE_RETENTION_INTERVAL: u64 = 5;

#[derive(Debug, Clone, Copy, Default)]
struct SilenceSkipping {
    enabled: bool,
    instant: bool,
}

#[derive(Debug, Clone, Copy)]
struct AudioNormalization {
    enabled: bool,
    level: LoudnessLevel,
}

impl Default for AudioNormalization {
    fn default() -> Self {
        Self {
            enabled: true,
            level: LoudnessLevel::Balanced,
        }
    }
}

fn normalization_gain_mb(
    normalization: AudioNormalization,
    measured_lufs_mb: Option<i32>,
) -> Option<i32> {
    normalization.enabled.then_some(())?;
    let measured_lufs_mb = measured_lufs_mb?;
    Some(
        normalization
            .level
            .target_lufs_mb()
            .saturating_sub(measured_lufs_mb)
            .clamp(MIN_NORMALIZATION_GAIN_MB, MAX_NORMALIZATION_GAIN_MB),
    )
}

fn gain_mb_to_linear(gain_mb: i32) -> f32 {
    10.0_f32.powf(gain_mb as f32 / 2_000.0)
}

struct ClampedGain<S> {
    inner: S,
    linear_gain: f32,
}

impl<S> ClampedGain<S> {
    fn new(inner: S, gain_mb: Option<i32>) -> Self {
        Self {
            inner,
            linear_gain: gain_mb.map_or(1.0, gain_mb_to_linear),
        }
    }
}

impl<S> Iterator for ClampedGain<S>
where
    S: Source,
{
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner
            .next()
            .map(|sample| (sample * self.linear_gain).clamp(-1.0, 1.0))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<S> Source for ClampedGain<S>
where
    S: Source,
{
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }

    fn channels(&self) -> ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(&mut self, position: Duration) -> std::result::Result<(), SeekError> {
        self.inner.try_seek(position)
    }
}

struct SilenceSkippingSource<S> {
    inner: S,
    settings: SilenceSkipping,
    channels: usize,
    sample_rate: u32,
    output: VecDeque<f32>,
    consecutive_silent_frames: u64,
    skipped_frames: Arc<AtomicU64>,
}

impl<S> SilenceSkippingSource<S>
where
    S: Source<Item = f32>,
{
    fn new(inner: S, settings: SilenceSkipping, skipped_frames: Arc<AtomicU64>) -> Self {
        Self {
            channels: usize::from(inner.channels().get()),
            sample_rate: inner.sample_rate().get(),
            inner,
            settings,
            output: VecDeque::new(),
            consecutive_silent_frames: 0,
            skipped_frames,
        }
    }

    fn duration_frames(&self, milliseconds: u64) -> u64 {
        u64::from(self.sample_rate).saturating_mul(milliseconds) / 1_000
    }

    fn fill_output(&mut self) {
        while self.output.is_empty() {
            let mut frame = Vec::with_capacity(self.channels);
            for _ in 0..self.channels {
                let Some(sample) = self.inner.next() else {
                    self.output.extend(frame);
                    return;
                };
                frame.push(sample);
            }

            if !self.settings.enabled {
                self.output.extend(frame);
                return;
            }

            let silent = frame.iter().all(|sample| sample.abs() < SILENCE_THRESHOLD);
            if !silent {
                self.consecutive_silent_frames = 0;
                self.output.extend(frame);
                return;
            }

            self.consecutive_silent_frames = self.consecutive_silent_frames.saturating_add(1);
            let compression_start = self.duration_frames(SILENCE_COMPRESSION_START_MS);
            let instant_start = self.duration_frames(INSTANT_SILENCE_SKIP_START_MS);
            let instant_skip =
                self.settings.instant && self.consecutive_silent_frames > instant_start;
            let compressed_skip = self.consecutive_silent_frames > compression_start
                && !(self.consecutive_silent_frames - compression_start)
                    .is_multiple_of(SILENCE_RETENTION_INTERVAL);
            if instant_skip || compressed_skip {
                self.skipped_frames.fetch_add(1, Ordering::Relaxed);
                continue;
            }

            self.output.extend(frame);
            return;
        }
    }
}

impl<S> Iterator for SilenceSkippingSource<S>
where
    S: Source<Item = f32>,
{
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        self.fill_output();
        self.output.pop_front()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.output.len(), self.inner.size_hint().1)
    }
}

impl<S> Source for SilenceSkippingSource<S>
where
    S: Source<Item = f32>,
{
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(&mut self, position: Duration) -> std::result::Result<(), SeekError> {
        self.inner.try_seek(position)?;
        self.output.clear();
        self.consecutive_silent_frames = 0;
        self.skipped_frames.store(0, Ordering::Relaxed);
        Ok(())
    }
}

#[derive(Clone, Copy, Default)]
struct BiquadState {
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

struct BiquadFilter {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    states: Vec<BiquadState>,
}

impl BiquadFilter {
    fn peaking(
        sample_rate: u32,
        channels: usize,
        frequency_hz: f64,
        gain_db: f64,
        q: f64,
    ) -> Option<Self> {
        let coefficients = BiquadCoefficients::peaking(sample_rate, frequency_hz, gain_db, q)?;
        Self::from_coefficients(channels, coefficients)
    }

    fn from_coefficients(channels: usize, coefficients: BiquadCoefficients) -> Option<Self> {
        (channels > 0).then(|| Self {
            b0: coefficients.b0,
            b1: coefficients.b1,
            b2: coefficients.b2,
            a1: coefficients.a1,
            a2: coefficients.a2,
            states: vec![BiquadState::default(); channels],
        })
    }

    fn from_parametric_band(
        sample_rate: u32,
        channels: usize,
        band: ParametricEqualizerBand,
    ) -> Option<Self> {
        Self::from_coefficients(channels, BiquadCoefficients::from_band(sample_rate, band)?)
    }

    fn process(&mut self, sample: f64, channel: usize) -> f64 {
        let state = &mut self.states[channel];
        let output = self.b0 * sample + self.b1 * state.x1 + self.b2 * state.x2
            - self.a1 * state.y1
            - self.a2 * state.y2;
        state.x2 = state.x1;
        state.x1 = sample;
        state.y2 = state.y1;
        state.y1 = output;
        output
    }

    fn reset(&mut self) {
        self.states.fill(BiquadState::default());
    }
}

struct EqualizerSource<S> {
    inner: S,
    filters: Vec<BiquadFilter>,
    preamp_gain: f64,
    channels: usize,
    channel: usize,
}

impl<S> EqualizerSource<S>
where
    S: Source,
{
    fn new(inner: S, settings: EqualizerSettings) -> Self {
        let channels = usize::from(inner.channels().get());
        let sample_rate = inner.sample_rate().get();
        let (filters, preamp_mb) = if !settings.enabled {
            (Vec::new(), 0)
        } else if let Some(profile) = &settings.active_profile {
            (
                profile
                    .equalizer
                    .bands
                    .iter()
                    .copied()
                    .filter_map(|band| {
                        BiquadFilter::from_parametric_band(sample_rate, channels, band)
                    })
                    .collect(),
                profile.equalizer.preamp_mb,
            )
        } else {
            (
                EQUALIZER_FREQUENCIES_HZ
                    .into_iter()
                    .zip(settings.gains_mb)
                    .filter_map(|(frequency, gain)| {
                        BiquadFilter::peaking(
                            sample_rate,
                            channels,
                            f64::from(frequency),
                            f64::from(gain) / 100.0,
                            2.0_f64.sqrt(),
                        )
                    })
                    .collect(),
                settings.headroom_mb(),
            )
        };
        let preamp_gain = 10.0_f64.powf(f64::from(preamp_mb) / 2_000.0);
        Self {
            inner,
            filters,
            preamp_gain,
            channels,
            channel: 0,
        }
    }

    fn reset(&mut self) {
        self.channel = 0;
        for filter in &mut self.filters {
            filter.reset();
        }
    }
}

impl<S> Iterator for EqualizerSource<S>
where
    S: Source,
{
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        let input = f64::from(self.inner.next()?);
        let mut output = input;
        for filter in &mut self.filters {
            output = filter.process(output, self.channel);
        }
        output *= self.preamp_gain;
        self.channel = (self.channel + 1) % self.channels;
        Some(output.clamp(-1.0, 1.0) as f32)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<S> Source for EqualizerSource<S>
where
    S: Source,
{
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }

    fn channels(&self) -> ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(&mut self, position: Duration) -> std::result::Result<(), SeekError> {
        self.inner.try_seek(position)?;
        self.reset();
        Ok(())
    }
}

struct TimeStretchSource<S> {
    inner: S,
    stages: Vec<wsola::TimeStretch>,
    output: VecDeque<f32>,
    channels: usize,
    sample_rate: SampleRate,
    total_duration: Option<Duration>,
    tempo_ratio: f64,
    position_ns: Arc<AtomicU64>,
    skipped_frames: Arc<AtomicU64>,
    media_sample_rate: u32,
    position_anchor_ns: u64,
    output_frames: u64,
    output_channel: usize,
    inner_exhausted: bool,
    flushed: bool,
}

impl<S> TimeStretchSource<S>
where
    S: Source<Item = f32>,
{
    fn new(
        inner: S,
        parameters: PlaybackParameters,
        position_ns: Arc<AtomicU64>,
        skipped_frames: Arc<AtomicU64>,
    ) -> Result<Self> {
        let parameters = parameters.validate()?;
        let channels = usize::from(inner.channels().get());
        let raw_sample_rate = inner.sample_rate().get();
        let adjusted_sample_rate =
            ((f64::from(raw_sample_rate) * f64::from(parameters.pitch_ratio())).round() as u32)
                .max(1);
        let sample_rate = SampleRate::new(adjusted_sample_rate)
            .ok_or_else(|| AppError::Playback("time-stretch sample rate is invalid".into()))?;
        let channel_count = u16::try_from(channels)
            .map_err(|_| AppError::Playback("too many audio channels".into()))?;
        let mut stages = Vec::new();
        for ratio in time_stretch_stage_ratios(parameters.stretch_ratio()) {
            let mut stage =
                wsola::TimeStretch::new(adjusted_sample_rate, channel_count).map_err(|error| {
                    AppError::Playback(format!("time-stretch processor failed: {error}"))
                })?;
            stage.set_tempo(ratio);
            stages.push(stage);
        }
        let total_duration = inner.total_duration();
        Ok(Self {
            inner,
            stages,
            output: VecDeque::new(),
            channels,
            sample_rate,
            total_duration,
            tempo_ratio: f64::from(parameters.tempo_ratio()),
            position_ns,
            skipped_frames,
            media_sample_rate: raw_sample_rate,
            position_anchor_ns: 0,
            output_frames: 0,
            output_channel: 0,
            inner_exhausted: false,
            flushed: false,
        })
    }

    fn process_chunk(&mut self, mut chunk: Vec<f32>) {
        for stage in &mut self.stages {
            stage.push(&chunk);
            chunk = stage.pull(usize::MAX);
        }
        self.output.extend(chunk);
    }

    fn fill_output(&mut self) {
        while self.output.is_empty() && !self.flushed {
            if !self.inner_exhausted {
                let mut chunk = Vec::with_capacity(TIME_STRETCH_CHUNK_FRAMES * self.channels);
                for _ in 0..TIME_STRETCH_CHUNK_FRAMES * self.channels {
                    let Some(sample) = self.inner.next() else {
                        self.inner_exhausted = true;
                        break;
                    };
                    chunk.push(sample);
                }
                if !chunk.is_empty() {
                    self.process_chunk(chunk);
                    continue;
                }
            }

            let mut tail = Vec::new();
            for stage in &mut self.stages {
                if !tail.is_empty() {
                    stage.push(&tail);
                }
                tail = stage.pull(usize::MAX);
                tail.extend(stage.flush());
            }
            self.output.extend(tail);
            self.flushed = true;
        }
    }

    fn reset_processing(&mut self, position: Duration) {
        for stage in &mut self.stages {
            stage.reset();
        }
        self.output.clear();
        self.output_frames = 0;
        self.output_channel = 0;
        self.inner_exhausted = false;
        self.flushed = false;
        self.position_anchor_ns = duration_ns(position);
        self.skipped_frames.store(0, Ordering::Relaxed);
        self.position_ns
            .store(self.position_anchor_ns, Ordering::Relaxed);
    }

    fn publish_position(&self) {
        let elapsed_ns = (self.output_frames as f64 * self.tempo_ratio * 1_000_000_000.0
            / f64::from(self.sample_rate.get()))
        .round() as u64;
        let skipped_ns = (u128::from(self.skipped_frames.load(Ordering::Relaxed)) * 1_000_000_000
            / u128::from(self.media_sample_rate))
        .min(u128::from(u64::MAX)) as u64;
        let position_ns = self
            .position_anchor_ns
            .saturating_add(elapsed_ns)
            .saturating_add(skipped_ns);
        let position_ns = self.total_duration.map_or(position_ns, |duration| {
            position_ns.min(duration_ns(duration))
        });
        self.position_ns.store(position_ns, Ordering::Relaxed);
    }
}

fn time_stretch_stage_ratios(ratio: f32) -> Vec<f32> {
    if (ratio - 1.0).abs() < 0.000_001 {
        Vec::new()
    } else if ratio < wsola::MIN_TEMPO {
        vec![wsola::MIN_TEMPO, ratio / wsola::MIN_TEMPO]
    } else {
        vec![ratio]
    }
}

fn duration_ns(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

impl<S> Iterator for TimeStretchSource<S>
where
    S: Source<Item = f32>,
{
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        self.fill_output();
        let Some(sample) = self.output.pop_front() else {
            self.publish_position();
            return None;
        };
        self.output_channel += 1;
        if self.output_channel == self.channels {
            self.output_channel = 0;
            self.output_frames = self.output_frames.saturating_add(1);
            self.publish_position();
        }
        Some(sample.clamp(-1.0, 1.0))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.output.len(), None)
    }
}

impl<S> Source for TimeStretchSource<S>
where
    S: Source<Item = f32>,
{
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.total_duration
    }

    fn try_seek(&mut self, position: Duration) -> std::result::Result<(), SeekError> {
        self.inner.try_seek(position)?;
        self.reset_processing(position);
        Ok(())
    }
}

struct PreparedPlayback {
    source: Box<dyn Source<Item = f32> + Send>,
    source_for_reload: PlaybackSource,
    source_failure: Arc<PlaybackReadFailure>,
    duration: Option<Duration>,
    normalization_gain_mb: Option<i32>,
    playback_position_ns: Arc<AtomicU64>,
}

struct RodioAudioPlayer {
    _device: MixerDeviceSink,
    player: Player,
    fading_player: Option<Player>,
    crossfade_duration: Option<Duration>,
    client: Arc<dyn HttpClient>,
    disk_cache: Option<Arc<AudioCache>>,
    download_store: Option<Arc<DownloadedAudioStore>>,
    loaded: bool,
    loaded_source: Option<PlaybackSource>,
    source_failure: Option<Arc<PlaybackReadFailure>>,
    duration: Option<Duration>,
    state: PlaybackState,
    normalization: AudioNormalization,
    silence_skipping: SilenceSkipping,
    normalization_gain_mb: Option<i32>,
    equalizer: EqualizerSettings,
    playback_parameters: PlaybackParameters,
    playback_position_ns: Arc<AtomicU64>,
    selected_output_device_id: String,
    volume: f32,
    volume_multiplier: f32,
}

impl RodioAudioPlayer {
    fn new(cache_limit: u64) -> Result<Self> {
        let disk_cache = match AudioCache::for_current_user(cache_limit) {
            Ok(cache) => Some(Arc::new(cache)),
            Err(error) => {
                tracing::warn!(%error, "audio disk cache is unavailable; continuing without it");
                None
            }
        };

        let download_store = match DownloadedAudioStore::for_current_user() {
            Ok(store) => Some(Arc::new(store)),
            Err(error) => {
                tracing::warn!(%error, "downloaded audio store is unavailable");
                None
            }
        };

        Self::with_dependencies(
            Arc::new(ReqwestClient::new()),
            disk_cache,
            download_store,
            None,
            AudioNormalization::default(),
            SilenceSkipping::default(),
            EqualizerSettings::default(),
            PlaybackParameters::default(),
        )
    }

    fn with_dependencies(
        client: Arc<dyn HttpClient>,
        disk_cache: Option<Arc<AudioCache>>,
        download_store: Option<Arc<DownloadedAudioStore>>,
        requested_device_id: Option<&str>,
        normalization: AudioNormalization,
        silence_skipping: SilenceSkipping,
        equalizer: EqualizerSettings,
        playback_parameters: PlaybackParameters,
    ) -> Result<Self> {
        let (mut device, selected_output_device_id) = open_output_device(requested_device_id)?;
        device.log_on_drop(false);
        let player = Player::connect_new(device.mixer());
        player.set_volume(0.8);

        Ok(Self {
            _device: device,
            player,
            fading_player: None,
            crossfade_duration: None,
            client,
            disk_cache,
            download_store,
            loaded: false,
            loaded_source: None,
            source_failure: None,
            duration: None,
            state: PlaybackState::Idle,
            normalization,
            silence_skipping,
            normalization_gain_mb: None,
            equalizer,
            playback_parameters: playback_parameters.validate()?,
            playback_position_ns: Arc::new(AtomicU64::new(0)),
            selected_output_device_id,
            volume: 0.8,
            volume_multiplier: 1.0,
        })
    }

    fn prepare_playback(&self, source: PlaybackSource) -> Result<PreparedPlayback> {
        let source_for_reload = source.clone();
        if source.access == PlaybackSourceAccess::CacheOnly {
            let content_length = source.content_length.ok_or_else(|| {
                AppError::Playback("cache-only playback requires a known content length".into())
            })?;
            let cache_key = source.disk_cache_key().ok_or_else(|| {
                AppError::Playback("cache-only playback requires a stable cache key".into())
            })?;
            let downloaded = self.download_store.as_ref().is_some_and(|store| {
                store
                    .contains_complete_resource(&cache_key, content_length, RANGE_CHUNK_SIZE)
                    .unwrap_or(false)
            });
            let cached = self.disk_cache.as_ref().is_some_and(|cache| {
                cache
                    .contains_complete_resource(&cache_key, content_length, RANGE_CHUNK_SIZE)
                    .unwrap_or(false)
            });
            if !downloaded && !cached {
                return Err(AppError::Playback(
                    "offline audio is incomplete; a fresh playback source is required".into(),
                ));
            }
        }

        let content_length = source.content_length;
        let normalization_gain_mb =
            normalization_gain_mb(self.normalization, source.loudness_lufs_mb);
        let mime_type = source
            .mime_type
            .split_once(';')
            .map_or(source.mime_type.as_str(), |(mime_type, _)| mime_type)
            .to_owned();
        let source_failure = Arc::new(PlaybackReadFailure::default());
        let range_source = HttpRangeMediaSource::new(self.client.clone(), source)
            .with_disk_cache(self.disk_cache.clone())
            .with_download_store(self.download_store.clone())
            .with_failure_reporter(source_failure.clone());
        let mut decoder = Decoder::builder()
            .with_data(range_source)
            .with_hint("m4a")
            .with_mime_type(&mime_type);
        if let Some(content_length) = content_length {
            decoder = decoder.with_byte_len(content_length).with_seekable(true);
        }
        let decoder = decoder
            .build()
            .map_err(|error| AppError::Playback(format!("audio decoder failed: {error}")))?;

        let duration = decoder.total_duration();
        let playback_position_ns = Arc::new(AtomicU64::new(0));
        let skipped_frames = Arc::new(AtomicU64::new(0));
        let processed = TimeStretchSource::new(
            EqualizerSource::new(
                ClampedGain::new(
                    SilenceSkippingSource::new(
                        decoder,
                        self.silence_skipping,
                        skipped_frames.clone(),
                    ),
                    normalization_gain_mb,
                ),
                self.equalizer.clone(),
            ),
            self.playback_parameters,
            playback_position_ns.clone(),
            skipped_frames,
        )?;
        Ok(PreparedPlayback {
            source: Box::new(processed),
            source_for_reload,
            source_failure,
            duration,
            normalization_gain_mb,
            playback_position_ns,
        })
    }

    fn install_prepared(&mut self, prepared: PreparedPlayback) {
        self.player.append(prepared.source);
        self.player.pause();
        self.loaded = true;
        self.loaded_source = Some(prepared.source_for_reload);
        self.source_failure = Some(prepared.source_failure);
        self.duration = prepared.duration;
        self.normalization_gain_mb = prepared.normalization_gain_mb;
        self.playback_position_ns = prepared.playback_position_ns;
        self.state = PlaybackState::Paused;
    }

    fn apply_output_volume(&self) {
        let output_volume = (self.volume * self.volume_multiplier).clamp(0.0, 1.0);
        let Some(fading_player) = &self.fading_player else {
            self.player.set_volume(output_volume);
            return;
        };
        let Some(duration) = self
            .crossfade_duration
            .filter(|duration| !duration.is_zero())
        else {
            fading_player.stop();
            self.player.set_volume(output_volume);
            return;
        };
        let position = Duration::from_nanos(self.playback_position_ns.load(Ordering::Relaxed));
        let progress = (position.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0);
        let remaining = 1.0 - progress;
        let fade_out = remaining * remaining;
        let fade_in = 1.0 - fade_out;
        self.player.set_volume(output_volume * fade_in);
        fading_player.set_volume(output_volume * fade_out);
        if progress >= 1.0 {
            fading_player.stop();
        }
    }

    fn rebuild_processing(
        &mut self,
        equalizer: EqualizerSettings,
        playback_parameters: PlaybackParameters,
    ) -> Result<()> {
        equalizer.validate()?;
        let playback_parameters = playback_parameters.validate()?;
        if equalizer == self.equalizer && playback_parameters == self.playback_parameters {
            return Ok(());
        }
        if !self.loaded {
            self.equalizer = equalizer;
            self.playback_parameters = playback_parameters;
            return Ok(());
        }

        let source = self.loaded_source.clone().ok_or_else(|| {
            AppError::Playback("loaded audio is missing its reload metadata".into())
        })?;
        let before = self.snapshot();
        let was_playing = self.state == PlaybackState::Playing;
        if was_playing {
            self.player.pause();
        }

        let old_player =
            std::mem::replace(&mut self.player, Player::connect_new(self._device.mixer()));
        self.apply_output_volume();
        let old_loaded = self.loaded;
        let old_loaded_source = self.loaded_source.clone();
        let old_source_failure = self.source_failure.clone();
        let old_duration = self.duration;
        let old_state = self.state;
        let old_normalization_gain_mb = self.normalization_gain_mb;
        let old_position = self.playback_position_ns.clone();
        let old_equalizer = self.equalizer.clone();
        let old_parameters = self.playback_parameters;
        self.equalizer = equalizer;
        self.playback_parameters = playback_parameters;

        let replacement = (|| {
            self.load(source)?;
            if !before.position.is_zero() {
                let resume_position = self
                    .duration
                    .map_or(before.position, |duration| before.position.min(duration));
                self.seek(resume_position)?;
            }
            match before.state {
                PlaybackState::Playing => self.play()?,
                PlaybackState::Ended => self.state = PlaybackState::Ended,
                PlaybackState::Paused | PlaybackState::Loading => {}
                PlaybackState::Idle | PlaybackState::Failed => {}
            }
            Ok(())
        })();

        match replacement {
            Ok(()) => {
                old_player.stop();
                Ok(())
            }
            Err(error) => {
                self.player.stop();
                self.player = old_player;
                self.loaded = old_loaded;
                self.loaded_source = old_loaded_source;
                self.source_failure = old_source_failure;
                self.duration = old_duration;
                self.state = old_state;
                self.normalization_gain_mb = old_normalization_gain_mb;
                self.playback_position_ns = old_position;
                self.equalizer = old_equalizer;
                self.playback_parameters = old_parameters;
                if was_playing {
                    self.player.play();
                }
                Err(error)
            }
        }
    }
}

struct SystemOutputDevice {
    device: cpal::Device,
    info: AudioOutputDevice,
    raw_name: String,
}

fn system_output_devices() -> Result<Vec<SystemOutputDevice>> {
    let host = cpal::default_host();
    let default_device = host.default_output_device();
    let default_id = default_device.as_ref().and_then(|device| device.id().ok());
    let devices = host
        .output_devices()
        .map_err(|error| AppError::Playback(format!("could not list audio outputs: {error}")))?;
    let mut outputs = Vec::new();
    for device in default_device.into_iter().chain(devices) {
        let Ok(id) = device.id() else {
            continue;
        };
        if outputs
            .iter()
            .any(|output: &SystemOutputDevice| output.info.id == id.to_string())
        {
            continue;
        }
        let Ok(description) = device.description() else {
            continue;
        };
        if description.driver().is_some_and(|driver| driver == "null") {
            continue;
        }
        // Some ALSA profiles are enumerable but cannot provide any output
        // configuration. Showing them would offer a device that can never be
        // opened, so keep the settings list limited to usable endpoints.
        if device.default_output_config().is_err() {
            continue;
        }
        let id = id.to_string();
        let raw_name = description.name().to_owned();
        let is_default = default_id
            .as_ref()
            .is_some_and(|default_id| default_id.to_string() == id);
        outputs.push(SystemOutputDevice {
            info: AudioOutputDevice {
                name: output_device_display_name(&id, &raw_name, is_default),
                id,
                is_default,
            },
            device,
            raw_name,
        });
    }

    // ALSA exposes low-level `hw` and `plughw` aliases in addition to the
    // user-facing profile for the same endpoint. Keep a raw alias only when
    // no higher-level profile with the same hardware description exists.
    let high_level_names = outputs
        .iter()
        .filter(|output| !is_low_level_output_alias(&output.info.id))
        .map(|output| output.raw_name.clone())
        .collect::<HashSet<_>>();
    outputs.retain(|output| {
        !is_low_level_output_alias(&output.info.id) || !high_level_names.contains(&output.raw_name)
    });
    outputs.sort_by(|left, right| {
        right
            .info
            .is_default
            .cmp(&left.info.is_default)
            .then_with(|| {
                output_device_preference(&left.info.id)
                    .cmp(&output_device_preference(&right.info.id))
            })
            .then_with(|| left.info.name.cmp(&right.info.name))
            .then_with(|| left.info.id.cmp(&right.info.id))
    });
    if outputs.is_empty() {
        return Err(AppError::Playback(
            "no usable audio output device was found".into(),
        ));
    }
    Ok(outputs)
}

fn is_low_level_output_alias(id: &str) -> bool {
    cfg!(target_os = "linux") && (id.starts_with("alsa:hw:") || id.starts_with("alsa:plughw:"))
}

fn output_device_preference(id: &str) -> u8 {
    match id {
        "alsa:pipewire" => 0,
        "alsa:pulse" => 1,
        id if id.starts_with("alsa:default:") => 2,
        id if id.starts_with("alsa:sysdefault:") => 3,
        id if id.starts_with("alsa:front:") => 4,
        _ => 5,
    }
}

fn output_device_display_name(id: &str, raw_name: &str, is_default: bool) -> String {
    if is_default && id == "alsa:default" {
        return "Default audio output".into();
    }

    let Some(profile) = id
        .strip_prefix("alsa:")
        .and_then(|id| id.split([':', ',']).next())
    else {
        return raw_name.into();
    };
    let profile = match profile {
        "default" => "Default profile",
        "front" => "Front stereo",
        "iec958" => "Digital S/PDIF",
        "surround21" => "2.1 surround",
        "surround40" => "4.0 surround",
        "surround41" => "4.1 surround",
        "surround50" => "5.0 surround",
        "surround51" => "5.1 surround",
        "surround71" => "7.1 surround",
        "sysdefault" => "System profile",
        "hw" => "Direct hardware",
        "plughw" => "Converted hardware",
        _ => return raw_name.into(),
    };
    format!("{raw_name} · {profile}")
}

fn open_output_device(requested_device_id: Option<&str>) -> Result<(MixerDeviceSink, String)> {
    let mut outputs = system_output_devices()?;
    if let Some(requested_device_id) = requested_device_id {
        let index = outputs
            .iter()
            .position(|output| output.info.id == requested_device_id)
            .ok_or_else(|| {
                AppError::Playback("selected audio output is no longer available".into())
            })?;
        let output = outputs.swap_remove(index);
        let sink = DeviceSinkBuilder::from_device(output.device)
            .and_then(|builder| builder.open_sink_or_fallback())
            .map_err(|error| {
                AppError::Playback(format!(
                    "could not open audio output '{}': {error}",
                    output.info.name
                ))
            })?;
        return Ok((sink, output.info.id));
    }

    let mut first_error = None;
    for output in outputs {
        match DeviceSinkBuilder::from_device(output.device)
            .and_then(|builder| builder.open_sink_or_fallback())
        {
            Ok(sink) => return Ok((sink, output.info.id)),
            Err(error) if first_error.is_none() => first_error = Some(error.to_string()),
            Err(_) => {}
        }
    }
    Err(AppError::Playback(format!(
        "audio output unavailable: {}",
        first_error.unwrap_or_else(|| "every output device failed".into())
    )))
}

impl AudioPlayer for RodioAudioPlayer {
    fn load(&mut self, source: PlaybackSource) -> Result<()> {
        if let Some(fading_player) = self.fading_player.take() {
            fading_player.stop();
        }
        self.crossfade_duration = None;
        self.player.clear();
        self.loaded = false;
        self.loaded_source = None;
        self.source_failure = None;
        self.duration = None;
        self.normalization_gain_mb = None;
        self.state = PlaybackState::Loading;
        let prepared = self.prepare_playback(source)?;
        self.install_prepared(prepared);
        self.apply_output_volume();
        Ok(())
    }

    fn load_with_crossfade(&mut self, source: PlaybackSource, duration: Duration) -> Result<()> {
        if !self.loaded || self.player.empty() || duration.is_zero() {
            return self.load(source);
        }
        let prepared = self.prepare_playback(source)?;
        if let Some(fading_player) = self.fading_player.take() {
            fading_player.stop();
        }
        let new_player = Player::connect_new(self._device.mixer());
        let fading_player = std::mem::replace(&mut self.player, new_player);
        self.fading_player = Some(fading_player);
        self.crossfade_duration = Some(duration);
        self.install_prepared(prepared);
        self.apply_output_volume();
        Ok(())
    }

    fn play(&mut self) -> Result<()> {
        if !self.loaded {
            return Err(AppError::Playback("no audio source is loaded".into()));
        }
        self.player.play();
        if let Some(fading_player) = &self.fading_player {
            fading_player.play();
        }
        self.state = PlaybackState::Playing;
        Ok(())
    }

    fn pause(&mut self) -> Result<()> {
        if !self.loaded {
            return Err(AppError::Playback("no audio source is loaded".into()));
        }
        self.player.pause();
        if let Some(fading_player) = &self.fading_player {
            fading_player.pause();
        }
        self.state = PlaybackState::Paused;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.player.stop();
        if let Some(fading_player) = self.fading_player.take() {
            fading_player.stop();
        }
        self.crossfade_duration = None;
        self.loaded = false;
        self.loaded_source = None;
        self.source_failure = None;
        self.duration = None;
        self.normalization_gain_mb = None;
        self.playback_position_ns.store(0, Ordering::Relaxed);
        self.state = PlaybackState::Idle;
        Ok(())
    }

    fn seek(&mut self, position: Duration) -> Result<()> {
        if !self.loaded {
            return Err(AppError::Playback("no audio source is loaded".into()));
        }
        if let Some(fading_player) = self.fading_player.take() {
            fading_player.stop();
        }
        self.crossfade_duration = None;
        let result = self
            .player
            .try_seek(position)
            .map_err(|error| AppError::Playback(format!("audio seek failed: {error}")));
        self.apply_output_volume();
        result
    }

    fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
        self.apply_output_volume();
    }

    fn set_volume_multiplier(&mut self, multiplier: f32) {
        self.volume_multiplier = multiplier.clamp(0.0, 1.0);
        self.apply_output_volume();
    }

    fn set_playback_parameters(&mut self, parameters: PlaybackParameters) -> Result<()> {
        self.rebuild_processing(self.equalizer.clone(), parameters)
    }

    fn set_equalizer(&mut self, equalizer: EqualizerSettings) -> Result<()> {
        self.rebuild_processing(equalizer, self.playback_parameters)
    }

    fn snapshot(&self) -> PlaybackSnapshot {
        self.apply_output_volume();
        let source_error = self
            .source_failure
            .as_ref()
            .and_then(|failure| failure.message());
        let state = if source_error.is_some() {
            PlaybackState::Failed
        } else if self.loaded && self.player.empty() {
            PlaybackState::Ended
        } else {
            self.state
        };
        let position = Duration::from_nanos(self.playback_position_ns.load(Ordering::Relaxed));
        PlaybackSnapshot {
            state,
            position,
            duration: self.duration,
            volume: self.volume,
            normalization_gain_mb: self.normalization_gain_mb,
            equalizer_active: self.loaded && self.equalizer.is_effective(),
            playback_parameters: self.playback_parameters,
            error: source_error,
        }
    }

    fn output_devices(&self) -> Result<Vec<AudioOutputDevice>> {
        Ok(system_output_devices()?
            .into_iter()
            .map(|output| output.info)
            .collect())
    }

    fn select_output_device(&mut self, device_id: &str) -> Result<()> {
        if device_id == self.selected_output_device_id {
            return Ok(());
        }

        let before = self.snapshot();
        let source = self.loaded_source.clone();
        let was_playing = before.state == PlaybackState::Playing;
        if was_playing {
            self.player.pause();
        }
        let position = before.position;
        let replacement = (|| {
            let mut replacement = Self::with_dependencies(
                self.client.clone(),
                self.disk_cache.clone(),
                self.download_store.clone(),
                Some(device_id),
                self.normalization,
                self.silence_skipping,
                self.equalizer.clone(),
                self.playback_parameters,
            )?;
            replacement.set_volume(before.volume);
            replacement.set_volume_multiplier(self.volume_multiplier);
            if let Some(source) = source {
                replacement.load(source)?;
                if !position.is_zero() {
                    let resume_position = replacement
                        .duration
                        .map_or(position, |duration| position.min(duration));
                    replacement.seek(resume_position)?;
                }
                match before.state {
                    PlaybackState::Playing => replacement.play()?,
                    PlaybackState::Ended => replacement.state = PlaybackState::Ended,
                    PlaybackState::Paused | PlaybackState::Loading => {}
                    PlaybackState::Idle | PlaybackState::Failed => {}
                }
            }
            Ok(replacement)
        })();

        match replacement {
            Ok(replacement) => {
                *self = replacement;
                Ok(())
            }
            Err(error) => {
                if was_playing {
                    self.player.play();
                }
                Err(error)
            }
        }
    }

    fn selected_output_device_id(&self) -> Option<String> {
        Some(self.selected_output_device_id.clone())
    }
}

enum AudioCommand {
    Load(PlaybackSource),
    LoadWithCrossfade {
        source: PlaybackSource,
        duration: Duration,
    },
    Play,
    Pause,
    Stop,
    Seek(Duration),
    SetVolume(f32),
    SetVolumeMultiplier(f32),
    SetPlaybackParameters {
        parameters: PlaybackParameters,
        response: mpsc::SyncSender<Result<()>>,
    },
    SetEqualizer {
        equalizer: EqualizerSettings,
        response: mpsc::SyncSender<Result<()>>,
    },
    RefreshOutputDevices,
    SelectOutputDevice(String),
    Shutdown,
}

type AudioBackendFactory = Box<dyn FnMut() -> Result<Box<dyn AudioPlayer>> + Send + 'static>;

/// Non-blocking facade around the native audio device and HTTP decoder. All
/// network, demuxing, and device work stays on one dedicated worker thread.
pub struct DesktopAudioPlayer {
    commands: mpsc::Sender<AudioCommand>,
    snapshot: Arc<Mutex<PlaybackSnapshot>>,
    device_snapshot: Arc<Mutex<AudioDeviceSnapshot>>,
    worker: Option<thread::JoinHandle<()>>,
}

#[derive(Clone)]
pub(crate) struct DesktopAudioParameterControl {
    commands: mpsc::Sender<AudioCommand>,
}

impl DesktopAudioParameterControl {
    pub fn set_playback_parameters(&self, parameters: PlaybackParameters) -> Result<()> {
        let parameters = parameters.validate()?;
        let (response, receiver) = mpsc::sync_channel(1);
        self.commands
            .send(AudioCommand::SetPlaybackParameters {
                parameters,
                response,
            })
            .map_err(|_| AppError::Playback("audio worker stopped unexpectedly".into()))?;
        receiver.recv().map_err(|_| {
            AppError::Playback("audio worker stopped before applying playback parameters".into())
        })?
    }

    pub fn set_equalizer(&self, equalizer: EqualizerSettings) -> Result<()> {
        equalizer.validate()?;
        let (response, receiver) = mpsc::sync_channel(1);
        self.commands
            .send(AudioCommand::SetEqualizer {
                equalizer,
                response,
            })
            .map_err(|_| AppError::Playback("audio worker stopped unexpectedly".into()))?;
        receiver.recv().map_err(|_| {
            AppError::Playback("audio worker stopped before applying equalizer settings".into())
        })?
    }
}

impl Default for DesktopAudioPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl DesktopAudioPlayer {
    pub fn new() -> Self {
        Self::with_cache_limit(DEFAULT_AUDIO_CACHE_BYTES)
    }

    pub fn with_cache_limit(cache_limit: u64) -> Self {
        Self::with_backend_factory(move || {
            Ok(Box::new(RodioAudioPlayer::new(cache_limit)?) as Box<dyn AudioPlayer>)
        })
    }

    pub fn with_settings(settings: &AppSettings) -> Result<Self> {
        let disk_cache = Arc::new(
            AudioCache::new(settings.audio_cache_root(), settings.audio_cache_bytes).map_err(
                |error| AppError::InvalidConfig(format!("audio cache is unavailable: {error}")),
            )?,
        );
        let download_store =
            Arc::new(DownloadedAudioStore::for_current_user().map_err(|error| {
                AppError::InvalidConfig(format!("downloaded audio store is unavailable: {error}"))
            })?);
        Self::with_settings_and_stores(settings, disk_cache, download_store)
    }

    pub(crate) fn with_settings_and_stores(
        settings: &AppSettings,
        disk_cache: Arc<AudioCache>,
        download_store: Arc<DownloadedAudioStore>,
    ) -> Result<Self> {
        let client = build_http_client(
            &settings.proxy,
            concat!("Metrolist-rs/", env!("CARGO_PKG_VERSION"), " audio-stream"),
        )?;
        let normalization = AudioNormalization {
            enabled: settings.audio_normalization,
            level: settings.loudness_level,
        };
        let equalizer = settings.equalizer.clone();
        let silence_skipping = SilenceSkipping {
            enabled: settings.skip_silence,
            instant: settings.skip_silence_instant,
        };
        let playback_parameters = settings.playback_parameters;
        Ok(Self::with_backend_factory(move || {
            Ok(Box::new(RodioAudioPlayer::with_dependencies(
                client.clone(),
                Some(disk_cache.clone()),
                Some(download_store.clone()),
                None,
                normalization,
                silence_skipping,
                equalizer.clone(),
                playback_parameters,
            )?) as Box<dyn AudioPlayer>)
        }))
    }

    /// Starts the command worker with a lazily-created backend.
    ///
    /// Platform adapters and tests can inject an `AudioPlayer` without
    /// changing the ordering, snapshot, or failure-recovery behavior of the
    /// desktop facade.
    pub fn with_backend_factory(
        factory: impl FnMut() -> Result<Box<dyn AudioPlayer>> + Send + 'static,
    ) -> Self {
        let (commands, receiver) = mpsc::channel();
        let snapshot = Arc::new(Mutex::new(PlaybackSnapshot::default()));
        let device_snapshot = Arc::new(Mutex::new(AudioDeviceSnapshot::default()));
        let worker_snapshot = snapshot.clone();
        let worker_device_snapshot = device_snapshot.clone();
        let factory: AudioBackendFactory = Box::new(factory);
        let worker = thread::Builder::new()
            .name("metrolist-audio".into())
            .spawn(move || {
                run_audio_worker(receiver, worker_snapshot, worker_device_snapshot, factory)
            })
            .expect("failed to start the audio worker thread");

        Self {
            commands,
            snapshot,
            device_snapshot,
            worker: Some(worker),
        }
    }

    fn send(&self, command: AudioCommand) -> Result<()> {
        self.commands
            .send(command)
            .map_err(|_| AppError::Playback("audio worker stopped unexpectedly".into()))
    }

    fn update_snapshot(&self, update: impl FnOnce(&mut PlaybackSnapshot)) {
        if let Ok(mut snapshot) = self.snapshot.lock() {
            update(&mut snapshot);
        }
    }

    pub fn refresh_output_devices(&self) -> Result<()> {
        if let Ok(mut snapshot) = self.device_snapshot.lock() {
            snapshot.operation = AudioDeviceOperation::Refreshing;
            snapshot.error = None;
        }
        let result = self.send(AudioCommand::RefreshOutputDevices);
        if let Err(error) = &result
            && let Ok(mut snapshot) = self.device_snapshot.lock()
        {
            snapshot.operation = AudioDeviceOperation::Idle;
            snapshot.error = Some(error.to_string());
        }
        result
    }

    pub fn select_output_device(&self, device_id: impl Into<String>) -> Result<()> {
        if let Ok(mut snapshot) = self.device_snapshot.lock() {
            snapshot.operation = AudioDeviceOperation::Switching;
            snapshot.error = None;
        }
        let result = self.send(AudioCommand::SelectOutputDevice(device_id.into()));
        if let Err(error) = &result
            && let Ok(mut snapshot) = self.device_snapshot.lock()
        {
            snapshot.operation = AudioDeviceOperation::Idle;
            snapshot.error = Some(error.to_string());
        }
        result
    }

    pub fn device_snapshot(&self) -> AudioDeviceSnapshot {
        self.device_snapshot
            .lock()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_else(|_| AudioDeviceSnapshot {
                error: Some("audio output state is unavailable".into()),
                ..AudioDeviceSnapshot::default()
            })
    }

    pub(crate) fn parameter_control(&self) -> DesktopAudioParameterControl {
        DesktopAudioParameterControl {
            commands: self.commands.clone(),
        }
    }
}

impl AudioPlayer for DesktopAudioPlayer {
    fn load(&mut self, source: PlaybackSource) -> Result<()> {
        self.update_snapshot(|snapshot| {
            snapshot.state = PlaybackState::Loading;
            snapshot.position = Duration::ZERO;
            snapshot.duration = None;
            snapshot.normalization_gain_mb = None;
            snapshot.equalizer_active = false;
            snapshot.error = None;
        });
        self.send(AudioCommand::Load(source))
    }

    fn load_with_crossfade(&mut self, source: PlaybackSource, duration: Duration) -> Result<()> {
        self.update_snapshot(|snapshot| {
            snapshot.state = PlaybackState::Loading;
            snapshot.position = Duration::ZERO;
            snapshot.duration = None;
            snapshot.normalization_gain_mb = None;
            snapshot.equalizer_active = false;
            snapshot.error = None;
        });
        self.send(AudioCommand::LoadWithCrossfade { source, duration })
    }

    fn play(&mut self) -> Result<()> {
        self.send(AudioCommand::Play)
    }

    fn pause(&mut self) -> Result<()> {
        self.send(AudioCommand::Pause)
    }

    fn stop(&mut self) -> Result<()> {
        self.send(AudioCommand::Stop)
    }

    fn seek(&mut self, position: Duration) -> Result<()> {
        self.update_snapshot(|snapshot| {
            snapshot.position = position;
        });
        self.send(AudioCommand::Seek(position))
    }

    fn set_volume(&mut self, volume: f32) {
        let volume = volume.clamp(0.0, 1.0);
        self.update_snapshot(|snapshot| snapshot.volume = volume);
        let _ = self.send(AudioCommand::SetVolume(volume));
    }

    fn set_volume_multiplier(&mut self, multiplier: f32) {
        let _ = self.send(AudioCommand::SetVolumeMultiplier(
            multiplier.clamp(0.0, 1.0),
        ));
    }

    fn set_playback_parameters(&mut self, parameters: PlaybackParameters) -> Result<()> {
        self.parameter_control().set_playback_parameters(parameters)
    }

    fn set_equalizer(&mut self, equalizer: EqualizerSettings) -> Result<()> {
        self.parameter_control().set_equalizer(equalizer)
    }

    fn snapshot(&self) -> PlaybackSnapshot {
        self.snapshot
            .lock()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_else(|_| PlaybackSnapshot {
                state: PlaybackState::Failed,
                error: Some("audio state is unavailable".into()),
                ..PlaybackSnapshot::default()
            })
    }
}

impl Drop for DesktopAudioPlayer {
    fn drop(&mut self) {
        let _ = self.commands.send(AudioCommand::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_audio_worker(
    receiver: mpsc::Receiver<AudioCommand>,
    snapshot: Arc<Mutex<PlaybackSnapshot>>,
    device_snapshot: Arc<Mutex<AudioDeviceSnapshot>>,
    mut backend_factory: AudioBackendFactory,
) {
    let mut backend: Option<Box<dyn AudioPlayer>> = None;
    let mut desired_volume = 0.8;
    let mut desired_volume_multiplier = 1.0;
    let mut desired_equalizer: Option<EqualizerSettings> = None;
    let mut last_error: Option<String> = None;

    loop {
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(AudioCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Ok(AudioCommand::RefreshOutputDevices) => {
                let result = ensure_audio_backend(
                    &mut backend,
                    desired_volume,
                    desired_volume_multiplier,
                    desired_equalizer.as_ref(),
                    &mut backend_factory,
                )
                .and_then(|backend| {
                    Ok((
                        backend.output_devices()?,
                        backend.selected_output_device_id(),
                    ))
                });
                if let Ok(mut current) = device_snapshot.lock() {
                    current.operation = AudioDeviceOperation::Idle;
                    match result {
                        Ok((devices, selected_id)) => {
                            current.devices = devices;
                            current.selected_id = selected_id;
                            current.error = None;
                        }
                        Err(error) => current.error = Some(error.to_string()),
                    }
                }
            }
            Ok(AudioCommand::SelectOutputDevice(device_id)) => {
                let result = ensure_audio_backend(
                    &mut backend,
                    desired_volume,
                    desired_volume_multiplier,
                    desired_equalizer.as_ref(),
                    &mut backend_factory,
                )
                .and_then(|backend| {
                    backend.select_output_device(&device_id)?;
                    let selected_id = backend.selected_output_device_id();
                    Ok((backend.output_devices(), selected_id))
                });
                if let Ok(mut current) = device_snapshot.lock() {
                    current.operation = AudioDeviceOperation::Idle;
                    match result {
                        Ok((Ok(devices), selected_id)) => {
                            current.devices = devices;
                            current.selected_id = selected_id;
                            current.error = None;
                        }
                        Ok((Err(error), selected_id)) => {
                            current.selected_id = selected_id;
                            current.error = Some(error.to_string());
                        }
                        Err(error) => current.error = Some(error.to_string()),
                    }
                }
            }
            Ok(AudioCommand::SetPlaybackParameters {
                parameters,
                response,
            }) => {
                let previous_snapshot = snapshot.lock().ok().map(|current| current.clone());
                if let Ok(mut current) = snapshot.lock() {
                    current.state = PlaybackState::Loading;
                    current.error = None;
                }
                let result = ensure_audio_backend(
                    &mut backend,
                    desired_volume,
                    desired_volume_multiplier,
                    desired_equalizer.as_ref(),
                    &mut backend_factory,
                )
                .and_then(|backend| backend.set_playback_parameters(parameters));
                if let Some(backend) = backend.as_ref()
                    && let Ok(mut current) = snapshot.lock()
                {
                    *current = backend.snapshot();
                } else if let Some(previous_snapshot) = previous_snapshot
                    && let Ok(mut current) = snapshot.lock()
                {
                    *current = previous_snapshot;
                }
                let _ = response.send(result);
            }
            Ok(AudioCommand::SetEqualizer {
                equalizer,
                response,
            }) => {
                let previous_snapshot = snapshot.lock().ok().map(|current| current.clone());
                if let Ok(mut current) = snapshot.lock() {
                    current.state = PlaybackState::Loading;
                    current.error = None;
                }
                let result = if let Some(backend) = backend.as_mut() {
                    backend.set_equalizer(equalizer.clone())
                } else {
                    Ok(())
                };
                if result.is_ok() {
                    desired_equalizer = Some(equalizer);
                }
                if let Some(backend) = backend.as_ref()
                    && let Ok(mut current) = snapshot.lock()
                {
                    *current = backend.snapshot();
                } else if let Some(previous_snapshot) = previous_snapshot
                    && let Ok(mut current) = snapshot.lock()
                {
                    *current = previous_snapshot;
                }
                let _ = response.send(result);
            }
            Ok(command) => {
                // `load`, optional `seek`, and `play` are queued together by the UI. If loading
                // failed, retain that useful root cause instead of replacing
                // it with a secondary "no source loaded" error.
                if last_error.is_some()
                    && matches!(&command, AudioCommand::Play | AudioCommand::Seek(_))
                {
                    continue;
                }
                let can_recover = !matches!(
                    &command,
                    AudioCommand::SetVolume(_) | AudioCommand::SetVolumeMultiplier(_)
                );
                let result = run_audio_command(
                    command,
                    &mut backend,
                    &mut desired_volume,
                    &mut desired_volume_multiplier,
                    desired_equalizer.as_ref(),
                    &mut backend_factory,
                );
                if let Err(error) = result {
                    last_error = Some(error.to_string());
                    if let Ok(mut current) = snapshot.lock() {
                        current.state = PlaybackState::Failed;
                        current.error.clone_from(&last_error);
                    }
                    continue;
                }
                if can_recover {
                    last_error = None;
                }
            }
        }

        if last_error.is_none()
            && let Some(backend) = backend.as_ref()
            && let Ok(mut current) = snapshot.lock()
        {
            *current = backend.snapshot();
        }
    }
}

fn run_audio_command(
    command: AudioCommand,
    backend: &mut Option<Box<dyn AudioPlayer>>,
    desired_volume: &mut f32,
    desired_volume_multiplier: &mut f32,
    desired_equalizer: Option<&EqualizerSettings>,
    backend_factory: &mut AudioBackendFactory,
) -> Result<()> {
    if let AudioCommand::SetVolume(volume) = command {
        *desired_volume = volume;
        if let Some(backend) = backend.as_mut() {
            backend.set_volume(volume);
        }
        return Ok(());
    }
    if let AudioCommand::SetVolumeMultiplier(multiplier) = command {
        *desired_volume_multiplier = multiplier;
        if let Some(backend) = backend.as_mut() {
            backend.set_volume_multiplier(multiplier);
        }
        return Ok(());
    }

    let backend = ensure_audio_backend(
        backend,
        *desired_volume,
        *desired_volume_multiplier,
        desired_equalizer,
        backend_factory,
    )?;

    match command {
        AudioCommand::Load(source) => backend.load(source),
        AudioCommand::LoadWithCrossfade { source, duration } => {
            backend.load_with_crossfade(source, duration)
        }
        AudioCommand::Play => backend.play(),
        AudioCommand::Pause => backend.pause(),
        AudioCommand::Stop => backend.stop(),
        AudioCommand::Seek(position) => backend.seek(position),
        AudioCommand::SetVolume(_)
        | AudioCommand::SetVolumeMultiplier(_)
        | AudioCommand::SetPlaybackParameters { .. }
        | AudioCommand::SetEqualizer { .. }
        | AudioCommand::RefreshOutputDevices
        | AudioCommand::SelectOutputDevice(_)
        | AudioCommand::Shutdown => Ok(()),
    }
}

fn ensure_audio_backend<'a>(
    backend: &'a mut Option<Box<dyn AudioPlayer>>,
    desired_volume: f32,
    desired_volume_multiplier: f32,
    desired_equalizer: Option<&EqualizerSettings>,
    backend_factory: &mut AudioBackendFactory,
) -> Result<&'a mut Box<dyn AudioPlayer>> {
    if backend.is_none() {
        let mut created = backend_factory()?;
        if let Some(equalizer) = desired_equalizer {
            created.set_equalizer(equalizer.clone())?;
        }
        *backend = Some(created);
    }
    let backend = backend
        .as_mut()
        .expect("audio backend was initialized immediately above");
    backend.set_volume(desired_volume);
    backend.set_volume_multiplier(desired_volume_multiplier);
    Ok(backend)
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        thread,
        time::Instant,
    };

    use base64::{Engine as _, engine::general_purpose::STANDARD};

    use super::*;
    use crate::ParametricFilterType;
    use crate::services::probe_audio_bytes;

    const FIXTURE_DURATION: Duration = Duration::from_millis(500);
    const LONG_FIXTURE_DURATION: Duration = Duration::from_secs(12 * 60 * 60);

    #[test]
    fn normalization_gain_matches_android_targets_and_bounds() {
        let balanced = AudioNormalization {
            enabled: true,
            level: LoudnessLevel::Balanced,
        };
        assert_eq!(normalization_gain_mb(balanced, Some(-1_800)), Some(300));
        assert_eq!(normalization_gain_mb(balanced, Some(-1_500)), Some(100));
        assert_eq!(normalization_gain_mb(balanced, Some(-500)), Some(-900));
        assert_eq!(normalization_gain_mb(balanced, Some(500)), Some(-1_500));
        assert_eq!(normalization_gain_mb(balanced, None), None);
        assert_eq!(
            normalization_gain_mb(
                AudioNormalization {
                    enabled: false,
                    level: LoudnessLevel::Aggressive,
                },
                Some(-2_000),
            ),
            None
        );
    }

    #[test]
    fn clamped_gain_limits_samples_and_preserves_seekability() {
        use std::num::NonZero;

        use rodio::buffer::SamplesBuffer;

        let channels = NonZero::new(1).unwrap();
        let sample_rate = NonZero::new(2).unwrap();
        let samples = vec![0.8, -0.8, 0.25, -0.25];
        let mut source = ClampedGain::new(
            SamplesBuffer::new(channels, sample_rate, samples),
            Some(300),
        );
        let linear_gain = gain_mb_to_linear(300);

        assert_eq!(source.next(), Some(1.0));
        assert_eq!(source.next(), Some(-1.0));
        source.try_seek(Duration::from_secs(1)).unwrap();
        let sample = source.next().unwrap();
        assert!((sample - 0.25 * linear_gain).abs() < 0.000_001);
        assert_eq!(source.channels(), channels);
        assert_eq!(source.sample_rate(), sample_rate);
        assert_eq!(source.total_duration(), Some(Duration::from_secs(2)));
    }

    fn equalized_sine_rms(frequency_hz: f64) -> f64 {
        use std::num::NonZero;

        use rodio::buffer::SamplesBuffer;

        const SAMPLE_RATE: u32 = 48_000;
        let samples = (0..SAMPLE_RATE)
            .map(|index| {
                (2.0 * std::f64::consts::PI * frequency_hz * f64::from(index)
                    / f64::from(SAMPLE_RATE))
                .sin() as f32
                    * 0.25
            })
            .collect::<Vec<_>>();
        let mut gains_mb = [0; crate::EQUALIZER_BAND_COUNT];
        gains_mb[5] = 600;
        let source = EqualizerSource::new(
            SamplesBuffer::new(
                NonZero::new(1).unwrap(),
                NonZero::new(SAMPLE_RATE).unwrap(),
                samples,
            ),
            EqualizerSettings {
                enabled: true,
                gains_mb,
                active_profile: None,
            },
        );
        let settled = source.skip(SAMPLE_RATE as usize / 10).collect::<Vec<_>>();
        (settled
            .iter()
            .map(|sample| f64::from(*sample).powi(2))
            .sum::<f64>()
            / settled.len() as f64)
            .sqrt()
    }

    #[test]
    fn graphic_equalizer_boosts_the_selected_band_with_automatic_headroom() {
        let center = equalized_sine_rms(1_000.0);
        let outside = equalized_sine_rms(100.0);

        assert!(
            center / outside > 1.8,
            "unexpected EQ ratio: {}",
            center / outside
        );
        assert!(
            center < 0.19,
            "automatic headroom was not applied: {center}"
        );
    }

    #[test]
    fn graphic_equalizer_keeps_channel_history_isolated_and_resets_on_seek() {
        use std::num::NonZero;

        use rodio::buffer::SamplesBuffer;

        let mut gains_mb = [0; crate::EQUALIZER_BAND_COUNT];
        gains_mb[5] = 600;
        let settings = EqualizerSettings {
            enabled: true,
            gains_mb,
            active_profile: None,
        };
        let stereo = (0..128)
            .flat_map(|index| [if index == 0 { 0.5 } else { 0.0 }, 0.0])
            .collect::<Vec<_>>();
        let output = EqualizerSource::new(
            SamplesBuffer::new(
                NonZero::new(2).unwrap(),
                NonZero::new(48_000).unwrap(),
                stereo,
            ),
            settings.clone(),
        )
        .collect::<Vec<_>>();
        assert!(
            output
                .iter()
                .skip(1)
                .step_by(2)
                .all(|sample| *sample == 0.0)
        );

        let mono = (0..256)
            .map(|index| (index as f32 / 17.0).sin() * 0.25)
            .collect::<Vec<_>>();
        let mut seekable = EqualizerSource::new(
            SamplesBuffer::new(
                NonZero::new(1).unwrap(),
                NonZero::new(48_000).unwrap(),
                mono,
            ),
            settings,
        );
        let first = seekable.by_ref().take(64).collect::<Vec<_>>();
        seekable.try_seek(Duration::ZERO).unwrap();
        let replayed = seekable.by_ref().take(64).collect::<Vec<_>>();
        assert_eq!(first, replayed);
    }

    fn parametric_settings(
        preamp_mb: i16,
        bands: Vec<ParametricEqualizerBand>,
    ) -> EqualizerSettings {
        EqualizerSettings {
            enabled: true,
            gains_mb: [0; crate::EQUALIZER_BAND_COUNT],
            active_profile: Some(crate::EqualizerProfile {
                id: "fixture-profile".into(),
                name: "Fixture profile".into(),
                device_model: "Fixture headphones".into(),
                equalizer: crate::ParametricEqualizer { preamp_mb, bands },
                source: "fixture".into(),
                rig: "fixture".into(),
                is_custom: true,
                added_at_ms: 0,
            }),
        }
    }

    fn parametric_sine_rms(frequency_hz: f64, band: ParametricEqualizerBand) -> f64 {
        use std::num::NonZero;

        use rodio::buffer::SamplesBuffer;

        const SAMPLE_RATE: u32 = 48_000;
        let samples = (0..SAMPLE_RATE)
            .map(|index| {
                (2.0 * std::f64::consts::PI * frequency_hz * f64::from(index)
                    / f64::from(SAMPLE_RATE))
                .sin() as f32
                    * 0.125
            })
            .collect::<Vec<_>>();
        let source = EqualizerSource::new(
            SamplesBuffer::new(
                NonZero::new(1).unwrap(),
                NonZero::new(SAMPLE_RATE).unwrap(),
                samples,
            ),
            parametric_settings(0, vec![band]),
        );
        let settled = source.skip(SAMPLE_RATE as usize / 10).collect::<Vec<_>>();
        (settled
            .iter()
            .map(|sample| f64::from(*sample).powi(2))
            .sum::<f64>()
            / settled.len() as f64)
            .sqrt()
    }

    #[test]
    fn parametric_eq_supports_peaking_low_shelf_and_high_shelf_responses() {
        let band = |filter_type, frequency_millihz| ParametricEqualizerBand {
            filter_type,
            frequency_millihz,
            gain_mb: 600,
            q_milli: 1_000,
            enabled: true,
        };

        let peak = band(ParametricFilterType::Peaking, 1_000_000);
        assert!(parametric_sine_rms(1_000.0, peak) / parametric_sine_rms(100.0, peak) > 1.7);

        let low = band(ParametricFilterType::LowShelf, 200_000);
        assert!(parametric_sine_rms(40.0, low) / parametric_sine_rms(4_000.0, low) > 1.7);

        let high = band(ParametricFilterType::HighShelf, 4_000_000);
        assert!(parametric_sine_rms(12_000.0, high) / parametric_sine_rms(100.0, high) > 1.7);
    }

    #[test]
    fn parametric_eq_uses_explicit_preamp_and_skips_bands_above_nyquist() {
        use std::num::NonZero;

        use rodio::buffer::SamplesBuffer;

        let settings = parametric_settings(
            -600,
            vec![ParametricEqualizerBand {
                filter_type: ParametricFilterType::Peaking,
                frequency_millihz: 100_000_000,
                gain_mb: 600,
                q_milli: 1_000,
                enabled: true,
            }],
        );
        let output = EqualizerSource::new(
            SamplesBuffer::new(
                NonZero::new(1).unwrap(),
                NonZero::new(48_000).unwrap(),
                vec![0.5; 4],
            ),
            settings,
        )
        .collect::<Vec<_>>();
        let expected = 0.5 * gain_mb_to_linear(-600);
        assert!(
            output
                .iter()
                .all(|sample| (*sample - expected).abs() < 0.000_001)
        );
    }

    fn sine_source(
        sample_rate: u32,
        channels: u16,
        duration_seconds: u32,
        frequency_hz: f32,
    ) -> rodio::buffer::SamplesBuffer {
        use std::num::NonZero;

        let frames = sample_rate as usize * duration_seconds as usize;
        let samples: Vec<f32> = (0..frames)
            .flat_map(|frame| {
                let left = (2.0 * std::f32::consts::PI * frequency_hz * frame as f32
                    / sample_rate as f32)
                    .sin()
                    * 0.25;
                (0..channels).map(move |channel| if channel == 0 { left } else { 0.0 })
            })
            .collect();
        rodio::buffer::SamplesBuffer::new(
            NonZero::new(channels).unwrap(),
            NonZero::new(sample_rate).unwrap(),
            samples,
        )
    }

    fn rising_zero_crossing_frequency(samples: &[f32], sample_rate: u32) -> f32 {
        let crossings = samples
            .windows(2)
            .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
            .count();
        crossings as f32 * sample_rate as f32 / samples.len() as f32
    }

    #[test]
    fn time_stretch_preserves_pitch_while_tempo_changes() {
        const SAMPLE_RATE: u32 = 8_000;
        let position = Arc::new(AtomicU64::new(0));
        let source = TimeStretchSource::new(
            sine_source(SAMPLE_RATE, 1, 2, 440.0),
            PlaybackParameters {
                varispeed: false,
                tempo_milli: 1_500,
                transpose_semitones: 0,
            },
            position,
            Arc::new(AtomicU64::new(0)),
        )
        .unwrap();
        let output_rate = source.sample_rate().get();
        let samples = source.collect::<Vec<_>>();
        let wall_seconds = samples.len() as f32 / output_rate as f32;
        assert!((wall_seconds - 2.0 / 1.5).abs() < 0.12, "{wall_seconds}");
        let settled = &samples[SAMPLE_RATE as usize / 4..];
        let frequency = rising_zero_crossing_frequency(settled, output_rate);
        assert!((frequency - 440.0).abs() < 25.0, "{frequency}");
    }

    #[test]
    fn independent_pitch_shift_keeps_duration_and_varispeed_moves_both() {
        const SAMPLE_RATE: u32 = 8_000;
        let normal = TimeStretchSource::new(
            sine_source(SAMPLE_RATE, 1, 2, 440.0),
            PlaybackParameters {
                varispeed: false,
                tempo_milli: 1_000,
                transpose_semitones: 12,
            },
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        )
        .unwrap();
        let normal_rate = normal.sample_rate().get();
        let normal_samples = normal.collect::<Vec<_>>();
        let normal_seconds = normal_samples.len() as f32 / normal_rate as f32;
        assert!((normal_seconds - 2.0).abs() < 0.15, "{normal_seconds}");
        let normal_frequency = rising_zero_crossing_frequency(
            &normal_samples[normal_rate as usize / 4..],
            normal_rate,
        );
        assert!(
            (normal_frequency - 880.0).abs() < 45.0,
            "{normal_frequency}"
        );

        let varispeed = TimeStretchSource::new(
            sine_source(SAMPLE_RATE, 1, 2, 440.0),
            PlaybackParameters {
                varispeed: true,
                tempo_milli: 1_500,
                transpose_semitones: 0,
            },
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        )
        .unwrap();
        let varispeed_rate = varispeed.sample_rate().get();
        let varispeed_samples = varispeed.collect::<Vec<_>>();
        let varispeed_seconds = varispeed_samples.len() as f32 / varispeed_rate as f32;
        let varispeed_frequency =
            rising_zero_crossing_frequency(&varispeed_samples, varispeed_rate);
        assert!((varispeed_seconds - 2.0 / 1.5).abs() < 0.01);
        assert!((varispeed_frequency - 660.0).abs() < 5.0);
    }

    #[test]
    fn time_stretch_keeps_channels_isolated_and_resets_on_seek() {
        const SAMPLE_RATE: u32 = 8_000;
        let parameters = PlaybackParameters {
            varispeed: false,
            tempo_milli: 750,
            transpose_semitones: 3,
        };
        let mut source = TimeStretchSource::new(
            sine_source(SAMPLE_RATE, 2, 2, 440.0),
            parameters,
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        )
        .unwrap();
        let first = source.by_ref().take(2_048).collect::<Vec<_>>();
        assert!(first.iter().skip(1).step_by(2).all(|sample| *sample == 0.0));
        source.try_seek(Duration::ZERO).unwrap();
        let replayed = source.by_ref().take(2_048).collect::<Vec<_>>();
        assert_eq!(first, replayed);
    }

    #[test]
    fn time_stretch_covers_the_slowest_high_pitch_combination_and_tracks_media_time() {
        assert_eq!(time_stretch_stage_ratios(0.125), vec![0.25, 0.5]);
        assert!(time_stretch_stage_ratios(1.0).is_empty());

        const SAMPLE_RATE: u32 = 8_000;
        let position = Arc::new(AtomicU64::new(0));
        let mut source = TimeStretchSource::new(
            sine_source(SAMPLE_RATE, 1, 2, 440.0),
            PlaybackParameters {
                varispeed: false,
                tempo_milli: 250,
                transpose_semitones: 12,
            },
            position.clone(),
            Arc::new(AtomicU64::new(0)),
        )
        .unwrap();
        assert_eq!(source.sample_rate().get(), SAMPLE_RATE * 2);
        source.try_seek(Duration::from_millis(500)).unwrap();
        assert_eq!(position.load(Ordering::Relaxed), 500_000_000);
        let _ = source.by_ref().take(SAMPLE_RATE as usize).count();
        let advanced = Duration::from_nanos(position.load(Ordering::Relaxed));
        assert!(advanced > Duration::from_millis(600));
        assert!(advanced < Duration::from_millis(700));
    }

    fn fixture_bytes() -> Vec<u8> {
        let encoded = include_str!("../../tests/fixtures/audio/tone_aac_lc.m4a.b64")
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        STANDARD.decode(encoded).unwrap()
    }

    fn fixture_source(url: &str) -> PlaybackSource {
        PlaybackSource {
            url: url.into(),
            mime_type: "audio/mp4; codecs=mp4a.40.2".into(),
            content_length: Some(fixture_bytes().len() as u64),
            loudness_lufs_mb: None,
            request_headers: Vec::new(),
            cache_key: Some("offline-fixture".into()),
            access: PlaybackSourceAccess::NetworkAndCache,
        }
    }

    struct FixtureAudioBackend {
        loaded: bool,
        state: PlaybackState,
        position: Duration,
        started_at: Option<Instant>,
        volume: f32,
        equalizer: EqualizerSettings,
        playback_parameters: PlaybackParameters,
        selected_output_device_id: String,
    }

    impl Default for FixtureAudioBackend {
        fn default() -> Self {
            Self {
                loaded: false,
                state: PlaybackState::Idle,
                position: Duration::ZERO,
                started_at: None,
                volume: 0.8,
                equalizer: EqualizerSettings::default(),
                playback_parameters: PlaybackParameters::default(),
                selected_output_device_id: "fixture-default".into(),
            }
        }
    }

    impl FixtureAudioBackend {
        fn require_loaded(&self) -> Result<()> {
            if self.loaded {
                Ok(())
            } else {
                Err(AppError::Playback("no fixture is loaded".into()))
            }
        }

        fn current_position(&self) -> Duration {
            if self.state == PlaybackState::Playing {
                self.position
                    .saturating_add(self.started_at.map_or(Duration::ZERO, |at| at.elapsed()))
                    .min(FIXTURE_DURATION)
            } else {
                self.position
            }
        }
    }

    impl AudioPlayer for FixtureAudioBackend {
        fn load(&mut self, source: PlaybackSource) -> Result<()> {
            if source.url == "fixture://failure" {
                return Err(AppError::Playback("fixture load failed".into()));
            }
            if source.url != "fixture://tone" {
                return Err(AppError::Playback("unexpected fixture URL".into()));
            }
            let decoded = probe_audio_bytes(fixture_bytes(), Some("m4a"))?;
            if decoded.sample_rate != 44_100 || decoded.channels != 2 {
                return Err(AppError::Playback(
                    "fixture decoded with unexpected audio parameters".into(),
                ));
            }
            self.loaded = true;
            self.state = PlaybackState::Paused;
            self.position = Duration::ZERO;
            self.started_at = None;
            Ok(())
        }

        fn play(&mut self) -> Result<()> {
            self.require_loaded()?;
            if self.state == PlaybackState::Ended {
                self.position = Duration::ZERO;
            }
            self.state = PlaybackState::Playing;
            self.started_at = Some(Instant::now());
            Ok(())
        }

        fn pause(&mut self) -> Result<()> {
            self.require_loaded()?;
            self.position = self.current_position();
            self.started_at = None;
            self.state = PlaybackState::Paused;
            Ok(())
        }

        fn stop(&mut self) -> Result<()> {
            self.loaded = false;
            self.state = PlaybackState::Idle;
            self.position = Duration::ZERO;
            self.started_at = None;
            Ok(())
        }

        fn seek(&mut self, position: Duration) -> Result<()> {
            self.require_loaded()?;
            if position > FIXTURE_DURATION {
                return Err(AppError::Playback("fixture seek is out of range".into()));
            }
            self.position = position;
            if self.state == PlaybackState::Playing {
                self.started_at = Some(Instant::now());
            }
            Ok(())
        }

        fn set_volume(&mut self, volume: f32) {
            self.volume = volume.clamp(0.0, 1.0);
        }

        fn set_playback_parameters(&mut self, parameters: PlaybackParameters) -> Result<()> {
            let parameters = parameters.validate()?;
            if parameters.transpose_semitones == crate::MAX_TRANSPOSE_SEMITONES {
                return Err(AppError::Playback(
                    "fixture playback parameter change failed".into(),
                ));
            }
            self.position = self.current_position();
            if self.state == PlaybackState::Playing {
                self.started_at = Some(Instant::now());
            }
            self.playback_parameters = parameters;
            Ok(())
        }

        fn set_equalizer(&mut self, equalizer: EqualizerSettings) -> Result<()> {
            equalizer.validate()?;
            if equalizer
                .active_profile
                .as_ref()
                .is_some_and(|profile| profile.id == "fixture-rejected")
            {
                return Err(AppError::Playback("fixture equalizer change failed".into()));
            }
            self.position = self.current_position();
            if self.state == PlaybackState::Playing {
                self.started_at = Some(Instant::now());
            }
            self.equalizer = equalizer;
            Ok(())
        }

        fn snapshot(&self) -> PlaybackSnapshot {
            let position = self.current_position();
            let state = if self.state == PlaybackState::Playing && position >= FIXTURE_DURATION {
                PlaybackState::Ended
            } else {
                self.state
            };
            PlaybackSnapshot {
                state,
                position,
                duration: self.loaded.then_some(FIXTURE_DURATION),
                volume: self.volume,
                normalization_gain_mb: None,
                equalizer_active: self.loaded && self.equalizer.is_effective(),
                playback_parameters: self.playback_parameters,
                error: None,
            }
        }

        fn output_devices(&self) -> Result<Vec<AudioOutputDevice>> {
            Ok(vec![
                AudioOutputDevice {
                    id: "fixture-default".into(),
                    name: "Fixture Speakers".into(),
                    is_default: true,
                },
                AudioOutputDevice {
                    id: "fixture-headphones".into(),
                    name: "Fixture Headphones".into(),
                    is_default: false,
                },
            ])
        }

        fn select_output_device(&mut self, device_id: &str) -> Result<()> {
            if device_id == "fixture-failure" {
                return Err(AppError::Playback("fixture device switch failed".into()));
            }
            if !self
                .output_devices()?
                .iter()
                .any(|device| device.id == device_id)
            {
                return Err(AppError::Playback(
                    "fixture output is no longer available".into(),
                ));
            }
            self.selected_output_device_id = device_id.into();
            Ok(())
        }

        fn selected_output_device_id(&self) -> Option<String> {
            Some(self.selected_output_device_id.clone())
        }
    }

    struct VirtualTimeAudioBackend {
        clock_millis: Arc<AtomicU64>,
        loaded: bool,
        state: PlaybackState,
        position: Duration,
        started_at_millis: Option<u64>,
        volume: f32,
    }

    impl VirtualTimeAudioBackend {
        fn new(clock_millis: Arc<AtomicU64>) -> Self {
            Self {
                clock_millis,
                loaded: false,
                state: PlaybackState::Idle,
                position: Duration::ZERO,
                started_at_millis: None,
                volume: 0.8,
            }
        }

        fn require_loaded(&self) -> Result<()> {
            if self.loaded {
                Ok(())
            } else {
                Err(AppError::Playback("no virtual fixture is loaded".into()))
            }
        }

        fn now_millis(&self) -> u64 {
            self.clock_millis.load(Ordering::Relaxed)
        }

        fn current_position(&self) -> Duration {
            if self.state == PlaybackState::Playing {
                let elapsed = self
                    .started_at_millis
                    .map_or(0, |started| self.now_millis().saturating_sub(started));
                self.position
                    .saturating_add(Duration::from_millis(elapsed))
                    .min(LONG_FIXTURE_DURATION)
            } else {
                self.position
            }
        }
    }

    impl AudioPlayer for VirtualTimeAudioBackend {
        fn load(&mut self, source: PlaybackSource) -> Result<()> {
            if source.url != "fixture://long" {
                return Err(AppError::Playback("unexpected virtual fixture URL".into()));
            }
            self.loaded = true;
            self.state = PlaybackState::Paused;
            self.position = Duration::ZERO;
            self.started_at_millis = None;
            Ok(())
        }

        fn play(&mut self) -> Result<()> {
            self.require_loaded()?;
            self.position = self.current_position();
            self.state = PlaybackState::Playing;
            self.started_at_millis = Some(self.now_millis());
            Ok(())
        }

        fn pause(&mut self) -> Result<()> {
            self.require_loaded()?;
            self.position = self.current_position();
            self.state = PlaybackState::Paused;
            self.started_at_millis = None;
            Ok(())
        }

        fn stop(&mut self) -> Result<()> {
            self.loaded = false;
            self.state = PlaybackState::Idle;
            self.position = Duration::ZERO;
            self.started_at_millis = None;
            Ok(())
        }

        fn seek(&mut self, position: Duration) -> Result<()> {
            self.require_loaded()?;
            if position > LONG_FIXTURE_DURATION {
                return Err(AppError::Playback(
                    "virtual fixture seek is out of range".into(),
                ));
            }
            self.position = position;
            if self.state == PlaybackState::Playing {
                self.started_at_millis = Some(self.now_millis());
            }
            Ok(())
        }

        fn set_volume(&mut self, volume: f32) {
            self.volume = volume.clamp(0.0, 1.0);
        }

        fn snapshot(&self) -> PlaybackSnapshot {
            let position = self.current_position();
            let state = if self.state == PlaybackState::Playing && position >= LONG_FIXTURE_DURATION
            {
                PlaybackState::Ended
            } else {
                self.state
            };
            PlaybackSnapshot {
                state,
                position,
                duration: self.loaded.then_some(LONG_FIXTURE_DURATION),
                volume: self.volume,
                normalization_gain_mb: None,
                equalizer_active: false,
                playback_parameters: PlaybackParameters::default(),
                error: None,
            }
        }
    }

    fn fixture_player() -> DesktopAudioPlayer {
        DesktopAudioPlayer::with_backend_factory(|| {
            Ok(Box::<FixtureAudioBackend>::default() as Box<dyn AudioPlayer>)
        })
    }

    fn wait_for_snapshot(
        player: &DesktopAudioPlayer,
        predicate: impl Fn(&PlaybackSnapshot) -> bool,
    ) -> PlaybackSnapshot {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let snapshot = player.snapshot();
            if predicate(&snapshot) {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for audio state; last snapshot: {snapshot:?}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_device_snapshot(
        player: &DesktopAudioPlayer,
        predicate: impl Fn(&AudioDeviceSnapshot) -> bool,
    ) -> AudioDeviceSnapshot {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let snapshot = player.device_snapshot();
            if predicate(&snapshot) {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for audio device state; last snapshot: {snapshot:?}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn local_aac_fixture_decodes_without_network_or_an_output_device() {
        let decoded = probe_audio_bytes(fixture_bytes(), Some("m4a")).unwrap();

        assert_eq!(decoded.sample_rate, 44_100);
        assert_eq!(decoded.channels, 2);
        assert!(decoded.decoded_frames >= 4_096);
    }

    #[test]
    fn injected_backend_exercises_the_complete_command_state_machine() {
        let mut player = fixture_player();
        player.set_volume(0.35);
        player.load(fixture_source("fixture://tone")).unwrap();
        let loaded = wait_for_snapshot(&player, |snapshot| snapshot.state == PlaybackState::Paused);
        assert_eq!(loaded.duration, Some(FIXTURE_DURATION));
        assert!((loaded.volume - 0.35).abs() < f32::EPSILON);

        player.play().unwrap();
        let advancing = wait_for_snapshot(&player, |snapshot| {
            snapshot.state == PlaybackState::Playing
                && snapshot.position >= Duration::from_millis(20)
        });
        assert!(advancing.position < FIXTURE_DURATION);

        player.pause().unwrap();
        let paused = wait_for_snapshot(&player, |snapshot| snapshot.state == PlaybackState::Paused);
        thread::sleep(Duration::from_millis(50));
        assert_eq!(player.snapshot().position, paused.position);

        player.seek(Duration::from_millis(400)).unwrap();
        let sought = wait_for_snapshot(&player, |snapshot| {
            snapshot.position == Duration::from_millis(400)
        });
        assert_eq!(sought.state, PlaybackState::Paused);

        player.play().unwrap();
        let ended = wait_for_snapshot(&player, |snapshot| snapshot.state == PlaybackState::Ended);
        assert_eq!(ended.position, FIXTURE_DURATION);

        player.stop().unwrap();
        let stopped = wait_for_snapshot(&player, |snapshot| snapshot.state == PlaybackState::Idle);
        assert_eq!(stopped.position, Duration::ZERO);
        assert_eq!(stopped.duration, None);
    }

    #[test]
    fn live_parameter_control_preserves_playing_state_and_rolls_back_failures() {
        let mut player = fixture_player();
        player.load(fixture_source("fixture://tone")).unwrap();
        wait_for_snapshot(&player, |snapshot| snapshot.state == PlaybackState::Paused);
        player.play().unwrap();
        let before = wait_for_snapshot(&player, |snapshot| {
            snapshot.state == PlaybackState::Playing
                && snapshot.position >= Duration::from_millis(20)
        });

        let parameters = PlaybackParameters {
            varispeed: false,
            tempo_milli: 1_250,
            transpose_semitones: -2,
        };
        player
            .parameter_control()
            .set_playback_parameters(parameters)
            .unwrap();
        let applied = wait_for_snapshot(&player, |snapshot| {
            snapshot.playback_parameters == parameters && snapshot.state == PlaybackState::Playing
        });
        assert!(applied.position >= before.position);

        let rejected = PlaybackParameters {
            transpose_semitones: crate::MAX_TRANSPOSE_SEMITONES,
            ..parameters
        };
        assert!(
            player
                .parameter_control()
                .set_playback_parameters(rejected)
                .is_err()
        );
        let rolled_back = player.snapshot();
        assert_eq!(rolled_back.playback_parameters, parameters);
        assert_eq!(rolled_back.state, PlaybackState::Playing);
        assert!(rolled_back.error.is_none());
    }

    #[test]
    fn parameter_control_restores_snapshot_when_backend_cannot_start() {
        let player = DesktopAudioPlayer::with_backend_factory(|| {
            Err(AppError::Playback("fixture backend is unavailable".into()))
        });
        let before = player.snapshot();
        let parameters = PlaybackParameters {
            tempo_milli: 1_050,
            ..PlaybackParameters::default()
        };

        assert!(
            player
                .parameter_control()
                .set_playback_parameters(parameters)
                .is_err()
        );
        assert_eq!(player.snapshot(), before);
    }

    #[test]
    fn live_equalizer_control_preserves_playing_state_and_backend_rolls_back() {
        let mut player = fixture_player();
        player.load(fixture_source("fixture://tone")).unwrap();
        wait_for_snapshot(&player, |snapshot| snapshot.state == PlaybackState::Paused);
        player.play().unwrap();
        let before = wait_for_snapshot(&player, |snapshot| {
            snapshot.state == PlaybackState::Playing
                && snapshot.position >= Duration::from_millis(20)
        });

        let applied_settings = parametric_settings(
            -300,
            vec![ParametricEqualizerBand {
                filter_type: ParametricFilterType::Peaking,
                frequency_millihz: 1_000_000,
                gain_mb: 300,
                q_milli: 1_410,
                enabled: true,
            }],
        );
        player
            .parameter_control()
            .set_equalizer(applied_settings.clone())
            .unwrap();
        let applied = wait_for_snapshot(&player, |snapshot| {
            snapshot.equalizer_active && snapshot.state == PlaybackState::Playing
        });
        assert!(applied.position >= before.position);

        let mut rejected = applied_settings;
        rejected.active_profile.as_mut().unwrap().id = "fixture-rejected".into();
        assert!(player.parameter_control().set_equalizer(rejected).is_err());
        let rolled_back = player.snapshot();
        assert!(rolled_back.equalizer_active);
        assert_eq!(rolled_back.state, PlaybackState::Playing);
        assert!(rolled_back.error.is_none());
    }

    #[test]
    fn equalizer_selection_is_deferred_until_audio_is_needed() {
        let factory_calls = Arc::new(AtomicU64::new(0));
        let worker_calls = factory_calls.clone();
        let mut player = DesktopAudioPlayer::with_backend_factory(move || {
            worker_calls.fetch_add(1, Ordering::Relaxed);
            Ok(Box::<FixtureAudioBackend>::default() as Box<dyn AudioPlayer>)
        });
        let settings = parametric_settings(
            -300,
            vec![ParametricEqualizerBand {
                filter_type: ParametricFilterType::Peaking,
                frequency_millihz: 1_000_000,
                gain_mb: 300,
                q_milli: 1_410,
                enabled: true,
            }],
        );

        player.parameter_control().set_equalizer(settings).unwrap();
        assert_eq!(factory_calls.load(Ordering::Relaxed), 0);
        assert_eq!(player.snapshot().state, PlaybackState::Idle);

        player.load(fixture_source("fixture://tone")).unwrap();
        let loaded = wait_for_snapshot(&player, |snapshot| {
            snapshot.state == PlaybackState::Paused && snapshot.equalizer_active
        });
        assert!(loaded.equalizer_active);
        assert_eq!(factory_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn virtual_twelve_hour_session_preserves_pause_seek_and_end_state() {
        let clock_millis = Arc::new(AtomicU64::new(0));
        let backend_clock = clock_millis.clone();
        let mut player = DesktopAudioPlayer::with_backend_factory(move || {
            Ok(
                Box::new(VirtualTimeAudioBackend::new(backend_clock.clone()))
                    as Box<dyn AudioPlayer>,
            )
        });

        player.load(fixture_source("fixture://long")).unwrap();
        let loaded = wait_for_snapshot(&player, |snapshot| snapshot.state == PlaybackState::Paused);
        assert_eq!(loaded.duration, Some(LONG_FIXTURE_DURATION));

        player.play().unwrap();
        wait_for_snapshot(&player, |snapshot| snapshot.state == PlaybackState::Playing);
        for hour in 1..=10 {
            clock_millis.fetch_add(60 * 60 * 1_000, Ordering::Relaxed);
            let expected = Duration::from_secs(hour * 60 * 60);
            wait_for_snapshot(&player, |snapshot| {
                snapshot.state == PlaybackState::Playing && snapshot.position == expected
            });
        }

        player.pause().unwrap();
        let paused = wait_for_snapshot(&player, |snapshot| snapshot.state == PlaybackState::Paused);
        assert_eq!(paused.position, Duration::from_secs(10 * 60 * 60));
        clock_millis.fetch_add(2 * 60 * 60 * 1_000, Ordering::Relaxed);
        thread::sleep(Duration::from_millis(150));
        assert_eq!(player.snapshot().position, paused.position);

        // Exercise command ordering with a burst of seeks, then use Play as
        // the acknowledgement that every preceding seek reached the backend.
        for index in 0..256_u64 {
            player
                .seek(Duration::from_secs((index * 173) % 43_000))
                .unwrap();
        }
        let near_end = LONG_FIXTURE_DURATION - Duration::from_secs(30);
        player.seek(near_end).unwrap();
        player.play().unwrap();
        wait_for_snapshot(&player, |snapshot| {
            snapshot.state == PlaybackState::Playing && snapshot.position == near_end
        });

        clock_millis.fetch_add(30_000, Ordering::Relaxed);
        let ended = wait_for_snapshot(&player, |snapshot| snapshot.state == PlaybackState::Ended);
        assert_eq!(ended.position, LONG_FIXTURE_DURATION);

        player.stop().unwrap();
        let stopped = wait_for_snapshot(&player, |snapshot| snapshot.state == PlaybackState::Idle);
        assert_eq!(stopped.position, Duration::ZERO);
        assert_eq!(stopped.duration, None);
    }

    #[test]
    fn a_failed_load_keeps_its_root_cause_and_a_new_load_recovers() {
        let mut player = fixture_player();
        player.load(fixture_source("fixture://failure")).unwrap();
        let failed = wait_for_snapshot(&player, |snapshot| snapshot.state == PlaybackState::Failed);
        assert_eq!(
            failed.error.as_deref(),
            Some("playback failed: fixture load failed")
        );

        player.seek(Duration::from_millis(250)).unwrap();
        player.play().unwrap();
        thread::sleep(Duration::from_millis(150));
        assert_eq!(player.snapshot().error, failed.error);

        player.load(fixture_source("fixture://tone")).unwrap();
        let recovered = wait_for_snapshot(&player, |snapshot| {
            snapshot.state == PlaybackState::Paused && snapshot.error.is_none()
        });
        assert_eq!(recovered.position, Duration::ZERO);
        player.play().unwrap();
        wait_for_snapshot(&player, |snapshot| snapshot.state == PlaybackState::Playing);
    }

    #[test]
    fn device_refresh_and_switch_preserve_playback_and_roll_back_failures() {
        let mut player = fixture_player();
        player.refresh_output_devices().unwrap();
        let devices = wait_for_device_snapshot(&player, |snapshot| {
            snapshot.operation == AudioDeviceOperation::Idle && snapshot.devices.len() == 2
        });
        assert_eq!(devices.selected_id.as_deref(), Some("fixture-default"));
        assert!(devices.devices[0].is_default);

        player.load(fixture_source("fixture://tone")).unwrap();
        player.play().unwrap();
        let before = wait_for_snapshot(&player, |snapshot| {
            snapshot.state == PlaybackState::Playing
                && snapshot.position >= Duration::from_millis(20)
        });
        player.select_output_device("fixture-headphones").unwrap();
        let switched = wait_for_device_snapshot(&player, |snapshot| {
            snapshot.operation == AudioDeviceOperation::Idle
                && snapshot.selected_id.as_deref() == Some("fixture-headphones")
        });
        assert!(switched.error.is_none());
        let after = wait_for_snapshot(&player, |snapshot| {
            snapshot.state == PlaybackState::Playing && snapshot.position >= before.position
        });
        assert!(after.position < FIXTURE_DURATION);

        player.select_output_device("fixture-failure").unwrap();
        let failed = wait_for_device_snapshot(&player, |snapshot| {
            snapshot.operation == AudioDeviceOperation::Idle && snapshot.error.is_some()
        });
        assert_eq!(failed.selected_id.as_deref(), Some("fixture-headphones"));
        assert_eq!(
            failed.error.as_deref(),
            Some("playback failed: fixture device switch failed")
        );
        assert_eq!(player.snapshot().state, PlaybackState::Playing);
    }
}
