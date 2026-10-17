//! Utterance segmentation and speculative transcription scheduling.
//!
//! This state machine is the heart of SwiftVoice's latency strategy. It
//! consumes 32 ms frames plus a VAD probability and emits:
//!
//! * `Snapshot(Pause)` as soon as the speaker pauses for `pause_ms`
//!   (default 200 ms). The audio up to that point is transcribed right away,
//!   while the app is still waiting to see whether the pause is the end.
//! * `UtteranceEnd` when the utterance ends (endpoint silence in toggle mode,
//!   key release in push-to-talk). It names the snapshot whose transcript
//!   is the final text. If no speech followed the last pause, that is the
//!   pause snapshot already being transcribed, so the result is usually
//!   ready the moment the endpoint is confirmed. Otherwise a `Final`
//!   snapshot is emitted first.
//! * `Commit` in long dictation: after `commit_after` seconds the window is
//!   cut at the next pause, so each transcription stays short (the text
//!   after release then only needs the audio since that pause) and memory
//!   stays bounded however long you talk. When an utterance ends while
//!   speech followed the last pause, the window is committed at that pause
//!   (its transcript is already running) and only the rest is transcribed.
//!   Snapshots after a commit start with the last phrase of the previous
//!   window (from an earlier pause) as context, so the recogniser continues
//!   the sentence instead of starting a new one; engines drop the words
//!   spoken in the context.
//! * `Snapshot(Partial)` periodically while speaking, for the live overlay.
//!
//! Window ids are local to an utterance; utterance ids are local to a
//! recording session. A snapshot is identified by `(utterance, window, end)`
//! where `end` is the snapshot length in samples.

use std::sync::Arc;

use super::vad::{FRAME_MS, FRAME_SAMPLES};

/// Bounds of the left context after a commit (see the module docs).
pub const CONTEXT_MIN_MS: u32 = 1000;
pub const CONTEXT_MAX_MS: u32 = 2500;
/// Shortest pause snapshot worth splitting at when an utterance ends.
pub const SPLIT_MIN_MS: u32 = 2000;
/// How far back a forced window cut looks for the quietest frame to cut at.
pub const SEAM_LOOKBACK_MS: u32 = 2000;

#[derive(Debug, Clone)]
pub struct SegmenterConfig {
    /// Probability that starts speech.
    pub threshold: f32,
    /// Probability below which ongoing speech counts as silence.
    pub neg_threshold: f32,
    /// Consecutive speech frames required for onset.
    pub min_speech_frames: u32,
    /// Silence frames after speech that trigger a pause snapshot.
    pub pause_frames: u32,
    /// Silence frames that end an utterance (`None` = push-to-talk).
    pub endpoint_frames: Option<u32>,
    /// Frames of audio kept before speech onset.
    pub pre_roll_frames: u32,
    /// Window length (frames) after which the next pause commits.
    pub commit_after_frames: u32,
    /// Hard window limit (frames); forces a cut even mid-speech.
    pub max_window_frames: u32,
    /// Context after a commit: audio from the latest pause point (or the
    /// window start) at least `context_min_samples` and at most
    /// `context_max_samples` before the commit; none if there is no such
    /// point. Starting the context in a pause matters: the recogniser can
    /// return nothing for a clip that begins mid-word.
    pub context_min_samples: usize,
    pub context_max_samples: usize,
    /// At the end of an utterance, a pause snapshot at least this long is
    /// committed and only the audio after it is transcribed.
    pub split_min_samples: usize,
    /// Frames between partial snapshots while speaking (`None` = off).
    pub partial_interval_frames: Option<u32>,
    /// Frames without any speech after the session starts before giving up.
    pub no_speech_timeout_frames: Option<u32>,
    /// Frames of silence after an utterance before the session stops
    /// (continuous mode).
    pub idle_timeout_frames: Option<u32>,
}

