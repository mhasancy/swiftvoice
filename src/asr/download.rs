//! Model installation: resumable download, digest verification, safe
//! extraction and atomic install.
//!
//! Layout: `<models_dir>/<model id>/<files>`; partial downloads live in
//! `<models_dir>/.downloads/` so an interrupted download resumes where it
//! stopped. Archives are extracted into `<id>.installing/` and renamed into
//! place only after every expected file is present with the expected size,
//! so a model folder is either complete or absent.
//!
//! A folder that contains an empty [`CUSTOM_EXPORT_MARKER`] file holds the
//! user's own export of that model (tools/model-export/README.md): its files
//! only need to exist, and digests are not checked.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Context, Result};
use sha1::Digest as _;

use super::catalog::{Digest, ModelInfo, Packaging};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Downloading,
    Verifying,
    Extracting,
    Done,
}

#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub phase: Phase,
    pub done: u64,
    pub total: Option<u64>,
}

impl Progress {
    pub fn percent(&self) -> Option<u32> {
        self.total.filter(|t| *t > 0).map(|t| ((self.done.min(t) * 100) / t) as u32)
    }
}

pub fn model_dir(models_dir: &Path, model: &ModelInfo) -> PathBuf {
    models_dir.join(model.id)
}

/// Marks a model folder as the user's own export (see the module docs).
pub const CUSTOM_EXPORT_MARKER: &str = "CUSTOM_EXPORT";

pub fn is_custom_export(models_dir: &Path, model: &ModelInfo) -> bool {
    model_dir(models_dir, model).join(CUSTOM_EXPORT_MARKER).is_file()
}

/// Every expected file exists (with the exact size when known, unless the
/// folder is a custom export).
pub fn is_installed(models_dir: &Path, model: &ModelInfo) -> bool {
    let dir = model_dir(models_dir, model);
    let custom = is_custom_export(models_dir, model);
    model.files.iter().all(|f| match fs::metadata(dir.join(f.name)) {
        Ok(m) => m.is_file() && (custom || f.size == 0 || m.len() == f.size),
        Err(_) => false,
    })
}

enum Hasher {
    Sha256(sha2::Sha256),
    Sha1(sha1::Sha1),
}

