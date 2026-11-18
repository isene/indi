//! Camera pictures: the FITS file a camera driver sends, read, measured
//! and stretched so the faint sky shows on a screen.
//!
//! A frame stays as the file came (to save it as it is); pixels are read
//! from it when needed. Facts are worked out once, when it arrives: the
//! lowest, middle and highest value, the stars and how sharp they are.

/// The middle of the background lands this bright after the stretch, on
/// a scale of 0 to 1 (as PixInsight's screen stretch does it).
const BACKGROUND: f64 = 0.25;
/// Values this many spreads below the middle are shown black.
const SHADOWS: f64 = 2.8;
/// A star is a peak this many spreads above the background.
const STAR_SIGMA: f64 = 5.0;
/// How far from its peak a star's light is gathered, in pixels.
const STAR_R: usize = 10;

/// How one colour is stretched: values are first put on a 0 to 1 scale
/// by `lo` and `span`, then cut at `black` and bent by `mid`.
#[derive(Debug, Clone, Copy)]
struct Stretch {
    black: f64,
    mid: f64,
    median: f64,
}

#[derive(Debug)]
pub struct Frame {
    pub w: usize,
    pub h: usize,
    /// The bytes as the camera sent them, for saving.
    pub bytes: Vec<u8>,
    bitpix: i32,
    bzero: f64,
    bscale: f64,
    data: usize,
    /// 3 for a picture in three colour planes, else 1.
    planes: usize,
    /// For a colour sensor read raw: where red, green and blue sit in
    /// each square of 2 × 2 pixels, as "RGGB", "BGGR", "GRBG" or "GBRG".
    pub bayer: Option<String>,
    /// When the exposure started, from the file: "2026-11-18T21:30:05".
    pub date: Option<String>,
    pub min: f64,
    pub max: f64,
    pub median: f64,
    lo: f64,
    span: f64,
    stretch: Vec<Stretch>,
    pub stars: usize,
    /// Half-flux radius in pixels: half a star's light falls inside it.
    /// Smaller is sharper, so focusing is making this number small.
    pub hfr: Option<f64>,
}

/// Decode base64, skipping line breaks and anything else not in it.
pub fn base64(text: &[u8]) -> Vec<u8> {
    // Each byte's six bits, or 64 for a byte that is not base64.
    const SIX: [u8; 256] = {
        let mut t = [64u8; 256];
        let mut i = 0;
        while i < 64 {
            t[b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"[i] as usize] = i as u8;
            i += 1;
        }
        t
    };
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    // `acc` gathers letters; `got` counts them in the group of four.
    let (mut acc, mut got) = (0u32, 0);
    let mut i = 0;
    while i < text.len() {
        // Four good letters at the start of a group give three bytes.
        if got == 0 && i + 4 <= text.len() {
            let q = [SIX[text[i] as usize], SIX[text[i + 1] as usize], SIX[text[i + 2] as usize], SIX[text[i + 3] as usize]];
            if q[0] | q[1] | q[2] | q[3] < 64 {
                let n = (q[0] as u32) << 18 | (q[1] as u32) << 12 | (q[2] as u32) << 6 | q[3] as u32;
                out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8]);
                i += 4;
                continue;
            }
        }
        let v = SIX[text[i] as usize];
        i += 1;
        if v == 64 {
            continue;
        }
        acc = (acc << 6) | v as u32;
        got += 1;
        // After the 2nd, 3rd and 4th letter of a group, a byte is whole.
        if got > 1 {
            out.push((acc >> (2 * (4 - got))) as u8);
        }
        if got == 4 {
            got = 0;
        }
    }
    out
}

/// The midtones curve: bends 0 to 1 so that `m` lands at one half.
fn mtf(m: f64, x: f64) -> f64 {
    if x <= 0.0 {
        0.0
    } else if x >= 1.0 {
        1.0
    } else {
        (m - 1.0) * x / ((2.0 * m - 1.0) * x - m)
    }
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mid = v.len() / 2;
    *v.select_nth_unstable_by(mid, f64::total_cmp).1
}

