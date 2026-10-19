//! Built-in model catalogue.
//!
//! Every download is pinned: the URL, byte size and digest below were
//! verified for this release (Parakeet and Silero files by downloading
//! them and computing SHA-256; whisper.cpp files by the SHA-1 table that
//! whisper.cpp publishes in models/README.md). A download whose digest does
//! not match is deleted and reported, never installed.
//!
//! RAM figures marked measured are the resident memory with the model loaded
//! (`asr.low_memory = true`), from `swiftvoice --benchmark`; see
//! docs/BENCHMARKS.md for the hardware and method. The others are estimates.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    /// Parakeet TDT transducer (encoder + decoder + joiner), ONNX Runtime.
    ParakeetTdt,
    /// NeMo CTC model, ONNX Runtime.
    NemoCtc,
    /// whisper.cpp ggml model.
    Whisper,
    /// Silero voice activity detector (not a speech recogniser).
    Vad,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Digest {
    Sha256(&'static str),
    /// Only used where upstream publishes SHA-1 alone (whisper.cpp).
    Sha1(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packaging {
    /// `.tar.bz2` archive; files are taken from `inner_dir` inside it.
    TarBz2 { inner_dir: &'static str },
    /// The download is the model file itself, saved as `file_name`.
    Single { file_name: &'static str },
}

#[derive(Debug, Clone, Copy)]
pub struct ModelFile {
    pub name: &'static str,
    /// Exact size in bytes; 0 = not published upstream (the download digest
    /// is still verified).
    pub size: u64,
    /// SHA-256 of the installed file, when known (verified on `models verify`).
    pub sha256: Option<&'static str>,
}

#[derive(Debug, Clone, Copy)]
pub struct ModelInfo {
    pub id: &'static str,
    pub display: &'static str,
    pub engine: EngineKind,
    /// ISO 639-1 codes; `["*"]` means Whisper's ~99 languages.
    pub languages: &'static [&'static str],
    pub license: &'static str,
    pub model_card: &'static str,
    pub url: &'static str,
    /// Download size (exact for archives; whisper.cpp sizes are the MiB
    /// figures upstream publishes and are only used for display).
    pub download_bytes: u64,
    pub digest: Digest,
    pub packaging: Packaging,
    pub files: &'static [ModelFile],
    /// Resident memory after loading and one inference, in MB.
    pub ram_mb: u32,
    /// Whether `ram_mb` / `speed` were measured (true) or estimated.
    pub measured: bool,
    /// Short human-readable speed note.
    pub speed: &'static str,
    /// Punctuation and capitalisation in the output.
    pub punctuation: bool,
}

impl ModelInfo {
    pub fn disk_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    pub fn supports_language(&self, lang: &str) -> bool {
        lang == "auto" || self.languages.contains(&"*") || self.languages.contains(&lang)
    }

    pub fn is_recognizer(&self) -> bool {
        self.engine != EngineKind::Vad
    }

    /// "25 languages", "English", "~99 languages".
    pub fn language_summary(&self) -> String {
        if self.languages.contains(&"*") {
            "~99 languages incl. Bengali".into()
        } else if self.languages == ["en"] {
            "English".into()
        } else {
            format!("{} languages", self.languages.len())
        }
    }

    /// One-line description for menus and `models list`.
    pub fn summary(&self) -> String {
        format!(
            "{} · {} · {} download · ~{} RAM{} · {}",
            self.language_summary(),
            self.license,
            human_bytes(self.download_bytes),
            human_mb(self.ram_mb),
            if self.measured { "" } else { " (est.)" },
            self.speed
        )
    }
}

pub fn human_bytes(b: u64) -> String {
    let mb = b as f64 / (1024.0 * 1024.0);
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else if mb >= 10.0 {
        format!("{mb:.0} MB")
    } else {
        format!("{mb:.1} MB")
    }
}

fn human_mb(mb: u32) -> String {
    if mb >= 1024 {
        format!("{:.1} GB", mb as f64 / 1024.0)
    } else {
        format!("{mb} MB")
    }
}

/// The 25 European languages of Parakeet TDT 0.6B v3 (per NVIDIA's model card).
pub const PARAKEET_V3_LANGUAGES: &[&str] = &[
    "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it", "lv", "lt", "mt", "pl", "pt", "ro", "sk", "sl", "es",
    "sv", "ru", "uk",
];

macro_rules! sherpa_url {
    ($file:literal) => {
        concat!("https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/", $file)
    };
}

macro_rules! whisper_url {
    ($file:literal) => {
        concat!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/", $file)
    };
}

pub const CATALOG: &[ModelInfo] = &[
    ModelInfo {
        id: "parakeet-tdt-0.6b-v3",
        display: "Parakeet TDT 0.6B v3",
        engine: EngineKind::ParakeetTdt,
        languages: PARAKEET_V3_LANGUAGES,
        license: "CC-BY-4.0",
        model_card: "https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3",
        url: sherpa_url!("sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2"),
        download_bytes: 487_170_055,
        digest: Digest::Sha256("5793d0fd397c5778d2cf2126994d58e9d56b1be7c04d13c7a15bb1b4eafb16bf"),
        packaging: Packaging::TarBz2 { inner_dir: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8" },
        files: &[
            ModelFile {
                name: "encoder.int8.onnx",
                size: 652_184_281,
                sha256: Some("acfc2b4456377e15d04f0243af540b7fe7c992f8d898d751cf134c3a55fd2247"),
            },
            ModelFile {
                name: "decoder.int8.onnx",
                size: 11_845_275,
                sha256: Some("179e50c43d1a9de79c8a24149a2f9bac6eb5981823f2a2ed88d655b24248db4e"),
            },
            ModelFile {
                name: "joiner.int8.onnx",
                size: 6_355_277,
                sha256: Some("3164c13fc2821009440d20fcb5fdc78bff28b4db2f8d0f0b329101719c0948b3"),
            },
            ModelFile {
                name: "tokens.txt",
                size: 93_939,
                sha256: Some("d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d"),
            },
        ],
        ram_mb: 850,
        measured: true,
        speed: "best accuracy, multilingual",
        punctuation: true,
    },
    ModelInfo {
        id: "parakeet-tdt-0.6b-v2",
        display: "Parakeet TDT 0.6B v2 (English)",
        engine: EngineKind::ParakeetTdt,
        languages: &["en"],
        license: "CC-BY-4.0",
        model_card: "https://huggingface.co/nvidia/parakeet-tdt-0.6b-v2",
        url: sherpa_url!("sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8.tar.bz2"),
        download_bytes: 482_468_385,
        digest: Digest::Sha256("157c157bc51155e03e37d2466522a3a737dd9c72bb25f36eb18912964161e1ad"),
        packaging: Packaging::TarBz2 { inner_dir: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8" },
        files: &[
            ModelFile {
                name: "encoder.int8.onnx",
                size: 652_184_296,
                sha256: Some("a32b12d17bbbc309d0686fbbcc2987b5e9b8333a7da83fa6b089f0a2acd651ab"),
            },
            ModelFile {
                name: "decoder.int8.onnx",
                size: 7_257_753,
                sha256: Some("b6bb64963457237b900e496ee9994b59294526439fbcc1fecf705b31a15c6b4e"),
            },
            ModelFile {
                name: "joiner.int8.onnx",
                size: 1_739_080,
                sha256: Some("7946164367946e7f9f29a122407c3252b680dbae9a51343eb2488d057c3c43d2"),
            },
            ModelFile { name: "tokens.txt", size: 9_384, sha256: Some("ec182b70dd42113aff6c5372c75cac58c952443eb22322f57bbd7f53977d497d") },
        ],
        ram_mb: 830,
        measured: true,
        speed: "best English accuracy",
        punctuation: true,
    },
    ModelInfo {
        id: "parakeet-tdt-ctc-110m-en",
        display: "Parakeet TDT-CTC 110M (English, light)",
        engine: EngineKind::NemoCtc,
        languages: &["en"],
        license: "CC-BY-4.0",
        model_card: "https://catalog.ngc.nvidia.com/orgs/nvidia/teams/nemo/models/parakeet-tdt_ctc-110m",
        url: sherpa_url!("sherpa-onnx-nemo-parakeet_tdt_ctc_110m-en-36000-int8.tar.bz2"),
        download_bytes: 104_337_827,
        digest: Digest::Sha256("17f945007b52ccd8b7200ffc7c5652e9e8e961dfdf479cefcabd06cf5703630b"),
        packaging: Packaging::TarBz2 { inner_dir: "sherpa-onnx-nemo-parakeet_tdt_ctc_110m-en-36000-int8" },
        files: &[
            ModelFile {
                name: "model.int8.onnx",
                size: 131_652_171,
                sha256: Some("9177a9146cf32ee0cc8152276ef95116f312018d316be37ccf57f7efea81fc1a"),
            },
            ModelFile { name: "tokens.txt", size: 9_953, sha256: Some("450e56bd2f036fe5b6aa821865838cc5aa9d8b0106134ce9a9ba0664abe6cd10") },
        ],
        ram_mb: 250,
        measured: true,
        speed: "fastest, lowest memory",
        punctuation: true,
    },
    ModelInfo {
        id: "whisper-large-v3-turbo-q5_0",
        display: "Whisper large-v3-turbo q5_0",
        engine: EngineKind::Whisper,
        languages: &["*"],
        license: "MIT",
        model_card: "https://huggingface.co/ggerganov/whisper.cpp",
        url: whisper_url!("ggml-large-v3-turbo-q5_0.bin"),
        download_bytes: 547 * 1024 * 1024,
        digest: Digest::Sha1("e050f7970618a659205450ad97eb95a18d69c9ee"),
        packaging: Packaging::Single { file_name: "ggml-large-v3-turbo-q5_0.bin" },
        files: &[ModelFile { name: "ggml-large-v3-turbo-q5_0.bin", size: 0, sha256: None }],
        ram_mb: 1000,
        measured: false,
        speed: "multilingual fallback, slower",
        punctuation: true,
    },
    ModelInfo {
        id: "whisper-small",
        display: "Whisper small",
        engine: EngineKind::Whisper,
        languages: &["*"],
        license: "MIT",
        model_card: "https://huggingface.co/ggerganov/whisper.cpp",
        url: whisper_url!("ggml-small.bin"),
        download_bytes: 466 * 1024 * 1024,
        digest: Digest::Sha1("55356645c2b361a969dfd0ef2c5a50d530afd8d5"),
        packaging: Packaging::Single { file_name: "ggml-small.bin" },
        files: &[ModelFile { name: "ggml-small.bin", size: 0, sha256: None }],
        ram_mb: 850,
        measured: false,
        speed: "multilingual, lower accuracy",
        punctuation: true,
    },
    ModelInfo {
        id: "whisper-base",
        display: "Whisper base",
        engine: EngineKind::Whisper,
        languages: &["*"],
        license: "MIT",
        model_card: "https://huggingface.co/ggerganov/whisper.cpp",
        url: whisper_url!("ggml-base.bin"),
        download_bytes: 142 * 1024 * 1024,
        digest: Digest::Sha1("465707469ff3a37a2b9b8d8f89f2f99de7299dac"),
        packaging: Packaging::Single { file_name: "ggml-base.bin" },
        files: &[ModelFile { name: "ggml-base.bin", size: 0, sha256: None }],
        ram_mb: 390,
        measured: false,
        speed: "multilingual, small and fast",
        punctuation: true,
    },
    ModelInfo {
        id: "silero-vad",
        display: "Silero VAD v6.2 (speech detector)",
        engine: EngineKind::Vad,
        languages: &["*"],
        license: "MIT",
        model_card: "https://github.com/snakers4/silero-vad",
        url: "https://raw.githubusercontent.com/snakers4/silero-vad/v6.2/src/silero_vad/data/silero_vad.onnx",
        download_bytes: 2_327_524,
        digest: Digest::Sha256("1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3"),
        packaging: Packaging::Single { file_name: "silero_vad.onnx" },
        files: &[ModelFile {
            name: "silero_vad.onnx",
            size: 2_327_524,
            sha256: Some("1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3"),
        }],
        ram_mb: 10,
        measured: false,
        speed: "runs on every 32 ms audio frame",
        punctuation: false,
    },
];

pub fn find(id: &str) -> Option<&'static ModelInfo> {
    CATALOG.iter().find(|m| m.id == id)
}

pub fn recognizers() -> impl Iterator<Item = &'static ModelInfo> {
    CATALOG.iter().filter(|m| m.is_recognizer())
}

/// Language names for menus (subset that SwiftVoice offers by name).
pub const LANGUAGE_NAMES: &[(&str, &str)] = &[
    ("auto", "Auto-detect"),
    ("en", "English"),
    ("de", "German"),
    ("fr", "French"),
    ("es", "Spanish"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("nl", "Dutch"),
    ("pl", "Polish"),
    ("ru", "Russian"),
    ("uk", "Ukrainian"),
    ("sv", "Swedish"),
    ("da", "Danish"),
    ("fi", "Finnish"),
    ("cs", "Czech"),
    ("sk", "Slovak"),
    ("sl", "Slovenian"),
    ("hr", "Croatian"),
    ("bg", "Bulgarian"),
    ("ro", "Romanian"),
    ("hu", "Hungarian"),
    ("el", "Greek"),
    ("et", "Estonian"),
    ("lv", "Latvian"),
    ("lt", "Lithuanian"),
    ("mt", "Maltese"),
    ("bn", "Bengali (Whisper)"),
    ("hi", "Hindi (Whisper)"),
    ("ar", "Arabic (Whisper)"),
    ("tr", "Turkish (Whisper)"),
    ("zh", "Chinese (Whisper)"),
    ("ja", "Japanese (Whisper)"),
    ("ko", "Korean (Whisper)"),
];

pub fn language_name(code: &str) -> &str {
    LANGUAGE_NAMES.iter().find(|(c, _)| *c == code).map(|(_, n)| *n).unwrap_or(code)
}

/// "auto" or a two-letter ISO 639-1 code, as accepted by the engines.
pub fn is_valid_language_setting(lang: &str) -> bool {
    lang == "auto" || (lang.len() == 2 && lang.bytes().all(|b| b.is_ascii_lowercase()))
}

/// Pick the recogniser for `language`: the main model if it supports the
/// language, otherwise the fallback model if it does.
pub fn route<'a>(language: &str, main: &'a str, fallback: &'a str) -> Result<&'a str, String> {
    let main_info = find(main).ok_or_else(|| format!("unknown model \"{main}\" (see: swiftvoice models list)"))?;
    if main_info.supports_language(language) {
        return Ok(main);
    }
    match find(fallback) {
        Some(fb) if fb.supports_language(language) => Ok(fallback),
        _ => Err(format!(
            "{} does not support {}; set asr.fallback_model to a Whisper model to use it",
            main_info.display,
            language_name(language)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_digests_well_formed() {
        let mut ids: Vec<_> = CATALOG.iter().map(|m| m.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), CATALOG.len());
        for m in CATALOG {
            match m.digest {
                Digest::Sha256(h) => assert_eq!(h.len(), 64, "{}", m.id),
                Digest::Sha1(h) => assert_eq!(h.len(), 40, "{}", m.id),
            }
            assert!(m.url.starts_with("https://"), "{}", m.id);
            for f in m.files {
                if let Some(h) = f.sha256 {
                    assert_eq!(h.len(), 64);
                }
            }
        }
    }

    #[test]
    fn parakeet_v3_covers_25_languages_but_not_bengali() {
        let m = find("parakeet-tdt-0.6b-v3").unwrap();
        assert_eq!(m.languages.len(), 25);
        assert!(m.supports_language("de") && m.supports_language("auto"));
        assert!(!m.supports_language("bn"));
        assert!(find("whisper-base").unwrap().supports_language("bn"));
    }

    #[test]
    fn routing_falls_back_for_unsupported_languages() {
        assert_eq!(route("en", "parakeet-tdt-0.6b-v3", "whisper-small"), Ok("parakeet-tdt-0.6b-v3"));
        assert_eq!(route("bn", "parakeet-tdt-0.6b-v3", "whisper-small"), Ok("whisper-small"));
        assert!(route("bn", "parakeet-tdt-0.6b-v3", "parakeet-tdt-0.6b-v2").is_err());
        assert!(route("en", "nope", "whisper-small").is_err());
        assert_eq!(route("de", "parakeet-tdt-0.6b-v2", "whisper-base"), Ok("whisper-base"));
    }

    #[test]
    fn language_settings() {
        assert!(is_valid_language_setting("auto"));
        assert!(is_valid_language_setting("bn"));
        assert!(!is_valid_language_setting("EN"));
        assert!(!is_valid_language_setting("english"));
        assert!(!is_valid_language_setting("eng"), "Whisper and Parakeet require two-letter codes");
    }
}
