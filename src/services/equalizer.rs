use crate::{
    AppError, MAX_PARAMETRIC_EQUALIZER_BANDS, ParametricEqualizer, ParametricEqualizerBand,
    ParametricFilterType, Result,
};

pub const MAX_EQUALIZER_APO_FILE_BYTES: usize = 1024 * 1024;
pub const EQUALIZER_RESPONSE_SAMPLE_RATE: u32 = 48_000;
pub const EQUALIZER_RESPONSE_POINT_COUNT: usize = 200;
pub const EQUALIZER_RESPONSE_MIN_FREQUENCY_HZ: f64 = 20.0;
pub const EQUALIZER_RESPONSE_MAX_FREQUENCY_HZ: f64 = 20_000.0;
pub const EQUALIZER_RESPONSE_DB_STEP: f64 = 2.5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EqualizerResponsePoint {
    pub frequency_hz: f64,
    pub gain_db: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EqualizerFrequencyResponse {
    pub points: Vec<EqualizerResponsePoint>,
    pub db_top: f64,
    pub db_bottom: f64,
    pub db_step: f64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BiquadCoefficients {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl BiquadCoefficients {
    pub(crate) fn peaking(
        sample_rate: u32,
        frequency_hz: f64,
        gain_db: f64,
        q: f64,
    ) -> Option<Self> {
        if sample_rate == 0
            || !frequency_hz.is_finite()
            || frequency_hz <= 0.0
            || frequency_hz >= f64::from(sample_rate) / 2.0
            || !gain_db.is_finite()
            || gain_db == 0.0
            || !q.is_finite()
            || q <= 0.0
        {
            return None;
        }
        let amplitude = 10.0_f64.powf(gain_db / 40.0);
        let omega = 2.0 * std::f64::consts::PI * frequency_hz / f64::from(sample_rate);
        let alpha = omega.sin() / (2.0 * q);
        let cosine = omega.cos();
        let a0 = 1.0 + alpha / amplitude;
        Self::normalized(
            1.0 + alpha * amplitude,
            -2.0 * cosine,
            1.0 - alpha * amplitude,
            a0,
            -2.0 * cosine,
            1.0 - alpha / amplitude,
        )
    }

    fn low_shelf(sample_rate: u32, frequency_hz: f64, gain_db: f64) -> Option<Self> {
        let (amplitude, cosine, two_root_amplitude_alpha) =
            Self::shelf_terms(sample_rate, frequency_hz, gain_db)?;
        let plus = amplitude + 1.0;
        let minus = amplitude - 1.0;
        Self::normalized(
            amplitude * (plus - minus * cosine + two_root_amplitude_alpha),
            2.0 * amplitude * (minus - plus * cosine),
            amplitude * (plus - minus * cosine - two_root_amplitude_alpha),
            plus + minus * cosine + two_root_amplitude_alpha,
            -2.0 * (minus + plus * cosine),
            plus + minus * cosine - two_root_amplitude_alpha,
        )
    }

    fn high_shelf(sample_rate: u32, frequency_hz: f64, gain_db: f64) -> Option<Self> {
        let (amplitude, cosine, two_root_amplitude_alpha) =
            Self::shelf_terms(sample_rate, frequency_hz, gain_db)?;
        let plus = amplitude + 1.0;
        let minus = amplitude - 1.0;
        Self::normalized(
            amplitude * (plus + minus * cosine + two_root_amplitude_alpha),
            -2.0 * amplitude * (minus + plus * cosine),
            amplitude * (plus + minus * cosine - two_root_amplitude_alpha),
            plus - minus * cosine + two_root_amplitude_alpha,
            2.0 * (minus - plus * cosine),
            plus - minus * cosine - two_root_amplitude_alpha,
        )
    }

    fn shelf_terms(sample_rate: u32, frequency_hz: f64, gain_db: f64) -> Option<(f64, f64, f64)> {
        if sample_rate == 0
            || !frequency_hz.is_finite()
            || frequency_hz <= 0.0
            || frequency_hz >= f64::from(sample_rate) / 2.0
            || !gain_db.is_finite()
            || gain_db == 0.0
        {
            return None;
        }
        // Android Metrolist and the playback DSP both use RBJ shelf slope S=1.
        let amplitude = 10.0_f64.powf(gain_db / 40.0);
        let omega = 2.0 * std::f64::consts::PI * frequency_hz / f64::from(sample_rate);
        let sine = omega.sin();
        let cosine = omega.cos();
        let alpha = sine / 2.0 * 2.0_f64.sqrt();
        Some((amplitude, cosine, 2.0 * amplitude.sqrt() * alpha))
    }

    #[allow(clippy::too_many_arguments)]
    fn normalized(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Option<Self> {
        if !a0.is_finite() || a0 == 0.0 {
            return None;
        }
        let coefficients = [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0];
        coefficients
            .iter()
            .all(|value| value.is_finite())
            .then_some(Self {
                b0: coefficients[0],
                b1: coefficients[1],
                b2: coefficients[2],
                a1: coefficients[3],
                a2: coefficients[4],
            })
    }

    pub(crate) fn from_band(sample_rate: u32, band: ParametricEqualizerBand) -> Option<Self> {
        if !band.enabled {
            return None;
        }
        match band.filter_type {
            ParametricFilterType::Peaking => {
                Self::peaking(sample_rate, band.frequency_hz(), band.gain_db(), band.q())
            }
            ParametricFilterType::LowShelf => {
                Self::low_shelf(sample_rate, band.frequency_hz(), band.gain_db())
            }
            ParametricFilterType::HighShelf => {
                Self::high_shelf(sample_rate, band.frequency_hz(), band.gain_db())
            }
        }
    }

    fn magnitude_db(self, frequency_hz: f64, sample_rate: u32) -> f64 {
        let omega = 2.0 * std::f64::consts::PI * frequency_hz / f64::from(sample_rate);
        let (sin_omega, cos_omega) = omega.sin_cos();
        let (sin_double, cos_double) = (2.0 * omega).sin_cos();
        let numerator_real = self.b0 + self.b1 * cos_omega + self.b2 * cos_double;
        let numerator_imag = -self.b1 * sin_omega - self.b2 * sin_double;
        let denominator_real = 1.0 + self.a1 * cos_omega + self.a2 * cos_double;
        let denominator_imag = -self.a1 * sin_omega - self.a2 * sin_double;
        let numerator_power = numerator_real.mul_add(numerator_real, numerator_imag.powi(2));
        let denominator_power =
            denominator_real.mul_add(denominator_real, denominator_imag.powi(2));
        if numerator_power <= 0.0 || denominator_power <= 0.0 {
            return 0.0;
        }
        10.0 * (numerator_power / denominator_power).log10()
    }
}

pub fn equalizer_frequency_response(
    equalizer: &ParametricEqualizer,
) -> Result<EqualizerFrequencyResponse> {
    equalizer.validate()?;
    let coefficients = equalizer
        .bands
        .iter()
        .copied()
        .filter_map(|band| BiquadCoefficients::from_band(EQUALIZER_RESPONSE_SAMPLE_RATE, band))
        .collect::<Vec<_>>();
    let log_min = EQUALIZER_RESPONSE_MIN_FREQUENCY_HZ.log10();
    let log_max = EQUALIZER_RESPONSE_MAX_FREQUENCY_HZ.log10();
    let preamp_db = f64::from(equalizer.preamp_mb) / 100.0;
    let mut points = Vec::with_capacity(EQUALIZER_RESPONSE_POINT_COUNT);
    let mut peak_abs = 0.0_f64;
    for index in 0..EQUALIZER_RESPONSE_POINT_COUNT {
        let fraction = index as f64 / (EQUALIZER_RESPONSE_POINT_COUNT - 1) as f64;
        let frequency_hz = 10.0_f64.powf(log_min + (log_max - log_min) * fraction);
        let gain_db = coefficients.iter().fold(preamp_db, |gain, coefficients| {
            gain + coefficients.magnitude_db(frequency_hz, EQUALIZER_RESPONSE_SAMPLE_RATE)
        });
        peak_abs = peak_abs.max(gain_db.abs());
        points.push(EqualizerResponsePoint {
            frequency_hz,
            gain_db,
        });
    }
    let half_range = (((peak_abs + 1.0) / EQUALIZER_RESPONSE_DB_STEP).ceil()
        * EQUALIZER_RESPONSE_DB_STEP)
        .max(EQUALIZER_RESPONSE_DB_STEP);
    Ok(EqualizerFrequencyResponse {
        points,
        db_top: half_range,
        db_bottom: -half_range,
        db_step: EQUALIZER_RESPONSE_DB_STEP,
    })
}

/// Parses the subset of Equalizer APO used by AutoEQ ParametricEQ files.
///
/// Supported filters are `PK`, `LSC`, and `HSC`. Disabled filters are ignored,
/// matching Android Metrolist's import behavior. Unknown metadata and comments
/// are preserved by the source file but do not affect playback.
pub fn parse_equalizer_apo(content: &str) -> Result<ParametricEqualizer> {
    if content.len() > MAX_EQUALIZER_APO_FILE_BYTES {
        return Err(AppError::InvalidConfig(format!(
            "equalizer profile exceeds the {} KiB import limit",
            MAX_EQUALIZER_APO_FILE_BYTES / 1024
        )));
    }

    let mut preamp_mb = 0;
    let mut bands = Vec::new();
    for (line_index, raw_line) in content.lines().enumerate() {
        let line_number = line_index + 1;
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        if starts_with_ascii_case(line, "Preamp:") {
            let value = line
                .split_once(':')
                .map(|(_, value)| value.trim())
                .unwrap_or_default();
            let value = strip_ascii_suffix(value, "dB")
                .ok_or_else(|| import_error(line_number, "preamp must end in the dB unit"))?;
            preamp_mb = scaled_decimal(value.trim(), 100, line_number, "preamp")?
                .try_into()
                .map_err(|_| import_error(line_number, "preamp is outside the supported range"))?;
            continue;
        }
        if starts_with_ascii_case(line, "Filter")
            && let Some(band) = parse_filter(line, line_number)?
        {
            if bands.len() == MAX_PARAMETRIC_EQUALIZER_BANDS {
                return Err(import_error(
                    line_number,
                    &format!(
                        "profile contains more than {MAX_PARAMETRIC_EQUALIZER_BANDS} enabled filters"
                    ),
                ));
            }
            bands.push(band);
        }
    }

    let equalizer = ParametricEqualizer { preamp_mb, bands };
    equalizer.validate().map_err(|error| {
        AppError::InvalidConfig(format!("invalid Equalizer APO profile: {error}"))
    })?;
    Ok(equalizer)
}

pub fn format_equalizer_apo(equalizer: &ParametricEqualizer) -> Result<String> {
    equalizer.validate()?;
    let mut output = format!(
        "Preamp: {} dB\n",
        format_scaled(i64::from(equalizer.preamp_mb), 100)
    );
    for (index, band) in equalizer.bands.iter().enumerate() {
        output.push_str(&format!(
            "Filter {}: {} {} Fc {} Hz Gain {} dB Q {}\n",
            index + 1,
            if band.enabled { "ON" } else { "OFF" },
            band.filter_type.apo_name(),
            format_scaled(i64::from(band.frequency_millihz), 1_000),
            format_scaled(i64::from(band.gain_mb), 100),
            format_scaled(i64::from(band.q_milli), 1_000),
        ));
    }
    Ok(output)
}

fn parse_filter(line: &str, line_number: usize) -> Result<Option<ParametricEqualizerBand>> {
    let body = line
        .split_once(':')
        .map(|(_, body)| body.trim())
        .ok_or_else(|| import_error(line_number, "filter line is missing ':'"))?;
    let tokens = body.split_ascii_whitespace().collect::<Vec<_>>();
    let Some(status) = tokens.first() else {
        return Err(import_error(line_number, "filter line is empty"));
    };
    if status.eq_ignore_ascii_case("OFF") {
        return Ok(None);
    }
    if !status.eq_ignore_ascii_case("ON") {
        return Err(import_error(line_number, "filter must be ON or OFF"));
    }
    let filter_type = match tokens.get(1).copied() {
        Some(value) if value.eq_ignore_ascii_case("PK") => ParametricFilterType::Peaking,
        Some(value) if value.eq_ignore_ascii_case("LSC") => ParametricFilterType::LowShelf,
        Some(value) if value.eq_ignore_ascii_case("HSC") => ParametricFilterType::HighShelf,
        Some(value) => {
            return Err(import_error(
                line_number,
                &format!("unsupported filter type '{value}'; expected PK, LSC, or HSC"),
            ));
        }
        None => return Err(import_error(line_number, "filter type is missing")),
    };

    let frequency = labeled_value(&tokens, "Fc", Some("Hz"), line_number)?;
    let gain = labeled_value(&tokens, "Gain", Some("dB"), line_number)?;
    let q = labeled_value(&tokens, "Q", None, line_number)?;
    let frequency_millihz = scaled_decimal(frequency, 1_000, line_number, "frequency")?
        .try_into()
        .map_err(|_| import_error(line_number, "frequency is outside the supported range"))?;
    let gain_mb = scaled_decimal(gain, 100, line_number, "gain")?
        .try_into()
        .map_err(|_| import_error(line_number, "gain is outside the supported range"))?;
    let q_milli = scaled_decimal(q, 1_000, line_number, "Q")?
        .try_into()
        .map_err(|_| import_error(line_number, "Q is outside the supported range"))?;
    Ok(Some(ParametricEqualizerBand {
        filter_type,
        frequency_millihz,
        gain_mb,
        q_milli,
        enabled: true,
    }))
}

fn labeled_value<'a>(
    tokens: &'a [&str],
    label: &str,
    unit: Option<&str>,
    line_number: usize,
) -> Result<&'a str> {
    let index = tokens
        .iter()
        .position(|token| token.eq_ignore_ascii_case(label))
        .ok_or_else(|| import_error(line_number, &format!("filter is missing {label}")))?;
    let value = tokens
        .get(index + 1)
        .copied()
        .ok_or_else(|| import_error(line_number, &format!("filter {label} value is missing")))?;
    if let Some(unit) = unit
        && !tokens
            .get(index + 2)
            .is_some_and(|token| token.eq_ignore_ascii_case(unit))
    {
        return Err(import_error(
            line_number,
            &format!("filter {label} must use the {unit} unit"),
        ));
    }
    Ok(value)
}

fn scaled_decimal(value: &str, scale: i64, line_number: usize, label: &str) -> Result<i64> {
    let value = value
        .parse::<f64>()
        .map_err(|_| import_error(line_number, &format!("{label} is not a number")))?;
    if !value.is_finite() {
        return Err(import_error(
            line_number,
            &format!("{label} must be finite"),
        ));
    }
    let scaled = (value * scale as f64).round();
    if !(i64::MIN as f64..=i64::MAX as f64).contains(&scaled) {
        return Err(import_error(
            line_number,
            &format!("{label} is outside the supported range"),
        ));
    }
    Ok(scaled as i64)
}

fn format_scaled(value: i64, scale: i64) -> String {
    let negative = value < 0;
    let magnitude = value.unsigned_abs();
    let scale = scale as u64;
    let whole = magnitude / scale;
    let fraction = magnitude % scale;
    let mut output = if fraction == 0 {
        whole.to_string()
    } else {
        let digits = scale.ilog10() as usize;
        let mut fraction = format!("{fraction:0digits$}");
        while fraction.ends_with('0') {
            fraction.pop();
        }
        format!("{whole}.{fraction}")
    };
    if negative {
        output.insert(0, '-');
    }
    output
}

fn strip_comment(line: &str) -> &str {
    let hash = line.find('#');
    let semicolon = line.find(';');
    match (hash, semicolon) {
        (Some(left), Some(right)) => &line[..left.min(right)],
        (Some(index), None) | (None, Some(index)) => &line[..index],
        (None, None) => line,
    }
}

fn starts_with_ascii_case(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
}

fn strip_ascii_suffix<'a>(value: &'a str, suffix: &str) -> Option<&'a str> {
    value
        .get(value.len().checked_sub(suffix.len())?..)
        .is_some_and(|end| end.eq_ignore_ascii_case(suffix))
        .then(|| &value[..value.len() - suffix.len()])
}

