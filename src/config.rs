use std::{fmt, path::PathBuf};

use http_client::Url;
use serde::{Deserialize, Serialize};

use crate::{
    AppError, Result, Route,
    services::{
        DEFAULT_AUDIO_CACHE_BYTES, DEFAULT_LISTEN_TOGETHER_SERVER_URL, LastFmScrobblePolicy,
    },
};

pub const MIN_WINDOW_WIDTH: f32 = 720.0;
pub const MIN_WINDOW_HEIGHT: f32 = 520.0;
pub const MIN_AUDIO_CACHE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_AUDIO_CACHE_BYTES: u64 = 64 * 1024 * 1024 * 1024;
pub const MIN_HISTORY_DURATION_SECONDS: u16 = 1;
pub const MAX_HISTORY_DURATION_SECONDS: u16 = 100;
pub const DEFAULT_HISTORY_DURATION_SECONDS: u16 = 30;
pub const MIN_CROSSFADE_SECONDS: u8 = 1;
pub const MAX_CROSSFADE_SECONDS: u8 = 15;
pub const DEFAULT_CROSSFADE_SECONDS: u8 = 5;
pub const SYSTEM_CONTENT_LOCALE: &str = "system";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentLocaleOption {
    pub code: &'static str,
    pub name: &'static str,
}

pub const CONTENT_LANGUAGES: &[ContentLocaleOption] = &[
    ContentLocaleOption {
        code: "af",
        name: "Afrikaans",
    },
    ContentLocaleOption {
        code: "az",
        name: "Azərbaycan",
    },
    ContentLocaleOption {
        code: "id",
        name: "Bahasa Indonesia",
    },
    ContentLocaleOption {
        code: "ms",
        name: "Bahasa Malaysia",
    },
    ContentLocaleOption {
        code: "ca",
        name: "Català",
    },
    ContentLocaleOption {
        code: "cs",
        name: "Čeština",
    },
    ContentLocaleOption {
        code: "da",
        name: "Dansk",
    },
    ContentLocaleOption {
        code: "de",
        name: "Deutsch",
    },
    ContentLocaleOption {
        code: "et",
        name: "Eesti",
    },
    ContentLocaleOption {
        code: "en-GB",
        name: "English (UK)",
    },
    ContentLocaleOption {
        code: "en",
        name: "English (US)",
    },
    ContentLocaleOption {
        code: "es",
        name: "Español (España)",
    },
    ContentLocaleOption {
        code: "es-419",
        name: "Español (Latinoamérica)",
    },
    ContentLocaleOption {
        code: "eu",
        name: "Euskara",
    },
    ContentLocaleOption {
        code: "fil",
        name: "Filipino",
    },
    ContentLocaleOption {
        code: "fr",
        name: "Français",
    },
    ContentLocaleOption {
        code: "fr-CA",
        name: "Français (Canada)",
    },
    ContentLocaleOption {
        code: "gl",
        name: "Galego",
    },
    ContentLocaleOption {
        code: "hr",
        name: "Hrvatski",
    },
    ContentLocaleOption {
        code: "zu",
        name: "IsiZulu",
    },
    ContentLocaleOption {
        code: "is",
        name: "Íslenska",
    },
    ContentLocaleOption {
        code: "it",
        name: "Italiano",
    },
    ContentLocaleOption {
        code: "sw",
        name: "Kiswahili",
    },
    ContentLocaleOption {
        code: "lt",
        name: "Lietuvių",
    },
    ContentLocaleOption {
        code: "hu",
        name: "Magyar",
    },
    ContentLocaleOption {
        code: "nl",
        name: "Nederlands",
    },
    ContentLocaleOption {
        code: "no",
        name: "Norsk",
    },
    ContentLocaleOption {
        code: "or",
        name: "Odia",
    },
    ContentLocaleOption {
        code: "uz",
        name: "O‘zbe",
    },
    ContentLocaleOption {
        code: "pl",
        name: "Polski",
    },
    ContentLocaleOption {
        code: "pt-PT",
        name: "Português",
    },
    ContentLocaleOption {
        code: "pt",
        name: "Português (Brasil)",
    },
    ContentLocaleOption {
        code: "ro",
        name: "Română",
    },
    ContentLocaleOption {
        code: "sq",
        name: "Shqip",
    },
    ContentLocaleOption {
        code: "sk",
        name: "Slovenčina",
    },
    ContentLocaleOption {
        code: "sl",
        name: "Slovenščina",
    },
    ContentLocaleOption {
        code: "fi",
        name: "Suomi",
    },
    ContentLocaleOption {
        code: "sv",
        name: "Svenska",
    },
    ContentLocaleOption {
        code: "bo",
        name: "Tibetan བོད་སྐད།",
    },
    ContentLocaleOption {
        code: "vi",
        name: "Tiếng Việt",
    },
    ContentLocaleOption {
        code: "tr",
        name: "Türkçe",
    },
    ContentLocaleOption {
        code: "bg",
        name: "Български",
    },
    ContentLocaleOption {
        code: "ky",
        name: "Кыргызча",
    },
    ContentLocaleOption {
        code: "kk",
        name: "Қазақ Тілі",
    },
    ContentLocaleOption {
        code: "mk",
        name: "Македонски",
    },
    ContentLocaleOption {
        code: "mn",
        name: "Монгол",
    },
    ContentLocaleOption {
        code: "ru",
        name: "Русский",
    },
    ContentLocaleOption {
        code: "sr",
        name: "Српски",
    },
    ContentLocaleOption {
        code: "uk",
        name: "Українська",
    },
    ContentLocaleOption {
        code: "el",
        name: "Ελληνικά",
    },
    ContentLocaleOption {
        code: "hy",
        name: "Հայերեն",
    },
    ContentLocaleOption {
        code: "iw",
        name: "עברית",
    },
    ContentLocaleOption {
        code: "ur",
        name: "اردو",
    },
    ContentLocaleOption {
        code: "ar",
        name: "العربية",
    },
    ContentLocaleOption {
        code: "fa",
        name: "فارسی",
    },
    ContentLocaleOption {
        code: "ne",
        name: "नेपाली",
    },
    ContentLocaleOption {
        code: "mr",
        name: "मराठी",
    },
    ContentLocaleOption {
        code: "hi",
        name: "हिन्दी",
    },
    ContentLocaleOption {
        code: "bn",
        name: "বাংলা",
    },
    ContentLocaleOption {
        code: "pa",
        name: "ਪੰਜਾਬੀ",
    },
    ContentLocaleOption {
        code: "gu",
        name: "ગુજરાતી",
    },
    ContentLocaleOption {
        code: "ta",
        name: "தமிழ்",
    },
    ContentLocaleOption {
        code: "te",
        name: "తెలుగు",
    },
    ContentLocaleOption {
        code: "kn",
        name: "ಕನ್ನಡ",
    },
    ContentLocaleOption {
        code: "ml",
        name: "മലയാളം",
    },
    ContentLocaleOption {
        code: "si",
        name: "සිංහල",
    },
    ContentLocaleOption {
        code: "th",
        name: "ภาษาไทย",
    },
    ContentLocaleOption {
        code: "lo",
        name: "ລາວ",
    },
    ContentLocaleOption {
        code: "my",
        name: "ဗမာ",
    },
    ContentLocaleOption {
        code: "ka",
        name: "ქართული",
    },
    ContentLocaleOption {
        code: "am",
        name: "አማርኛ",
    },
    ContentLocaleOption {
        code: "km",
        name: "ខ្មែរ",
    },
    ContentLocaleOption {
        code: "zh-CN",
        name: "中文 (简体)",
    },
    ContentLocaleOption {
        code: "zh-TW",
        name: "中文 (繁體)",
    },
    ContentLocaleOption {
        code: "zh-HK",
        name: "中文 (香港)",
    },
    ContentLocaleOption {
        code: "ja",
        name: "日本語",
    },
    ContentLocaleOption {
        code: "ko",
        name: "한국어",
    },
];

