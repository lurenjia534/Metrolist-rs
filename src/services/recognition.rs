use std::{
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::Engine as _;
use futures::{AsyncReadExt as _, channel::oneshot};
use http_client::{AsyncBody, HttpClient, HttpRequestExt as _, Request, StatusCode, Url};
use rodio::cpal::{
    self, SampleFormat,
    traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _},
};
use serde_json::Value;

use crate::services::build_http_client;
use crate::{AppError, AppSettings, Result};

// Adapted for Rust from Metrolist Android's GPL-3.0-only pure-Kotlin implementation,
// which in turn follows SongRec/vibra's Shazam-compatible fingerprint format.

pub const RECOGNITION_SAMPLE_RATE: u32 = 16_000;
pub const RECOGNITION_CAPTURE_DURATION: Duration = Duration::from_secs(12);

const FFT_SIZE: usize = 2_048;
const FFT_OUTPUT_SIZE: usize = FFT_SIZE / 2 + 1;
const RING_BUFFER_SIZE: usize = 256;
const FRAME_SAMPLES: usize = 128;
const MAX_PEAKS: usize = 255;
const TARGET_SECONDS: f64 = 12.0;
const MAX_INPUT_SECONDS: usize = 30;
const SIGNATURE_PREFIX: &str = "data:audio/vnd.shazam.sig;base64,";
const CAPTURE_POLL_INTERVAL: Duration = Duration::from_millis(10);
const CAPTURE_GRACE_PERIOD: Duration = Duration::from_secs(3);
const DEVICE_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
const SHAZAM_DISCOVERY_ROOT: &str = "https://amp.shazam.com/discovery/v5/en/US/android/-/tag";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecognitionResult {
    pub track_id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub cover_art_url: Option<String>,
    pub genre: Option<String>,
    pub release_date: Option<String>,
    pub label: Option<String>,
    pub shazam_url: Option<String>,
    pub isrc: Option<String>,
    pub youtube_video_id: Option<String>,
}

#[derive(Clone)]
pub struct RecognitionClient {
    http: Arc<dyn HttpClient>,
}

impl RecognitionClient {
    pub fn with_settings(settings: &AppSettings) -> Result<Self> {
        Ok(Self {
            http: build_http_client(
                &settings.proxy,
                concat!("Metrolist-rs/", env!("CARGO_PKG_VERSION")),
            )?,
        })
    }

    pub async fn recognize(
        &self,
        signature: &ShazamSignature,
    ) -> Result<Option<RecognitionResult>> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| AppError::Recognition(error.to_string()))?
            .as_secs();
        let mut url = Url::parse(&format!(
            "{SHAZAM_DISCOVERY_ROOT}/{}/{}",
            random_uuid(true),
            random_uuid(false)
        ))
        .map_err(|error| AppError::Recognition(error.to_string()))?;
        url.query_pairs_mut()
            .append_pair("sync", "true")
            .append_pair("webv3", "true")
            .append_pair("sampling", "true")
            .append_pair("connected", "")
            .append_pair("shazamapiversion", "v3")
            .append_pair("sharehub", "true")
            .append_pair("video", "v3");
        let body = serde_json::to_vec(&serde_json::json!({
            "geolocation": {
                "altitude": 100.0 + fastrand::f64() * 400.0,
                "latitude": -90.0 + fastrand::f64() * 180.0,
                "longitude": -180.0 + fastrand::f64() * 360.0,
            },
            "signature": {
                "samplems": signature.sample_duration().as_millis() as u64,
                "timestamp": timestamp,
                "uri": signature.as_uri(),
            },
            "timestamp": timestamp,
            "timezone": "Asia/Shanghai",
        }))
        .map_err(|error| AppError::Recognition(error.to_string()))?;
        let request = Request::builder()
            .method("POST")
            .uri(url.as_str())
            .header("Accept", "application/json")
            .header("Content-Language", "en_US")
            .header("Content-Type", "application/json")
            .header(
                "User-Agent",
                "Dalvik/2.1.0 (Linux; U; Android 6.0.1; SM-G920F Build/MMB29K)",
            )
            .timeout(Duration::from_secs(30))
            .body(AsyncBody::from(body))
            .map_err(|error| AppError::Recognition(error.to_string()))?;
        let mut response = self
            .http
            .send(request)
            .await
            .map_err(|error| AppError::Recognition(error.to_string()))?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(AppError::Recognition(format!(
                "Shazam returned HTTP {}",
                response.status()
            )));
        }
        let mut body = Vec::new();
        response
            .body_mut()
            .read_to_end(&mut body)
            .await
            .map_err(|error| AppError::Recognition(error.to_string()))?;
        parse_recognition_response(&body)
    }
}

