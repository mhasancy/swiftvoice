// Procedural SwiftVoice icon: a white microphone on a coloured disk.
//
// This file is deliberately self-contained (no `use crate::...`) because
// build.rs `include!`s it to generate the executable's .ico at build time,
// while the tray uses it at run time to draw state-coloured icons. Drawing
// in code keeps binary assets out of the repository and lets every DPI get
// a crisp, anti-aliased icon.

/// Visual state of the icon. Each state maps to one disk colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IconTone {
    /// Ready and idle.
    Idle,
    /// Microphone open, recording speech.
    Listening,
    /// Transcribing or inserting.
    Busy,
    /// Model loading or downloading.
    Loading,
    /// Something needs attention (no model, mic blocked, ...).
    Error,
}

impl IconTone {
    /// Disk colour as (r, g, b).
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            IconTone::Idle => (0x3C, 0x4A, 0x5C),
            IconTone::Listening => (0xE5, 0x39, 0x35),
            IconTone::Busy => (0xF2, 0x99, 0x00),
            IconTone::Loading => (0x1E, 0x7F, 0xD8),
            IconTone::Error => (0x8A, 0x8F, 0x96),
        }
    }
}

fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

/// Signed distance from `p` to a vertical capsule (segment a-b, radius r).
fn sd_capsule(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32, r: f32) -> f32 {
    let (pax, pay) = (px - ax, py - ay);
    let (bax, bay) = (bx - ax, by - ay);
    let h = clamp01((pax * bax + pay * bay) / (bax * bax + bay * bay));
    let (dx, dy) = (pax - bax * h, pay - bay * h);
    (dx * dx + dy * dy).sqrt() - r
}

/// Signed distance to a ring segment: the lower half of a circle outline.
fn sd_lower_arc(px: f32, py: f32, cx: f32, cy: f32, radius: f32, half_thickness: f32) -> f32 {
    if py >= cy {
        let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
        (d - radius).abs() - half_thickness
    } else {
        // Above the centre line: distance to the two arc end caps.
        let d1 = ((px - (cx - radius)).powi(2) + (py - cy).powi(2)).sqrt();
        let d2 = ((px - (cx + radius)).powi(2) + (py - cy).powi(2)).sqrt();
        d1.min(d2) - half_thickness
    }
}

/// Render the icon as straight (non-premultiplied) RGBA, row-major, top-down.
pub fn render_rgba(size: u32, tone: IconTone) -> Vec<u8> {
    let n = size.max(1) as usize;
    let s = n as f32;
    let (br, bg, bb) = tone.rgb();
    let mut out = vec![0u8; n * n * 4];
    // One pixel in normalised units, used for anti-aliasing.
    let px_w = 1.0 / s;
    for y in 0..n {
        for x in 0..n {
            let u = (x as f32 + 0.5) / s;
            let v = (y as f32 + 0.5) / s;
            // Background disk.
            let d_disk = ((u - 0.5).powi(2) + (v - 0.5).powi(2)).sqrt() - 0.48;
            let disk = clamp01(0.5 - d_disk / px_w);
            // Microphone glyph: head capsule, holder arc, stem and base.
            let head = sd_capsule(u, v, 0.5, 0.27, 0.5, 0.47, 0.105);
            let arc = sd_lower_arc(u, v, 0.5, 0.46, 0.185, 0.032);
            let stem = sd_capsule(u, v, 0.5, 0.645, 0.5, 0.735, 0.032);
            let base = sd_capsule(u, v, 0.395, 0.755, 0.605, 0.755, 0.032);
            let d_glyph = head.min(arc).min(stem).min(base);
            let glyph = clamp01(0.5 - d_glyph / px_w) * disk;
            let r = br as f32 * (1.0 - glyph) + 255.0 * glyph;
            let g = bg as f32 * (1.0 - glyph) + 255.0 * glyph;
            let b = bb as f32 * (1.0 - glyph) + 255.0 * glyph;
            let i = (y * n + x) * 4;
            out[i] = r.round() as u8;
            out[i + 1] = g.round() as u8;
            out[i + 2] = b.round() as u8;
            out[i + 3] = (disk * 255.0).round() as u8;
        }
    }
    out
}

/// Encode several sizes as a Windows .ico file with 32-bit BMP entries.
pub fn encode_ico(sizes: &[u32], tone: IconTone) -> Vec<u8> {
    let mut images: Vec<Vec<u8>> = Vec::new();
    for &size in sizes {
        let n = size as usize;
        let rgba = render_rgba(size, tone);
        let mask_row = n.div_ceil(32) * 4;
        let mut img = Vec::with_capacity(40 + n * n * 4 + mask_row * n);
        // BITMAPINFOHEADER; height is doubled (XOR image + AND mask).
        img.extend_from_slice(&40u32.to_le_bytes());
        img.extend_from_slice(&(size as i32).to_le_bytes());
        img.extend_from_slice(&((size as i32) * 2).to_le_bytes());
        img.extend_from_slice(&1u16.to_le_bytes());
        img.extend_from_slice(&32u16.to_le_bytes());
        img.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
        img.extend_from_slice(&((n * n * 4) as u32).to_le_bytes());
        img.extend_from_slice(&[0u8; 16]);
        // Pixels: bottom-up BGRA.
        for y in (0..n).rev() {
            for x in 0..n {
                let i = (y * n + x) * 4;
                img.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
            }
        }
        // AND mask: all zero (alpha channel carries transparency).
        img.extend(std::iter::repeat_n(0u8, mask_row * n));
        images.push(img);
    }
    let count = images.len();
    let mut ico = Vec::new();
    ico.extend_from_slice(&0u16.to_le_bytes());
    ico.extend_from_slice(&1u16.to_le_bytes());
    ico.extend_from_slice(&(count as u16).to_le_bytes());
    let mut offset = 6 + 16 * count;
    for (img, &size) in images.iter().zip(sizes) {
        let dim = if size >= 256 { 0u8 } else { size as u8 };
        ico.extend_from_slice(&[dim, dim, 0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes());
        ico.extend_from_slice(&32u16.to_le_bytes());
        ico.extend_from_slice(&(img.len() as u32).to_le_bytes());
        ico.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += img.len();
    }
    for img in images {
        ico.extend_from_slice(&img);
    }
    ico
}