pub const CONTENT_COUNTRIES: &[ContentLocaleOption] = &[
    ContentLocaleOption {
        code: "DZ",
        name: "Algeria",
    },
    ContentLocaleOption {
        code: "AR",
        name: "Argentina",
    },
    ContentLocaleOption {
        code: "AU",
        name: "Australia",
    },
    ContentLocaleOption {
        code: "AT",
        name: "Austria",
    },
    ContentLocaleOption {
        code: "AZ",
        name: "Azerbaijan",
    },
    ContentLocaleOption {
        code: "BH",
        name: "Bahrain",
    },
    ContentLocaleOption {
        code: "BD",
        name: "Bangladesh",
    },
    ContentLocaleOption {
        code: "BY",
        name: "Belarus",
    },
    ContentLocaleOption {
        code: "BE",
        name: "Belgium",
    },
    ContentLocaleOption {
        code: "BO",
        name: "Bolivia",
    },
    ContentLocaleOption {
        code: "BA",
        name: "Bosnia and Herzegovina",
    },
    ContentLocaleOption {
        code: "BR",
        name: "Brazil",
    },
    ContentLocaleOption {
        code: "BG",
        name: "Bulgaria",
    },
    ContentLocaleOption {
        code: "KH",
        name: "Cambodia",
    },
    ContentLocaleOption {
        code: "CA",
        name: "Canada",
    },
    ContentLocaleOption {
        code: "CL",
        name: "Chile",
    },
    ContentLocaleOption {
        code: "HK",
        name: "Hong Kong",
    },
    ContentLocaleOption {
        code: "CO",
        name: "Colombia",
    },
    ContentLocaleOption {
        code: "CR",
        name: "Costa Rica",
    },
    ContentLocaleOption {
        code: "HR",
        name: "Croatia",
    },
    ContentLocaleOption {
        code: "CY",
        name: "Cyprus",
    },
    ContentLocaleOption {
        code: "CZ",
        name: "Czech Republic",
    },
    ContentLocaleOption {
        code: "DK",
        name: "Denmark",
    },
    ContentLocaleOption {
        code: "DO",
        name: "Dominican Republic",
    },
    ContentLocaleOption {
        code: "EC",
        name: "Ecuador",
    },
    ContentLocaleOption {
        code: "EG",
        name: "Egypt",
    },
    ContentLocaleOption {
        code: "SV",
        name: "El Salvador",
    },
    ContentLocaleOption {
        code: "EE",
        name: "Estonia",
    },
    ContentLocaleOption {
        code: "FI",
        name: "Finland",
    },
    ContentLocaleOption {
        code: "FR",
        name: "France",
    },
    ContentLocaleOption {
        code: "GE",
        name: "Georgia",
    },
    ContentLocaleOption {
        code: "DE",
        name: "Germany",
    },
    ContentLocaleOption {
        code: "GH",
        name: "Ghana",
    },
    ContentLocaleOption {
        code: "GR",
        name: "Greece",
    },
    ContentLocaleOption {
        code: "GT",
        name: "Guatemala",
    },
    ContentLocaleOption {
        code: "HN",
        name: "Honduras",
    },
    ContentLocaleOption {
        code: "HU",
        name: "Hungary",
    },
    ContentLocaleOption {
        code: "IS",
        name: "Iceland",
    },
    ContentLocaleOption {
        code: "IN",
        name: "India",
    },
    ContentLocaleOption {
        code: "ID",
        name: "Indonesia",
    },
    ContentLocaleOption {
        code: "IQ",
        name: "Iraq",
    },
    ContentLocaleOption {
        code: "IE",
        name: "Ireland",
    },
    ContentLocaleOption {
        code: "IL",
        name: "Israel",
    },
    ContentLocaleOption {
        code: "IT",
        name: "Italy",
    },
    ContentLocaleOption {
        code: "JM",
        name: "Jamaica",
    },
    ContentLocaleOption {
        code: "JP",
        name: "Japan",
    },
    ContentLocaleOption {
        code: "JO",
        name: "Jordan",
    },
    ContentLocaleOption {
        code: "KZ",
        name: "Kazakhstan",
    },
    ContentLocaleOption {
        code: "KE",
        name: "Kenya",
    },
    ContentLocaleOption {
        code: "KR",
        name: "South Korea",
    },
    ContentLocaleOption {
        code: "KW",
        name: "Kuwait",
    },
    ContentLocaleOption {
        code: "LA",
        name: "Lao",
    },
    ContentLocaleOption {
        code: "LV",
        name: "Latvia",
    },
    ContentLocaleOption {
        code: "LB",
        name: "Lebanon",
    },
    ContentLocaleOption {
        code: "LY",
        name: "Libya",
    },
    ContentLocaleOption {
        code: "LI",
        name: "Liechtenstein",
    },
    ContentLocaleOption {
        code: "LT",
        name: "Lithuania",
    },
    ContentLocaleOption {
        code: "LU",
        name: "Luxembourg",
    },
    ContentLocaleOption {
        code: "MK",
        name: "Macedonia",
    },
    ContentLocaleOption {
        code: "MY",
        name: "Malaysia",
    },
    ContentLocaleOption {
        code: "MT",
        name: "Malta",
    },
    ContentLocaleOption {
        code: "MX",
        name: "Mexico",
    },
    ContentLocaleOption {
        code: "ME",
        name: "Montenegro",
    },
    ContentLocaleOption {
        code: "MA",
        name: "Morocco",
    },
    ContentLocaleOption {
        code: "NP",
        name: "Nepal",
    },
    ContentLocaleOption {
        code: "NL",
        name: "Netherlands",
    },
    ContentLocaleOption {
        code: "NZ",
        name: "New Zealand",
    },
    ContentLocaleOption {
        code: "NI",
        name: "Nicaragua",
    },
    ContentLocaleOption {
        code: "NG",
        name: "Nigeria",
    },
    ContentLocaleOption {
        code: "NO",
        name: "Norway",
    },
    ContentLocaleOption {
        code: "OM",
        name: "Oman",
    },
    ContentLocaleOption {
        code: "PK",
        name: "Pakistan",
    },
    ContentLocaleOption {
        code: "PA",
        name: "Panama",
    },
    ContentLocaleOption {
        code: "PG",
        name: "Papua New Guinea",
    },
    ContentLocaleOption {
        code: "PY",
        name: "Paraguay",
    },
    ContentLocaleOption {
        code: "PE",
        name: "Peru",
    },
    ContentLocaleOption {
        code: "PH",
        name: "Philippines",
    },
    ContentLocaleOption {
        code: "PL",
        name: "Poland",
    },
    ContentLocaleOption {
        code: "PT",
        name: "Portugal",
    },
    ContentLocaleOption {
        code: "PR",
        name: "Puerto Rico",
    },
    ContentLocaleOption {
        code: "QA",
        name: "Qatar",
    },
    ContentLocaleOption {
        code: "RO",
        name: "Romania",
    },
    ContentLocaleOption {
        code: "RU",
        name: "Russian Federation",
    },
    ContentLocaleOption {
        code: "SA",
        name: "Saudi Arabia",
    },
    ContentLocaleOption {
        code: "SN",
        name: "Senegal",
    },
    ContentLocaleOption {
        code: "RS",
        name: "Serbia",
    },
    ContentLocaleOption {
        code: "SG",
        name: "Singapore",
    },
    ContentLocaleOption {
        code: "SK",
        name: "Slovakia",
    },
    ContentLocaleOption {
        code: "SI",
        name: "Slovenia",
    },
    ContentLocaleOption {
        code: "ZA",
        name: "South Africa",
    },
    ContentLocaleOption {
        code: "ES",
        name: "Spain",
    },
    ContentLocaleOption {
        code: "LK",
        name: "Sri Lanka",
    },
    ContentLocaleOption {
        code: "SE",
        name: "Sweden",
    },
    ContentLocaleOption {
        code: "CH",
        name: "Switzerland",
    },
    ContentLocaleOption {
        code: "TW",
        name: "Taiwan",
    },
    ContentLocaleOption {
        code: "TZ",
        name: "Tanzania",
    },
    ContentLocaleOption {
        code: "TH",
        name: "Thailand",
    },
    ContentLocaleOption {
        code: "TN",
        name: "Tunisia",
    },
    ContentLocaleOption {
        code: "TR",
        name: "Turkey",
    },
    ContentLocaleOption {
        code: "UG",
        name: "Uganda",
    },
    ContentLocaleOption {
        code: "UA",
        name: "Ukraine",
    },
    ContentLocaleOption {
        code: "AE",
        name: "United Arab Emirates",
    },
    ContentLocaleOption {
        code: "GB",
        name: "United Kingdom",
    },
    ContentLocaleOption {
        code: "US",
        name: "United States",
    },
    ContentLocaleOption {
        code: "UY",
        name: "Uruguay",
    },
    ContentLocaleOption {
        code: "VE",
        name: "Venezuela (Bolivarian Republic)",
    },
    ContentLocaleOption {
        code: "VN",
        name: "Vietnam",
    },
    ContentLocaleOption {
        code: "YE",
        name: "Yemen",
    },
    ContentLocaleOption {
        code: "ZW",
        name: "Zimbabwe",
    },
];