fn import_error(line_number: usize, message: &str) -> AppError {
    AppError::InvalidConfig(format!(
        "invalid Equalizer APO profile at line {line_number}: {message}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUTO_EQ: &str = r#"
# AutoEQ sample
Preamp: -5.2 dB
Filter 1: ON LSC Fc 105 Hz Gain 8.8 dB Q 0.70
Filter 2: ON PK Fc 70.5 Hz Gain -6.7 dB Q 0.29
Filter 3: OFF PK Fc 1000 Hz Gain 3 dB Q 1.41
Filter 4: ON HSC Fc 10000 Hz Gain -1.25 dB Q 1.00
"#;

    #[test]
    fn parses_autoeq_apo_types_and_ignores_disabled_filters() {
        let equalizer = parse_equalizer_apo(AUTO_EQ).unwrap();
        assert_eq!(equalizer.preamp_mb, -520);
        assert_eq!(equalizer.bands.len(), 3);
        assert_eq!(
            equalizer.bands[0],
            ParametricEqualizerBand {
                filter_type: ParametricFilterType::LowShelf,
                frequency_millihz: 105_000,
                gain_mb: 880,
                q_milli: 700,
                enabled: true,
            }
        );
        assert_eq!(equalizer.bands[1].frequency_millihz, 70_500);
        assert_eq!(
            equalizer.bands[2].filter_type,
            ParametricFilterType::HighShelf
        );
    }

    #[test]
    fn canonical_apo_format_round_trips_without_float_drift() {
        let equalizer = parse_equalizer_apo(AUTO_EQ).unwrap();
        let formatted = format_equalizer_apo(&equalizer).unwrap();
        assert_eq!(parse_equalizer_apo(&formatted).unwrap(), equalizer);
        assert!(formatted.contains("Preamp: -5.2 dB"));
        assert!(formatted.contains("Filter 2: ON PK Fc 70.5 Hz Gain -6.7 dB Q 0.29"));
    }

    #[test]
    fn malformed_unsupported_and_oversized_profiles_are_rejected() {
        for invalid in [
            "Preamp: 0 dB\nFilter 1: ON LPQ Fc 100 Hz Gain 1 dB Q 1\n",
            "Preamp: 0 dB\nFilter 1: ON PK Fc nope Hz Gain 1 dB Q 1\n",
            "Preamp: 0 dB\nFilter 1: ON PK Fc 100 Hz Gain 31 dB Q 1\n",
            "Preamp: 0 dB\nFilter 1: ON PK Fc 100 Hz Gain 1 dB Q 0\n",
            "Preamp: 0 dB\n",
        ] {
            assert!(
                parse_equalizer_apo(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
        assert!(parse_equalizer_apo(&"x".repeat(MAX_EQUALIZER_APO_FILE_BYTES + 1)).is_err());
    }

    #[test]
    fn profile_rejects_more_than_twenty_enabled_filters() {
        let filters = (1..=MAX_PARAMETRIC_EQUALIZER_BANDS + 1)
            .map(|index| format!("Filter {index}: ON PK Fc 100 Hz Gain 1 dB Q 1"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(parse_equalizer_apo(&format!("Preamp: 0 dB\n{filters}")).is_err());
    }

    #[test]
    fn frequency_response_has_log_endpoints_and_includes_preamp() {
        let equalizer = ParametricEqualizer {
            preamp_mb: -600,
            bands: vec![ParametricEqualizerBand {
                filter_type: ParametricFilterType::Peaking,
                frequency_millihz: 1_000_000,
                gain_mb: 0,
                q_milli: 1_000,
                enabled: true,
            }],
        };
        let response = equalizer_frequency_response(&equalizer).unwrap();
        assert_eq!(response.points.len(), EQUALIZER_RESPONSE_POINT_COUNT);
        assert!((response.points.first().unwrap().frequency_hz - 20.0).abs() < 0.000_001);
        assert!((response.points.last().unwrap().frequency_hz - 20_000.0).abs() < 0.000_001);
        assert!(
            response
                .points
                .iter()
                .all(|point| (point.gain_db + 6.0).abs() < 0.000_001)
        );
        assert_eq!(response.db_top, 7.5);
        assert_eq!(response.db_bottom, -7.5);
    }

    #[test]
    fn frequency_response_matches_peaking_gain_near_center_frequency() {
        let equalizer = ParametricEqualizer {
            preamp_mb: -200,
            bands: vec![ParametricEqualizerBand {
                filter_type: ParametricFilterType::Peaking,
                frequency_millihz: 1_000_000,
                gain_mb: 600,
                q_milli: 1_000,
                enabled: true,
            }],
        };
        let response = equalizer_frequency_response(&equalizer).unwrap();
        let center = response
            .points
            .iter()
            .min_by(|left, right| {
                (left.frequency_hz - 1_000.0)
                    .abs()
                    .total_cmp(&(right.frequency_hz - 1_000.0).abs())
            })
            .unwrap();
        assert!((center.gain_db - 4.0).abs() < 0.02);
    }
}