fn random_uuid(uppercase: bool) -> String {
    let mut bytes = [0_u8; 16];
    for byte in &mut bytes {
        *byte = fastrand::u8(..);
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let value = format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    );
    if uppercase {
        value.to_uppercase()
    } else {
        value
    }
}

fn parse_recognition_response(body: &[u8]) -> Result<Option<RecognitionResult>> {
    let root: Value =
        serde_json::from_slice(body).map_err(|error| AppError::Recognition(error.to_string()))?;
    let Some(track) = root.get("track").filter(|track| track.is_object()) else {
        return Ok(None);
    };
    let title = value_string(track, "title").unwrap_or_default();
    let artist = value_string(track, "subtitle").unwrap_or_default();
    if title.is_empty() || artist.is_empty() {
        return Err(AppError::Recognition(
            "Shazam returned an incomplete track".into(),
        ));
    }
    let sections = track
        .get("sections")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    let mut album = None;
    let mut label = None;
    let mut release_date = None;
    for section in sections {
        if section.get("type").and_then(Value::as_str) != Some("SONG") {
            continue;
        }
        for metadata in section
            .get("metadata")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let title = metadata.get("title").and_then(Value::as_str);
            let text = metadata
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_owned);
            match title {
                Some("Album") => album = text,
                Some("Label") => label = text,
                Some("Released") => release_date = text,
                _ => {}
            }
        }
    }
    let youtube_video_id = track
        .pointer("/hub/options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|option| {
            option
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.to_ascii_lowercase().contains("video"))
        })
        .and_then(|option| option.get("actions"))
        .and_then(Value::as_array)
        .and_then(|actions| actions.first())
        .and_then(|action| action.get("uri"))
        .and_then(Value::as_str)
        .and_then(youtube_video_id);
    Ok(Some(RecognitionResult {
        track_id: value_string(track, "key")
            .or_else(|| value_string(&root, "tagid"))
            .unwrap_or_default(),
        title,
        artist,
        album,
        cover_art_url: track
            .pointer("/images/coverarthq")
            .or_else(|| track.pointer("/images/coverart"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        genre: track
            .pointer("/genres/primary")
            .and_then(Value::as_str)
            .map(str::to_owned),
        release_date,
        label,
        shazam_url: value_string(track, "url"),
        isrc: value_string(track, "isrc"),
        youtube_video_id,
    }))
}

fn value_string(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn youtube_video_id(uri: &str) -> Option<String> {
    let url = Url::parse(uri).ok()?;
    url.query_pairs()
        .find_map(|(key, value)| (key == "v").then(|| value.into_owned()))
        .or_else(|| {
            url.path_segments()?
                .next_back()
                .filter(|value| value.len() == 11)
                .map(str::to_owned)
        })
}

pub trait MicrophoneRecorder: Send + Sync {
    fn start(&self, duration: Duration) -> Result<MicrophoneCapture>;
}

#[derive(Debug, Default)]
pub struct SystemMicrophoneRecorder;

impl MicrophoneRecorder for SystemMicrophoneRecorder {
    fn start(&self, duration: Duration) -> Result<MicrophoneCapture> {
        validate_capture_duration(duration)?;
        let cancellation = MicrophoneCancellation::default();
        let worker_cancellation = cancellation.clone();
        let (sender, receiver) = oneshot::channel();
        thread::Builder::new()
            .name("metrolist-microphone-capture".into())
            .spawn(move || {
                let result = capture_system_microphone(duration, &worker_cancellation);
                let _ = sender.send(result);
            })
            .map_err(|error| {
                AppError::Recognition(format!("microphone worker could not start: {error}"))
            })?;
        Ok(MicrophoneCapture::new(cancellation, receiver))
    }
}

#[derive(Clone, Default)]
pub struct MicrophoneCancellation {
    cancelled: Arc<AtomicBool>,
}

impl MicrophoneCancellation {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

impl fmt::Debug for MicrophoneCancellation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MicrophoneCancellation")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

pub struct MicrophoneCapture {
    cancellation: MicrophoneCancellation,
    receiver: Option<oneshot::Receiver<Result<RecordedPcm>>>,
}

impl MicrophoneCapture {
    fn new(
        cancellation: MicrophoneCancellation,
        receiver: oneshot::Receiver<Result<RecordedPcm>>,
    ) -> Self {
        Self {
            cancellation,
            receiver: Some(receiver),
        }
    }

    pub fn cancellation(&self) -> MicrophoneCancellation {
        self.cancellation.clone()
    }

    pub async fn finish(mut self) -> Result<RecordedPcm> {
        let receiver = self.receiver.take().ok_or_else(|| {
            AppError::Recognition("microphone result was already consumed".into())
        })?;
        receiver.await.map_err(|_| {
            AppError::Recognition("microphone worker stopped before returning audio".into())
        })?
    }
}

impl fmt::Debug for MicrophoneCapture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MicrophoneCapture")
            .field("cancelled", &self.cancellation.is_cancelled())
            .field("has_pending_result", &self.receiver.is_some())
            .finish()
    }
}

impl Drop for MicrophoneCapture {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

pub struct RecordedPcm {
    samples: Vec<i16>,
    sample_rate: u32,
    source_name: String,
}

impl RecordedPcm {
    fn new(samples: Vec<i16>, sample_rate: u32, source_name: String) -> Self {
        Self {
            samples,
            sample_rate,
            source_name,
        }
    }

    pub fn samples(&self) -> &[i16] {
        &self.samples
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.samples.len() as f64 / self.sample_rate as f64)
    }

    pub fn into_parts(self) -> (Vec<i16>, u32, String) {
        (self.samples, self.sample_rate, self.source_name)
    }
}

impl fmt::Debug for RecordedPcm {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RecordedPcm")
            .field("samples", &"[redacted]")
            .field("sample_count", &self.samples.len())
            .field("sample_rate", &self.sample_rate)
            .field("source_name", &self.source_name)
            .finish()
    }
}

