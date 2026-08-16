pub mod app;
pub mod config;
pub mod domain;
pub mod error;
pub mod services;
pub mod storage;
pub mod ui;

pub use app::{AppModel, Route};
pub use config::{
    AppConfig, AppSettings, AppTheme, AudioQuality, EQUALIZER_BAND_COUNT, EQUALIZER_FREQUENCIES_HZ,
    EqualizerPreset, EqualizerProfile, EqualizerSettings, ListenTogetherSettings, LoudnessLevel,
    MAX_EQUALIZER_GAIN_MB, MAX_PARAMETRIC_EQUALIZER_BANDS, MAX_PARAMETRIC_FREQUENCY_MILLIHZ,
    MAX_PARAMETRIC_GAIN_MB, MAX_PARAMETRIC_PREAMP_MB, MAX_PARAMETRIC_Q_MILLI,
    MAX_PLAYBACK_RATE_MILLI, MAX_TRANSPOSE_SEMITONES, MIN_EQUALIZER_GAIN_MB,
    MIN_PARAMETRIC_GAIN_MB, MIN_PARAMETRIC_PREAMP_MB, MIN_PLAYBACK_RATE_MILLI,
    MIN_TRANSPOSE_SEMITONES, PLAYBACK_RATE_STEP_MILLI, ParametricEqualizer,
    ParametricEqualizerBand, ParametricFilterType, PlaybackParameters, ProxyKind, ProxySettings,
};
pub use error::{AppError, Result};
