//! Where things are in the sky: a target by name (a planet, a Messier
//! or Caldwell object, a named star), and the sums a mount needs. A
//! mount takes positions for today's date, so catalogue positions for
//! the year 2000 are moved on by precession first.

use orbit::astro;

/// A place to point at. `ra` in hours and `dec` in degrees, for the
/// current date.
pub struct Target {
    pub name: String,
    pub ra: f64,
    pub dec: f64,
}

pub fn now_jd() -> f64 {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    astro::jd_of_unix(secs as i64)
}

/// A position for the year 2000 (degrees) moved to the date `jd`, by
/// the IAU 1976 precession (Meeus, chapter 21).
pub fn precess(ra: f64, dec: f64, jd: f64) -> (f64, f64) {
    let t = (jd - 2451545.0) / 36525.0;
    let arcsec = |a: f64, b: f64, c: f64| (a * t + b * t * t + c * t * t * t) / 3600.0;
    let zeta = arcsec(2306.2181, 0.30188, 0.017998).to_radians();
    let z = arcsec(2306.2181, 1.09468, 0.018203).to_radians();
    let theta = arcsec(2004.3109, -0.42665, -0.041833).to_radians();
    let (a0, d0) = (ra.to_radians(), dec.to_radians());
    let a = d0.cos() * (a0 + zeta).sin();
    let b = theta.cos() * d0.cos() * (a0 + zeta).cos() - theta.sin() * d0.sin();
    let c = theta.sin() * d0.cos() * (a0 + zeta).cos() + theta.cos() * d0.sin();
    ((a.atan2(b) + z).to_degrees().rem_euclid(360.0), c.clamp(-1.0, 1.0).asin().to_degrees())
}

/// Letters and digits only, in lower case: "M 42" and "m42" match.
fn key(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Find a target by name: "Jupiter", "Moon", "M42", "C14", "NGC 1976",
/// "Orion Nebula" or "Vega". A whole name wins over a part of one.
/// `site` is latitude and longitude, for where the Moon seems to be.
pub fn find(q: &str, jd: f64, site: Option<(f64, f64)>) -> Result<Target, String> {
    let k = key(q);
    if k.is_empty() {
        return Err("type a name, or RA and Dec".into());
    }
    if k == "sun" {
        return Err("indi does not point a telescope at the Sun".into());
    }
    if let Some(n) = astro::NAMES.iter().find(|n| **n == k) {
        let (lat, lon) = site.unwrap_or((0.0, 0.0));
        let p = astro::pos(&astro::sky_jd(jd, lat, lon, 0.0), n);
        let (ra, dec) = if site.is_some() { (p.ra, p.dec) } else { (p.gra, p.gdec) };
        let mut name = n.to_string();
        name[..1].make_ascii_uppercase();
        return Ok(Target { name, ra: ra / 15.0, dec });
    }
    let dsos = starmap::dsos();
    let stars = starmap::stars();
    let whole = |s: &str| !s.is_empty() && key(s) == k;
    let part = |s: &str| k.len() >= 3 && key(s).contains(&k);
    let (name, ra, dec) = dsos.iter().find(|d| whole(d.id) || whole(d.alt) || whole(d.name)).map(|d| (d.label(), d.ra, d.dec))
        .or_else(|| stars.iter().find(|s| whole(s.name)).map(|s| (s.label(), s.ra, s.dec)))
        .or_else(|| dsos.iter().find(|d| part(d.name)).map(|d| (d.label(), d.ra, d.dec)))
        .or_else(|| stars.iter().find(|s| part(s.name)).map(|s| (s.label(), s.ra, s.dec)))
        .ok_or_else(|| format!("no planet, Messier or Caldwell object or named star is called {}", q.trim()))?;
    let (ra, dec) = precess(ra, dec, jd);
    Ok(Target { name, ra: ra / 15.0, dec })
}

/// How far a place (`ra` hours, `dec` degrees) is from the Sun, in degrees.
pub fn sun_distance(ra: f64, dec: f64, jd: f64) -> f64 {
    let sun = astro::pos(&astro::sky_jd(jd, 0.0, 0.0, 0.0), "sun");
    let (a1, d1) = ((ra * 15.0).to_radians(), dec.to_radians());
    let (a2, d2) = (sun.gra.to_radians(), sun.gdec.to_radians());
    let c = d1.sin() * d2.sin() + d1.cos() * d2.cos() * (a1 - a2).cos();
    c.clamp(-1.0, 1.0).acos().to_degrees()
}

/// Height above the horizon and compass direction, in degrees, of a
/// place (`ra` hours, `dec` degrees) seen from `site`.
pub fn alt_az(ra: f64, dec: f64, jd: f64, (lat, lon): (f64, f64)) -> (f64, f64) {
    starmap::altaz(ra * 15.0, dec, starmap::lst_deg(jd, lon), lat)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precession_matches_meeus() {
        // Meeus, example 21.b: theta Persei to 2028 November 13.19.
        let (ra, dec) = precess(41.054063, 49.227750, 2462088.69);
        assert!((ra - 41.547214).abs() < 2e-5, "{ra}");
        assert!((dec - 49.348483).abs() < 2e-5, "{dec}");
    }

    #[test]
    fn names_find_their_objects() {
        let jd = 2461363.5;
        let m42 = find("m 42", jd, None).unwrap();
        assert!(m42.name.starts_with("M42"), "{}", m42.name);
        assert!((m42.ra - 5.59).abs() < 0.05 && (m42.dec + 5.39).abs() < 0.1);
        assert_eq!(find("orion neb", jd, None).unwrap().name, m42.name);
        let vega = find("Vega", jd, None).unwrap();
        assert!((vega.dec - 38.8).abs() < 0.1);
        assert_eq!(find("jupiter", jd, Some((51.48, 0.0))).unwrap().name, "Jupiter");
        assert!(find("sun", jd, None).is_err());
        assert!(find("xyzzy", jd, None).is_err());
    }

    #[test]
    fn the_sun_is_far_from_the_opposite_sky() {
        let jd = 2461363.5;
        let sun = astro::pos(&astro::sky_jd(jd, 0.0, 0.0, 0.0), "sun");
        assert!(sun_distance(sun.gra / 15.0, sun.gdec, jd) < 0.01);
        assert!(sun_distance((sun.gra / 15.0 + 12.0) % 24.0, -sun.gdec, jd) > 179.0);
    }
}