impl Frame {
    /// A camera's BLOB: base64 text, and its format (".fits").
    pub fn from_blob(text: &str, format: &str) -> Result<Frame, String> {
        if format != ".fits" {
            return Err(format!("the camera sent a {format} picture; indi shows .fits (turn off compression)"));
        }
        Frame::parse(base64(text.as_bytes()))
    }

    /// Read a FITS file: 80-character header cards in blocks of 2880
    /// bytes, then the pixels, big-endian.
    pub fn parse(bytes: Vec<u8>) -> Result<Frame, String> {
        let mut key = std::collections::HashMap::new();
        let mut at = 0;
        loop {
            let card = bytes.get(at..at + 80).ok_or("the FITS header has no end")?;
            at += 80;
            // Header cards are ASCII; anything else is read as '?'.
            let card: String = card.iter().map(|&b| if b.is_ascii() { b as char } else { '?' }).collect();
            let name = card[..8].trim().to_string();
            if name == "END" {
                break;
            }
            if card.get(8..10) == Some("= ") {
                let v = card[10..].trim();
                let v = match v.strip_prefix('\'') {
                    Some(s) => s.split('\'').next().unwrap_or("").trim().to_string(),
                    None => v.split('/').next().unwrap_or("").trim().to_string(),
                };
                key.insert(name, v);
            }
        }
        let data = at.div_ceil(2880) * 2880;
        let int = |k: &str| key.get(k).and_then(|v| v.parse::<i64>().ok());
        let num = |k: &str, d: f64| key.get(k).and_then(|v| v.parse::<f64>().ok()).unwrap_or(d);
        let bitpix = int("BITPIX").ok_or("the FITS header has no BITPIX")? as i32;
        if !matches!(bitpix, 8 | 16 | 32 | -32 | -64) {
            return Err(format!("FITS pixels of BITPIX {bitpix} are not read"));
        }
        let (w, h) = (int("NAXIS1").unwrap_or(0) as usize, int("NAXIS2").unwrap_or(0) as usize);
        let planes = match (int("NAXIS"), int("NAXIS3")) {
            (Some(2), _) | (Some(3), Some(1)) => 1,
            (Some(3), Some(3)) => 3,
            _ => return Err("the FITS file is no picture of one or three colours".into()),
        };
        if w == 0 || h == 0 {
            return Err("the FITS picture is empty".into());
        }
        let need = w * h * planes * (bitpix.unsigned_abs() as usize / 8);
        if bytes.len() < data + need {
            return Err("the FITS file is cut short".into());
        }
        let bayer = key.get("BAYERPAT").filter(|p| planes == 1 && ["RGGB", "BGGR", "GRBG", "GBRG"].contains(&p.as_str())).cloned();
        let date = key.get("DATE-OBS").map(|d| d.chars().take(19).collect());
        let mut f = Frame {
            w,
            h,
            bitpix,
            bzero: num("BZERO", 0.0),
            bscale: num("BSCALE", 1.0),
            data,
            planes,
            bayer,
            date,
            bytes,
            min: 0.0,
            max: 0.0,
            median: 0.0,
            lo: 0.0,
            span: 1.0,
            stretch: Vec::new(),
            stars: 0,
            hfr: None,
        };
        f.measure();
        Ok(f)
    }

    /// Pixel `i` of the file's pixels, as a value.
    fn raw(&self, i: usize) -> f64 {
        let b = &self.bytes;
        let at = |n: usize| self.data + n * i;
        let four = |o: usize| [b[o], b[o + 1], b[o + 2], b[o + 3]];
        let v = match self.bitpix {
            16 => i16::from_be_bytes([b[at(2)], b[at(2) + 1]]) as f64,
            8 => b[at(1)] as f64,
            32 => i32::from_be_bytes(four(at(4))) as f64,
            -32 => f32::from_be_bytes(four(at(4))) as f64,
            _ => {
                let mut a = [0u8; 8];
                a.copy_from_slice(&b[at(8)..at(8) + 8]);
                f64::from_be_bytes(a)
            }
        };
        let v = self.bzero + self.bscale * v;
        if v.is_finite() { v } else { 0.0 }
    }