fn validate_capture_duration(duration: Duration) -> Result<()> {
    if duration.is_zero() || duration > Duration::from_secs(MAX_INPUT_SECONDS as u64) {
        return Err(AppError::Recognition(
            "microphone duration must be between 1 sample and 30 seconds".into(),
        ));
    }
    Ok(())
}

fn capture_system_microphone(
    duration: Duration,
    cancellation: &MicrophoneCancellation,
) -> Result<RecordedPcm> {
    if cancellation.is_cancelled() {
        return Err(capture_cancelled_error());
    }

    let host = cpal::default_host();
    let device = host.default_input_device().ok_or_else(|| {
        AppError::Recognition(
            "no default microphone is available; connect one and grant microphone permission"
                .into(),
        )
    })?;
    let source_name = device
        .description()
        .map(|description| description.name().to_string())
        .unwrap_or_else(|_| "Default microphone".into());
    let supported = device.default_input_config().map_err(|error| {
        AppError::Recognition(format!(
            "default microphone format is unavailable; check microphone permission: {error}"
        ))
    })?;
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let channels = usize::from(config.channels);
    if channels == 0 {
        return Err(AppError::Recognition(
            "default microphone reported zero channels".into(),
        ));
    }
    let sample_rate = config.sample_rate;
    let target_frames = duration_to_frames(duration, sample_rate)?;
    let samples = Arc::new(Mutex::new(Vec::with_capacity(target_frames)));
    let stream_error = Arc::new(Mutex::new(None));

    macro_rules! build_stream {
        ($sample:ty) => {
            build_capture_stream::<$sample>(
                &device,
                &config,
                channels,
                target_frames,
                samples.clone(),
                stream_error.clone(),
                cancellation.clone(),
            )
        };
    }

    let stream = match sample_format {
        SampleFormat::I8 => build_stream!(i8),
        SampleFormat::I16 => build_stream!(i16),
        SampleFormat::I24 => build_stream!(cpal::I24),
        SampleFormat::I32 => build_stream!(i32),
        SampleFormat::I64 => build_stream!(i64),
        SampleFormat::U8 => build_stream!(u8),
        SampleFormat::U16 => build_stream!(u16),
        SampleFormat::U24 => build_stream!(cpal::U24),
        SampleFormat::U32 => build_stream!(u32),
        SampleFormat::U64 => build_stream!(u64),
        SampleFormat::F32 => build_stream!(f32),
        SampleFormat::F64 => build_stream!(f64),
        SampleFormat::DsdU8 | SampleFormat::DsdU16 | SampleFormat::DsdU32 => {
            return Err(AppError::Recognition(format!(
                "default microphone uses unsupported DSD format {sample_format}"
            )));
        }
        _ => {
            return Err(AppError::Recognition(format!(
                "default microphone uses unsupported sample format {sample_format}"
            )));
        }
    }
    .map_err(|error| {
        AppError::Recognition(format!(
            "microphone stream could not be opened; check microphone permission: {error}"
        ))
    })?;
    stream.play().map_err(|error| {
        AppError::Recognition(format!(
            "microphone stream could not start; check microphone permission: {error}"
        ))
    })?;

    let deadline = Instant::now() + duration + CAPTURE_GRACE_PERIOD;
    loop {
        if cancellation.is_cancelled() {
            return Err(capture_cancelled_error());
        }
        if let Some(error) = stream_error
            .lock()
            .map_err(|_| AppError::Recognition("microphone error state was poisoned".into()))?
            .take()
        {
            return Err(AppError::Recognition(format!(
                "microphone stopped while recording: {error}"
            )));
        }
        let complete = samples
            .lock()
            .map_err(|_| AppError::Recognition("microphone sample buffer was poisoned".into()))?
            .len()
            >= target_frames;
        if complete {
            break;
        }
        if Instant::now() >= deadline {
            return Err(AppError::Recognition(
                "microphone did not provide audio before the capture deadline".into(),
            ));
        }
        thread::sleep(CAPTURE_POLL_INTERVAL);
    }
    drop(stream);

    let mut samples = Arc::try_unwrap(samples)
        .map_err(|_| AppError::Recognition("microphone sample buffer is still in use".into()))?
        .into_inner()
        .map_err(|_| AppError::Recognition("microphone sample buffer was poisoned".into()))?;
    samples.truncate(target_frames);
    Ok(RecordedPcm::new(samples, sample_rate, source_name))
}