impl SegmenterConfig {
    /// Build from user settings. `endpoint_ms` = `None` for push-to-talk.
    #[allow(clippy::too_many_arguments)]
    pub fn from_ms(
        threshold: f32,
        pause_ms: u32,
        endpoint_ms: Option<u32>,
        pre_roll_ms: u32,
        commit_after_secs: u32,
        partial_interval_ms: Option<u32>,
        no_speech_timeout_ms: Option<u32>,
        idle_timeout_ms: Option<u32>,
    ) -> SegmenterConfig {
        let frames = |ms: u32| ms.div_ceil(super::vad::FRAME_MS).max(1);
        let commit_after_frames = frames(commit_after_secs * 1000);
        SegmenterConfig {
            threshold,
            neg_threshold: (threshold - 0.15).max(0.05),
            min_speech_frames: 2,
            pause_frames: frames(pause_ms),
            endpoint_frames: endpoint_ms.map(frames),
            pre_roll_frames: pre_roll_ms / super::vad::FRAME_MS,
            commit_after_frames,
            max_window_frames: (commit_after_frames + frames(10_000)).min(frames(28_000)),
            context_min_samples: CONTEXT_MIN_MS as usize * 16,
            context_max_samples: CONTEXT_MAX_MS as usize * 16,
            split_min_samples: SPLIT_MIN_MS as usize * 16,
            partial_interval_frames: partial_interval_ms.map(frames),
            no_speech_timeout_frames: no_speech_timeout_ms.map(frames),
            idle_timeout_frames: idle_timeout_ms.map(frames),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotKind {
    /// Live preview; may be dropped if the recogniser is busy.
    Partial,
    /// Speculative transcription at a pause.
    Pause,
    /// Transcription needed at the end of an utterance (no matching pause
    /// snapshot exists) or at a forced window cut.
    Final,
}

/// Identity of a snapshot transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SnapKey {
    pub utterance: u32,
    pub window: u32,
    pub end: usize,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub key: SnapKey,
    pub kind: SnapshotKind,
    /// `context` samples from the previous window, then the window's first
    /// `key.end` samples.
    pub audio: Arc<[f32]>,
    pub context: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    /// Silence reached the endpoint (toggle mode).
    Endpoint,
    /// Push-to-talk release or manual stop.
    Finish,
}

#[derive(Debug, Clone)]
pub enum SegEvent {
    SpeechStart {
        utterance: u32,
    },
    Snapshot(Snapshot),
    /// `key` is final for its window; the next window starts after it.
    Commit {
        key: SnapKey,
    },
    /// Final text = all committed windows of this utterance, then `last`.
    UtteranceEnd {
        utterance: u32,
        last: Option<SnapKey>,
        reason: EndReason,
    },
    /// The session ended (or timed out) without any speech.
    NoSpeech,
    /// Continuous mode: no speech for the idle timeout after an utterance.
    IdleTimeout,
}

pub struct Segmenter {
    cfg: SegmenterConfig,
    utterance: u32,
    window_id: u32,
    window: Vec<f32>,
    /// Last phrase of the previous (committed) window of this utterance.
    context: Vec<f32>,
    /// Pause snapshot positions in the current window.
    pauses: Vec<usize>,
    window_has_speech: bool,
    utterance_has_speech: bool,
    session_had_speech: bool,
    speaking: bool,
    speech_run: u32,
    silence_frames: u32,
    last_speech_end: usize,
    last_snapshot: Option<SnapKey>,
    frames_since_partial: u32,
    quiet_frames: u32,
    timeout_reported: bool,
}

impl Segmenter {
    pub fn new(cfg: SegmenterConfig) -> Segmenter {
        Segmenter {
            cfg,
            utterance: 0,
            window_id: 0,
            window: Vec::with_capacity(16_000 * 4),
            context: Vec::new(),
            pauses: Vec::new(),
            window_has_speech: false,
            utterance_has_speech: false,
            session_had_speech: false,
            speaking: false,
            speech_run: 0,
            silence_frames: 0,
            last_speech_end: 0,
            last_snapshot: None,
            frames_since_partial: 0,
            quiet_frames: 0,
            timeout_reported: false,
        }
    }

    pub fn config(&self) -> &SegmenterConfig {
        &self.cfg
    }

    /// True while speech is in progress or the speaker paused for less than
    /// `pause_ms`: a push-to-talk release now would cut the last word, so
    /// the caller keeps recording for the release tail.
    pub fn needs_tail(&self) -> bool {
        self.window_has_speech && self.silence_frames < self.cfg.pause_frames
    }

    pub fn has_speech(&self) -> bool {
        self.session_had_speech
    }

    /// Samples currently buffered (bounded by `max_window_frames`).
    pub fn buffered_samples(&self) -> usize {
        self.window.len()
    }

    /// Feed one frame of [`FRAME_SAMPLES`] samples with its VAD probability.
    pub fn push_frame(&mut self, frame: &[f32], prob: f32, out: &mut Vec<SegEvent>) {
        debug_assert_eq!(frame.len(), FRAME_SAMPLES);
        self.window.extend_from_slice(frame);
        let threshold = if self.speaking { self.cfg.neg_threshold } else { self.cfg.threshold };
        let is_speech = prob >= threshold;

        if !self.window_has_speech {
            self.speech_run = if is_speech { self.speech_run + 1 } else { 0 };
            if self.speech_run >= self.cfg.min_speech_frames {
                self.onset(out);
            } else {
                self.trim_leading_silence();
                if self.utterance_has_speech {
                    // Silence after a commit: still counts toward the endpoint.
                    self.silence_frames += 1;
                } else {
                    self.quiet_frames += 1;
                    self.check_timeouts(out);
                }
            }
        } else {
            if is_speech {
                self.speaking = true;
                self.silence_frames = 0;
                self.last_speech_end = self.window.len();
            } else {
                self.speaking = false;
                self.silence_frames += 1;
            }
            if self.silence_frames == self.cfg.pause_frames {
                let end = self.window.len();
                self.emit_snapshot(SnapshotKind::Pause, end, out);
                self.pauses.push(end);
                if self.window.len() >= self.cfg.commit_after_frames as usize * FRAME_SAMPLES {
                    self.commit(end, out);
                }
            } else if self.window.len() >= self.cfg.max_window_frames as usize * FRAME_SAMPLES {
                // Continuous speech without a usable pause: cut at the
                // quietest recent frame, not at this arbitrary (likely
                // mid-word) one.
                let end = self.quietest_cut();
                self.emit_snapshot(SnapshotKind::Final, end, out);
                self.commit(end, out);
            }
            if self.window_has_speech && self.speaking {
                if let Some(interval) = self.cfg.partial_interval_frames {
                    self.frames_since_partial += 1;
                    if self.frames_since_partial >= interval {
                        self.frames_since_partial = 0;
                        let end = self.window.len();
                        self.emit_snapshot(SnapshotKind::Partial, end, out);
                    }
                }
            }
        }

        if self.utterance_has_speech {
            if let Some(endpoint) = self.cfg.endpoint_frames {
                if self.silence_frames >= endpoint {
                    self.end_utterance(EndReason::Endpoint, out);
                }
            }
        }
    }

    /// End the current utterance now (push-to-talk release, manual stop).
    pub fn finish(&mut self, out: &mut Vec<SegEvent>) {
        if self.utterance_has_speech {
            self.end_utterance(EndReason::Finish, out);
        } else {
            out.push(SegEvent::NoSpeech);
        }
    }

    fn onset(&mut self, out: &mut Vec<SegEvent>) {
        // Start the window exactly `pre_roll` before the first speech frame.
        let keep = (self.cfg.pre_roll_frames + self.speech_run) as usize * FRAME_SAMPLES;
        if self.window.len() > keep {
            let cut = self.window.len() - keep;
            self.window.drain(..cut);
        }
        self.window_has_speech = true;
        self.speaking = true;
        self.silence_frames = 0;
        self.last_speech_end = self.window.len();
        self.frames_since_partial = 0;
        self.quiet_frames = 0;
        self.timeout_reported = false;
        if !self.utterance_has_speech {
            self.utterance_has_speech = true;
            self.session_had_speech = true;
            out.push(SegEvent::SpeechStart { utterance: self.utterance });
        }
    }

    fn trim_leading_silence(&mut self) {
        let keep = (self.cfg.pre_roll_frames + self.speech_run) as usize * FRAME_SAMPLES;
        // Trim in batches so the copy cost stays negligible.
        if self.window.len() > keep + 16 * FRAME_SAMPLES {
            let cut = self.window.len() - keep;
            self.window.drain(..cut);
        }
    }

    fn check_timeouts(&mut self, out: &mut Vec<SegEvent>) {
        if self.timeout_reported {
            return;
        }
        let limit = if self.session_had_speech { self.cfg.idle_timeout_frames } else { self.cfg.no_speech_timeout_frames };
        if let Some(limit) = limit {
            if self.quiet_frames >= limit {
                self.timeout_reported = true;
                out.push(if self.session_had_speech { SegEvent::IdleTimeout } else { SegEvent::NoSpeech });
            }
        }
    }

    fn key(&self, end: usize) -> SnapKey {
        SnapKey { utterance: self.utterance, window: self.window_id, end }
    }

    fn emit_snapshot(&mut self, kind: SnapshotKind, end: usize, out: &mut Vec<SegEvent>) {
        let key = self.key(end);
        if kind != SnapshotKind::Partial {
            self.last_snapshot = Some(key);
        }
        let mut audio = Vec::with_capacity(self.context.len() + end);
        audio.extend_from_slice(&self.context);
        audio.extend_from_slice(&self.window[..end]);
        out.push(SegEvent::Snapshot(Snapshot { key, kind, audio: Arc::from(audio), context: self.context.len() }));
    }

    fn commit(&mut self, end: usize, out: &mut Vec<SegEvent>) {
        out.push(SegEvent::Commit { key: self.key(end) });
        // The last phrase of the committed audio becomes the next window's
        // context. Candidates start in silence: the window start (pre-roll
        // or the rest of the previous pause) and pause points.
        let (min, max) = (self.cfg.context_min_samples, self.cfg.context_max_samples);
        let start = std::iter::once(0).chain(self.pauses.iter().copied()).filter(|&p| p < end && end - p >= min).max();
        self.context.clear();
        if let Some(p) = start.filter(|&p| end - p <= max) {
            self.context.extend_from_slice(&self.window[p..end]);
        }
        self.pauses.clear();
        self.window.drain(..end);
        self.window_id += 1;
        // A forced cut happens mid-speech; a pause cut happens in silence.
        self.window_has_speech = self.speaking;
        self.speech_run = 0;
        self.last_speech_end = 0;
        self.last_snapshot = None;
        self.frames_since_partial = 0;
    }

    /// Where a forced window cut should land: just before the quietest frame
    /// of the last [`SEAM_LOOKBACK_MS`], so the next window starts in the
    /// most likely word gap instead of mid-word (the recogniser can return
    /// nothing for a clip that begins mid-word; see the context docs on
    /// [`SegmenterConfig`]).
    fn quietest_cut(&self) -> usize {
        let frames_back = SEAM_LOOKBACK_MS.div_ceil(FRAME_MS) as usize;
        let first = self.window.len().saturating_sub(frames_back * FRAME_SAMPLES);
        let mut best: Option<(f64, usize)> = None;
        let mut start = first;
        while start + FRAME_SAMPLES <= self.window.len() {
            let frame = &self.window[start..start + FRAME_SAMPLES];
            let energy: f64 = frame.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
            if best.is_none_or(|(e, _)| energy < e) {
                best = Some((energy, start));
            }
            start += FRAME_SAMPLES;
        }
        best.map_or(self.window.len(), |(_, start)| start)
    }

    fn end_utterance(&mut self, reason: EndReason, out: &mut Vec<SegEvent>) {
        let last = if self.window_has_speech {
            let tail = self.cfg.pause_frames as usize * FRAME_SAMPLES;
            let end = (self.last_speech_end + tail).min(self.window.len());
            match self.last_snapshot {
                Some(k) if k.window == self.window_id && k.end == end => Some(k),
                Some(k) if k.window == self.window_id && k.end >= self.cfg.split_min_samples && end > k.end => {
                    // Speech followed the last pause: keep that transcript
                    // (already running) and transcribe only what came after.
                    self.commit(k.end, out);
                    let end = end - k.end;
                    self.emit_snapshot(SnapshotKind::Final, end, out);
                    Some(self.key(end))
                }
                _ => {
                    self.emit_snapshot(SnapshotKind::Final, end, out);
                    Some(self.key(end))
                }
            }
        } else {
            None
        };
        out.push(SegEvent::UtteranceEnd { utterance: self.utterance, last, reason });
        // Prepare the next utterance (continuous mode); keep the trailing
        // silence as pre-roll.
        let consumed = last.map(|k| k.end).unwrap_or(0).min(self.window.len());
        self.window.drain(..consumed);
        self.context.clear();
        self.pauses.clear();
        self.utterance += 1;
        self.window_id = 0;
        self.window_has_speech = false;
        self.utterance_has_speech = false;
        self.speaking = false;
        self.speech_run = 0;
        self.silence_frames = 0;
        self.last_speech_end = 0;
        self.last_snapshot = None;
        self.frames_since_partial = 0;
        self.quiet_frames = 0;
        self.timeout_reported = false;
        self.trim_leading_silence();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const F: usize = FRAME_SAMPLES;

    fn cfg_toggle() -> SegmenterConfig {
        // pause 192 ms (6 frames), endpoint 800 ms (25 frames), pre-roll 320 ms.
        SegmenterConfig::from_ms(0.5, 192, Some(800), 320, 12, None, Some(4000), Some(3200))
    }

    fn cfg_ptt() -> SegmenterConfig {
        SegmenterConfig::from_ms(0.5, 192, None, 320, 12, None, None, None)
    }

    /// Drive the segmenter with a probability per frame; each frame's
    /// samples carry the global frame index so slices can be checked.
    struct Driver {
        seg: Segmenter,
        frame_no: usize,
        events: Vec<SegEvent>,
    }

    impl Driver {
        fn new(cfg: SegmenterConfig) -> Driver {
            Driver { seg: Segmenter::new(cfg), frame_no: 0, events: Vec::new() }
        }
        fn feed(&mut self, frames: usize, prob: f32) {
            for _ in 0..frames {
                let frame = vec![self.frame_no as f32; F];
                self.seg.push_frame(&frame, prob, &mut self.events);
                self.frame_no += 1;
            }
        }
        /// Like [`Driver::feed`], but with every sample equal to `value`; used
        /// to place a quiet frame among loud ones.
        fn feed_values(&mut self, frames: usize, prob: f32, value: f32) {
            for _ in 0..frames {
                let frame = vec![value; F];
                self.seg.push_frame(&frame, prob, &mut self.events);
                self.frame_no += 1;
            }
        }
        fn snapshots(&self, kind: SnapshotKind) -> Vec<&Snapshot> {
            self.events
                .iter()
                .filter_map(|e| match e {
                    SegEvent::Snapshot(s) if s.kind == kind => Some(s),
                    _ => None,
                })
                .collect()
        }
        fn ends(&self) -> Vec<(u32, Option<SnapKey>, EndReason)> {
            self.events
                .iter()
                .filter_map(|e| match e {
                    SegEvent::UtteranceEnd { utterance, last, reason } => Some((*utterance, *last, *reason)),
                    _ => None,
                })
                .collect()
        }
        fn commits(&self) -> Vec<SnapKey> {
            self.events
                .iter()
                .filter_map(|e| match e {
                    SegEvent::Commit { key } => Some(*key),
                    _ => None,
                })
                .collect()
        }
    }

    #[test]
    fn endpoint_reuses_the_pause_snapshot() {
        let mut d = Driver::new(cfg_toggle());
        d.feed(20, 0.0); // 640 ms silence
        d.feed(30, 0.9); // ~1 s speech
        d.feed(30, 0.0); // silence past the 800 ms endpoint
        assert!(matches!(d.events[0], SegEvent::SpeechStart { utterance: 0 }));
        let pauses = d.snapshots(SnapshotKind::Pause);
        assert_eq!(pauses.len(), 1);
        assert!(d.snapshots(SnapshotKind::Final).is_empty(), "final snapshot should be reused");
        let ends = d.ends();
        assert_eq!(ends.len(), 1);
        assert_eq!(ends[0].1, Some(pauses[0].key));
        assert_eq!(ends[0].2, EndReason::Endpoint);
        // The snapshot starts with the pre-roll (10 frames) before onset and
        // ends `pause` frames after the last speech frame.
        let audio = &pauses[0].audio;
        assert_eq!(audio.len(), (10 + 30 + 6) * F, "pre-roll + speech + pause");
        assert_eq!(audio[0], 10.0, "window must start exactly at the pre-roll");
        assert_eq!(*audio.last().unwrap(), 55.0);
    }

    #[test]
    fn push_to_talk_release_mid_speech_needs_a_final_snapshot() {
        let mut d = Driver::new(cfg_ptt());
        d.feed(5, 0.0);
        d.feed(40, 0.9);
        assert!(d.seg.needs_tail());
        d.seg.finish(&mut d.events);
        let finals = d.snapshots(SnapshotKind::Final);
        assert_eq!(finals.len(), 1);
        assert_eq!(finals[0].audio.len(), 45 * F, "all audio when released while speaking");
        assert_eq!(d.ends()[0].1, Some(finals[0].key));
        assert_eq!(d.ends()[0].2, EndReason::Finish);
    }

    #[test]
    fn push_to_talk_release_after_a_pause_reuses_the_snapshot() {
        let mut d = Driver::new(cfg_ptt());
        d.feed(40, 0.9);
        d.feed(9, 0.0);
        assert!(!d.seg.needs_tail());
        d.seg.finish(&mut d.events);
        assert!(d.snapshots(SnapshotKind::Final).is_empty());
        let pause = d.snapshots(SnapshotKind::Pause)[0].key;
        assert_eq!(d.ends()[0].1, Some(pause));
    }

    #[test]
    fn release_after_more_speech_commits_at_the_last_pause() {
        let mut d = Driver::new(cfg_ptt());
        d.feed(5, 0.0);
        d.feed(62, 0.9); // ~2 s
        d.feed(10, 0.0); // pause: snapshot after 6 frames
        d.feed(31, 0.9); // ~1 s more, released while speaking
        d.seg.finish(&mut d.events);
        let pause = d.snapshots(SnapshotKind::Pause)[0].key;
        assert_eq!(pause.end, 73 * F);
        assert_eq!(d.commits(), vec![pause], "the running pause transcript is kept");
        let finals = d.snapshots(SnapshotKind::Final);
        assert_eq!(finals.len(), 1);
        let fin = finals[0];
        assert_eq!(fin.key.window, 1);
        assert_eq!(d.ends()[0].1, Some(fin.key));
        // Only the audio after the pause is new. The context is the last
        // phrase of window 0, from its start (no earlier pause): 2.3 s.
        assert_eq!(fin.context, 73 * F);
        assert_eq!(fin.key.end, 35 * F);
        assert_eq!(fin.audio.len(), 108 * F);
        assert_eq!(fin.audio[0], 0.0);
        assert_eq!(fin.audio[73 * F], 73.0);
        assert_eq!(*fin.audio.last().unwrap(), 107.0);
    }

    #[test]
    fn context_needs_a_nearby_pause_point() {
        let mut d = Driver::new(cfg_ptt());
        d.feed(100, 0.9); // 3.2 s without a pause (context limit 2.5 s)
        d.feed(10, 0.0);
        d.feed(31, 0.9);
        d.seg.finish(&mut d.events);
        assert_eq!(d.commits().len(), 1);
        let fin = d.snapshots(SnapshotKind::Final)[0];
        assert_eq!(fin.key.window, 1);
        assert_eq!(fin.context, 0, "a context starting mid-speech would be worse than none");
        assert_eq!(fin.audio.len(), fin.key.end);
    }

    #[test]
    fn short_utterances_are_not_split() {
        let mut d = Driver::new(cfg_ptt());
        d.feed(30, 0.9); // < 2 s before the pause
        d.feed(10, 0.0);
        d.feed(20, 0.9);
        d.seg.finish(&mut d.events);
        assert!(d.commits().is_empty());
        let finals = d.snapshots(SnapshotKind::Final);
        assert_eq!(finals.len(), 1);
        assert_eq!((finals[0].key.window, finals[0].context), (0, 0));
        assert_eq!(finals[0].audio.len(), 60 * F);
    }

    #[test]
    fn speech_after_a_short_pause_supersedes_the_first_snapshot() {
        let mut d = Driver::new(cfg_toggle());
        d.feed(30, 0.9);
        d.feed(10, 0.0); // pause > 192 ms, < endpoint
        d.feed(30, 0.9);
        d.feed(30, 0.0);
        let pauses = d.snapshots(SnapshotKind::Pause);
        assert_eq!(pauses.len(), 2);
        assert!(pauses[1].key.end > pauses[0].key.end);
        assert_eq!(pauses[0].key.window, pauses[1].key.window);
        assert_eq!(d.ends()[0].1, Some(pauses[1].key));
    }

    #[test]
    fn hysteresis_keeps_weak_frames_inside_speech() {
        let mut d = Driver::new(cfg_toggle());
        d.feed(10, 0.9);
        d.feed(10, 0.4); // above neg_threshold (0.35): still speech
        d.feed(10, 0.9);
        assert!(d.snapshots(SnapshotKind::Pause).is_empty());
    }

    #[test]
    fn no_speech_times_out_once() {
        let mut d = Driver::new(cfg_toggle());
        d.feed(200, 0.0);
        let n = d.events.iter().filter(|e| matches!(e, SegEvent::NoSpeech)).count();
        assert_eq!(n, 1);
        // Leading silence never accumulates beyond the pre-roll + batch.
        assert!(d.seg.buffered_samples() <= (10 + 17) * F);
    }

    #[test]
    fn finish_without_speech_reports_no_speech() {
        let mut d = Driver::new(cfg_ptt());
        d.feed(10, 0.1);
        d.seg.finish(&mut d.events);
        assert!(matches!(d.events.last(), Some(SegEvent::NoSpeech)));
        assert!(d.ends().is_empty());
    }

    #[test]
    fn long_dictation_commits_at_pauses_and_bounds_memory() {
        let mut d = Driver::new(cfg_ptt());
        // 60 s of speech in 2 s phrases with 320 ms pauses.
        for _ in 0..25 {
            d.feed(62, 0.9);
            d.feed(10, 0.0);
            let max = (d.seg.config().max_window_frames as usize + 1) * F;
            assert!(d.seg.buffered_samples() <= max);
        }
        d.seg.finish(&mut d.events);
        let commits = d.commits();
        assert!(commits.len() >= 3, "expected several commits, got {}", commits.len());
        for (i, c) in commits.iter().enumerate() {
            assert_eq!(c.window, i as u32);
            // Each commit point is exactly a pause snapshot.
            assert!(d.snapshots(SnapshotKind::Pause).iter().any(|s| s.key == *c));
            // A committed window holds at least commit_after (12 s) of audio.
            assert!(c.end >= 12_000 * 16);
        }
        let (_, last, _) = d.ends()[0];
        let last = last.unwrap();
        assert_eq!(last.window, commits.len() as u32);
        // Snapshots after a commit start with the committed window's last
        // phrase: from the previous pause point (72 frames earlier).
        for s in d.snapshots(SnapshotKind::Pause).iter().filter(|s| s.key.window > 0) {
            assert_eq!(s.context, 72 * F);
            assert_eq!(s.audio.len(), 72 * F + s.key.end);
        }
        // Every audio sample after onset is covered exactly once.
        let covered: usize = commits.iter().map(|c| c.end).sum::<usize>() + last.end;
        assert!(covered >= 25 * 72 * F - 2 * 4 * F);
    }

    #[test]
    fn continuous_speech_is_cut_at_the_quietest_recent_frame() {
        let mut d = Driver::new(cfg_ptt());
        let max = d.seg.config().max_window_frames as usize;
        d.feed(max + 50, 0.9);
        let commits = d.commits();
        assert_eq!(commits.len(), 1);
        // Sample values grow with the frame index, so the quietest frame in
        // the lookback is its earliest one; the cut moves back from the
        // window limit to just before it.
        let lookback = SEAM_LOOKBACK_MS.div_ceil(FRAME_MS) as usize;
        assert_eq!(commits[0].end, (max - lookback) * F);
        assert_eq!(d.snapshots(SnapshotKind::Final).len(), 1);
        d.seg.finish(&mut d.events);
        let (_, last, _) = d.ends()[0];
        assert_eq!(last.unwrap().window, 1);
    }

    #[test]
    fn forced_cut_lands_just_before_the_quietest_frame() {
        let mut d = Driver::new(cfg_ptt());
        let max = d.seg.config().max_window_frames as usize;
        // One very quiet frame sits 28 frames before the window limit, well
        // inside the lookback.
        let quiet = max - 28;
        d.feed(quiet, 0.9);
        d.feed_values(1, 0.9, 0.001);
        d.feed(max - quiet - 1, 0.9);
        let commits = d.commits();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].end, quiet * F);
        let finals = d.snapshots(SnapshotKind::Final);
        assert_eq!(finals.len(), 1);
        assert_eq!(finals[0].key.end, quiet * F);
        assert_eq!(d.seg.buffered_samples(), (max - quiet) * F, "audio after the seam stays for the next window");
    }

    #[test]
    fn continuous_mode_starts_new_utterances_and_idles_out() {
        let mut d = Driver::new(cfg_toggle());
        d.feed(20, 0.9);
        d.feed(30, 0.0);
        d.feed(20, 0.9);
        d.feed(30, 0.0);
        let ends = d.ends();
        assert_eq!(ends.len(), 2);
        assert_eq!(ends[0].0, 0);
        assert_eq!(ends[1].0, 1);
        assert_eq!(ends[1].1.unwrap().utterance, 1);
        d.feed(120, 0.0);
        assert!(d.events.iter().any(|e| matches!(e, SegEvent::IdleTimeout)));
    }

    #[test]
    fn partial_snapshots_follow_the_interval() {
        let cfg = SegmenterConfig::from_ms(0.5, 192, None, 320, 12, Some(320), None, None);
        let mut d = Driver::new(cfg);
        d.feed(2, 0.9);
        d.feed(50, 0.9);
        let partials = d.snapshots(SnapshotKind::Partial);
        assert_eq!(partials.len(), 5);
        assert!(partials.windows(2).all(|w| w[1].key.end > w[0].key.end));
    }
}