    /// Every pixel as the file's whole number, for a 16-bit frame.
    fn raw16(&self) -> Option<impl Iterator<Item = i16> + '_> {
        let n = self.w * self.h * self.planes;
        (self.bitpix == 16).then(|| self.bytes[self.data..self.data + 2 * n].chunks_exact(2).map(|c| i16::from_be_bytes([c[0], c[1]])))
    }

    /// The picture's own grid: a raw colour sensor is read in squares of
    /// 2 × 2 pixels, one colour pixel each.
    fn grid(&self) -> (usize, usize) {
        if self.bayer.is_some() { (self.w / 2, self.h / 2) } else { (self.w, self.h) }
    }

    pub fn colours(&self) -> usize {
        if self.bayer.is_some() { 3 } else { self.planes }
    }

    /// Colour `ch` at grid point `x`, `y`.
    fn value(&self, x: usize, y: usize, ch: usize) -> f64 {
        let w = self.w;
        match &self.bayer {
            None => self.raw(ch * w * self.h + y * w + x),
            Some(pat) => {
                let at = (2 * y) * w + 2 * x;
                let want = b"RGB"[ch];
                let (mut sum, mut n) = (0.0, 0.0);
                for (k, c) in pat.bytes().enumerate() {
                    if c == want {
                        sum += self.raw(at + (k / 2) * w + k % 2);
                        n += 1.0;
                    }
                }
                sum / n
            }
        }
    }

    /// Colour `ch` added up over a box of grid points. A one-colour
    /// 16-bit frame is added as whole numbers, row by row.
    fn sum(&self, xs: std::ops::Range<usize>, ys: std::ops::Range<usize>, ch: usize) -> f64 {
        if self.bitpix == 16 && self.colours() == 1 {
            let mut total = 0i64;
            for y in ys.clone() {
                let row = self.data + 2 * (y * self.w);
                let px = &self.bytes[row + 2 * xs.start..row + 2 * xs.end];
                total += px.chunks_exact(2).map(|c| i16::from_be_bytes([c[0], c[1]]) as i64).sum::<i64>();
            }
            let n = (xs.len() * ys.len()) as f64;
            return self.bscale * total as f64 + self.bzero * n;
        }
        ys.flat_map(|y| xs.clone().map(move |x| (x, y))).map(|(x, y)| self.value(x, y, ch)).sum()
    }

    /// The brightness at a grid point: all colours together.
    fn light(&self, x: usize, y: usize) -> f64 {
        if self.bayer.is_none() && self.planes == 1 {
            return self.raw(y * self.w + x);
        }
        let n = self.colours();
        (0..n).map(|c| self.value(x, y, c)).sum::<f64>() / n as f64
    }

    fn measure(&mut self) {
        let n = self.w * self.h * self.planes;
        let (lo, hi) = match self.raw16() {
            Some(px) => {
                let (a, b) = px.fold((i16::MAX, i16::MIN), |(a, b), v| (a.min(v), b.max(v)));
                let (a, b) = (self.bzero + self.bscale * a as f64, self.bzero + self.bscale * b as f64);
                (a.min(b), a.max(b))
            }
            None => (0..n).map(|i| self.raw(i)).fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), v| (a.min(v), b.max(v))),
        };
        self.min = lo;
        self.max = hi;
        self.lo = lo;
        self.span = if hi > lo { hi - lo } else { 1.0 };
        // A quarter of a million points tell the middle and the spread
        // well enough, and keep a big sensor quick.
        let (gw, gh) = self.grid();
        let step = (gw * gh / 250_000).max(1);
        let mut stretch = Vec::new();
        for ch in 0..self.colours() {
            let mut s: Vec<f64> = (0..gw * gh).step_by(step).map(|k| (self.value(k % gw, k / gw, ch) - self.lo) / self.span).collect();
            let m = median(&mut s);
            let mut dev: Vec<f64> = s.iter().map(|v| (v - m).abs()).collect();
            let spread = 1.4826 * median(&mut dev);
            stretch.push(if spread > 0.0 {
                let black = (m - SHADOWS * spread).max(0.0);
                let bg = (m - black) / (1.0 - black);
                Stretch { black, mid: mtf(BACKGROUND, bg).clamp(1e-6, 1.0 - 1e-6), median: m }
            } else {
                // A flat field: shown as it is, from black to white.
                Stretch { black: 0.0, mid: 0.5, median: m }
            });
        }
        // Mono shows its one colour; colour shows green, the middle one.
        self.median = self.lo + stretch[stretch.len() / 2].median * self.span;
        self.stretch = stretch;
        self.find_stars();
    }

    /// Find the stars and how sharp they are: peaks well above the
    /// background, each measured by the radius that holds half its light.
    fn find_stars(&mut self) {
        let (gw, gh) = self.grid();
        let r = STAR_R;
        if gw <= 2 * r + 2 || gh <= 2 * r + 2 {
            return;
        }
        let step = (gw * gh / 250_000).max(1);
        let mut s: Vec<f64> = (0..gw * gh).step_by(step).map(|k| self.light(k % gw, k / gw)).collect();
        let bg = median(&mut s);
        // The noise, from the steps between neighbours: a nebula or a
        // sky that brightens to one side changes slowly and leaves it be.
        let mut steps: Vec<f64> = (0..gw * gh)
            .step_by(step)
            .filter(|k| k % gw + 1 < gw)
            .map(|k| (self.light(k % gw + 1, k / gw) - self.light(k % gw, k / gw)).abs())
            .collect();
        let sigma = 1.4826 * median(&mut steps) / std::f64::consts::SQRT_2;
        if sigma <= 0.0 {
            return;
        }
        let at = |x: usize, y: usize| self.light(x, y);
        let thr = bg + STAR_SIGMA * sigma;
        // The points above the bar. A one-colour 16-bit frame, the usual
        // kind, is sifted as whole numbers: five times as quick.
        let bright: Vec<(usize, usize)> = match self.raw16().filter(|_| self.colours() == 1 && self.bscale > 0.0) {
            Some(px) => {
                let t = ((thr - self.bzero) / self.bscale).ceil().clamp(i16::MIN as f64, i16::MAX as f64) as i16;
                px.enumerate().filter(|&(_, v)| v >= t).map(|(i, _)| (i % gw, i / gw)).collect()
            }
            None => (0..gw * gh).map(|i| (i % gw, i / gw)).filter(|&(x, y)| at(x, y) >= thr).collect(),
        };
        let mut peaks = Vec::new();
        for (x, y) in bright {
            if x < r || y < r || x >= gw - r || y >= gh - r {
                continue;
            }
            let v = at(x, y);
            // The highest point near it; ties go to the first one.
            let top = v > at(x - 1, y) && v > at(x, y - 1) && v > at(x - 1, y - 1) && v > at(x + 1, y - 1)
                && v >= at(x + 1, y) && v >= at(x, y + 1) && v >= at(x - 1, y + 1) && v >= at(x + 1, y + 1);
            if top {
                peaks.push((v, x, y));
            }
        }
        peaks.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut kept: Vec<(usize, usize)> = Vec::new();
        let mut hfrs = Vec::new();
        for &(v, px, py) in &peaks {
            if hfrs.len() >= 100 {
                break;
            }
            if kept.iter().any(|&(x, y)| x.abs_diff(px) <= r && y.abs_diff(py) <= r) {
                continue;
            }
            // The sky right around it, from the edge of the box: a star on
            // a nebula is measured against the nebula, not the dark sky,
            // and a bump in the nebula is no star.
            let mut edge = Vec::with_capacity(8 * r);
            for i in 0..=2 * r {
                edge.extend([at(px - r + i, py - r), at(px - r + i, py + r)]);
            }
            for i in 1..2 * r {
                edge.extend([at(px - r, py - r + i), at(px + r, py - r + i)]);
            }
            let sky = median(&mut edge);
            if v - sky < STAR_SIGMA * sigma {
                continue;
            }
            kept.push((px, py));
            // Only light well above the noise and the glow around it
            // counts: a twentieth of the peak or three times the noise. Or
            // the noise all over the box would make every star look soft.
            let floor = (3.0 * sigma).max(0.05 * (v - sky));
            let light = |x: usize, y: usize| Some(at(x, y) - sky).filter(|f| *f > floor).unwrap_or(0.0);
            // The centre of the light first, then the half-flux radius.
            let (mut sum, mut sx, mut sy) = (0.0, 0.0, 0.0);
            for y in py - r..=py + r {
                for x in px - r..=px + r {
                    let f = light(x, y);
                    sum += f;
                    sx += f * x as f64;
                    sy += f * y as f64;
                }
            }
            if sum <= 0.0 {
                continue;
            }
            let (cx, cy) = (sx / sum, sy / sum);
            let mut fr = 0.0;
            for y in py - r..=py + r {
                for x in px - r..=px + r {
                    fr += light(x, y) * ((x as f64 - cx).powi(2) + (y as f64 - cy).powi(2)).sqrt();
                }
            }
            let hfr = fr / sum;
            // A hot pixel has all its light in one place: no star. A sharp
            // star on a colour camera, read in squares of four, is not far
            // from that, so the bar is low.
            if hfr > 0.3 {
                hfrs.push(hfr);
            }
        }
        self.stars = hfrs.len();
        if !hfrs.is_empty() {
            // On a raw colour sensor the grid is 2 pixels a step.
            let scale = if self.bayer.is_some() { 2.0 } else { 1.0 };
            self.hfr = Some(median(&mut hfrs) * scale);
        }
    }

    /// One colour at a grid point, stretched to 0 to 255.
    fn shade(&self, v: f64, ch: usize) -> u8 {
        let s = self.stretch[ch.min(self.stretch.len() - 1)];
        let x = (v - self.lo) / self.span;
        let x = if x <= s.black { 0.0 } else { (x - s.black) / (1.0 - s.black) };
        (mtf(s.mid, x) * 255.0).round() as u8
    }

    /// How many cells a picture needs in a box of `cols` × `rows` cells
    /// of `cell` pixels, keeping its shape: the whole frame, or, zoomed,
    /// its middle pixel for pixel.
    pub fn fit(&self, cols: u16, rows: u16, cell: (f64, f64), zoom: bool) -> (u16, u16) {
        let (gw, gh) = self.grid();
        let (bw, bh) = (cols as f64 * cell.0, rows as f64 * cell.1);
        let s = if zoom { 1.0 } else { (bw / gw as f64).min(bh / gh as f64) };
        let c = ((gw as f64 * s).min(bw) / cell.0).ceil().max(1.0) as u16;
        let r = ((gh as f64 * s).min(bh) / cell.1).ceil().max(1.0) as u16;
        (c.min(cols), r.min(rows))
    }

    /// Paint the frame on `canvas`: the whole of it, shape kept, or,
    /// zoomed, its middle one pixel to one pixel, for focusing. Night
    /// paints it red.
    pub fn paint(&self, canvas: &mut glow::Canvas, zoom: bool, night: bool) {
        let (gw, gh) = self.grid();
        let (cw, ch) = (canvas.w, canvas.h);
        let s = if zoom { 1.0 } else { (cw as f64 / gw as f64).min(ch as f64 / gh as f64) };
        // The part of the frame shown: all of it, or the middle.
        let (vw, vh) = (((cw as f64 / s) as usize).min(gw), ((ch as f64 / s) as usize).min(gh));
        let (x0, y0) = ((gw - vw) / 2, (gh - vh) / 2);
        let (ow, oh) = (((vw as f64 * s) as usize).min(cw), ((vh as f64 * s) as usize).min(ch));
        let colours = self.colours();
        for dy in 0..oh {
            let ya = y0 + (dy as f64 / s) as usize;
            let yb = (y0 + ((dy + 1) as f64 / s) as usize).clamp(ya + 1, y0 + vh);
            for dx in 0..ow {
                let xa = x0 + (dx as f64 / s) as usize;
                let xb = (x0 + ((dx + 1) as f64 / s) as usize).clamp(xa + 1, x0 + vw);
                // Shrinking: the mean of the pixels under this one.
                let n = ((yb - ya) * (xb - xa)) as f64;
                let mut rgb = [0u8; 3];
                for (c, out) in rgb.iter_mut().enumerate().take(colours) {
                    *out = self.shade(self.sum(xa..xb, ya..yb, c) / n, c);
                }
                let px = match (colours, night) {
                    (1, false) => (rgb[0], rgb[0], rgb[0]),
                    (1, true) => (rgb[0], 0, 0),
                    (_, false) => (rgb[0], rgb[1], rgb[2]),
                    (_, true) => (((rgb[0] as u16 + rgb[1] as u16 + rgb[2] as u16) / 3) as u8, 0, 0),
                };
                canvas.put(dx, dy, px);
            }
        }
    }

    /// The facts line: size, depth, lowest, middle and highest value.
    pub fn facts(&self) -> String {
        let depth = if self.bitpix < 0 { "float".to_string() } else { format!("{} bit", self.bitpix) };
        let colour = match (&self.bayer, self.planes) {
            (Some(p), _) => format!(" · colour {p}"),
            (None, 3) => " · colour".to_string(),
            _ => String::new(),
        };
        let n = |v: f64| if v.fract() == 0.0 { format!("{v:.0}") } else { format!("{v:.3}") };
        format!("{}×{} · {depth}{colour} · min {} · median {} · max {}", self.w, self.h, n(self.min), n(self.median), n(self.max))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 16-bit FITS file of `w` × `h` with a background of 1000 plus a
    /// little noise, and a round star at (`sx`, `sy`).
    fn fits(w: usize, h: usize, star: Option<(f64, f64, f64)>, bayer: Option<&str>) -> Vec<u8> {
        fits_glow(w, h, star, bayer, 0.0)
    }

    /// The same, on a broad glow `glow` bright at the middle, as a star
    /// in a nebula.
    fn fits_glow(w: usize, h: usize, star: Option<(f64, f64, f64)>, bayer: Option<&str>, glow: f64) -> Vec<u8> {
        let mut head = String::new();
        let mut card = |s: String| head.push_str(&format!("{s:<80}"));
        card("SIMPLE  =                    T".into());
        card("BITPIX  =                   16".into());
        card("NAXIS   =                    2".into());
        card(format!("NAXIS1  = {w:>20}"));
        card(format!("NAXIS2  = {h:>20}"));
        card("BZERO   =                32768".into());
        card("DATE-OBS= '2026-11-18T21:30:05.123' / start".into());
        if let Some(p) = bayer {
            card(format!("BAYERPAT= '{p}'"));
        }
        card("END".into());
        let mut b = head.into_bytes();
        b.resize(b.len().div_ceil(2880) * 2880, b' ');
        let mut seed = 7u32;
        for y in 0..h {
            for x in 0..w {
                seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
                let mut v = 1000.0 + ((seed >> 16) % 20) as f64;
                let (mx, my) = (x as f64 - w as f64 / 2.0, y as f64 - h as f64 / 2.0);
                v += glow * (-(mx * mx + my * my) / 800.0).exp();
                if let Some((sx, sy, sd)) = star {
                    let d2 = (x as f64 - sx).powi(2) + (y as f64 - sy).powi(2);
                    v += 30000.0 * (-d2 / (2.0 * sd * sd)).exp();
                }
                b.extend_from_slice(&((v as i32 - 32768) as i16).to_be_bytes());
            }
        }
        b.resize(b.len().div_ceil(2880) * 2880, 0);
        b
    }

    #[test]
    fn base64_decodes_across_line_breaks() {
        assert_eq!(base64(b"SGVs\nbG8s IHN0YXJz"), b"Hello, stars");
        assert_eq!(base64(b"YQ=="), b"a");
        // Every byte, encoded, with a line break every 61 letters.
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let data: Vec<u8> = (0..=255u8).chain((0..=255u8).rev()).collect();
        let mut text = Vec::new();
        for c in data.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            for k in 0..4 {
                text.push(if k <= c.len() { A[(n >> (18 - 6 * k) & 63) as usize] } else { b'=' });
            }
        }
        let wrapped: Vec<u8> = text.chunks(61).flat_map(|l| l.iter().chain(b"\n")).copied().collect();
        assert_eq!(base64(&wrapped), data);
    }

    #[test]
    fn a_fits_file_reads_with_its_facts() {
        let f = Frame::parse(fits(64, 48, None, None)).unwrap();
        assert_eq!((f.w, f.h, f.colours()), (64, 48, 1));
        assert_eq!(f.date.as_deref(), Some("2026-11-18T21:30:05"));
        assert!(f.min >= 1000.0 && f.max < 1020.0, "{} {}", f.min, f.max);
        assert!((f.median - 1010.0).abs() < 3.0, "{}", f.median);
        assert_eq!(f.stars, 0);
        assert!(Frame::parse(b"SIMPLE  =  T".to_vec()).is_err());
    }

    #[test]
    fn a_sharper_star_has_a_smaller_hfr() {
        let sharp = Frame::parse(fits(80, 60, Some((40.0, 30.0, 1.2)), None)).unwrap();
        let soft = Frame::parse(fits(80, 60, Some((40.0, 30.0, 2.5)), None)).unwrap();
        assert_eq!(sharp.stars, 1);
        assert_eq!(soft.stars, 1);
        let (a, b) = (sharp.hfr.unwrap(), soft.hfr.unwrap());
        assert!(a < b && a > 0.7, "{a} {b}");
    }

    #[test]
    fn a_star_in_a_nebula_is_as_sharp_as_on_dark_sky() {
        let dark = Frame::parse(fits(80, 60, Some((40.0, 30.0, 1.5)), None)).unwrap();
        let nebula = Frame::parse(fits_glow(80, 60, Some((40.0, 30.0, 1.5)), None, 5000.0)).unwrap();
        assert_eq!(nebula.stars, 1, "the nebula itself is no star");
        let (a, b) = (dark.hfr.unwrap(), nebula.hfr.unwrap());
        assert!((a - b).abs() < 0.1 * a, "dark sky {a}, nebula {b}");
    }

    #[test]
    fn a_sharp_star_on_a_colour_camera_counts() {
        let f = Frame::parse(fits(80, 60, Some((40.0, 30.0, 0.9)), Some("RGGB"))).unwrap();
        assert_eq!(f.stars, 1);
        assert!(f.hfr.unwrap() < 2.0, "{:?}", f.hfr);
    }

    #[test]
    fn the_stretch_puts_the_background_low_and_the_star_bright() {
        let f = Frame::parse(fits(80, 60, Some((40.0, 30.0, 1.5)), None)).unwrap();
        let mut c = glow::Canvas::with_cell(8, 6, (10, 10));
        f.paint(&mut c, false, false);
        let px = |x: usize, y: usize| c.rgba[(y * c.w + x) * 4];
        let bg = (0..100).map(|i| px(i % 10, i / 10) as u32).sum::<u32>() / 100;
        assert!((30..110).contains(&bg), "background {bg}");
        assert!(px(40, 30) > 240, "star {}", px(40, 30));
        f.paint(&mut c, false, true);
        assert_eq!(c.rgba[(30 * c.w + 40) * 4 + 1], 0, "night is red only");
    }

    #[test]
    fn a_raw_colour_frame_is_read_in_squares_and_fits_its_box() {
        let f = Frame::parse(fits(80, 60, Some((40.0, 30.0, 2.0)), Some("RGGB"))).unwrap();
        assert_eq!(f.colours(), 3);
        assert!(f.hfr.is_some());
        // A 40 × 30 grid in a box of 20 × 15 cells of 10 × 20 pixels
        // grows five times, to 20 × 8 cells; zoomed it stays 40 × 30.
        assert_eq!(f.fit(20, 15, (10.0, 20.0), false), (20, 8));
        assert_eq!(f.fit(100, 50, (10.0, 20.0), true), (4, 2));
    }
}

