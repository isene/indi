//! indi: telescope gear through an INDI server, in the terminal.
//!
//! The devices on the left, the selected one on the right: a control
//! block for a focuser or a mount, then every property the driver has,
//! one group at a time. A thread reads the server and wakes the key
//! wait, so the program sleeps until a key or a change comes.

mod camera;
mod config;
mod proto;
mod sky;

use camera::Frame;
use config::Config;
use crust::input::KeyState;
use crust::{style, Crust, Input, Pane};
use proto::{parse_number, show_number, Device, Element, Kind, Model, Parser, Prop};
use sky::Target;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "indi: telescope gear through an INDI server, in the terminal

Usage: indi [host[:port]]      (localhost:7624 when left out)

With no server running, d picks drivers and starts one. Or start it
yourself with the drivers for your gear, for example
  indiserver indi_ieaf_focuser indi_eqmod_telescope
or, to try it without gear,
  indiserver indi_simulator_telescope indi_simulator_focus indi_simulator_ccd

Keys anywhere
  Tab          devices, control, settings
  Up Down      pick a device or a setting
  Left Right   the group of settings
  Enter        connect a device, or change a setting
  + -          nudge a number setting by its step
  /            find a setting
  c            connect or disconnect the device
  W            have the driver save the device's settings
  d            pick drivers and start a server
  m            the server's messages
  n            night colours (red on black)
  r            connect to the server again
  Ctrl+A       ask Claude about the device on screen
  ?            this help
  q            quit (a moving mount is stopped first)

Focuser
  Left Right 10 steps, Shift+Left Right 100, [ ] 1 step,
  g goes to a position, 1 to 9 go to a saved position,
  s saves the position in a slot, Space stops

Mount
  hold an arrow to move north, south, east or west; Space stops
  + - speed, t tracking, p park
  g goes to a name (M42, Vega, Jupiter) or an RA and Dec
  s picks a star on a map of the sky, y syncs on a name

Camera
  Space takes a picture (or stops one), e sets the exposure time,
  + - double or halve it, l takes pictures over and over,
  s saves the picture, a saves every picture, z shows the middle
  pixel for pixel, t cools the sensor, g sets the gain, b the binning

Settings are in ~/.indirc: night, lat and lon (for when the mount
does not know its site), drivers, the focuser's saved positions,
the exposure time, and the folder for pictures (pictures).";

/// Colours for day and for night. Night is all red, which keeps the
/// eyes used to the dark.
#[derive(Clone, Copy)]
struct Pal {
    text: u16,
    bar: u16,
    list: u16,
    accent: u8,
    ok: u8,
    busy: u8,
    alert: u8,
    grey: u8,
}

const DAY: Pal = Pal { text: 252, bar: 236, list: 235, accent: 75, ok: 71, busy: 178, alert: 167, grey: 245 };
const NIGHT: Pal = Pal { text: 124, bar: 233, list: 232, accent: 160, ok: 88, busy: 160, alert: 196, grey: 52 };

/// Closer to the Sun than this, a go-to asks first.
const SUN_WARN: f64 = 15.0;
/// Closer than this, indi will not go at all.
const SUN_REFUSE: f64 = 2.0;

/// The lines of a camera's control block, above its picture.
const CAM_LINES: u16 = 4;

/// What the reading thread hands over, marked with the connection it
/// came from, so a closed one is not mistaken for the new one.
enum Ev {
    El(u32, Element),
    /// A camera's picture, read in the thread: a big one takes a while.
    Frame(u32, String, Result<Frame, String>),
    Gone(u32, String),
}

/// Where a picture is on screen and how it was drawn: when nothing in
/// it changes, the picture is not drawn again.
#[derive(Clone, Copy, PartialEq)]
struct Place {
    seq: u32,
    x: u16,
    y: u16,
    cols: u16,
    rows: u16,
    zoom: bool,
    night: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Focus {
    Devices,
    Control,
    Props,
}

/// What the right side shows.
#[derive(Clone, Copy, PartialEq)]
enum View {
    Device,
    Log,
    Help,
    Drivers,
}

/// A line of the property list: a property's heading, or one of its
/// elements (property index, element index).
enum Line {
    Head(usize),
    Elem(usize, usize),
}

struct App {
    addr: String,
    cfg: Config,
    pal: Pal,
    model: Model,
    net: Option<TcpStream>,
    gen: u32,
    link: String,
    /// The server indi started, if it did: it ends with indi.
    server: Option<Child>,
    dev: usize,
    focus: Focus,
    view: View,
    group: usize,
    row: usize,
    /// The driver list and its ticks, while picking drivers.
    drivers: Vec<(String, bool)>,
    drv_row: usize,
    /// How many server messages the user has seen: a newer one shows
    /// in the bottom bar until the next key.
    seen: usize,
    note: Option<(String, u8)>,
    /// Where the mount was last sent.
    target: Option<String>,
    /// The motion last asked for and when, so a held arrow key's
    /// repeats are not sent again and again.
    held: Option<(String, Instant)>,
    /// Each focuser's temperatures as they came in.
    temps: HashMap<String, Vec<f64>>,
    /// Each camera's last picture, with a number that grows per picture.
    frames: HashMap<String, (u32, Frame)>,
    seq: u32,
    /// The cameras whose pictures the server was asked for.
    blobs: HashSet<String>,
    /// The camera taking pictures over and over.
    looping: Option<String>,
    /// Save every picture as it comes.
    autosave: bool,
    /// Show the middle of the picture pixel for pixel.
    zoom: bool,
    /// Each camera's star sharpness (HFR), picture by picture.
    hfrs: HashMap<String, Vec<f64>>,
    /// Focuser position and HFR for each picture with stars, to find the
    /// sharpest position.
    focus_log: Vec<(f64, f64)>,
    img: Option<glow::Display>,
    shown: Option<Place>,
    tx: Sender<Ev>,
    rx: Receiver<Ev>,
    wake: Option<crust::Wake>,
    top: Pane,
    left: Pane,
    right: Pane,
    foot: Pane,
}

fn state_col(pal: &Pal, s: &str) -> u8 {
    match s {
        "Ok" => pal.ok,
        "Busy" => pal.busy,
        "Alert" => pal.alert,
        _ => pal.grey,
    }
}

/// A server message's colour: red for an error, yellow for a warning.
fn message_col(pal: &Pal, m: &str) -> Option<u8> {
    if m.contains("[ERROR]") {
        Some(pal.alert)
    } else if m.contains("[WARNING]") {
        Some(pal.busy)
    } else {
        None
    }
}

fn cut(s: &str, w: usize) -> String {
    s.chars().take(w).collect()
}

fn pad(s: &str, w: usize) -> String {
    let s = cut(s, w);
    let n = s.chars().count();
    format!("{s}{}", " ".repeat(w - n))
}

/// A number element as its format shows it.
fn num_text(p: &Prop, name: &str) -> Option<String> {
    let e = p.elem(name)?;
    parse_number(&e.value).map(|v| show_number(v, &e.format))
}

fn value_text(pal: &Pal, p: &Prop, i: usize) -> String {
    let e = &p.elems[i];
    match p.kind {
        Kind::Switch if e.value == "On" => style::fg("On", pal.ok),
        Kind::Switch => style::fg("Off", pal.grey),
        Kind::Light => style::fg(&e.value, state_col(pal, &e.value)),
        Kind::Number => parse_number(&e.value).map(|v| show_number(v, &e.format)).unwrap_or_else(|| e.value.clone()),
        Kind::Text => e.value.clone(),
        Kind::Blob => style::dim(if e.value.is_empty() { "(data)" } else { &e.value }),
    }
}

/// Hours or degrees as h:m:s.
fn sexa(v: f64) -> String {
    show_number(v, "%9.6m").trim().to_string()
}

/// A line of bars, one per reading, low to high.
fn sparkline(v: &[f64]) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let lo = v.iter().cloned().fold(f64::INFINITY, f64::min);
    let hi = v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let span = (hi - lo).max(0.5);
    v.iter().map(|x| BARS[(((x - lo) / span) * 7.0).round() as usize]).collect()
}

fn is_local(addr: &str) -> bool {
    let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr);
    matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "")
}