fn duration_to_frames(duration: Duration, sample_rate: u32) -> Result<usize> {
    let frames = duration
        .as_nanos()
        .checked_mul(u128::from(sample_rate))
        .and_then(|value| value.checked_div(1_000_000_000))
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| AppError::Recognition("microphone sample count overflowed".into()))?;
    if frames == 0 {
        return Err(AppError::Recognition(
            "microphone duration is shorter than one sample".into(),
        ));
    }
    Ok(frames)
}

fn build_capture_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    target_frames: usize,
    samples: Arc<Mutex<Vec<i16>>>,
    stream_error: Arc<Mutex<Option<String>>>,
    cancellation: MicrophoneCancellation,
) -> std::result::Result<cpal::Stream, cpal::BuildStreamError>
where
    T: cpal::SizedSample + cpal::Sample,
    i16: cpal::FromSample<T>,
{
    let callback_error = stream_error.clone();
    device.build_input_stream(
        config,
        move |input: &[T], _| {
            if cancellation.is_cancelled() {
                return;
            }
            match samples.lock() {
                Ok(mut output) => {
                    append_mono_samples(input, channels, target_frames, &mut output);
                }
                Err(_) => {
                    if let Ok(mut error) = callback_error.lock()
                        && error.is_none()
                    {
                        *error = Some("microphone sample buffer became unavailable".into());
                    }
                }
            }
        },
        move |error| {
            if let Ok(mut slot) = stream_error.lock()
                && slot.is_none()
            {
                *slot = Some(error.to_string());
            }
        },
        Some(DEVICE_OPERATION_TIMEOUT),
    )
}

fn append_mono_samples<T>(input: &[T], channels: usize, target_frames: usize, output: &mut Vec<i16>)
where
    T: cpal::Sample,
    i16: cpal::FromSample<T>,
{
    if channels == 0 || output.len() >= target_frames {
        return;
    }
    for frame in input.chunks_exact(channels) {
        if output.len() >= target_frames {
            break;
        }
        let sum = frame
            .iter()
            .copied()
            .map(|sample| i64::from(sample.to_sample::<i16>()))
            .sum::<i64>();
        output.push((sum / channels as i64) as i16);
    }
}

fn capture_cancelled_error() -> AppError {
    AppError::Recognition("microphone capture was cancelled".into())
}

#[derive(Clone, PartialEq, Eq)]
pub struct ShazamSignature {
    uri: String,
    sample_count: u32,
    peak_count: usize,
}

impl ShazamSignature {
    pub fn as_uri(&self) -> &str {
        &self.uri
    }

    pub fn into_uri(self) -> String {
        self.uri
    }

    pub fn sample_count(&self) -> u32 {
        self.sample_count
    }

    pub fn sample_duration(&self) -> Duration {
        Duration::from_secs_f64(self.sample_count as f64 / RECOGNITION_SAMPLE_RATE as f64)
    }

    pub fn peak_count(&self) -> usize {
        self.peak_count
    }
}

impl fmt::Debug for ShazamSignature {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ShazamSignature")
            .field("uri", &"[redacted]")
            .field("sample_count", &self.sample_count)
            .field("peak_count", &self.peak_count)
            .finish()
    }
}

pub fn linear_resample_mono_i16(
    samples: &[i16],
    input_sample_rate: u32,
    output_sample_rate: u32,
) -> Result<Vec<i16>> {
    validate_pcm(samples, input_sample_rate)?;
    if output_sample_rate == 0 || output_sample_rate > 384_000 {
        return Err(AppError::Protocol(
            "recognition output sample rate is outside the supported range".into(),
        ));
    }
    if input_sample_rate == output_sample_rate {
        return Ok(samples.to_vec());
    }

    let ratio = output_sample_rate as f64 / input_sample_rate as f64;
    let output_len = (samples.len() as f64 * ratio) as usize;
    let max_output = output_sample_rate as usize * MAX_INPUT_SECONDS;
    if output_len == 0 || output_len > max_output {
        return Err(AppError::Protocol(
            "recognition resample output is outside the supported duration".into(),
        ));
    }

    let mut output = Vec::with_capacity(output_len);
    for index in 0..output_len {
        let source_position = index as f64 / ratio;
        let source_index = source_position as usize;
        let fraction = source_position - source_index as f64;
        let sample = if let Some(next) = samples.get(source_index + 1) {
            (samples[source_index] as f64 * (1.0 - fraction) + *next as f64 * fraction) as i16
        } else {
            samples[source_index]
        };
        output.push(sample);
    }
    Ok(output)
}

