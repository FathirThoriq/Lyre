use serde::{Deserialize, Serialize};

pub const BAND_FREQS: [u32; 10] = [31, 62, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];

pub fn freq_label(f: u32) -> String {
    if f >= 1000 {
        format!("{}k", f / 1000)
    } else {
        format!("{}", f)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomPreset {
    pub name: String,
    pub bands: [f64; 10],
    pub preamp: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EqState {
    pub bands: [f64; 10],
    pub preamp: f64,
    /// Name of the currently applied preset ("Flat", "Rock", "Custom 1", ...).
    pub preset_name: String,
    pub custom_presets: Vec<CustomPreset>,
}

impl Default for EqState {
    fn default() -> Self {
        EqState {
            bands: [0.0; 10],
            preamp: 0.0,
            preset_name: "Flat".to_string(),
            custom_presets: Vec::new(),
        }
    }
}

/// Built-in preset table: (name, band gains in dB, preamp in dB).
pub fn builtin_presets() -> Vec<(&'static str, [f64; 10], f64)> {
    vec![
        ("Flat", [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], 0.0),
        ("Bass Boost", [7.0, 6.0, 5.0, 3.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0], -2.0),
        ("Treble Boost", [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 3.0, 5.0, 6.0, 7.0], -2.0),
        ("Vocal", [-2.0, -1.0, 0.0, 2.0, 4.0, 4.0, 3.0, 1.0, 0.0, 0.0], 0.0),
        ("Pop", [3.0, 2.0, 1.0, 0.0, 1.0, 2.0, 3.0, 3.0, 2.0, 1.0], 0.0),
        ("Rock", [5.0, 4.0, 2.0, 0.0, -1.0, 1.0, 3.0, 4.0, 5.0, 5.0], -1.0),
        ("Jazz", [3.0, 2.0, 1.0, 2.0, 2.0, 2.0, 1.0, 2.0, 3.0, 3.0], 0.0),
        ("Classical", [2.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 2.0, 3.0], 0.0),
        ("Electronic", [6.0, 5.0, 3.0, 0.0, -1.0, 0.0, 2.0, 3.0, 4.0, 4.0], -2.0),
        ("Acoustic", [2.0, 2.0, 1.0, 1.0, 2.0, 2.0, 2.0, 2.0, 1.0, 1.0], 0.0),
        ("Lo-Fi", [4.0, 3.0, 2.0, 1.0, 0.0, -1.0, -2.0, -4.0, -6.0, -7.0], 1.0),
        ("Podcast", [-3.0, -2.0, 0.0, 2.0, 4.0, 5.0, 4.0, 2.0, 0.0, -1.0], 1.0),
    ]
}

impl EqState {
    pub fn apply_builtin_preset(&mut self, name: &str) -> bool {
        for (n, bands, preamp) in builtin_presets() {
            if n == name {
                self.bands = bands;
                self.preamp = preamp;
                self.preset_name = n.to_string();
                return true;
            }
        }
        false
    }

    pub fn preset_names(&self) -> Vec<String> {
        let mut names: Vec<String> = builtin_presets().iter().map(|(n, _, _)| n.to_string()).collect();
        for c in &self.custom_presets {
            names.push(c.name.clone());
        }
        names
    }

    pub fn apply_preset_by_name(&mut self, name: &str) -> bool {
        if self.apply_builtin_preset(name) {
            return true;
        }
        if let Some(c) = self.custom_presets.iter().find(|c| c.name == name).cloned() {
            self.bands = c.bands;
            self.preamp = c.preamp;
            self.preset_name = c.name.clone();
            return true;
        }
        false
    }

    /// Save the current band values into Custom slot 1..=3 (rotating slots).
    pub fn save_custom_preset(&mut self) -> String {
        let existing = self.custom_presets.len();
        let slot = if existing < 3 { existing + 1 } else { 1 };
        let name = format!("Custom {}", slot);
        let entry = CustomPreset { name: name.clone(), bands: self.bands, preamp: self.preamp };
        if let Some(pos) = self.custom_presets.iter().position(|c| c.name == name) {
            self.custom_presets[pos] = entry;
        } else {
            self.custom_presets.push(entry);
        }
        self.custom_presets.sort_by(|a, b| a.name.cmp(&b.name));
        self.preset_name = name.clone();
        name
    }

    pub fn is_flat(&self) -> bool {
        self.bands.iter().all(|b| *b == 0.0) && self.preamp == 0.0
    }

    /// Build the mpv --af value: a chain of ffmpeg `equalizer` filters inside
    /// lavfi, one per non-zero band, plus a volume filter for the preamp.
    /// Returns an empty string when the EQ is flat (no filter needed).
    pub fn to_mpv_filter(&self) -> String {
        if self.is_flat() {
            return String::new();
        }
        let mut parts: Vec<String> = Vec::new();
        if self.preamp != 0.0 {
            parts.push(format!("volume=volume={}dB", fmt_db(self.preamp)));
        }
        for (i, gain) in self.bands.iter().enumerate() {
            if *gain != 0.0 {
                parts.push(format!(
                    "equalizer=f={}:width_type=o:width=2:g={}",
                    BAND_FREQS[i],
                    fmt_db(*gain)
                ));
            }
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("lavfi=[{}]", parts.join(","))
        }
    }

    pub fn clamp_band(&mut self, idx: usize, delta: f64) {
        if idx < 10 {
            self.bands[idx] = (self.bands[idx] + delta).clamp(-12.0, 12.0);
            self.preset_name = "Custom (unsaved)".to_string();
        } else {
            self.preamp = (self.preamp + delta).clamp(-12.0, 12.0);
            self.preset_name = "Custom (unsaved)".to_string();
        }
    }
}

fn fmt_db(v: f64) -> String {
    if v == v.trunc() {
        format!("{}", v as i64)
    } else {
        format!("{:.1}", v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_application_sets_bands() {
        let mut eq = EqState::default();
        assert!(eq.apply_builtin_preset("Bass Boost"));
        assert_eq!(eq.bands[0], 7.0);
        assert_eq!(eq.bands[1], 6.0);
        assert_eq!(eq.bands[9], 0.0);
        assert_eq!(eq.preset_name, "Bass Boost");
        assert!(eq.apply_builtin_preset("Flat"));
        assert!(eq.is_flat());
        assert!(!eq.apply_builtin_preset("No Such Preset"));
    }

    #[test]
    fn all_twelve_builtin_presets_exist() {
        assert_eq!(builtin_presets().len(), 12);
    }

    #[test]
    fn mpv_filter_string_generation() {
        let mut eq = EqState::default();
        assert_eq!(eq.to_mpv_filter(), "");
        eq.apply_builtin_preset("Rock");
        let f = eq.to_mpv_filter();
        assert!(f.starts_with("lavfi=["), "filter: {}", f);
        assert!(f.contains("equalizer=f=31:"), "filter: {}", f);
        assert!(f.contains("g=5"), "filter: {}", f);
        assert!(f.contains("equalizer=f=16000:"), "filter: {}", f);
        assert!(f.contains("volume=volume=-1dB"), "filter: {}", f);
    }

    #[test]
    fn custom_preset_save_and_reapply() {
        let mut eq = EqState::default();
        eq.bands[0] = 4.0;
        let name = eq.save_custom_preset();
        assert_eq!(name, "Custom 1");
        eq.apply_builtin_preset("Flat");
        assert_eq!(eq.bands[0], 0.0);
        assert!(eq.apply_preset_by_name("Custom 1"));
        assert_eq!(eq.bands[0], 4.0);
    }

    #[test]
    fn band_clamp_limits() {
        let mut eq = EqState::default();
        for _ in 0..40 {
            eq.clamp_band(0, 1.0);
        }
        assert_eq!(eq.bands[0], 12.0);
        for _ in 0..60 {
            eq.clamp_band(0, -1.0);
        }
        assert_eq!(eq.bands[0], -12.0);
    }
}