pub fn content_language_name(code: &str) -> Option<&'static str> {
    CONTENT_LANGUAGES
        .iter()
        .find(|option| option.code == code)
        .map(|option| option.name)
}

pub fn content_country_name(code: &str) -> Option<&'static str> {
    CONTENT_COUNTRIES
        .iter()
        .find(|option| option.code == code)
        .map(|option| option.name)
}

fn system_locale_parts() -> (String, String) {
    let locale = sys_locale::get_locale()
        .unwrap_or_else(|| "en-US".into())
        .split(['.', '@'])
        .next()
        .unwrap_or("en-US")
        .replace('_', "-");
    let locale_country = locale
        .split('-')
        .skip(1)
        .find(|part| {
            part.len() == 2
                && part
                    .chars()
                    .all(|character| character.is_ascii_alphabetic())
        })
        .map(str::to_ascii_uppercase);
    let country = locale_country
        .clone()
        .filter(|code| content_country_name(code).is_some())
        .unwrap_or_else(|| "US".into());
    let language = CONTENT_LANGUAGES
        .iter()
        .find(|option| option.code.eq_ignore_ascii_case(&locale))
        .or_else(|| {
            let primary = locale.split('-').next().unwrap_or("en");
            let regional = format!("{primary}-{}", locale_country.as_deref().unwrap_or("US"));
            CONTENT_LANGUAGES
                .iter()
                .find(|option| option.code.eq_ignore_ascii_case(&regional))
        })
        .or_else(|| {
            let primary = locale.split('-').next().unwrap_or("en");
            CONTENT_LANGUAGES
                .iter()
                .find(|option| option.code.eq_ignore_ascii_case(primary))
        })
        .map(|option| option.code)
        .unwrap_or("en")
        .to_owned();
    (language, country)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioQuality {
    #[default]
    Auto,
    Low,
    High,
}

impl AudioQuality {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Low, Self::High];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Low => "Low",
            Self::High => "High",
        }
    }

    pub fn playback_cache_key(self, video_id: &str) -> String {
        format!("{video_id}-quality-{}", self.storage_value())
    }

    pub(crate) const fn storage_value(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Low => "low",
            Self::High => "high",
        }
    }

    pub(crate) fn from_storage(value: &str) -> Result<Self> {
        match value {
            "auto" => Ok(Self::Auto),
            "low" => Ok(Self::Low),
            "high" => Ok(Self::High),
            _ => Err(AppError::Storage(format!(
                "unknown stored audio quality '{value}'"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoudnessLevel {
    Aggressive,
    Loud,
    #[default]
    Balanced,
    Quiet,
}

impl LoudnessLevel {
    pub const ALL: [Self; 4] = [Self::Aggressive, Self::Loud, Self::Balanced, Self::Quiet];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Aggressive => "Aggressive",
            Self::Loud => "Loud",
            Self::Balanced => "Balanced",
            Self::Quiet => "Quiet",
        }
    }

    pub const fn target_lufs_mb(self) -> i32 {
        match self {
            Self::Aggressive => -700,
            Self::Loud => -1_100,
            Self::Balanced => -1_400,
            Self::Quiet => -1_900,
        }
    }

    pub(crate) const fn storage_value(self) -> &'static str {
        match self {
            Self::Aggressive => "aggressive",
            Self::Loud => "loud",
            Self::Balanced => "balanced",
            Self::Quiet => "quiet",
        }
    }

    pub(crate) fn from_storage(value: &str) -> Result<Self> {
        match value {
            "aggressive" => Ok(Self::Aggressive),
            "loud" => Ok(Self::Loud),
            "balanced" => Ok(Self::Balanced),
            "quiet" => Ok(Self::Quiet),
            _ => Err(AppError::Storage(format!(
                "unknown stored loudness level '{value}'"
            ))),
        }
    }
}

pub const EQUALIZER_BAND_COUNT: usize = 10;
pub const EQUALIZER_FREQUENCIES_HZ: [u32; EQUALIZER_BAND_COUNT] =
    [31, 62, 125, 250, 500, 1_000, 2_000, 4_000, 8_000, 16_000];
pub const MIN_EQUALIZER_GAIN_MB: i16 = -1_200;
pub const MAX_EQUALIZER_GAIN_MB: i16 = 1_200;
pub const MAX_PARAMETRIC_EQUALIZER_BANDS: usize = 20;
pub const MIN_PARAMETRIC_PREAMP_MB: i16 = -5_000;
pub const MAX_PARAMETRIC_PREAMP_MB: i16 = 5_000;
pub const MIN_PARAMETRIC_GAIN_MB: i16 = -3_000;
pub const MAX_PARAMETRIC_GAIN_MB: i16 = 3_000;
pub const MAX_PARAMETRIC_FREQUENCY_MILLIHZ: u32 = 100_000_000;
pub const MAX_PARAMETRIC_Q_MILLI: u32 = 20_000;

pub const MIN_PLAYBACK_RATE_MILLI: u16 = 250;
pub const MAX_PLAYBACK_RATE_MILLI: u16 = 2_000;
pub const PLAYBACK_RATE_STEP_MILLI: u16 = 50;
pub const MIN_TRANSPOSE_SEMITONES: i8 = -12;
pub const MAX_TRANSPOSE_SEMITONES: i8 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackParameters {
    pub varispeed: bool,
    pub tempo_milli: u16,
    pub transpose_semitones: i8,
}

impl Default for PlaybackParameters {
    fn default() -> Self {
        Self {
            varispeed: false,
            tempo_milli: 1_000,
            transpose_semitones: 0,
        }
    }
}

impl PlaybackParameters {
    pub fn validate(self) -> Result<Self> {
        if !(MIN_PLAYBACK_RATE_MILLI..=MAX_PLAYBACK_RATE_MILLI).contains(&self.tempo_milli)
            || !(MIN_TRANSPOSE_SEMITONES..=MAX_TRANSPOSE_SEMITONES)
                .contains(&self.transpose_semitones)
        {
            return Err(AppError::InvalidConfig(
                "playback speed must be between 0.25x and 2.00x and transpose between -12 and +12 semitones"
                    .into(),
            ));
        }
        if !(self.tempo_milli - MIN_PLAYBACK_RATE_MILLI).is_multiple_of(PLAYBACK_RATE_STEP_MILLI) {
            return Err(AppError::InvalidConfig(
                "playback speed must use 0.05x steps".into(),
            ));
        }
        Ok(self)
    }

    pub fn tempo_ratio(self) -> f32 {
        f32::from(self.tempo_milli) / 1_000.0
    }

    pub fn pitch_ratio(self) -> f32 {
        if self.varispeed {
            self.tempo_ratio()
        } else {
            2.0_f32.powf(f32::from(self.transpose_semitones) / 12.0)
        }
    }

    pub fn stretch_ratio(self) -> f32 {
        self.tempo_ratio() / self.pitch_ratio()
    }

    pub fn is_active(self) -> bool {
        self.tempo_milli != 1_000 || (!self.varispeed && self.transpose_semitones != 0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EqualizerPreset {
    #[default]
    Flat,
    Bass,
    Vocal,
    Treble,
}

impl EqualizerPreset {
    pub const ALL: [Self; 4] = [Self::Flat, Self::Bass, Self::Vocal, Self::Treble];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Flat => "Flat",
            Self::Bass => "Bass",
            Self::Vocal => "Vocal",
            Self::Treble => "Treble",
        }
    }

    pub const fn gains_mb(self) -> [i16; EQUALIZER_BAND_COUNT] {
        match self {
            Self::Flat => [0; EQUALIZER_BAND_COUNT],
            Self::Bass => [600, 450, 300, 100, 0, -100, -150, -150, -100, 0],
            Self::Vocal => [-200, -100, 0, 200, 350, 400, 300, 150, 0, -100],
            Self::Treble => [-150, -150, -100, 0, 100, 200, 350, 500, 600, 500],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParametricFilterType {
    #[serde(rename = "PK")]
    Peaking,
    #[serde(rename = "LSC")]
    LowShelf,
    #[serde(rename = "HSC")]
    HighShelf,
}

impl ParametricFilterType {
    pub const fn apo_name(self) -> &'static str {
        match self {
            Self::Peaking => "PK",
            Self::LowShelf => "LSC",
            Self::HighShelf => "HSC",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParametricEqualizerBand {
    pub filter_type: ParametricFilterType,
    pub frequency_millihz: u32,
    pub gain_mb: i16,
    pub q_milli: u32,
    pub enabled: bool,
}

impl ParametricEqualizerBand {
    pub fn frequency_hz(self) -> f64 {
        f64::from(self.frequency_millihz) / 1_000.0
    }

    pub fn gain_db(self) -> f64 {
        f64::from(self.gain_mb) / 100.0
    }

    pub fn q(self) -> f64 {
        f64::from(self.q_milli) / 1_000.0
    }

    fn validate(self, index: usize) -> Result<()> {
        if !(1..=MAX_PARAMETRIC_FREQUENCY_MILLIHZ).contains(&self.frequency_millihz) {
            return Err(AppError::InvalidConfig(format!(
                "parametric EQ band {} frequency must be above 0 and at most 100000 Hz",
                index + 1
            )));
        }
        if !(MIN_PARAMETRIC_GAIN_MB..=MAX_PARAMETRIC_GAIN_MB).contains(&self.gain_mb) {
            return Err(AppError::InvalidConfig(format!(
                "parametric EQ band {} gain must be between -30 and +30 dB",
                index + 1
            )));
        }
        if !(1..=MAX_PARAMETRIC_Q_MILLI).contains(&self.q_milli) {
            return Err(AppError::InvalidConfig(format!(
                "parametric EQ band {} Q must be above 0 and at most 20",
                index + 1
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParametricEqualizer {
    pub preamp_mb: i16,
    pub bands: Vec<ParametricEqualizerBand>,
}

impl ParametricEqualizer {
    pub fn validate(&self) -> Result<()> {
        if !(MIN_PARAMETRIC_PREAMP_MB..=MAX_PARAMETRIC_PREAMP_MB).contains(&self.preamp_mb) {
            return Err(AppError::InvalidConfig(
                "parametric EQ preamp must be between -50 and +50 dB".into(),
            ));
        }
        if self.bands.is_empty() {
            return Err(AppError::InvalidConfig(
                "parametric EQ must contain at least one enabled band".into(),
            ));
        }
        if self.bands.len() > MAX_PARAMETRIC_EQUALIZER_BANDS {
            return Err(AppError::InvalidConfig(format!(
                "parametric EQ supports at most {MAX_PARAMETRIC_EQUALIZER_BANDS} bands"
            )));
        }
        for (index, band) in self.bands.iter().copied().enumerate() {
            band.validate(index)?;
        }
        Ok(())
    }

    pub fn is_effective(&self) -> bool {
        self.preamp_mb != 0
            || self
                .bands
                .iter()
                .any(|band| band.enabled && band.gain_mb != 0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EqualizerProfile {
    pub id: String,
    pub name: String,
    pub device_model: String,
    pub equalizer: ParametricEqualizer,
    pub source: String,
    pub rig: String,
    pub is_custom: bool,
    pub added_at_ms: i64,
}

impl EqualizerProfile {
    pub fn validate(&self) -> Result<()> {
        fn valid_text(value: &str, max_chars: usize) -> bool {
            !value.trim().is_empty()
                && value.chars().count() <= max_chars
                && !value.chars().any(char::is_control)
        }

        if !valid_text(&self.id, 256) {
            return Err(AppError::InvalidConfig(
                "equalizer profile id must contain 1 to 256 printable characters".into(),
            ));
        }
        if !valid_text(&self.name, 256) || !valid_text(&self.device_model, 256) {
            return Err(AppError::InvalidConfig(
                "equalizer profile name and device model must contain 1 to 256 printable characters"
                    .into(),
            ));
        }
        if !valid_text(&self.source, 256) || !valid_text(&self.rig, 256) {
            return Err(AppError::InvalidConfig(
                "equalizer profile source and rig must contain 1 to 256 printable characters"
                    .into(),
            ));
        }
        if self.added_at_ms < 0 {
            return Err(AppError::InvalidConfig(
                "equalizer profile timestamp cannot be negative".into(),
            ));
        }
        self.equalizer.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EqualizerSettings {
    pub enabled: bool,
    pub gains_mb: [i16; EQUALIZER_BAND_COUNT],
    pub active_profile: Option<EqualizerProfile>,
}

impl Default for EqualizerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            gains_mb: EqualizerPreset::Flat.gains_mb(),
            active_profile: None,
        }
    }
}

impl EqualizerSettings {
    pub const fn from_preset(preset: EqualizerPreset) -> Self {
        Self {
            enabled: !matches!(preset, EqualizerPreset::Flat),
            gains_mb: preset.gains_mb(),
            active_profile: None,
        }
    }

    pub fn preset(&self) -> Option<EqualizerPreset> {
        if self.active_profile.is_some() {
            return None;
        }
        EqualizerPreset::ALL
            .into_iter()
            .find(|preset| self.gains_mb == preset.gains_mb())
    }

    pub fn validate(&self) -> Result<()> {
        if self
            .gains_mb
            .iter()
            .any(|gain| !(MIN_EQUALIZER_GAIN_MB..=MAX_EQUALIZER_GAIN_MB).contains(gain))
        {
            return Err(AppError::InvalidConfig(format!(
                "equalizer gains must be between {:.0} dB and +{:.0} dB",
                f32::from(MIN_EQUALIZER_GAIN_MB) / 100.0,
                f32::from(MAX_EQUALIZER_GAIN_MB) / 100.0,
            )));
        }
        if let Some(profile) = &self.active_profile {
            profile.validate()?;
        }
        Ok(())
    }

    pub fn headroom_mb(&self) -> i16 {
        -self
            .gains_mb
            .iter()
            .copied()
            .max()
            .unwrap_or_default()
            .max(0)
    }

    pub fn is_effective(&self) -> bool {
        self.enabled
            && self.active_profile.as_ref().map_or_else(
                || self.gains_mb.iter().any(|gain| *gain != 0),
                |profile| profile.equalizer.is_effective(),
            )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProxyKind {
    #[default]
    Http,
    Socks5,
}

impl ProxyKind {
    pub const ALL: [Self; 2] = [Self::Http, Self::Socks5];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Http => "HTTP",
            Self::Socks5 => "SOCKS5",
        }
    }

    const fn default_scheme(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Socks5 => "socks5h",
        }
    }

    pub(crate) const fn storage_value(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Socks5 => "socks5",
        }
    }

    pub(crate) fn from_storage(value: &str) -> Result<Self> {
        match value {
            "http" => Ok(Self::Http),
            "socks5" => Ok(Self::Socks5),
            _ => Err(AppError::Storage(format!(
                "unknown stored proxy kind '{value}'"
            ))),
        }
    }

    fn accepts_scheme(self, scheme: &str) -> bool {
        match self {
            Self::Http => matches!(scheme, "http" | "https"),
            Self::Socks5 => matches!(scheme, "socks5" | "socks5h"),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Default)]
pub struct ProxySettings {
    pub enabled: bool,
    pub kind: ProxyKind,
    pub address: String,
    pub username: String,
    pub password: String,
}

impl fmt::Debug for ProxySettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let password = if self.password.is_empty() {
            "<empty>"
        } else {
            "<redacted>"
        };
        formatter
            .debug_struct("ProxySettings")
            .field("enabled", &self.enabled)
            .field("kind", &self.kind)
            .field("address", &self.address)
            .field("username", &self.username)
            .field("password", &password)
            .finish()
    }
}

impl ProxySettings {
    pub fn resolved_url(&self) -> Result<Option<Url>> {
        if !self.enabled {
            return Ok(None);
        }
        let address = self.address.trim();
        if address.is_empty() {
            return Err(AppError::InvalidConfig(
                "an enabled proxy requires an address".into(),
            ));
        }
        let candidate = if address.contains("://") {
            address.to_owned()
        } else {
            format!("{}://{address}", self.kind.default_scheme())
        };
        let mut url = Url::parse(&candidate)
            .map_err(|error| AppError::InvalidConfig(format!("invalid proxy address: {error}")))?;
        if !self.kind.accepts_scheme(url.scheme()) {
            return Err(AppError::InvalidConfig(format!(
                "{} proxy cannot use the '{}' URL scheme",
                self.kind.label(),
                url.scheme()
            )));
        }
        if url.host_str().is_none() {
            return Err(AppError::InvalidConfig(
                "proxy address must include a host".into(),
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(AppError::InvalidConfig(
                "proxy credentials must use the masked username and password fields".into(),
            ));
        }
        if !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() {
            return Err(AppError::InvalidConfig(
                "proxy address cannot contain a path, query, or fragment".into(),
            ));
        }
        url.set_username(&self.username).map_err(|_| {
            AppError::InvalidConfig("proxy username contains unsupported characters".into())
        })?;
        url.set_password((!self.password.is_empty()).then_some(self.password.as_str()))
            .map_err(|_| {
                AppError::InvalidConfig("proxy password contains unsupported characters".into())
            })?;
        Ok(Some(url))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppTheme {
    Light,
    #[default]
    Dark,
}

impl AppTheme {
    pub(crate) const fn storage_value(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub(crate) fn from_storage(value: &str) -> Result<Self> {
        match value {
            "light" => Ok(Self::Light),
            "dark" => Ok(Self::Dark),
            _ => Err(AppError::Storage(format!("unknown stored theme '{value}'"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenTogetherSettings {
    pub server_url: String,
    pub username: String,
    pub auto_approve_joins: bool,
    pub auto_approve_suggestions: bool,
    pub sync_host_volume: bool,
}

impl Default for ListenTogetherSettings {
    fn default() -> Self {
        Self {
            server_url: DEFAULT_LISTEN_TOGETHER_SERVER_URL.into(),
            username: String::new(),
            auto_approve_joins: false,
            auto_approve_suggestions: false,
            sync_host_volume: true,
        }
    }
}

impl ListenTogetherSettings {
    pub fn validate(mut self) -> Result<Self> {
        self.server_url = self.server_url.trim().to_owned();
        self.username = self.username.trim().to_owned();
        let url = Url::parse(&self.server_url).map_err(|_| {
            AppError::InvalidConfig(
                "Listen Together server must be a valid ws:// or wss:// URL".into(),
            )
        })?;
        if !matches!(url.scheme(), "ws" | "wss")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || self.server_url.chars().count() > 2_048
        {
            return Err(AppError::InvalidConfig(
                "Listen Together server must be a credential-free ws:// or wss:// URL with a host"
                    .into(),
            ));
        }
        if self.username.chars().count() > 128 || self.username.chars().any(char::is_control) {
            return Err(AppError::InvalidConfig(
                "Listen Together username must be at most 128 characters without control characters"
                    .into(),
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSettings {
    pub proxy: ProxySettings,
    pub content_language: String,
    pub content_country: String,
    pub audio_quality: AudioQuality,
    pub audio_normalization: bool,
    pub loudness_level: LoudnessLevel,
    pub equalizer: EqualizerSettings,
    pub playback_parameters: PlaybackParameters,
    pub cache_root: PathBuf,
    pub audio_cache_bytes: u64,
    pub auto_radio: bool,
    pub auto_radio_queue: bool,
    pub auto_load_more: bool,
    pub sleep_timer_stop_after_current_song: bool,
    pub sleep_timer_fade_out: bool,
    pub persistent_queue: bool,
    pub autoplay: bool,
    pub auto_skip_next_on_error: bool,
    pub auto_download_on_like: bool,
    pub pause_on_mute: bool,
    pub progressive_seek: bool,
    pub skip_silence: bool,
    pub skip_silence_instant: bool,
    pub crossfade: bool,
    pub crossfade_seconds: u8,
    pub crossfade_gapless_albums: bool,
    pub persistent_shuffle_across_queues: bool,
    pub shuffle_playlist_first: bool,
    pub remember_shuffle_and_repeat: bool,
    pub prevent_duplicate_tracks_in_queue: bool,
    pub disable_load_more_when_repeat_all: bool,
    pub youtube_history_sync: bool,
    pub pause_listening_history: bool,
    pub pause_search_history: bool,
    pub history_duration_seconds: u16,
    pub lastfm_scrobbling: bool,
    pub lastfm_now_playing: bool,
    pub lastfm_sync_likes: bool,
    pub lastfm_scrobble_policy: LastFmScrobblePolicy,
    pub discord_rich_presence: bool,
    pub listen_together: ListenTogetherSettings,
    pub theme: AppTheme,
}

impl AppSettings {
    pub fn for_current_user(audio_cache_bytes: u64) -> Result<Self> {
        let cache_root = dirs::cache_dir()
            .ok_or_else(|| {
                AppError::InvalidConfig("the operating system has no cache directory".into())
            })?
            .join("metrolist");
        Self {
            proxy: ProxySettings::default(),
            content_language: SYSTEM_CONTENT_LOCALE.into(),
            content_country: SYSTEM_CONTENT_LOCALE.into(),
            audio_quality: AudioQuality::Auto,
            audio_normalization: true,
            loudness_level: LoudnessLevel::Balanced,
            equalizer: EqualizerSettings::default(),
            playback_parameters: PlaybackParameters::default(),
            cache_root,
            audio_cache_bytes,
            auto_radio: true,
            auto_radio_queue: true,
            auto_load_more: true,
            sleep_timer_stop_after_current_song: false,
            sleep_timer_fade_out: false,
            persistent_queue: true,
            autoplay: true,
            auto_skip_next_on_error: false,
            auto_download_on_like: false,
            pause_on_mute: false,
            progressive_seek: false,
            skip_silence: false,
            skip_silence_instant: false,
            crossfade: false,
            crossfade_seconds: DEFAULT_CROSSFADE_SECONDS,
            crossfade_gapless_albums: true,
            persistent_shuffle_across_queues: false,
            shuffle_playlist_first: false,
            remember_shuffle_and_repeat: true,
            prevent_duplicate_tracks_in_queue: false,
            disable_load_more_when_repeat_all: false,
            youtube_history_sync: true,
            pause_listening_history: false,
            pause_search_history: false,
            history_duration_seconds: DEFAULT_HISTORY_DURATION_SECONDS,
            lastfm_scrobbling: false,
            lastfm_now_playing: false,
            lastfm_sync_likes: false,
            lastfm_scrobble_policy: LastFmScrobblePolicy::default(),
            discord_rich_presence: false,
            listen_together: ListenTogetherSettings::default(),
            theme: AppTheme::Dark,
        }
        .validate()
    }

    pub fn validate(mut self) -> Result<Self> {
        self.proxy.resolved_url()?;
        self.content_language = self.content_language.trim().to_owned();
        self.content_country = self.content_country.trim().to_owned();
        if self
            .content_language
            .eq_ignore_ascii_case(SYSTEM_CONTENT_LOCALE)
        {
            self.content_language = SYSTEM_CONTENT_LOCALE.into();
        } else if content_language_name(&self.content_language).is_none() {
            return Err(AppError::InvalidConfig(format!(
                "unknown content language '{}'",
                self.content_language
            )));
        }
        if self
            .content_country
            .eq_ignore_ascii_case(SYSTEM_CONTENT_LOCALE)
        {
            self.content_country = SYSTEM_CONTENT_LOCALE.into();
        } else {
            self.content_country = self.content_country.to_ascii_uppercase();
            if content_country_name(&self.content_country).is_none() {
                return Err(AppError::InvalidConfig(format!(
                    "unknown content country or region '{}'",
                    self.content_country
                )));
            }
        }
        if !self.equalizer.enabled {
            self.equalizer.active_profile = None;
        }
        self.equalizer.validate()?;
        self.playback_parameters.validate()?;
        self.lastfm_scrobble_policy.validate()?;
        self.listen_together = self.listen_together.validate()?;
        if !self.cache_root.is_absolute() {
            return Err(AppError::InvalidConfig(
                "cache location must be an absolute path".into(),
            ));
        }
        if self.cache_root.parent().is_none() {
            return Err(AppError::InvalidConfig(
                "the filesystem root cannot be used directly as the cache location".into(),
            ));
        }
        if !(MIN_AUDIO_CACHE_BYTES..=MAX_AUDIO_CACHE_BYTES).contains(&self.audio_cache_bytes) {
            return Err(AppError::InvalidConfig(format!(
                "audio cache capacity must be between {} MiB and {} GiB",
                MIN_AUDIO_CACHE_BYTES / 1024 / 1024,
                MAX_AUDIO_CACHE_BYTES / 1024 / 1024 / 1024
            )));
        }
        if !(MIN_HISTORY_DURATION_SECONDS..=MAX_HISTORY_DURATION_SECONDS)
            .contains(&self.history_duration_seconds)
        {
            return Err(AppError::InvalidConfig(format!(
                "listening history duration must be between {MIN_HISTORY_DURATION_SECONDS} and {MAX_HISTORY_DURATION_SECONDS} seconds"
            )));
        }
        if !(MIN_CROSSFADE_SECONDS..=MAX_CROSSFADE_SECONDS).contains(&self.crossfade_seconds) {
            return Err(AppError::InvalidConfig(format!(
                "crossfade duration must be between {MIN_CROSSFADE_SECONDS} and {MAX_CROSSFADE_SECONDS} seconds"
            )));
        }
        Ok(self)
    }

    pub fn audio_cache_root(&self) -> PathBuf {
        self.cache_root.join("audio")
    }

    pub fn resolved_content_language(&self) -> String {
        if self.content_language == SYSTEM_CONTENT_LOCALE {
            system_locale_parts().0
        } else {
            self.content_language.clone()
        }
    }

    pub fn resolved_content_country(&self) -> String {
        if self.content_country == SYSTEM_CONTENT_LOCALE {
            system_locale_parts().1
        } else {
            self.content_country.clone()
        }
    }

    pub fn thumbnail_cache_root(&self) -> PathBuf {
        self.cache_root.join("thumbnails")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppConfig {
    pub window_width: f32,
    pub window_height: f32,
    pub initial_route: Route,
    pub audio_cache_bytes: u64,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            window_width: 1180.0,
            window_height: 760.0,
            initial_route: Route::Home,
            audio_cache_bytes: DEFAULT_AUDIO_CACHE_BYTES,
        }
    }
}

impl AppConfig {
    pub fn validate(self) -> Result<Self> {
        if self.window_width < MIN_WINDOW_WIDTH || self.window_height < MIN_WINDOW_HEIGHT {
            return Err(AppError::InvalidConfig(format!(
                "window must be at least {MIN_WINDOW_WIDTH:.0}x{MIN_WINDOW_HEIGHT:.0}"
            )));
        }
        if self.audio_cache_bytes == 0 {
            return Err(AppError::InvalidConfig(
                "audio cache capacity must be positive".into(),
            ));
        }

        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_window_size_is_valid() {
        assert!(AppConfig::default().validate().is_ok());
    }

    #[test]
    fn rejects_an_unusable_window_size() {
        let config = AppConfig {
            window_width: MIN_WINDOW_WIDTH - 1.0,
            ..AppConfig::default()
        };

        assert!(matches!(config.validate(), Err(AppError::InvalidConfig(_))));
    }

    #[test]
    fn rejects_an_empty_audio_cache_capacity() {
        let config = AppConfig {
            audio_cache_bytes: 0,
            ..AppConfig::default()
        };

        assert!(matches!(config.validate(), Err(AppError::InvalidConfig(_))));
    }

    #[test]
    fn proxy_url_accepts_scheme_defaults_and_redacts_passwords() {
        let proxy = ProxySettings {
            enabled: true,
            kind: ProxyKind::Socks5,
            address: "127.0.0.1:1080".into(),
            username: "listener".into(),
            password: "not-in-debug-output".into(),
        };

        let url = proxy.resolved_url().unwrap().unwrap();
        assert_eq!(url.scheme(), "socks5h");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert_eq!(url.port(), Some(1080));
        assert_eq!(url.username(), "listener");
        assert_eq!(url.password(), Some("not-in-debug-output"));
        assert!(!format!("{proxy:?}").contains("not-in-debug-output"));
    }

    #[test]
    fn proxy_url_rejects_mismatched_schemes_and_ambiguous_credentials() {
        let wrong_scheme = ProxySettings {
            enabled: true,
            kind: ProxyKind::Socks5,
            address: "http://127.0.0.1:8080".into(),
            ..ProxySettings::default()
        };
        assert!(wrong_scheme.resolved_url().is_err());

        let duplicate_credentials = ProxySettings {
            enabled: true,
            address: "http://embedded:secret@127.0.0.1:8080".into(),
            username: "separate".into(),
            ..ProxySettings::default()
        };
        assert!(duplicate_credentials.resolved_url().is_err());
    }

    #[test]
    fn settings_require_a_safe_absolute_cache_root() {
        let settings = AppSettings {
            proxy: ProxySettings::default(),
            content_language: SYSTEM_CONTENT_LOCALE.into(),
            content_country: SYSTEM_CONTENT_LOCALE.into(),
            audio_quality: AudioQuality::Auto,
            audio_normalization: true,
            loudness_level: LoudnessLevel::Balanced,
            equalizer: EqualizerSettings::default(),
            playback_parameters: PlaybackParameters::default(),
            cache_root: "relative/cache".into(),
            audio_cache_bytes: DEFAULT_AUDIO_CACHE_BYTES,
            auto_radio: true,
            auto_radio_queue: true,
            auto_load_more: true,
            sleep_timer_stop_after_current_song: false,
            sleep_timer_fade_out: false,
            persistent_queue: true,
            autoplay: true,
            auto_skip_next_on_error: false,
            auto_download_on_like: false,
            pause_on_mute: false,
            progressive_seek: false,
            skip_silence: false,
            skip_silence_instant: false,
            crossfade: false,
            crossfade_seconds: DEFAULT_CROSSFADE_SECONDS,
            crossfade_gapless_albums: true,
            persistent_shuffle_across_queues: false,
            shuffle_playlist_first: false,
            remember_shuffle_and_repeat: true,
            prevent_duplicate_tracks_in_queue: false,
            disable_load_more_when_repeat_all: false,
            youtube_history_sync: true,
            pause_listening_history: false,
            pause_search_history: false,
            history_duration_seconds: DEFAULT_HISTORY_DURATION_SECONDS,
            lastfm_scrobbling: false,
            lastfm_now_playing: false,
            lastfm_sync_likes: false,
            lastfm_scrobble_policy: LastFmScrobblePolicy::default(),
            discord_rich_presence: false,
            listen_together: ListenTogetherSettings::default(),
            theme: AppTheme::Dark,
        };
        assert!(settings.validate().is_err());
    }

    #[test]
    fn equalizer_presets_are_bounded_and_reserve_boost_headroom() {
        for preset in EqualizerPreset::ALL {
            let settings = EqualizerSettings::from_preset(preset);
            assert_eq!(settings.preset(), Some(preset));
            assert!(settings.validate().is_ok());
            assert_eq!(
                settings.headroom_mb(),
                -settings.gains_mb.into_iter().max().unwrap().max(0)
            );
        }
        assert!(!EqualizerSettings::from_preset(EqualizerPreset::Flat).enabled);
        assert!(EqualizerSettings::from_preset(EqualizerPreset::Bass).enabled);
    }

    #[test]
    fn playback_parameters_match_android_ranges_and_modes() {
        let normal = PlaybackParameters {
            varispeed: false,
            tempo_milli: 750,
            transpose_semitones: 12,
        };
        assert_eq!(normal.validate().unwrap(), normal);
        assert!((normal.tempo_ratio() - 0.75).abs() < f32::EPSILON);
        assert!((normal.pitch_ratio() - 2.0).abs() < 0.000_001);
        assert!((normal.stretch_ratio() - 0.375).abs() < 0.000_001);

        let varispeed = PlaybackParameters {
            varispeed: true,
            tempo_milli: 1_250,
            transpose_semitones: -12,
        };
        assert!((varispeed.pitch_ratio() - 1.25).abs() < f32::EPSILON);
        assert!((varispeed.stretch_ratio() - 1.0).abs() < f32::EPSILON);
        assert!(varispeed.is_active());
        assert!(!PlaybackParameters::default().is_active());

        for invalid in [
            PlaybackParameters {
                tempo_milli: 200,
                ..PlaybackParameters::default()
            },
            PlaybackParameters {
                tempo_milli: 1_025,
                ..PlaybackParameters::default()
            },
            PlaybackParameters {
                transpose_semitones: 13,
                ..PlaybackParameters::default()
            },
        ] {
            assert!(invalid.validate().is_err());
        }
    }

    #[test]
    fn equalizer_rejects_gains_outside_twelve_decibels() {
        let mut settings = EqualizerSettings::default();
        settings.gains_mb[4] = MAX_EQUALIZER_GAIN_MB + 1;
        assert!(matches!(
            settings.validate(),
            Err(AppError::InvalidConfig(_))
        ));
    }

    #[test]
    fn parametric_equalizer_matches_android_bounds_and_disabled_settings_drop_activation() {
        let band = ParametricEqualizerBand {
            filter_type: ParametricFilterType::Peaking,
            frequency_millihz: 1_000_000,
            gain_mb: 300,
            q_milli: 1_410,
            enabled: true,
        };
        let profile = EqualizerProfile {
            id: "fixture".into(),
            name: "Fixture".into(),
            device_model: "Fixture headphones".into(),
            equalizer: ParametricEqualizer {
                preamp_mb: -300,
                bands: vec![band],
            },
            source: "fixture".into(),
            rig: "fixture".into(),
            is_custom: true,
            added_at_ms: 0,
        };
        assert!(profile.validate().is_ok());

        for invalid in [
            ParametricEqualizerBand {
                frequency_millihz: 0,
                ..band
            },
            ParametricEqualizerBand {
                gain_mb: MAX_PARAMETRIC_GAIN_MB + 1,
                ..band
            },
            ParametricEqualizerBand {
                q_milli: MAX_PARAMETRIC_Q_MILLI + 1,
                ..band
            },
        ] {
            let mut invalid_profile = profile.clone();
            invalid_profile.equalizer.bands = vec![invalid];
            assert!(invalid_profile.validate().is_err());
        }
        let mut too_many = profile.clone();
        too_many.equalizer.bands = vec![band; MAX_PARAMETRIC_EQUALIZER_BANDS + 1];
        assert!(too_many.validate().is_err());
        let mut invalid_preamp = profile.clone();
        invalid_preamp.equalizer.preamp_mb = MAX_PARAMETRIC_PREAMP_MB + 1;
        assert!(invalid_preamp.validate().is_err());

        let mut settings = AppSettings::for_current_user(DEFAULT_AUDIO_CACHE_BYTES).unwrap();
        settings.equalizer.enabled = false;
        settings.equalizer.active_profile = Some(profile);
        assert!(
            settings
                .validate()
                .unwrap()
                .equalizer
                .active_profile
                .is_none()
        );
    }
}
