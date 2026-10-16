//! End-to-end recognition tests against real model files.
//!
//! These need downloaded models and ONNX Runtime, so they are skipped
//! (with a message) unless `SWIFTVOICE_TEST_MODELS` points to a models
//! folder laid out like `<dir>/<model id>/...` (what `swiftvoice models
//! download` creates) and ONNX Runtime can be found (next to the test
//! binary or via `SWIFTVOICE_ORT_DYLIB`). Test audio comes from
//! `SWIFTVOICE_TEST_AUDIO` (a folder with `en.wav`, `de.wav`, ...). With
//! `SWIFTVOICE_TEST_STRICT=1` a missing model or clip fails the test
//! instead of skipping, so the `golden-audio` CI job, which sets it via
//! `scripts/golden-audio.sh`, cannot pass without transcribing.
//!
//! Expected transcripts are the output of sherpa-onnx 1.13.8 (an independent
//! C++ implementation) for the same model files. One exception: for es.wav
//! sherpa-onnx writes "que" where SwiftVoice, whose front end matches NeMo's
//! preprocessor exactly (tests/feature_parity.rs), writes the correct "qué";
//! the expectation uses the correct Spanish sentence.

use std::path::PathBuf;

use swiftvoice::asr::{self, catalog};
use swiftvoice::audio::wav;

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_dir())
}

/// Skips become failures when `SWIFTVOICE_TEST_STRICT=1`.
fn strict() -> bool {
    std::env::var("SWIFTVOICE_TEST_STRICT").is_ok_and(|v| v == "1")
}

fn normalise(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c.is_whitespace() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn check(model_id: &str, cases: &[(&str, &str)]) {
    let (Some(models), Some(audio)) = (env_dir("SWIFTVOICE_TEST_MODELS"), env_dir("SWIFTVOICE_TEST_AUDIO")) else {
        assert!(
            !strict(),
            "SWIFTVOICE_TEST_STRICT=1 needs SWIFTVOICE_TEST_MODELS and SWIFTVOICE_TEST_AUDIO to point to the models and audio folders"
        );
        eprintln!("skipping {model_id}: set SWIFTVOICE_TEST_MODELS and SWIFTVOICE_TEST_AUDIO to run");
        return;
    };
    let info = catalog::find(model_id).unwrap();
    if !asr::download::is_installed(&models, info) {
        assert!(!strict(), "{model_id} is not installed in {}", models.display());
        eprintln!("skipping {model_id}: not installed in {}", models.display());
        return;
    }
    let mut engine = asr::load_engine(info, &models, 0, "").expect("model loads");
    for (file, expected) in cases {
        let path = audio.join(file);
        if !path.exists() {
            assert!(!strict(), "{} is missing", path.display());
            eprintln!("skipping {}: missing", path.display());
            continue;
        }
        let w = wav::read_wav(&path).unwrap();
        let text = engine.transcribe(&w.to_16k(), None).unwrap();
        eprintln!("{model_id} {file}: {text}");
        assert_eq!(normalise(&text), normalise(expected), "{model_id} on {file}");
    }
}

#[test]
fn parakeet_v3_matches_reference_transcripts() {
    check(
        "parakeet-tdt-0.6b-v3",
        &[
            ("en.wav", "Ask not what your country can do for you, ask what you can do for your country."),
            ("de.wav", "Alles hat ein Ende, nur die Wurst hat zwei."),
            ("es.wav", "No preguntes qué puede hacer tu país por ti, pregunta qué puedes hacer tú por tu país."),
            ("fr.wav", "Ne vous demandez pas ce que votre pays peut faire pour vous. Demandez-vous plutôt ce que vous pouvez faire pour lui."),
            ("0.wav", "Well, I don't wish to see it any more, observed Phoebe, turning away her eyes. It is certainly very like the old portrait."),
            ("1.wav", "I love you."),
        ],
    );
}

#[test]
fn parakeet_ctc_110m_matches_reference_transcripts() {
    check(
        "parakeet-tdt-ctc-110m-en",
        &[
            ("0.wav", "Well, I don't wish to see it any more, observed Phoebe, turning away her eyes. It is certainly very like the old portrait."),
            ("1.wav", "I love you"),
            ("en.wav", "Ask not what your country can do for you. Ask what you can do for your country"),
        ],
    );
}