/// The INDI drivers on the PATH: programs named indi_ that are not the
/// command-line clients.
fn find_drivers() -> Vec<String> {
    let mut out = Vec::new();
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("indi_") && !matches!(name.as_str(), "indi_getprop" | "indi_setprop" | "indi_eval") {
                out.push(name);
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// "5:35:17 -5:23:28" as RA hours and Dec degrees.
fn parse_radec(t: &str) -> Option<(f64, f64)> {
    let parts: Vec<&str> = t.split_whitespace().collect();
    if parts.len() != 2 {
        return None;
    }
    let ra = parse_number(parts[0]).filter(|v| (0.0..24.0).contains(v))?;
    let dec = parse_number(parts[1]).filter(|v| (-90.0..=90.0).contains(v))?;
    Some((ra, dec))
}

/// Open a connection and start the thread that reads it.
fn connect(addr: &str, gen: u32, tx: Sender<Ev>, waker: Option<crust::Waker>) -> Result<TcpStream, String> {
    let sa = addr.to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("no such address")?;
    let mut s = TcpStream::connect_timeout(&sa, Duration::from_secs(3)).map_err(|e| e.to_string())?;
    let mut r = s.try_clone().map_err(|e| e.to_string())?;
    std::thread::spawn(move || {
        let wake = || if let Some(w) = &waker { w.wake() };
        let mut parser = Parser::default();
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = match r.read(&mut buf) {
                Ok(0) => 0,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => 0,
            };
            if n == 0 {
                let _ = tx.send(Ev::Gone(gen, "the server closed the connection".into()));
                wake();
                return;
            }
            for mut el in parser.feed(&buf[..n]) {
                // A picture is read here, and only its size goes on.
                if el.name == "setBLOBVector" && el.attr("name") == Some("CCD1") {
                    let dev = el.attr("device").unwrap_or("").to_string();
                    for c in el.children.iter_mut().filter(|c| !c.text.trim().is_empty()) {
                        let text = std::mem::take(&mut c.text);
                        let format = c.attr("format").unwrap_or("").trim().to_string();
                        let frame = Frame::from_blob(&text, &format);
                        c.text = format!("{:.1} MB {format}", text.len() as f64 * 0.75 / 1e6);
                        if tx.send(Ev::Frame(gen, dev.clone(), frame)).is_err() {
                            return;
                        }
                    }
                }
                if tx.send(Ev::El(gen, el)).is_err() {
                    return;
                }
            }
            wake();
        }
    });
    s.write_all(proto::get_properties().as_bytes()).map_err(|e| e.to_string())?;
    Ok(s)
}

impl App {
    fn new(addr: String) -> App {
        let (tx, rx) = channel();
        let cfg = Config::load();
        let pal = if cfg.night { NIGHT } else { DAY };
        let blank = || Pane::new(1, 1, 1, 1, 252, 0);
        let mut app = App {
            addr,
            cfg,
            pal,
            model: Model::default(),
            net: None,
            gen: 0,
            link: String::new(),
            server: None,
            dev: 0,
            focus: Focus::Devices,
            view: View::Device,
            group: 0,
            row: 0,
            drivers: Vec::new(),
            drv_row: 0,
            seen: 0,
            note: None,
            target: None,
            held: None,
            temps: HashMap::new(),
            frames: HashMap::new(),
            seq: 0,
            blobs: HashSet::new(),
            looping: None,
            autosave: false,
            zoom: false,
            hfrs: HashMap::new(),
            focus_log: Vec::new(),
            img: None,
            shown: None,
            tx,
            rx,
            wake: crust::Wake::new(),
            top: blank(),
            left: blank(),
            right: blank(),
            foot: blank(),
        };
        app.layout();
        app.open();
        // No server, but drivers picked before: start one.
        if app.net.is_none() && !app.cfg.drivers.is_empty() && is_local(&app.addr) {
            app.start_server();
        }
        app
    }

    fn layout(&mut self) {
        self.hide_frame();
        let (cols, rows) = Crust::terminal_size();
        let rows = rows.max(6);
        let lw = 28.min(cols / 3).max(12);
        let p = self.pal;
        self.top = Pane::new(1, 1, cols, 1, p.text, p.bar);
        self.left = Pane::new(1, 2, lw, rows - 2, p.text, p.list);
        self.right = Pane::new(lw + 1, 2, cols.saturating_sub(lw), rows - 2, p.text, 0);
        self.foot = Pane::new(1, rows, cols, 1, p.text, p.bar);
        for p in [&mut self.top, &mut self.left, &mut self.right, &mut self.foot] {
            p.wrap = false;
            p.scroll = false;
        }
        Crust::clear_screen();
    }

    /// Connect to the server, forgetting what the last one said.
    fn open(&mut self) {
        self.gen += 1;
        self.model = Model::default();
        self.dev = 0;
        self.group = 0;
        self.row = 0;
        self.seen = 0;
        self.blobs.clear();
        self.looping = None;
        let waker = self.wake.as_ref().map(|w| w.waker());
        match connect(&self.addr, self.gen, self.tx.clone(), waker) {
            Ok(s) => {
                self.net = Some(s);
                self.link = "connected".into();
            }
            Err(e) => {
                self.net = None;
                self.link = format!("no server: {e}");
            }
        }
    }

    fn send(&mut self, xml: String) {
        let Some(s) = self.net.as_mut() else {
            self.say("not connected to a server; r tries again", self.pal.alert);
            return;
        };
        if s.write_all(xml.as_bytes()).is_err() {
            self.net = None;
            self.link = "the connection is lost".into();
        }
    }

    fn say(&mut self, text: &str, col: u8) {
        self.note = Some((text.to_string(), col));
    }

    /// Take in what the thread read. True when the screen changes.
    fn take(&mut self, ev: Ev) -> bool {
        match ev {
            Ev::El(g, el) if g == self.gen => {
                let dev = el.attr("device").unwrap_or("").to_string();
                let name = el.attr("name").unwrap_or("").to_string();
                let watched = el.name.starts_with("set") && matches!(name.as_str(), "EQUATORIAL_EOD_COORD" | "FOCUS_TEMPERATURE");
                let was = self.model.device(&dev).and_then(|d| d.prop(&name)).map(|p| p.state.clone());
                self.model.apply(&el);
                if watched {
                    self.after_set(&dev, &name, was.as_deref().unwrap_or(""));
                }
                true
            }
            Ev::Frame(g, dev, frame) if g == self.gen => {
                match frame {
                    Ok(f) => self.new_frame(dev, f),
                    Err(e) => {
                        self.looping = None;
                        self.say(&e, self.pal.alert);
                    }
                }
                true
            }
            Ev::Gone(g, why) if g == self.gen => {
                self.net = None;
                self.link = why;
                true
            }
            _ => false,
        }
    }

    /// Notice the end of a go-to, and keep the focuser's temperatures.
    fn after_set(&mut self, dev: &str, name: &str, was: &str) {
        let Some(p) = self.model.device(dev).and_then(|d| d.prop(name)) else { return };
        if name == "FOCUS_TEMPERATURE" {
            let Some(t) = p.num("TEMPERATURE") else { return };
            let v = self.temps.entry(dev.to_string()).or_default();
            if v.last() != Some(&t) {
                v.push(t);
                if v.len() > 40 {
                    v.remove(0);
                }
            }
            return;
        }
        let now = p.state.clone();
        if was == "Busy" && now != "Busy" {
            if let Some(t) = self.target.clone() {
                match now.as_str() {
                    "Alert" => self.say(&format!("the go-to to {t} failed"), self.pal.alert),
                    _ => self.say(&format!("arrived at {t}"), self.pal.ok),
                }
            }
        }
    }

    fn device(&self) -> Option<&Device> {
        self.model.devices.get(self.dev)
    }

    fn is_mount(&self) -> bool {
        self.device().is_some_and(|d| d.is_mount())
    }

    fn is_camera(&self) -> bool {
        self.device().is_some_and(|d| d.is_camera())
    }

    fn has_control(&self) -> bool {
        self.device().is_some_and(|d| d.is_camera() || d.is_focuser() || d.is_mount())
    }

    /// Latitude and longitude: from a device that knows, else ~/.indirc.
    fn site(&self) -> Option<(f64, f64)> {
        let from_gear = self.model.devices.iter().filter_map(|d| d.prop("GEOGRAPHIC_COORD")).find_map(|p| {
            let (lat, lon) = (p.num("LAT")?, p.num("LONG")?);
            (lat != 0.0 || lon != 0.0).then_some((lat, lon))
        });
        from_gear.or(self.cfg.site)
    }

    /// Send new values for a property of the selected device.
    fn set(&mut self, prop: &str, values: Vec<(String, String)>) {
        let Some(name) = self.device().map(|d| d.name.clone()) else { return };
        self.set_on(&name, prop, values);
    }

    /// Send new values for a property of the device called `dev`.
    fn set_on(&mut self, dev: &str, prop: &str, values: Vec<(String, String)>) {
        let Some(d) = self.model.device(dev) else { return };
        let Some(p) = d.prop(prop) else {
            let t = format!("{} has no {prop}", d.name);
            self.say(&t, self.pal.alert);
            return;
        };
        let xml = proto::new_vector(&d.name, p, &values);
        self.send(xml);
    }

    /// Turn on switch `elem` of `prop`, by the property's rule.
    fn pick(&mut self, prop: &str, elem: &str) {
        let Some(p) = self.device().and_then(|d| d.prop(prop)) else { return };
        let values = proto::pick(p, elem);
        self.set(prop, values);
    }

    fn toggle_connection(&mut self) {
        let Some(d) = self.device() else { return };
        let to = if d.connected() { "DISCONNECT" } else { "CONNECT" };
        self.pick("CONNECTION", to);
    }

    // ─────────────────────────── the server ──────────────────────────

    /// Start indiserver with the drivers in the settings, on this
    /// machine, and connect to it.
    fn start_server(&mut self) {
        if !is_local(&self.addr) {
            self.say("indi starts a server only on this machine", self.pal.alert);
            return;
        }
        if self.net.is_some() && self.server.is_none() {
            let t = format!("a server already runs at {}", self.addr);
            self.say(&t, self.pal.alert);
            return;
        }
        self.stop_server();
        let port = self.addr.rsplit(':').next().unwrap_or("7624").to_string();
        let child = Command::new("indiserver")
            .arg("-p")
            .arg(&port)
            .args(&self.cfg.drivers)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match child {
            Ok(c) => self.server = Some(c),
            Err(e) => {
                self.say(&format!("indiserver would not start: {e}"), self.pal.alert);
                return;
            }
        }
        // It needs a moment before it listens; three seconds at most.
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(150));
            if let Some(Ok(Some(_))) = self.server.as_mut().map(|c| c.try_wait()) {
                self.server = None;
                self.say(&format!("indiserver ended at once; is port {port} in use?"), self.pal.alert);
                return;
            }
            self.open();
            if self.net.is_some() {
                self.link = "connected to the server indi started".into();
                return;
            }
        }
    }

    fn stop_server(&mut self) {
        if let Some(mut c) = self.server.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    fn open_drivers(&mut self) {
        let found = find_drivers();
        if found.is_empty() {
            self.say("no INDI drivers on the PATH; on Debian and Ubuntu they come in indi-bin", self.pal.alert);
            return;
        }
        self.drivers = found.into_iter().map(|d| {
            let on = self.cfg.drivers.contains(&d);
            (d, on)
        }).collect();
        self.drv_row = self.drivers.iter().position(|(_, on)| *on).unwrap_or(0);
        self.view = View::Drivers;
    }

    /// Keys while picking drivers. False for a key it leaves to the rest.
    fn driver_key(&mut self, k: &str) -> bool {
        match k {
            "UP" => self.drv_row = self.drv_row.saturating_sub(1),
            "DOWN" => self.drv_row = (self.drv_row + 1).min(self.drivers.len().saturating_sub(1)),
            " " => {
                if let Some(d) = self.drivers.get_mut(self.drv_row) {
                    d.1 = !d.1;
                }
            }
            "ENTER" => {
                let picked: Vec<String> = self.drivers.iter().filter(|d| d.1).map(|d| d.0.clone()).collect();
                if picked.is_empty() {
                    self.say("tick a driver with Space first", self.pal.alert);
                    return true;
                }
                self.cfg.drivers = picked;
                self.cfg.save();
                self.view = View::Device;
                self.start_server();
            }
            "ESC" => self.view = View::Device,
            _ => return false,
        }
        true
    }

    // ─────────────────────────── the lists ───────────────────────────

    fn groups(&self) -> Vec<String> {
        self.device().map(|d| d.groups()).unwrap_or_default()
    }

    fn lines(&self) -> Vec<Line> {
        let Some(d) = self.device() else { return Vec::new() };
        let Some(g) = self.groups().get(self.group).cloned() else { return Vec::new() };
        let mut out = Vec::new();
        for (pi, p) in d.props.iter().enumerate().filter(|(_, p)| p.group == g) {
            out.push(Line::Head(pi));
            out.extend((0..p.elems.len()).map(|ei| Line::Elem(pi, ei)));
        }
        out
    }

    /// The selected element: property index and element index.
    fn selected(&self) -> Option<(usize, usize)> {
        self.lines().into_iter().filter_map(|l| match l {
            Line::Elem(p, e) => Some((p, e)),
            Line::Head(_) => None,
        }).nth(self.row)
    }

    /// Keep the selections inside what the server now has.
    fn clamp(&mut self) {
        self.dev = self.dev.min(self.model.devices.len().saturating_sub(1));
        self.group = self.group.min(self.groups().len().saturating_sub(1));
        let n = self.lines().iter().filter(|l| matches!(l, Line::Elem(..))).count();
        self.row = self.row.min(n.saturating_sub(1));
        if self.focus == Focus::Control && !self.has_control() {
            self.focus = Focus::Devices;
        }
    }

    // ─────────────────────────── drawing ─────────────────────────────

    fn render(&mut self) {
        self.clamp();
        // A picture leaving or changing: the text under it is drawn anew,
        // which in a terminal without pixels wipes the old one.
        let want = self.frame_place();
        if want != self.shown {
            match want {
                None => self.hide_frame(),
                Some(_) => self.right.invalidate(),
            }
        }
        let pal = self.pal;
        let link = style::fg(&self.link, if self.net.is_some() { pal.ok } else { pal.alert });
        let night = if self.cfg.night { style::fg("   night", pal.accent) } else { String::new() };
        self.top.say(&format!(" {}   {}   {link}{night}", style::bold("indi"), self.addr));

        let lw = self.left.w as usize;
        let mut s = self.title("Devices", Focus::Devices);
        for (i, d) in self.model.devices.iter().enumerate() {
            let dot = if d.connected() { style::fg("●", pal.ok) } else { style::fg("○", pal.grey) };
            let name = cut(&d.name, lw.saturating_sub(5));
            let mark = if i == self.dev { "→" } else { " " };
            let name = if i == self.dev && self.focus == Focus::Devices { style::bold(&name) } else { name };
            s.push_str(&format!("\n{mark}{dot} {name}"));
        }
        self.left.say(&s);

        let body = match self.view {
            View::Device => self.device_lines(),
            View::Log => self.log_lines(),
            View::Help => self.help_lines(),
            View::Drivers => self.driver_lines(),
        };
        self.right.say(&body.join("\n"));

        let text = match &self.note {
            Some((t, c)) => style::fg(&format!(" {t}"), *c),
            None => match self.model.messages.last() {
                Some(m) if self.model.messages.len() > self.seen => {
                    style::fg(&format!(" {m}"), message_col(&pal, m).unwrap_or(pal.busy))
                }
                _ => style::dim(&crust::key_help(self.hints())),
            },
        };
        self.foot.say(&crust::with_version(&text, self.foot.w as usize, "indi", VERSION));
        if let Some(p) = want.filter(|p| Some(*p) != self.shown) {
            self.show_frame(p);
        }
    }

    // ─────────────────────────── the picture ─────────────────────────

    /// Where the selected camera's picture goes: below its control
    /// block, while the settings are not in use.
    fn frame_place(&self) -> Option<Place> {
        if self.view != View::Device || self.focus == Focus::Props {
            return None;
        }
        let d = self.device().filter(|d| d.is_camera())?;
        let (seq, _) = self.frames.get(&d.name)?;
        let top = CAM_LINES + 2;
        let rows = self.right.h.saturating_sub(top);
        let cols = self.right.w.saturating_sub(2);
        if rows < 3 || cols < 8 {
            return None;
        }
        Some(Place { seq: *seq, x: self.right.x + 1, y: self.right.y + top, cols, rows, zoom: self.zoom, night: self.cfg.night })
    }

    fn show_frame(&mut self, p: Place) {
        let Some(name) = self.device().map(|d| d.name.clone()) else { return };
        let Some((_, f)) = self.frames.get(&name) else { return };
        let (bw, bh) = glow::cell_box(p.cols, p.rows);
        let cell = (bw as f64 / p.cols as f64, bh as f64 / p.rows as f64);
        let (cols, rows) = f.fit(p.cols, p.rows, cell, p.zoom);
        let mut canvas = glow::Canvas::new(cols, rows);
        f.paint(&mut canvas, p.zoom, p.night);
        self.img.get_or_insert_with(glow::Display::new).swap_canvas(&canvas, p.x, p.y);
        self.shown = Some(p);
    }

    /// Take the picture off the screen, before other text or another
    /// program takes its place.
    fn hide_frame(&mut self) {
        if self.shown.take().is_some() {
            if let Some(img) = self.img.as_mut() {
                img.drop_canvas();
            }
            self.right.invalidate();
        }
    }

    fn new_frame(&mut self, dev: String, f: Frame) {
        if let Some(h) = f.hfr {
            let v = self.hfrs.entry(dev.clone()).or_default();
            v.push(h);
            if v.len() > 40 {
                v.remove(0);
            }
            let pos = self.model.devices.iter().find_map(|d| d.prop("ABS_FOCUS_POSITION")?.num("FOCUS_ABSOLUTE_POSITION"));
            if let Some(pos) = pos {
                self.focus_log.push((pos, h));
            }
        }
        self.seq += 1;
        self.frames.insert(dev.clone(), (self.seq, f));
        if self.autosave {
            self.save_frame(&dev);
        }
        if self.looping.as_deref() == Some(dev.as_str()) {
            self.expose(&dev);
        }
    }

    /// Start an exposure. The first one asks the server for the
    /// camera's pictures, which it does not send unasked.
    fn expose(&mut self, dev: &str) {
        if self.blobs.insert(dev.to_string()) {
            self.send(proto::enable_blob(dev));
        }
        let secs = self.cfg.exposure.to_string();
        self.set_on(dev, "CCD_EXPOSURE", vec![("CCD_EXPOSURE_VALUE".into(), secs)]);
    }

    fn exposing(&self) -> bool {
        self.device().and_then(|d| d.prop("CCD_EXPOSURE")).is_some_and(|p| p.state == "Busy")
    }

    /// Save a camera's last picture as the FITS file it came as, named
    /// by the target (or the camera) and the time it was taken.
    fn save_frame(&mut self, dev: &str) {
        let Some((_, f)) = self.frames.get(dev) else {
            return self.say("no picture yet; Space takes one", self.pal.grey);
        };
        let dir = self.cfg.pictures_dir();
        let stamp = f.date.clone().unwrap_or_else(|| {
            let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            secs.to_string()
        });
        let what = self.target.as_deref().unwrap_or(dev);
        let base: String = format!("{what}_{stamp}")
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' { c } else if c == ':' { '-' } else { '_' })
            .collect();
        let mut path = dir.join(format!("{base}.fits"));
        let mut n = 2;
        while path.exists() {
            path = dir.join(format!("{base}_{n}.fits"));
            n += 1;
        }
        let res = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, &f.bytes));
        match res {
            Ok(()) => self.say(&format!("saved {}", path.display()), self.pal.ok),
            Err(e) => self.say(&format!("could not save in {}: {e}", dir.display()), self.pal.alert),
        }
    }

    fn title(&self, t: &str, f: Focus) -> String {
        if self.focus == f {
            style::bold(&style::fg(&format!(" {t}"), self.pal.accent))
        } else {
            style::dim(&format!(" {t}"))
        }
    }

    fn heading(&self, t: &str) -> String {
        style::bold(&style::fg(&format!(" {t}"), self.pal.accent))
    }

    fn hints(&self) -> &'static str {
        match (self.view, self.focus) {
            (View::Drivers, _) => " ↑↓ driver · Space tick · Enter start the server · Esc back",
            (View::Log | View::Help, _) => " Esc back · q quit",
            (_, Focus::Devices) => " ↑↓ device · Enter connect · Tab on · d drivers · n night · ? help · q quit",
            (_, Focus::Control) if self.is_camera() => {
                " Space expose · e time · + - longer, shorter · l loop · s save · a save all · z zoom · t cool · g gain · b bin"
            }
            (_, Focus::Control) if self.is_mount() => {
                " hold ↑↓←→ move · Space stop · g go to · s sky · y sync · + - speed · t track · p park"
            }
            (_, Focus::Control) => " ←→ 10 · Shift+←→ 100 · [ ] 1 · g go to · 1-9 saved · s save · Space stop",
            (_, Focus::Props) => " ↑↓ setting · ←→ group · Enter change · + - nudge · / find · W save · ? help",
        }
    }

    fn log_lines(&self) -> Vec<String> {
        let h = self.right.h as usize;
        let w = self.right.w as usize;
        let mut out = vec![self.heading("Messages from the server  (m closes)")];
        let start = self.model.messages.len().saturating_sub(h.saturating_sub(1));
        out.extend(self.model.messages[start..].iter().map(|m| {
            let line = format!(" {}", cut(m, w.saturating_sub(2)));
            match message_col(&self.pal, m) {
                Some(c) => style::fg(&line, c),
                None => line,
            }
        }));
        out
    }

    fn help_lines(&self) -> Vec<String> {
        let mut out = vec![self.heading("Help  (? closes)")];
        out.extend(HELP.lines().map(|l| format!(" {l}")));
        out
    }

    fn driver_lines(&self) -> Vec<String> {
        let h = self.right.h as usize;
        let mut out = vec![
            self.heading("Drivers for a server on this machine"),
            style::dim(" Tick the ones for your gear. The server ends when indi does."),
            String::new(),
        ];
        let room = h.saturating_sub(out.len());
        let first = (self.drv_row + 1).saturating_sub(room);
        for (i, (name, on)) in self.drivers.iter().enumerate().skip(first).take(room) {
            let mark = if i == self.drv_row { "→" } else { " " };
            let tick = if *on { style::fg("[x]", self.pal.ok) } else { style::dim("[ ]") };
            let name = if i == self.drv_row { style::bold(name) } else { name.clone() };
            out.push(format!(" {mark} {tick} {name}"));
        }
        out
    }

    fn device_lines(&self) -> Vec<String> {
        let w = self.right.w as usize;
        let h = self.right.h as usize;
        let Some(d) = self.device() else { return self.empty_lines() };
        let mut out = Vec::new();
        if d.is_camera() {
            out.push(self.title(&format!("Camera · {}", d.name), Focus::Control));
            out.extend(self.camera_lines(d));
            out.push(String::new());
            // The picture fills the rest.
            if self.frame_place().is_some() {
                return out;
            }
        } else if d.is_focuser() {
            out.push(self.title(&format!("Focuser · {}", d.name), Focus::Control));
            out.extend(self.focuser_lines(d));
            out.push(String::new());
        } else if d.is_mount() {
            out.push(self.title(&format!("Mount · {}", d.name), Focus::Control));
            out.extend(self.mount_lines(d));
            out.push(String::new());
        }
        // The group names, the open one marked.
        let groups = self.groups();
        let tabs: Vec<String> = groups.iter().enumerate().map(|(i, g)| {
            if i == self.group {
                let c = if self.focus == Focus::Props { self.pal.accent } else { self.pal.text as u8 };
                style::bold(&style::fg(g, c))
            } else {
                style::dim(g)
            }
        }).collect();
        out.push(crust::truncate_ansi(&format!(" {}", tabs.join(" · ")), w.saturating_sub(1)));

        let lines = self.lines();
        let room = h.saturating_sub(out.len());
        // The line the selected element is on, kept in view.
        let mut n = 0;
        let at = lines.iter().position(|l| {
            if let Line::Elem(..) = l {
                n += 1;
                n == self.row + 1
            } else {
                false
            }
        }).unwrap_or(0);
        let first = (at + 2).saturating_sub(room);
        let lw = 26.min(w / 2);
        for (i, l) in lines.iter().enumerate().skip(first).take(room) {
            out.push(match *l {
                Line::Head(pi) => {
                    let p = &d.props[pi];
                    let ro = if p.writable() { String::new() } else { style::dim(" · read only") };
                    format!(" {} {}{ro}", style::fg("●", state_col(&self.pal, &p.state)), cut(&p.label, w.saturating_sub(16)))
                }
                Line::Elem(pi, ei) => {
                    let p = &d.props[pi];
                    let sel = i == at && self.focus == Focus::Props;
                    let mark = if i == at { "→" } else { " " };
                    let label = pad(&p.elems[ei].label, lw);
                    let label = if sel { style::bold(&label) } else { label };
                    let v = crust::truncate_ansi(&value_text(&self.pal, p, ei), w.saturating_sub(lw + 6));
                    format!("  {mark} {label} {v}")
                }
            });
        }
        out
    }

    fn focuser_lines(&self, d: &Device) -> Vec<String> {
        let pal = &self.pal;
        let mut out = Vec::new();
        if let Some(p) = d.prop("ABS_FOCUS_POSITION") {
            let pos = p.num("FOCUS_ABSOLUTE_POSITION").unwrap_or(0.0);
            let max = d.prop("FOCUS_MAX").and_then(|m| m.num("FOCUS_MAX_VALUE"))
                .or_else(|| p.elem("FOCUS_ABSOLUTE_POSITION").map(|e| e.max))
                .filter(|m| *m > 0.0);
            let mut line = format!("   Position {}", style::bold(&format!("{pos:.0}")));
            if let Some(max) = max {
                let full = ((pos / max).clamp(0.0, 1.0) * 24.0).round() as usize;
                let bar = format!("{}{}", "█".repeat(full), "░".repeat(24 - full));
                line.push_str(&format!(" of {max:.0}   {}", style::fg(&bar, state_col(pal, &p.state))));
            }
            if p.state == "Busy" {
                line.push_str(&style::fg("   moving", pal.busy));
            }
            out.push(line);
        }
        if let Some(t) = d.prop("FOCUS_TEMPERATURE").and_then(|p| p.num("TEMPERATURE")) {
            let trend = self.temps.get(&d.name).filter(|v| v.len() > 1).map(|v| sparkline(v)).unwrap_or_default();
            out.push(format!("   Temperature {t:.1} °C   {}", style::fg(&trend, pal.accent)));
        }
        if let Some(&(_, now)) = self.focus_log.last() {
            let best = self.focus_log.iter().cloned().min_by(|a, b| a.1.total_cmp(&b.1)).unwrap_or((0.0, now));
            let trend: Vec<f64> = self.focus_log.iter().rev().take(40).rev().map(|x| x.1).collect();
            out.push(format!(
                "   Stars HFR {now:.2} px · sharpest {:.2} at {:.0}   {}",
                best.1,
                best.0,
                style::fg(&sparkline(&trend), pal.accent)
            ));
        }
        let saved: Vec<String> = self.cfg.presets.iter().enumerate().filter_map(|(i, p)| {
            p.as_ref().map(|(pos, name)| format!("{} {} {pos:.0}", style::fg(&(i + 1).to_string(), pal.accent), name))
        }).collect();
        if !saved.is_empty() {
            out.push(format!("   {}", saved.join("   ")));
        }
        out
    }

    /// The camera's control block: always CAM_LINES lines, so the
    /// picture below does not move.
    fn camera_lines(&self, d: &Device) -> Vec<String> {
        let pal = &self.pal;
        let mut out = Vec::new();
        let mut l = format!("   Exposure {}", style::bold(&format!("{} s", self.cfg.exposure)));
        if let Some(p) = d.prop("CCD_EXPOSURE").filter(|p| p.state == "Busy") {
            let left = p.num("CCD_EXPOSURE_VALUE").map(|v| format!(", {v:.1} s left")).unwrap_or_default();
            l.push_str(&style::fg(&format!("   exposing{left}"), pal.busy));
        } else if d.prop("CCD_EXPOSURE").is_some_and(|p| p.state == "Alert") {
            l.push_str(&style::fg("   the exposure failed", pal.alert));
        }
        let mut modes = Vec::new();
        if self.looping.as_deref() == Some(d.name.as_str()) {
            modes.push("over and over");
        }
        if self.autosave {
            modes.push("saving each picture");
        }
        if self.zoom {
            modes.push("middle, pixel for pixel");
        }
        if !modes.is_empty() {
            l.push_str(&style::fg(&format!("   {}", modes.join(" · ")), pal.accent));
        }
        out.push(l);
        let mut sensor = Vec::new();
        if let Some(t) = d.prop("CCD_TEMPERATURE").and_then(|p| p.num("CCD_TEMPERATURE_VALUE")) {
            sensor.push(format!("sensor {t:.1} °C"));
        }
        if let Some(p) = d.prop("CCD_COOLER") {
            let power = d.prop("CCD_COOLER_POWER").and_then(|p| p.elems.first().and_then(|e| parse_number(&e.value)));
            sensor.push(match (p.on("COOLER_ON"), power) {
                (true, Some(w)) => style::fg(&format!("cooler {w:.0}%"), pal.ok),
                (true, None) => style::fg("cooler on", pal.ok),
                _ => "cooler off".to_string(),
            });
        }
        if let Some(g) = self.gain_elem().and_then(|(p, e)| d.prop(&p)?.num(&e)) {
            sensor.push(format!("gain {g:.0}"));
        }
        if let Some(b) = d.prop("CCD_BINNING").and_then(|p| Some((p.num("HOR_BIN")?, p.num("VER_BIN")?))) {
            sensor.push(format!("bin {:.0}×{:.0}", b.0, b.1));
        }
        out.push(if sensor.is_empty() { String::new() } else { format!("   {}", sensor.join(" · ")) });
        match self.frames.get(&d.name) {
            Some((_, f)) => {
                out.push(format!("   {}", f.facts()));
                let when = f.date.as_deref().and_then(|t| t.get(11..19)).map(|t| format!("   taken {t}")).unwrap_or_default();
                let stars = match f.hfr {
                    Some(h) => {
                        let trend = self.hfrs.get(&d.name).filter(|v| v.len() > 1).map(|v| sparkline(v)).unwrap_or_default();
                        format!("{} stars · HFR {h:.2} px   {}", f.stars, style::fg(&trend, pal.accent))
                    }
                    None => "no stars found".to_string(),
                };
                out.push(format!("   {stars}{}", style::dim(&when)));
            }
            None => {
                out.push(style::dim("   Space takes a picture"));
                out.push(String::new());
            }
        }
        out
    }

    /// The gain setting, by the name this driver gives it.
    fn gain_elem(&self) -> Option<(String, String)> {
        let d = self.device()?;
        if d.prop("CCD_GAIN").is_some_and(|p| p.elem("GAIN").is_some()) {
            return Some(("CCD_GAIN".into(), "GAIN".into()));
        }
        d.prop("CCD_CONTROLS").filter(|p| p.elem("Gain").is_some()).map(|_| ("CCD_CONTROLS".into(), "Gain".into()))
    }

    fn mount_lines(&self, d: &Device) -> Vec<String> {
        let pal = &self.pal;
        let mut out = Vec::new();
        let eq = d.prop("EQUATORIAL_EOD_COORD");
        if let Some(p) = eq {
            let ra = num_text(p, "RA").unwrap_or_default();
            let dec = num_text(p, "DEC").unwrap_or_default();
            let busy = if p.state == "Busy" { style::fg("   slewing", pal.busy) } else { String::new() };
            out.push(format!("   RA {}   Dec {}{busy}", style::bold(&ra), style::bold(&dec)));
        }
        // Height and direction: from the driver, or worked out here.
        let altaz = d.prop("HORIZONTAL_COORD").and_then(|p| Some((p.num("ALT")?, p.num("AZ")?))).or_else(|| {
            let p = eq?;
            Some(sky::alt_az(p.num("RA")?, p.num("DEC")?, sky::now_jd(), self.site()?))
        });
        if let Some((alt, az)) = altaz {
            let low = if alt < 0.0 { style::fg("   below the horizon", pal.alert) } else { String::new() };
            out.push(format!("   Alt {alt:.1}°   Az {az:.1}°{low}"));
        }
        if let Some(t) = &self.target {
            out.push(format!("   Target {}", style::fg(t, pal.accent)));
        }
        let mut facts = Vec::new();
        if let Some(p) = d.prop("TELESCOPE_TRACK_STATE") {
            facts.push(if p.on("TRACK_ON") { style::fg("tracking", pal.ok) } else { "not tracking".to_string() });
        }
        if let Some(e) = d.prop("TELESCOPE_SLEW_RATE").and_then(|p| p.which_on()) {
            facts.push(format!("speed {}", e.label));
        }
        if let Some(p) = d.prop("TELESCOPE_PARK") {
            facts.push(if p.on("PARK") { style::fg("parked", pal.busy) } else { "unparked".to_string() });
        }
        for (prop, names) in [
            ("TELESCOPE_MOTION_NS", [("MOTION_NORTH", "north"), ("MOTION_SOUTH", "south")]),
            ("TELESCOPE_MOTION_WE", [("MOTION_EAST", "east"), ("MOTION_WEST", "west")]),
        ] {
            if let Some(p) = d.prop(prop) {
                for (n, word) in names {
                    if p.on(n) {
                        facts.push(style::fg(&format!("moving {word}"), pal.busy));
                    }
                }
            }
        }
        if !facts.is_empty() {
            out.push(format!("   {}", facts.join(" · ")));
        }
        out
    }

    fn empty_lines(&self) -> Vec<String> {
        let a = self.pal.accent;
        let mut out = vec![String::new()];
        if self.net.is_some() {
            out.push(" Connected, but the server has no devices yet.".into());
            out.push(" Is a driver running?".into());
        } else {
            out.push(format!(" No INDI server at {}.", self.addr));
            out.push(format!(" Press {} to pick drivers and start one here, or start it yourself:", style::fg("d", a)));
            out.push(String::new());
            out.push(style::fg("   indiserver indi_ieaf_focuser indi_eqmod_telescope", a));
            out.push(String::new());
            out.push(" To try it without gear:".into());
            out.push(String::new());
            out.push(style::fg("   indiserver indi_simulator_telescope indi_simulator_focus indi_simulator_ccd", a));
        }
        out
    }

    // ─────────────────────────── keys ────────────────────────────────

    /// Handle a key. True to quit.
    fn key(&mut self, k: &str) -> bool {
        self.note = None;
        self.seen = self.model.messages.len();
        if self.view == View::Drivers && self.driver_key(k) {
            return false;
        }
        match k {
            "q" | "Q" => {
                self.quit();
                return true;
            }
            "TAB" => self.cycle(1),
            "S-TAB" => self.cycle(-1),
            "ESC" => {
                self.view = View::Device;
                self.focus = Focus::Devices;
            }
            "r" => self.open(),
            "c" => self.toggle_connection(),
            "m" => self.flip(View::Log),
            "?" => self.flip(View::Help),
            "d" => self.open_drivers(),
            "n" => self.night(),
            "/" => self.find(),
            "W" => self.save_driver_settings(),
            "C-A" => self.claude(),
            _ => match self.focus {
                Focus::Devices => self.device_key(k),
                Focus::Control if self.is_camera() => self.camera_key(k),
                Focus::Control if self.is_mount() => self.mount_key(k),
                Focus::Control => self.focuser_key(k),
                Focus::Props => self.prop_key(k),
            },
        }
        // A mount moved by a held key stops when the keys go elsewhere.
        if self.held.is_some() && self.focus != Focus::Control {
            self.stop_mount(self.dev);
        }
        false
    }

    /// A key let go: the end of a held arrow on a mount. True when
    /// something was sent.
    fn release(&mut self, k: &str) -> bool {
        if self.focus != Focus::Control || !self.is_mount() {
            return false;
        }
        let prop = match k {
            "UP" | "DOWN" => "TELESCOPE_MOTION_NS",
            "LEFT" | "RIGHT" => "TELESCOPE_MOTION_WE",
            _ => return false,
        };
        self.held = None;
        let Some(p) = self.device().and_then(|d| d.prop(prop)) else { return false };
        let off: Vec<(String, String)> = p.elems.iter().map(|e| (e.name.clone(), "Off".into())).collect();
        self.set(prop, off);
        true
    }

    fn flip(&mut self, v: View) {
        self.view = if self.view == v { View::Device } else { v };
    }

    fn cycle(&mut self, by: i32) {
        let order = [Focus::Devices, Focus::Control, Focus::Props];
        let mut i = order.iter().position(|f| *f == self.focus).unwrap_or(0) as i32;
        loop {
            i = (i + by).rem_euclid(3);
            if order[i as usize] != Focus::Control || self.has_control() {
                break;
            }
        }
        self.focus = order[i as usize];
        self.view = View::Device;
    }

    fn night(&mut self) {
        self.cfg.night = !self.cfg.night;
        self.pal = if self.cfg.night { NIGHT } else { DAY };
        self.cfg.save();
        self.layout();
    }

    /// Jump to the first setting whose name has the typed text in it.
    fn find(&mut self) {
        let Some(q) = self.ask(" Find a setting: ", "").map(|q| q.to_lowercase()).filter(|q| !q.is_empty()) else { return };
        let Some(d) = self.device() else { return };
        let has = |s: &str| s.to_lowercase().contains(&q);
        let hit = d.props.iter().enumerate().find_map(|(pi, p)| {
            if has(&p.label) || has(&p.name) {
                return Some((pi, 0));
            }
            p.elems.iter().position(|e| has(&e.label) || has(&e.name)).map(|ei| (pi, ei))
        });
        let Some((pi, ei)) = hit else {
            self.say(&format!("no setting of {} has {q} in its name", d.name), self.pal.grey);
            return;
        };
        let g = d.props[pi].group.clone();
        let group = d.groups().iter().position(|x| *x == g).unwrap_or(0);
        let row = d.props[..pi].iter().filter(|p| p.group == g).map(|p| p.elems.len()).sum::<usize>() + ei;
        self.group = group;
        self.row = row;
        self.focus = Focus::Props;
        self.view = View::Device;
    }

    fn save_driver_settings(&mut self) {
        let Some(d) = self.device() else { return };
        let name = d.name.clone();
        if d.prop("CONFIG_PROCESS").is_none() {
            self.say(&format!("{name} keeps no settings"), self.pal.grey);
            return;
        }
        self.pick("CONFIG_PROCESS", "CONFIG_SAVE");
        self.say(&format!("asked the driver to save the settings of {name}"), self.pal.ok);
    }

    /// The device on screen, as text for Claude.
    fn claude_context(&self) -> String {
        let mut s = format!("INDI server {} ({})\n", self.addr, self.link);
        let names: Vec<&str> = self.model.devices.iter().map(|d| d.name.as_str()).collect();
        s.push_str(&format!("Devices: {}\n", names.join(", ")));
        if let Some(d) = self.device() {
            s.push_str(&format!("\nOn screen: {} ({})\n", d.name, if d.connected() { "connected" } else { "not connected" }));
            if let Some(t) = &self.target {
                s.push_str(&format!("Last go-to target: {t}\n"));
            }
            if let Some((_, f)) = self.frames.get(&d.name) {
                let hfr = f.hfr.map(|h| format!(", median HFR {h:.2} px")).unwrap_or_default();
                s.push_str(&format!("Last picture: {}; {} stars found{hfr}\n", f.facts(), f.stars));
            }
            for p in &d.props {
                s.push_str(&format!("\n[{}] {} ({}) state {}, {}\n", p.group, p.label, p.name, p.state, p.perm));
                for e in &p.elems {
                    s.push_str(&format!("  {} ({}) = {}\n", e.label, e.name, e.value));
                }
            }
        }
        let start = self.model.messages.len().saturating_sub(40);
        s.push_str("\nRecent messages from the server:\n");
        for m in &self.model.messages[start..] {
            s.push_str(&format!("  {m}\n"));
        }
        s
    }

    fn claude(&mut self) {
        self.hide_frame();
        let ctx = self.claude_context();
        Crust::disable_modifier_keys();
        let started = crust::claude_session(
            "indi",
            "I am in indi, my terminal app for telescope gear through an INDI server. The file has the device on screen, its settings and the server's messages.",
            &ctx,
        );
        Crust::enable_key_release();
        self.layout();
        if !started {
            self.say("claude is not on the PATH", self.pal.alert);
        }
    }

    fn device_key(&mut self, k: &str) {
        match k {
            "UP" if self.dev > 0 => self.dev -= 1,
            "DOWN" if self.dev + 1 < self.model.devices.len() => self.dev += 1,
            "ENTER" => self.toggle_connection(),
            "RIGHT" => self.cycle(1),
            _ => return,
        }
        if k == "UP" || k == "DOWN" {
            self.group = 0;
            self.row = 0;
        }
    }

    fn prop_key(&mut self, k: &str) {
        let groups = self.groups().len();
        match k {
            "UP" => self.row = self.row.saturating_sub(1),
            "DOWN" => self.row += 1,
            "LEFT" if self.group > 0 => {
                self.group -= 1;
                self.row = 0;
            }
            "RIGHT" if self.group + 1 < groups => {
                self.group += 1;
                self.row = 0;
            }
            "ENTER" => self.edit(),
            "+" | "-" => self.nudge(if k == "+" { 1.0 } else { -1.0 }),
            _ => {}
        }
    }

    /// The selected writable property and element, or a note why not.
    fn writable(&mut self) -> Option<(Prop, usize)> {
        let (pi, ei) = self.selected()?;
        let p = self.device()?.props[pi].clone();
        if !p.writable() {
            self.say(&format!("{} is read only", p.label), self.pal.grey);
            return None;
        }
        Some((p, ei))
    }

    /// Move the selected number by one step of its own.
    fn nudge(&mut self, by: f64) {
        let Some((p, ei)) = self.writable() else { return };
        let e = &p.elems[ei];
        if p.kind != Kind::Number {
            return;
        }
        let step = if e.step > 0.0 { e.step } else { 1.0 };
        let mut v = parse_number(&e.value).unwrap_or(0.0) + by * step;
        if e.max > e.min {
            v = v.clamp(e.min, e.max);
        }
        self.set(&p.name, vec![(e.name.clone(), v.to_string())]);
    }

    /// Change the selected element: flip a switch, or ask for a value.
    fn edit(&mut self) {
        let Some((p, ei)) = self.writable() else { return };
        let e = p.elems[ei].clone();
        match p.kind {
            Kind::Switch => self.pick(&p.name, &e.name),
            Kind::Number => {
                let now = num_text(&p, &e.name).unwrap_or_default();
                let Some(t) = self.ask(&format!(" {}: ", e.label), &now) else { return };
                let Some(v) = parse_number(&t) else {
                    self.say(&format!("{t} is not a number"), self.pal.alert);
                    return;
                };
                if e.max > e.min && (v < e.min || v > e.max) {
                    let range = format!("{} to {}", show_number(e.min, &e.format), show_number(e.max, &e.format));
                    self.say(&format!("{} must be from {range}", e.label), self.pal.alert);
                    return;
                }
                self.set(&p.name, vec![(e.name, v.to_string())]);
            }
            Kind::Text => {
                let Some(t) = self.ask(&format!(" {}: ", e.label), &e.value) else { return };
                self.set(&p.name, vec![(e.name, t)]);
            }
            Kind::Light | Kind::Blob => {}
        }
    }

    fn ask(&mut self, prompt: &str, initial: &str) -> Option<String> {
        let t = self.foot.ask_or_cancel(prompt, initial);
        self.foot.prompt.clear();
        t.map(|t| t.trim().to_string())
    }

    fn yes(&mut self, prompt: &str) -> bool {
        self.ask(prompt, "").is_some_and(|a| a.eq_ignore_ascii_case("y"))
    }

    // ─────────────────────────── the camera ──────────────────────────

    fn camera_key(&mut self, k: &str) {
        let Some(dev) = self.device().map(|d| d.name.clone()) else { return };
        match k {
            " " if self.exposing() => {
                self.looping = None;
                self.pick("CCD_ABORT_EXPOSURE", "ABORT");
                self.say("stopped the exposure", self.pal.busy);
            }
            " " => self.expose(&dev),
            "e" => {
                let now = self.cfg.exposure.to_string();
                let Some(t) = self.ask(" Exposure in seconds: ", &now) else { return };
                match t.parse::<f64>() {
                    Ok(v) if v > 0.0 => self.set_exposure(v),
                    _ => self.say(&format!("{t} is not a number of seconds"), self.pal.alert),
                }
            }
            "+" => self.set_exposure(self.cfg.exposure * 2.0),
            "-" => self.set_exposure(self.cfg.exposure / 2.0),
            "l" if self.looping.as_deref() == Some(dev.as_str()) => {
                self.looping = None;
                self.say("the picture now taking is the last", self.pal.grey);
            }
            "l" => {
                self.looping = Some(dev.clone());
                if !self.exposing() {
                    self.expose(&dev);
                }
            }
            "s" => self.save_frame(&dev),
            "a" => {
                self.autosave = !self.autosave;
                let t = if self.autosave {
                    format!("each picture is saved in {}", self.cfg.pictures_dir().display())
                } else {
                    "pictures are saved only with s".to_string()
                };
                self.say(&t, self.pal.ok);
            }
            "z" => self.zoom = !self.zoom,
            "t" => {
                let Some(p) = self.device().and_then(|d| d.prop("CCD_TEMPERATURE")) else {
                    return self.say("this camera has no cooler", self.pal.grey);
                };
                let now = p.num("CCD_TEMPERATURE_VALUE").map(|v| format!("{v:.0}")).unwrap_or_default();
                let Some(t) = self.ask(" Cool the sensor to (°C): ", &now) else { return };
                match parse_number(&t) {
                    Some(v) => self.set("CCD_TEMPERATURE", vec![("CCD_TEMPERATURE_VALUE".into(), v.to_string())]),
                    None => self.say(&format!("{t} is not a temperature"), self.pal.alert),
                }
            }
            "g" => {
                let Some((prop, elem)) = self.gain_elem() else {
                    return self.say("this camera has no gain setting", self.pal.grey);
                };
                let now = self.device().and_then(|d| d.prop(&prop)?.num(&elem)).map(|v| format!("{v:.0}")).unwrap_or_default();
                let Some(t) = self.ask(" Gain: ", &now) else { return };
                match parse_number(&t) {
                    Some(v) => self.set(&prop, vec![(elem, v.to_string())]),
                    None => self.say(&format!("{t} is not a gain"), self.pal.alert),
                }
            }
            "b" => {
                let Some(p) = self.device().and_then(|d| d.prop("CCD_BINNING")) else {
                    return self.say("this camera does not bin", self.pal.grey);
                };
                let now = p.num("HOR_BIN").unwrap_or(1.0);
                let max = p.elem("HOR_BIN").map(|e| e.max).filter(|m| *m >= 1.0).unwrap_or(4.0);
                let to = if now + 1.0 > max { 1.0 } else { now + 1.0 };
                self.set("CCD_BINNING", vec![("HOR_BIN".into(), to.to_string()), ("VER_BIN".into(), to.to_string())]);
            }
            _ => {}
        }
    }

    /// A new exposure time, kept inside what the camera takes and saved.
    fn set_exposure(&mut self, v: f64) {
        let range = self.device().and_then(|d| d.prop("CCD_EXPOSURE")?.elem("CCD_EXPOSURE_VALUE").map(|e| (e.min, e.max)));
        let v = match range {
            Some((lo, hi)) if hi > lo => v.clamp(lo, hi),
            _ => v,
        };
        // Short times keep three figures, so halving 1 s gives 0.5, 0.25, 0.125.
        self.cfg.exposure = if v < 1.0 { format!("{v:.3}").parse().unwrap_or(v) } else { (v * 10.0).round() / 10.0 };
        self.cfg.save();
    }

    // ─────────────────────────── the focuser ─────────────────────────

    fn focus_pos(&self) -> Option<f64> {
        self.device()?.prop("ABS_FOCUS_POSITION")?.num("FOCUS_ABSOLUTE_POSITION")
    }

    fn focuser_key(&mut self, k: &str) {
        let steps = match k {
            "LEFT" => -10.0,
            "RIGHT" => 10.0,
            "S-LEFT" => -100.0,
            "S-RIGHT" => 100.0,
            "[" => -1.0,
            "]" => 1.0,
            "g" => {
                let now = self.focus_pos().map(|v| format!("{v:.0}")).unwrap_or_default();
                let Some(t) = self.ask(" Go to position: ", &now) else { return };
                match parse_number(&t) {
                    Some(v) => self.focus_to(v),
                    None => self.say(&format!("{t} is not a position"), self.pal.alert),
                }
                return;
            }
            "s" => return self.save_preset(),
            " " => return self.pick("FOCUS_ABORT_MOTION", "ABORT"),
            _ => {
                let slot = k.parse::<usize>().ok().filter(|n| (1..=9).contains(n));
                if let Some(n) = slot {
                    match self.cfg.presets[n - 1].clone() {
                        Some((pos, _)) => self.focus_to(pos),
                        None => self.say(&format!("slot {n} is empty; s saves the position in a slot"), self.pal.grey),
                    }
                }
                return;
            }
        };
        match self.focus_pos() {
            Some(pos) => self.focus_to(pos + steps),
            None => {
                // Only relative moves: say which way, then how far.
                self.pick("FOCUS_MOTION", if steps < 0.0 { "FOCUS_INWARD" } else { "FOCUS_OUTWARD" });
                self.set("REL_FOCUS_POSITION", vec![("FOCUS_RELATIVE_POSITION".into(), steps.abs().to_string())]);
            }
        }
    }

    fn save_preset(&mut self) {
        let Some(pos) = self.focus_pos() else {
            self.say("this focuser does not tell its position", self.pal.grey);
            return;
        };
        let Some(t) = self.ask(&format!(" Save {pos:.0} in slot 1 to 9, with a name (1 Visual): "), "") else { return };
        let (n, name) = t.split_once(' ').unwrap_or((&t, ""));
        let Some(n) = n.parse::<usize>().ok().filter(|n| (1..=9).contains(n)) else {
            self.say("start with a slot from 1 to 9", self.pal.alert);
            return;
        };
        let name = if name.trim().is_empty() { format!("slot {n}") } else { name.trim().to_string() };
        self.cfg.presets[n - 1] = Some((pos, name.clone()));
        self.cfg.save();
        self.say(&format!("saved {pos:.0} as {name}; {n} goes back to it"), self.pal.ok);
    }

    /// Move the focuser to a position, kept inside its range.
    fn focus_to(&mut self, v: f64) {
        let Some(p) = self.device().and_then(|d| d.prop("ABS_FOCUS_POSITION")) else { return };
        let Some(e) = p.elem("FOCUS_ABSOLUTE_POSITION") else { return };
        let v = if e.max > e.min { v.clamp(e.min, e.max) } else { v.max(0.0) };
        self.set("ABS_FOCUS_POSITION", vec![("FOCUS_ABSOLUTE_POSITION".into(), v.round().to_string())]);
    }

    // ─────────────────────────── the mount ───────────────────────────

    fn mount_key(&mut self, k: &str) {
        match k {
            "UP" => self.motion("TELESCOPE_MOTION_NS", "MOTION_NORTH"),
            "DOWN" => self.motion("TELESCOPE_MOTION_NS", "MOTION_SOUTH"),
            "LEFT" => self.motion("TELESCOPE_MOTION_WE", "MOTION_EAST"),
            "RIGHT" => self.motion("TELESCOPE_MOTION_WE", "MOTION_WEST"),
            " " => {
                self.stop_mount(self.dev);
            }
            "+" | "-" => self.speed(if k == "+" { 1 } else { -1 }),
            "t" => {
                let on = self.device().and_then(|d| d.prop("TELESCOPE_TRACK_STATE")).is_some_and(|p| p.on("TRACK_ON"));
                self.pick("TELESCOPE_TRACK_STATE", if on { "TRACK_OFF" } else { "TRACK_ON" });
            }
            "p" => {
                let parked = self.device().and_then(|d| d.prop("TELESCOPE_PARK")).is_some_and(|p| p.on("PARK"));
                let what = if parked { "Unpark" } else { "Park" };
                if self.yes(&format!(" {what} the mount? y/n: ")) {
                    self.pick("TELESCOPE_PARK", if parked { "UNPARK" } else { "PARK" });
                }
            }
            "g" => self.goto(false),
            "y" => self.goto(true),
            "s" => self.sky_pick(),
            _ => {}
        }
    }

    /// Start moving one way. A held key repeats: the same motion is not
    /// sent again while the mount is on it, or within two seconds.
    fn motion(&mut self, prop: &str, elem: &str) {
        let Some(p) = self.device().and_then(|d| d.prop(prop)) else { return };
        let recent = self.held.as_ref().is_some_and(|(h, t)| h == elem && t.elapsed() < Duration::from_secs(2));
        if p.on(elem) || recent {
            return;
        }
        let values = p.elems.iter().map(|e| (e.name.clone(), if e.name == elem { "On" } else { "Off" }.to_string())).collect();
        self.set(prop, values);
        self.held = Some((elem.to_string(), Instant::now()));
    }

    fn speed(&mut self, by: i32) {
        let Some(p) = self.device().and_then(|d| d.prop("TELESCOPE_SLEW_RATE")) else { return };
        let now = p.elems.iter().position(|e| e.value == "On").unwrap_or(0) as i32;
        let to = (now + by).clamp(0, p.elems.len() as i32 - 1) as usize;
        let name = p.elems[to].name.clone();
        self.pick("TELESCOPE_SLEW_RATE", &name);
    }

    /// Ask for a target, by name or by RA and Dec, and go there, or
    /// sync on it (tell the mount it points there now).
    fn goto(&mut self, sync: bool) {
        if !self.device().is_some_and(|d| d.prop("EQUATORIAL_EOD_COORD").is_some()) {
            self.say("this mount takes no coordinates", self.pal.alert);
            return;
        }
        let prompt = if sync { " Sync on (a name, or RA Dec): " } else { " Go to (M42, Vega, Jupiter, or RA Dec as 5:35:17 -5:23:28): " };
        let Some(t) = self.ask(prompt, "").filter(|t| !t.is_empty()) else { return };
        let target = match parse_radec(&t) {
            Some((ra, dec)) => Target { name: format!("RA {} Dec {}", sexa(ra), sexa(dec)), ra, dec },
            None => match sky::find(&t, sky::now_jd(), self.site()) {
                Ok(target) => target,
                Err(e) => return self.say(&e, self.pal.alert),
            },
        };
        self.aim(target, sync);
    }

    /// The sky over the site as a map; the star picked is the target.
    fn sky_pick(&mut self) {
        self.hide_frame();
        let jd = sky::now_jd();
        let proj = match self.site() {
            Some((lat, lon)) => starmap::Projection::Horizon { lst_deg: starmap::lst_deg(jd, lon), lat_deg: lat },
            None => starmap::Projection::Hemisphere { north: true },
        };
        let picked = starmap::pick(starmap::View::new(proj), starmap::Opts::default(), "Pick a star for the mount");
        self.layout();
        self.render();
        if let Some(p) = picked {
            let (ra, dec) = sky::precess(p.star.ra, p.star.dec, jd);
            self.aim(Target { name: p.star.label(), ra: ra / 15.0, dec }, false);
        }
    }

    /// Send the mount to a target, after the checks a go-to needs: not
    /// at the Sun, and not below the horizon without a yes.
    fn aim(&mut self, t: Target, sync: bool) {
        if !sync {
            let jd = sky::now_jd();
            let sun = sky::sun_distance(t.ra, t.dec, jd);
            if sun < SUN_REFUSE {
                return self.say("indi does not point a telescope at the Sun", self.pal.alert);
            }
            if sun < SUN_WARN && !self.yes(&format!(" {} is {sun:.0}° from the Sun. Go anyway? y/n: ", t.name)) {
                return;
            }
            if let Some(site) = self.site() {
                let (alt, _) = sky::alt_az(t.ra, t.dec, jd, site);
                if alt < 0.0 && !self.yes(&format!(" {} is {:.0}° below the horizon. Go anyway? y/n: ", t.name, -alt)) {
                    return;
                }
            }
        }
        let on_set = self.device().and_then(|d| d.prop("ON_COORD_SET"));
        let mode = match (sync, on_set) {
            (true, Some(p)) if p.elem("SYNC").is_some() => Some("SYNC"),
            (true, _) => return self.say("this mount cannot sync", self.pal.alert),
            // Track the target once there, where the driver lets us say so.
            (false, Some(p)) if p.elem("TRACK").is_some() => Some("TRACK"),
            (false, Some(p)) if p.elem("SLEW").is_some() => Some("SLEW"),
            (false, _) => None,
        };
        if let Some(m) = mode {
            self.pick("ON_COORD_SET", m);
        }
        self.set("EQUATORIAL_EOD_COORD", vec![("RA".into(), t.ra.to_string()), ("DEC".into(), t.dec.to_string())]);
        let at = format!("RA {}  Dec {}", sexa(t.ra), sexa(t.dec));
        if sync {
            self.say(&format!("synced on {}  ({at})", t.name), self.pal.ok);
        } else {
            self.say(&format!("going to {}  ({at})", t.name), self.pal.busy);
            self.target = Some(t.name);
        }
    }

    /// Stop a mount: abort, and turn off any motion still on. True when
    /// something was sent.
    fn stop_mount(&mut self, i: usize) -> bool {
        self.held = None;
        let Some(d) = self.model.devices.get(i) else { return false };
        let mut out = String::new();
        if let Some(p) = d.prop("TELESCOPE_ABORT_MOTION") {
            out.push_str(&proto::new_vector(&d.name, p, &[("ABORT".into(), "On".into())]));
        }
        for name in ["TELESCOPE_MOTION_NS", "TELESCOPE_MOTION_WE"] {
            if let Some(p) = d.prop(name) {
                let off: Vec<(String, String)> = p.elems.iter().map(|e| (e.name.clone(), "Off".into())).collect();
                out.push_str(&proto::new_vector(&d.name, p, &off));
            }
        }
        if out.is_empty() {
            return false;
        }
        self.send(out);
        true
    }

    /// Before quitting: a mount left moving keeps moving after the
    /// program ends, and a server indi started ends with it.
    fn quit(&mut self) {
        self.hide_frame();
        let mut sent = false;
        for i in 0..self.model.devices.len() {
            let d = &self.model.devices[i];
            let moving = ["TELESCOPE_MOTION_NS", "TELESCOPE_MOTION_WE"]
                .iter()
                .any(|n| d.prop(n).is_some_and(|p| p.which_on().is_some()));
            if moving || (self.held.is_some() && i == self.dev) {
                sent |= self.stop_mount(i);
            }
        }
        if sent && self.server.is_some() {
            // Let the stop reach the driver before the server goes.
            std::thread::sleep(Duration::from_millis(300));
        }
        self.stop_server();
    }

    fn run(&mut self) {
        self.render();
        loop {
            // With a wake the wait has no end: a key or the server ends it.
            let wait = if self.wake.is_some() { 86_400_000 } else { 1000 };
            let (key, state) = match Input::event_ms(wait) {
                Some((k, s)) => (Some(k), s),
                None => (None, KeyState::Pressed),
            };
            if key.as_deref() == Some("WAKE") {
                if let Some(w) = self.wake.as_mut() {
                    w.drain();
                }
            }
            let mut dirty = false;
            while let Ok(ev) = self.rx.try_recv() {
                dirty |= self.take(ev);
            }
            match key.as_deref() {
                None | Some("WAKE") if !dirty => continue,
                None | Some("WAKE") => {}
                Some("RESIZE") => self.layout(),
                Some(k) if matches!(state, KeyState::Released) => {
                    if !self.release(k) && !dirty {
                        continue;
                    }
                }
                Some(k) => {
                    if self.key(k) {
                        break;
                    }
                }
            }
            self.render();
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{HELP}");
        return;
    }
    if args.iter().any(|a| a == "-v" || a == "--version") {
        println!("indi {VERSION}");
        return;
    }
    let mut addr = args.first().cloned().unwrap_or_else(|| "localhost".into());
    if !addr.contains(':') {
        addr.push_str(":7624");
    }
    Crust::init();
    Crust::set_app_identity("indi");
    // Where the terminal reports key releases, a mount moves while an
    // arrow is held and stops when it is let go.
    Crust::enable_key_release();
    App::new(addr).run();
    Crust::disable_modifier_keys();
    Crust::cleanup();
    Crust::clear_screen();
}
