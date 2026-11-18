//! ~/.indirc: what indi keeps between runs, as `key = value` lines.
//! indi rewrites the file when a setting changes from inside the app.

use std::path::PathBuf;

#[derive(Default)]
pub struct Config {
    /// Red on black, to keep the eyes used to the dark.
    pub night: bool,
    /// Latitude and longitude (east positive), for when the mount
    /// does not know where it stands.
    pub site: Option<(f64, f64)>,
    /// The drivers indi starts a server with, when none is running.
    pub drivers: Vec<String>,
    /// Focuser positions in slots 1 to 9, each with a name.
    pub presets: [Option<(f64, String)>; 9],
    path: Option<PathBuf>,
}

fn yes(v: &str) -> bool {
    matches!(v, "yes" | "on" | "true" | "1")
}

impl Config {
    pub fn load() -> Config {
        let path = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".indirc"));
        let text = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        let mut c = Config::parse(&text);
        c.path = path;
        c
    }

    pub fn parse(text: &str) -> Config {
        let mut c = Config::default();
        let (mut lat, mut lon) = (None, None);
        for line in text.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "night" => c.night = yes(v),
                "lat" => lat = v.parse().ok(),
                "lon" => lon = v.parse().ok(),
                "drivers" => c.drivers = v.split_whitespace().map(String::from).collect(),
                _ => {
                    let slot = k.strip_prefix("focus").and_then(|n| n.parse::<usize>().ok()).filter(|n| (1..=9).contains(n));
                    let (pos, name) = v.split_once(' ').unwrap_or((v, ""));
                    if let (Some(n), Ok(pos)) = (slot, pos.parse::<f64>()) {
                        c.presets[n - 1] = Some((pos, name.trim().to_string()));
                    }
                }
            }
        }
        if let (Some(lat), Some(lon)) = (lat, lon) {
            c.site = Some((lat, lon));
        }
        c
    }

    pub fn text(&self) -> String {
        let mut out = String::from("# indi settings. indi rewrites this file when you change one in the app.\n");
        out.push_str(&format!("night = {}\n", if self.night { "yes" } else { "no" }));
        if let Some((lat, lon)) = self.site {
            out.push_str(&format!("lat = {lat}\nlon = {lon}\n"));
        }
        if !self.drivers.is_empty() {
            out.push_str(&format!("drivers = {}\n", self.drivers.join(" ")));
        }
        for (i, p) in self.presets.iter().enumerate() {
            if let Some((pos, name)) = p {
                out.push_str(&format!("focus{} = {pos} {name}\n", i + 1));
            }
        }
        out
    }

    pub fn save(&self) {
        if let Some(p) = &self.path {
            let _ = std::fs::write(p, self.text());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_reads_back_as_written() {
        let c = Config::parse("night = yes\nlat = 51.48\nlon = 0.0\ndrivers = indi_a indi_b\nfocus2 = 31250 Visual use\nfocus12 = 5 x\n");
        assert!(c.night);
        assert_eq!(c.site, Some((51.48, 0.0)));
        assert_eq!(c.drivers, ["indi_a", "indi_b"]);
        assert_eq!(c.presets[1], Some((31250.0, "Visual use".to_string())));
        assert!(c.presets[0].is_none());
        let again = Config::parse(&c.text());
        assert_eq!(again.text(), c.text());
    }
}