pub fn generate_shazam_signature(samples: &[i16]) -> Result<ShazamSignature> {
    validate_pcm(samples, RECOGNITION_SAMPLE_RATE)?;
    if samples.len() < FRAME_SAMPLES {
        return Err(AppError::Protocol(
            "recognition PCM must contain at least 128 samples".into(),
        ));
    }
    SignatureGenerator::new().process(samples)
}

fn validate_pcm(samples: &[i16], sample_rate: u32) -> Result<()> {
    if samples.is_empty() {
        return Err(AppError::Protocol("recognition PCM cannot be empty".into()));
    }
    if sample_rate == 0 || sample_rate > 384_000 {
        return Err(AppError::Protocol(
            "recognition input sample rate is outside the supported range".into(),
        ));
    }
    if samples.len() > sample_rate as usize * MAX_INPUT_SECONDS {
        return Err(AppError::Protocol(
            "recognition PCM exceeds the 30 second safety limit".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct FrequencyPeak {
    fft_pass_number: u32,
    peak_magnitude: i32,
    corrected_frequency_bin: i32,
}

struct SignatureGenerator {
    samples_ring: Vec<i32>,
    samples_position: usize,
    fft_outputs: Vec<Vec<f64>>,
    fft_position: usize,
    spread_ffts: Vec<Vec<f64>>,
    spread_position: usize,
    spread_written: u32,
    sample_count: u32,
    band_peaks: [Vec<FrequencyPeak>; 4],
    peak_count: usize,
    hanning: Vec<f64>,
}

impl SignatureGenerator {
    fn new() -> Self {
        let hanning = (0..FFT_SIZE)
            .map(|index| {
                0.5 * (1.0 - (2.0 * std::f64::consts::PI * (index + 1) as f64 / 2_049.0).cos())
            })
            .collect();
        Self {
            samples_ring: vec![0; FFT_SIZE],
            samples_position: 0,
            fft_outputs: vec![vec![0.0; FFT_OUTPUT_SIZE]; RING_BUFFER_SIZE],
            fft_position: 0,
            spread_ffts: vec![vec![0.0; FFT_OUTPUT_SIZE]; RING_BUFFER_SIZE],
            spread_position: 0,
            spread_written: 0,
            sample_count: 0,
            band_peaks: std::array::from_fn(|_| Vec::new()),
            peak_count: 0,
            hanning,
        }
    }

    fn process(mut self, pcm: &[i16]) -> Result<ShazamSignature> {
        let mut offset = 0;
        while offset + FRAME_SAMPLES <= pcm.len() {
            let elapsed_seconds = self.sample_count as f64 / RECOGNITION_SAMPLE_RATE as f64;
            if elapsed_seconds >= TARGET_SECONDS && self.peak_count >= MAX_PEAKS {
                break;
            }
            self.sample_count = self
                .sample_count
                .checked_add(FRAME_SAMPLES as u32)
                .ok_or_else(|| AppError::Protocol("recognition sample count overflowed".into()))?;
            self.feed_samples(&pcm[offset..offset + FRAME_SAMPLES]);
            self.perform_fft();
            self.spread_and_recognize();
            offset += FRAME_SAMPLES;
        }
        self.encode()
    }

    fn feed_samples(&mut self, samples: &[i16]) {
        for sample in samples {
            self.samples_ring[self.samples_position] = *sample as i32;
            self.samples_position = (self.samples_position + 1) % FFT_SIZE;
        }
    }

    fn perform_fft(&mut self) {
        let windowed = (0..FFT_SIZE)
            .map(|index| {
                self.samples_ring[(self.samples_position + index) % FFT_SIZE] as f64
                    * self.hanning[index]
            })
            .collect::<Vec<_>>();
        self.fft_outputs[self.fft_position] = compute_real_fft_magnitudes(&windowed);
        self.fft_position = (self.fft_position + 1) % RING_BUFFER_SIZE;
    }

    fn spread_and_recognize(&mut self) {
        self.spread_peaks();
        if self.spread_written >= 47 {
            self.recognize_peaks();
        }
    }

    fn spread_peaks(&mut self) {
        let last_fft = (self.fft_position + RING_BUFFER_SIZE - 1) % RING_BUFFER_SIZE;
        let mut spread = self.fft_outputs[last_fft].clone();
        for position in 0..FFT_OUTPUT_SIZE - 2 {
            spread[position] = spread[position]
                .max(spread[position + 1])
                .max(spread[position + 2]);
        }

        for (position, current) in spread.iter().copied().enumerate() {
            let mut maximum = current;
            for offset in [-1, -3, -6] {
                let index = ring_index(self.spread_position, offset);
                maximum = maximum.max(self.spread_ffts[index][position]);
                self.spread_ffts[index][position] = maximum;
            }
        }

        self.spread_ffts[self.spread_position] = spread;
        self.spread_position = (self.spread_position + 1) % RING_BUFFER_SIZE;
        self.spread_written += 1;
    }

    fn recognize_peaks(&mut self) {
        const SPREAD_NEIGHBORS: [isize; 8] = [-10, -7, -4, -3, 1, 2, 5, 8];
        const OTHER_OFFSETS: [isize; 14] = [
            -53, -45, 165, 172, 179, 186, 193, 200, 214, 221, 228, 235, 242, 249,
        ];
        let fft_minus_46 = self.fft_outputs[ring_index(self.fft_position, -46)].clone();
        let spread_minus_49 = self.spread_ffts[ring_index(self.spread_position, -49)].clone();

        for bin_position in 10..FFT_OUTPUT_SIZE - 8 {
            let fft_value = fft_minus_46[bin_position];
            if fft_value < 1.0 / 64.0 || fft_value < spread_minus_49[bin_position] {
                continue;
            }

            let mut maximum_neighbor: f64 = 0.0;
            for offset in SPREAD_NEIGHBORS {
                maximum_neighbor = maximum_neighbor
                    .max(spread_minus_49[(bin_position as isize + offset) as usize]);
            }
            if fft_value <= maximum_neighbor {
                continue;
            }

            for offset in OTHER_OFFSETS {
                maximum_neighbor = maximum_neighbor.max(
                    self.spread_ffts[ring_index(self.spread_position, offset)][bin_position - 1],
                );
            }
            if fft_value <= maximum_neighbor {
                continue;
            }

            let peak_magnitude = fft_value.max(1.0 / 64.0).ln() * 1_477.3 + 6_144.0;
            let before = fft_minus_46[bin_position - 1].max(1.0 / 64.0).ln() * 1_477.3 + 6_144.0;
            let after = fft_minus_46[bin_position + 1].max(1.0 / 64.0).ln() * 1_477.3 + 6_144.0;
            let variation_1 = peak_magnitude * 2.0 - before - after;
            let variation_2 = (after - before) * 32.0 / variation_1;
            let corrected_bin = bin_position as f64 * 64.0 + variation_2;
            let frequency_hz = corrected_bin * (16_000.0 / 2.0 / 1_024.0 / 64.0);
            let band = match frequency_hz {
                value if value < 250.0 => continue,
                value if value < 520.0 => 0,
                value if value < 1_450.0 => 1,
                value if value < 3_500.0 => 2,
                value if value <= 5_500.0 => 3,
                _ => continue,
            };
            self.band_peaks[band].push(FrequencyPeak {
                fft_pass_number: self.spread_written - 46,
                peak_magnitude: peak_magnitude as i32,
                corrected_frequency_bin: corrected_bin as i32,
            });
            self.peak_count += 1;
        }
    }

    fn encode(self) -> Result<ShazamSignature> {
        let mut contents = Vec::new();
        for (band, peaks) in self.band_peaks.iter().enumerate() {
            if peaks.is_empty() {
                continue;
            }
            let mut encoded_peaks = Vec::new();
            let mut previous_fft_pass = 0;
            for peak in peaks {
                let difference = peak.fft_pass_number - previous_fft_pass;
                if difference >= 255 {
                    encoded_peaks.push(0xff);
                    write_u32_le(&mut encoded_peaks, peak.fft_pass_number);
                    previous_fft_pass = peak.fft_pass_number;
                }
                encoded_peaks.push((peak.fft_pass_number - previous_fft_pass) as u8);
                write_u16_le(&mut encoded_peaks, peak.peak_magnitude as u16);
                write_u16_le(&mut encoded_peaks, peak.corrected_frequency_bin as u16);
                previous_fft_pass = peak.fft_pass_number;
            }
            write_u32_le(&mut contents, 0x6003_0040 + band as u32);
            write_u32_le(
                &mut contents,
                u32::try_from(encoded_peaks.len()).map_err(|_| {
                    AppError::Protocol("recognition peak payload is too large".into())
                })?,
            );
            contents.extend_from_slice(&encoded_peaks);
            contents.resize(contents.len().next_multiple_of(4), 0);
        }

        let contents_len = u32::try_from(contents.len())
            .map_err(|_| AppError::Protocol("recognition signature is too large".into()))?;
        let mut bytes = Vec::with_capacity(56 + contents.len());
        write_u32_le(&mut bytes, 0xcafe_2580);
        write_u32_le(&mut bytes, 0);
        write_u32_le(&mut bytes, contents_len + 8);
        write_u32_le(&mut bytes, 0x9411_9c00);
        for _ in 0..3 {
            write_u32_le(&mut bytes, 0);
        }
        write_u32_le(&mut bytes, 3 << 27);
        write_u32_le(&mut bytes, 0);
        write_u32_le(&mut bytes, 0);
        write_u32_le(&mut bytes, self.sample_count + 3_840);
        write_u32_le(&mut bytes, (15 << 19) + 0x4_0000);
        write_u32_le(&mut bytes, 0x4000_0000);
        write_u32_le(&mut bytes, contents_len + 8);
        bytes.extend_from_slice(&contents);

        let checksum = crc32fast::hash(&bytes[8..]);
        bytes[4..8].copy_from_slice(&checksum.to_le_bytes());
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        Ok(ShazamSignature {
            uri: format!("{SIGNATURE_PREFIX}{encoded}"),
            sample_count: self.sample_count,
            peak_count: self.peak_count,
        })
    }
}

fn ring_index(position: usize, offset: isize) -> usize {
    (position as isize + offset).rem_euclid(RING_BUFFER_SIZE as isize) as usize
}

fn compute_real_fft_magnitudes(windowed: &[f64]) -> Vec<f64> {
    debug_assert_eq!(windowed.len(), FFT_SIZE);
    let mut real = windowed.to_vec();
    let mut imaginary = vec![0.0; FFT_SIZE];

    let mut reversed = 0;
    for index in 1..FFT_SIZE {
        let mut bit = FFT_SIZE >> 1;
        while reversed & bit != 0 {
            reversed ^= bit;
            bit >>= 1;
        }
        reversed ^= bit;
        if index < reversed {
            real.swap(index, reversed);
            imaginary.swap(index, reversed);
        }
    }

    let mut length = 2;
    while length <= FFT_SIZE {
        let half = length >> 1;
        let angle = -std::f64::consts::PI / half as f64;
        let base_real = angle.cos();
        let base_imaginary = angle.sin();
        for start in (0..FFT_SIZE).step_by(length) {
            let mut weight_real = 1.0;
            let mut weight_imaginary = 0.0;
            for offset in 0..half {
                let even = start + offset;
                let odd = even + half;
                let even_real = real[even];
                let even_imaginary = imaginary[even];
                let odd_real = real[odd] * weight_real - imaginary[odd] * weight_imaginary;
                let odd_imaginary = real[odd] * weight_imaginary + imaginary[odd] * weight_real;
                real[even] = even_real + odd_real;
                imaginary[even] = even_imaginary + odd_imaginary;
                real[odd] = even_real - odd_real;
                imaginary[odd] = even_imaginary - odd_imaginary;
                let next_weight_real = weight_real * base_real - weight_imaginary * base_imaginary;
                weight_imaginary = weight_real * base_imaginary + weight_imaginary * base_real;
                weight_real = next_weight_real;
            }
        }
        length <<= 1;
    }

    (0..FFT_OUTPUT_SIZE)
        .map(|index| {
            ((real[index] * real[index] + imaginary[index] * imaginary[index]) / 131_072.0)
                .max(1e-10)
        })
        .collect()
}

fn write_u16_le(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn write_u32_le(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeMicrophoneRecorder;

    impl MicrophoneRecorder for FakeMicrophoneRecorder {
        fn start(&self, duration: Duration) -> Result<MicrophoneCapture> {
            validate_capture_duration(duration)?;
            let cancellation = MicrophoneCancellation::default();
            let (sender, receiver) = oneshot::channel();
            sender
                .send(Ok(RecordedPcm::new(
                    vec![1_234; 128],
                    44_100,
                    "Test microphone".into(),
                )))
                .unwrap();
            Ok(MicrophoneCapture::new(cancellation, receiver))
        }
    }

    fn decode(signature: &ShazamSignature) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode(signature.as_uri().strip_prefix(SIGNATURE_PREFIX).unwrap())
            .unwrap()
    }

    fn u32_at(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    #[test]
    fn linear_resampler_matches_android_interpolation_and_validates_bounds() {
        assert_eq!(
            linear_resample_mono_i16(&[0, 1_000, 2_000, 3_000], 4, 8).unwrap(),
            [0, 500, 1_000, 1_500, 2_000, 2_500, 3_000, 3_000]
        );
        assert!(linear_resample_mono_i16(&[], 44_100, 16_000).is_err());
        assert!(linear_resample_mono_i16(&[0], 0, 16_000).is_err());
    }

    #[test]
    fn interleaved_capture_is_mixed_to_mono_and_stops_at_the_target() {
        let mut signed = Vec::new();
        append_mono_samples(
            &[-1_000_i16, 1_000, 2_000, 4_000, 10_000, 12_000],
            2,
            2,
            &mut signed,
        );
        assert_eq!(signed, [0, 3_000]);

        let mut unsigned = Vec::new();
        append_mono_samples(
            &[32_768_u16, 32_768, u16::MAX, u16::MAX],
            2,
            2,
            &mut unsigned,
        );
        assert_eq!(unsigned, [0, i16::MAX]);
    }

    #[test]
    fn microphone_boundary_is_injectable_cancellable_and_redacts_pcm() {
        let recorder: Arc<dyn MicrophoneRecorder> = Arc::new(FakeMicrophoneRecorder);
        let capture = recorder.start(Duration::from_millis(10)).unwrap();
        let cancellation = capture.cancellation();
        assert!(!cancellation.is_cancelled());
        let recording = futures::executor::block_on(capture.finish()).unwrap();
        assert_eq!(recording.sample_rate(), 44_100);
        assert_eq!(recording.source_name(), "Test microphone");
        assert_eq!(recording.samples().len(), 128);
        assert!(!format!("{recording:?}").contains("1234"));

        let cancellation = MicrophoneCancellation::default();
        let (_sender, receiver) = oneshot::channel();
        let capture = MicrophoneCapture::new(cancellation.clone(), receiver);
        drop(capture);
        assert!(cancellation.is_cancelled());
    }

    #[test]
    fn capture_duration_is_bounded_and_converts_to_exact_frames() {
        assert!(validate_capture_duration(Duration::ZERO).is_err());
        assert!(validate_capture_duration(Duration::from_secs(31)).is_err());
        assert_eq!(
            duration_to_frames(RECOGNITION_CAPTURE_DURATION, 44_100).unwrap(),
            529_200
        );
        assert!(duration_to_frames(Duration::from_nanos(1), 16_000).is_err());
    }

    #[test]
    fn silence_signature_has_android_header_crc_and_redacted_debug_output() {
        let signature =
            generate_shazam_signature(&vec![0; RECOGNITION_SAMPLE_RATE as usize]).unwrap();
        assert_eq!(signature.sample_count(), RECOGNITION_SAMPLE_RATE);
        assert_eq!(signature.sample_duration(), Duration::from_secs(1));
        assert_eq!(signature.peak_count(), 0);
        assert!(!format!("{signature:?}").contains(signature.as_uri()));

        let bytes = decode(&signature);
        assert_eq!(bytes.len(), 56);
        assert_eq!(u32_at(&bytes, 0), 0xcafe_2580);
        assert_eq!(u32_at(&bytes, 8), 8);
        assert_eq!(u32_at(&bytes, 12), 0x9411_9c00);
        assert_eq!(u32_at(&bytes, 28), 3 << 27);
        assert_eq!(u32_at(&bytes, 40), RECOGNITION_SAMPLE_RATE + 3_840);
        assert_eq!(u32_at(&bytes, 48), 0x4000_0000);
        assert_eq!(u32_at(&bytes, 52), 8);
        assert_eq!(u32_at(&bytes, 4), crc32fast::hash(&bytes[8..]));
    }

    #[test]
    fn deterministic_synthetic_audio_produces_bounded_frequency_peaks() {
        let mut state = 0x1234_5678_u32;
        let samples = (0..RECOGNITION_SAMPLE_RATE as usize * 3)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state >> 16) as i16
            })
            .collect::<Vec<_>>();
        let first = generate_shazam_signature(&samples).unwrap();
        let second = generate_shazam_signature(&samples).unwrap();
        assert_eq!(first, second);
        assert!(first.peak_count() > 0);
        assert!(first.peak_count() < 100_000);
        let bytes = decode(&first);
        assert_eq!(u32_at(&bytes, 4), crc32fast::hash(&bytes[8..]));
        assert_eq!(u32_at(&bytes, 8) as usize, bytes.len() - 48);
    }

    #[test]
    fn twelve_second_android_capture_shape_resamples_and_signs_offline() {
        let mut state = 0x9e37_79b9_u32;
        let captured = (0..44_100_usize * 12)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 16) as i16
            })
            .collect::<Vec<_>>();
        let resampled =
            linear_resample_mono_i16(&captured, 44_100, RECOGNITION_SAMPLE_RATE).unwrap();
        assert_eq!(resampled.len(), RECOGNITION_SAMPLE_RATE as usize * 12);

        let signature = generate_shazam_signature(&resampled).unwrap();
        assert_eq!(signature.sample_duration(), Duration::from_secs(12));
        assert!(signature.peak_count() >= MAX_PEAKS);
        assert!(signature.as_uri().starts_with(SIGNATURE_PREFIX));
    }
}