impl Hasher {
    fn for_digest(d: &Digest) -> Hasher {
        match d {
            Digest::Sha256(_) => Hasher::Sha256(sha2::Sha256::new()),
            Digest::Sha1(_) => Hasher::Sha1(sha1::Sha1::new()),
        }
    }
    fn update(&mut self, data: &[u8]) {
        match self {
            Hasher::Sha256(h) => h.update(data),
            Hasher::Sha1(h) => h.update(data),
        }
    }
    fn hex(self) -> String {
        match self {
            Hasher::Sha256(h) => hex(&h.finalize()),
            Hasher::Sha1(h) => hex(&h.finalize()),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn expected_hex(d: &Digest) -> &'static str {
    match d {
        Digest::Sha256(h) | Digest::Sha1(h) => h,
    }
}

/// SHA-256 of a file, streamed.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

/// Check installed files against the catalogue (sizes and SHA-256).
pub fn verify(models_dir: &Path, model: &ModelInfo) -> Result<()> {
    let dir = model_dir(models_dir, model);
    if is_custom_export(models_dir, model) {
        for f in model.files {
            if !dir.join(f.name).is_file() {
                bail!("{} is missing (custom export)", dir.join(f.name).display());
            }
        }
        log::info!("{}: custom export ({CUSTOM_EXPORT_MARKER} file present); digests not checked", dir.display());
        return Ok(());
    }
    for f in model.files {
        let path = dir.join(f.name);
        let meta = fs::metadata(&path).with_context(|| format!("{} is missing", path.display()))?;
        if f.size != 0 && meta.len() != f.size {
            bail!("{} has {} bytes, expected {}", path.display(), meta.len(), f.size);
        }
        if let Some(expected) = f.sha256 {
            let actual = sha256_file(&path)?;
            if actual != expected {
                bail!("{} is corrupted (SHA-256 {actual}, expected {expected})", path.display());
            }
        }
    }
    Ok(())
}

pub fn remove(models_dir: &Path, model: &ModelInfo) -> Result<()> {
    let dir = model_dir(models_dir, model);
    if dir.exists() {
        fs::remove_dir_all(&dir).with_context(|| format!("cannot delete {}", dir.display()))?;
    }
    let _ = fs::remove_file(partial_path(models_dir, model));
    Ok(())
}

fn partial_path(models_dir: &Path, model: &ModelInfo) -> PathBuf {
    models_dir.join(".downloads").join(format!("{}.part", model.id))
}

/// Download, verify and install `model`. `progress` is called often;
/// setting `cancel` aborts (the partial file is kept for resuming).
pub fn install(models_dir: &Path, model: &ModelInfo, progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<PathBuf> {
    let part = partial_path(models_dir, model);
    fs::create_dir_all(part.parent().expect("has parent")).with_context(|| format!("cannot create {}", models_dir.display()))?;
    download_verified(model.url, &model.digest, &part, progress, cancel)?;
    progress(Progress { phase: Phase::Extracting, done: 0, total: None });
    let staging = models_dir.join(format!("{}.installing", model.id));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(&staging)?;
    match model.packaging {
        Packaging::TarBz2 { inner_dir } => {
            extract_tar_bz2(&part, inner_dir, model, &staging, cancel)?;
            fs::remove_file(&part).ok();
        }
        Packaging::Single { file_name } => {
            fs::rename(&part, staging.join(file_name)).with_context(|| format!("cannot move the download into {}", staging.display()))?;
        }
    }
    for f in model.files {
        let p = staging.join(f.name);
        let meta = fs::metadata(&p).with_context(|| format!("the download did not contain {}", f.name))?;
        if f.size != 0 && meta.len() != f.size {
            bail!("{} has {} bytes after extraction, expected {}", f.name, meta.len(), f.size);
        }
    }
    let target = model_dir(models_dir, model);
    if target.exists() {
        fs::remove_dir_all(&target).with_context(|| format!("cannot replace {}", target.display()))?;
    }
    fs::rename(&staging, &target).with_context(|| format!("cannot install into {}", target.display()))?;
    progress(Progress { phase: Phase::Done, done: 1, total: Some(1) });
    log::info!("model {} installed in {}", model.id, target.display());
    Ok(target)
}

/// Download `url` to `part` (resuming), verifying `digest` over the whole file.
pub fn download_verified(url: &str, digest: &Digest, part: &Path, progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<()> {
    let mut hasher = Hasher::for_digest(digest);
    let mut have = 0u64;
    if part.exists() {
        // Hash what we already have so the final digest covers everything.
        progress(Progress { phase: Phase::Verifying, done: 0, total: None });
        let mut f = File::open(part)?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            have += n as u64;
        }
    }
    let resp = super::http::get(url, have)?;
    let mut file = match resp.status {
        206 if have > 0 => OpenOptions::new().append(true).open(part)?,
        200 => {
            // Server ignored the range (or fresh download): start over.
            hasher = Hasher::for_digest(digest);
            have = 0;
            File::create(part).with_context(|| format!("cannot create {}", part.display()))?
        }
        416 if have > 0 => OpenOptions::new().append(true).open(part)?, // already complete
        code => bail!("download failed: HTTP {code} from {url}"),
    };
    let total = resp.content_length.map(|l| if resp.status == 206 { l + have } else { l });
    if resp.status != 416 {
        let mut body = resp.body;
        let mut buf = vec![0u8; 256 * 1024];
        let mut last_report = std::time::Instant::now();
        loop {
            if cancel.load(Ordering::Relaxed) {
                file.flush().ok();
                bail!("download cancelled (it will resume next time)");
            }
            let n = body.read(&mut buf).context("network error while downloading")?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).context("cannot write the download (is the disk full?)")?;
            hasher.update(&buf[..n]);
            have += n as u64;
            if last_report.elapsed().as_millis() >= 200 {
                last_report = std::time::Instant::now();
                progress(Progress { phase: Phase::Downloading, done: have, total });
            }
        }
    }
    file.flush()?;
    drop(file);
    if let Some(t) = total {
        if have < t {
            bail!("download incomplete ({have} of {t} bytes); try again to resume");
        }
    }
    progress(Progress { phase: Phase::Verifying, done: have, total });
    let actual = hasher.hex();
    let expected = expected_hex(digest);
    if actual != expected {
        let _ = fs::remove_file(part);
        bail!("downloaded file is corrupted or was tampered with (digest {actual}, expected {expected}); it was deleted");
    }
    Ok(())
}

/// Extract only the expected model files from `inner_dir/` in the archive.
fn extract_tar_bz2(archive: &Path, inner_dir: &str, model: &ModelInfo, dest: &Path, cancel: &AtomicBool) -> Result<()> {
    let file = File::open(archive)?;
    let decoder = bzip2::read::BzDecoder::new(std::io::BufReader::with_capacity(1 << 20, file));
    let mut tar = tar::Archive::new(decoder);
    let wanted: Vec<&str> = model.files.iter().map(|f| f.name).collect();
    for entry in tar.entries().context("corrupt archive")? {
        if cancel.load(Ordering::Relaxed) {
            bail!("installation cancelled");
        }
        let mut entry = entry.context("corrupt archive entry")?;
        let path = entry.path()?.into_owned();
        let parts: Vec<String> = path
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        // Accept exactly "<inner_dir>/<file>" for expected files; ignore the
        // rest (test audio, READMEs). Paths with ".." never match.
        if parts.len() == 2 && parts[0] == inner_dir && wanted.contains(&parts[1].as_str()) && entry.header().entry_type().is_file() {
            let out_path = dest.join(&parts[1]);
            let mut out = File::create(&out_path).with_context(|| format!("cannot write {}", out_path.display()))?;
            std::io::copy(&mut entry, &mut out).with_context(|| format!("cannot extract {}", parts[1]))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asr::catalog::ModelFile;

    fn sha256_hex(data: &[u8]) -> String {
        let mut h = sha2::Sha256::new();
        h.update(data);
        hex(&h.finalize())
    }

    #[test]
    fn extracts_only_expected_files_from_the_inner_folder() {
        let dir = crate::paths::test_dir("extract");
        let archive = dir.join("m.tar.bz2");
        {
            let f = File::create(&archive).unwrap();
            let enc = bzip2::write::BzEncoder::new(f, bzip2::Compression::fast());
            let mut b = tar::Builder::new(enc);
            for (name, data) in [
                ("m/tokens.txt", &b"a 0\n<blk> 1\n"[..]),
                ("m/model.onnx", &b"model-bytes"[..]),
                ("m/test_wavs/0.wav", &b"RIFF"[..]),
                ("other/model.onnx", &b"wrong"[..]),
            ] {
                let mut h = tar::Header::new_gnu();
                h.set_size(data.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                b.append_data(&mut h, name, data).unwrap();
            }
            b.into_inner().unwrap().finish().unwrap();
        }
        static FILES: &[ModelFile] =
            &[ModelFile { name: "model.onnx", size: 11, sha256: None }, ModelFile { name: "tokens.txt", size: 12, sha256: None }];
        let model = ModelInfo { files: FILES, ..*crate::asr::catalog::find("parakeet-tdt-ctc-110m-en").unwrap() };
        let dest = dir.join("out");
        fs::create_dir_all(&dest).unwrap();
        extract_tar_bz2(&archive, "m", &model, &dest, &AtomicBool::new(false)).unwrap();
        assert_eq!(fs::read(dest.join("model.onnx")).unwrap(), b"model-bytes");
        assert!(dest.join("tokens.txt").exists());
        assert!(!dest.join("0.wav").exists());
        assert_eq!(fs::read_dir(&dest).unwrap().count(), 2);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn verify_detects_corruption() {
        let dir = crate::paths::test_dir("verify");
        let data = b"hello model";
        let digest = Box::leak(sha256_hex(data).into_boxed_str());
        let files: &'static [ModelFile] =
            Box::leak(vec![ModelFile { name: "w.bin", size: data.len() as u64, sha256: Some(digest) }].into_boxed_slice());
        let model = ModelInfo { id: "test-model", files, ..*crate::asr::catalog::find("silero-vad").unwrap() };
        let mdir = model_dir(&dir, &model);
        fs::create_dir_all(&mdir).unwrap();
        fs::write(mdir.join("w.bin"), data).unwrap();
        assert!(is_installed(&dir, &model));
        verify(&dir, &model).unwrap();
        fs::write(mdir.join("w.bin"), b"hello modeX").unwrap();
        assert!(verify(&dir, &model).unwrap_err().to_string().contains("corrupted"));
        fs::write(mdir.join("w.bin"), b"short").unwrap();
        assert!(!is_installed(&dir, &model));
        // The user's own export: files only need to exist.
        fs::write(mdir.join(CUSTOM_EXPORT_MARKER), b"").unwrap();
        assert!(is_installed(&dir, &model));
        verify(&dir, &model).unwrap();
        remove(&dir, &model).unwrap();
        assert!(!mdir.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn progress_percent() {
        let p = Progress { phase: Phase::Downloading, done: 50, total: Some(200) };
        assert_eq!(p.percent(), Some(25));
        assert_eq!(Progress { phase: Phase::Downloading, done: 5, total: None }.percent(), None);
    }
}
