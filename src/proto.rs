//! The INDI protocol: XML over TCP, version 1.7.
//!
//! The server sends `def…Vector` once for each property a driver has,
//! `set…Vector` each time values change, `delProperty` when one goes,
//! and `message` for news. The client sends `getProperties` once, then
//! `new…Vector` to change something. BLOBs (camera pictures) stay off,
//! the server's default, until `enable_blob` asks for a camera's.

/// One XML element with what it holds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub text: String,
    pub children: Vec<Element>,
}

impl Element {
    pub fn attr(&self, key: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

/// Reads the server's stream a chunk at a time and gives back each
/// top-level element once its end has arrived.
#[derive(Default)]
pub struct Parser {
    buf: Vec<u8>,
    stack: Vec<Element>,
}

impl Parser {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Element> {
        self.buf.extend_from_slice(bytes);
        let mut done = Vec::new();
        let mut pos = 0;
        loop {
            let Some(lt) = self.buf[pos..].iter().position(|&b| b == b'<').map(|i| pos + i) else {
                // A picture's base64 text can be tens of megabytes: it goes
                // into its element as it comes, not into the buffer until
                // its end tag. Base64 has no entities to cut in half.
                if let Some(top) = self.stack.last_mut().filter(|t| t.name == "oneBLOB") {
                    top.text.push_str(&String::from_utf8_lossy(&self.buf[pos..]));
                    pos = self.buf.len();
                }
                break;
            };
            let Some(gt) = tag_end(&self.buf, lt) else { break };
            if let Some(top) = self.stack.last_mut() {
                top.text.push_str(&unescape(&String::from_utf8_lossy(&self.buf[pos..lt])));
            }
            let tag = String::from_utf8_lossy(&self.buf[lt + 1..gt]).into_owned();
            pos = gt + 1;
            if tag.starts_with('?') || tag.starts_with('!') {
                continue;
            }
            if let Some(name) = tag.strip_prefix('/') {
                // An end tag closes the open element, whatever it names.
                let _ = name;
                if let Some(el) = self.stack.pop() {
                    self.close(el, &mut done);
                }
                continue;
            }
            let selfclosing = tag.ends_with('/');
            let el = start_tag(tag.trim_end_matches('/'));
            if selfclosing {
                self.close(el, &mut done);
            } else {
                self.stack.push(el);
            }
        }
        // Text between tags waits until the next tag comes, so an entity
        // split between two reads is never cut in half.
        self.buf.drain(..pos);
        done
    }

    fn close(&mut self, el: Element, done: &mut Vec<Element>) {
        match self.stack.last_mut() {
            Some(parent) => parent.children.push(el),
            None => done.push(el),
        }
    }
}

/// Where the tag that opens at `lt` ends: the first `>` outside quotes.
fn tag_end(buf: &[u8], lt: usize) -> Option<usize> {
    let mut quote = 0u8;
    for (i, &b) in buf.iter().enumerate().skip(lt + 1) {
        match b {
            b'"' | b'\'' if quote == 0 => quote = b,
            b if b == quote => quote = 0,
            b'>' if quote == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

fn start_tag(tag: &str) -> Element {
    let tag = tag.trim();
    let end = tag.find(char::is_whitespace).unwrap_or(tag.len());
    let mut el = Element { name: tag[..end].to_string(), ..Default::default() };
    let mut rest = &tag[end..];
    while let Some(eq) = rest.find('=') {
        let key = rest[..eq].trim().to_string();
        let after = rest[eq + 1..].trim_start();
        let Some(q) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else { break };
        let Some(close) = after[1..].find(q) else { break };
        el.attrs.push((key, unescape(&after[1..1 + close])));
        rest = &after[close + 2..];
    }
    el
}

pub fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let Some(semi) = rest.find(';').filter(|&i| i < 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..semi];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

// ─────────────────────────── the devices ────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Number,
    Text,
    Switch,
    Light,
    Blob,
}

impl Kind {
    fn from_tag(tag: &str) -> Option<Kind> {
        Some(match tag {
            "Number" => Kind::Number,
            "Text" => Kind::Text,
            "Switch" => Kind::Switch,
            "Light" => Kind::Light,
            "BLOB" => Kind::Blob,
            _ => return None,
        })
    }

    fn tag(self) -> &'static str {
        match self {
            Kind::Number => "Number",
            Kind::Text => "Text",
            Kind::Switch => "Switch",
            Kind::Light => "Light",
            Kind::Blob => "BLOB",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Elem {
    pub name: String,
    pub label: String,
    pub value: String,
    pub format: String,
    pub min: f64,
    pub max: f64,
    /// How far one nudge moves a number; 0 where the driver gives none.
    pub step: f64,
}

#[derive(Debug, Clone)]
pub struct Prop {
    pub name: String,
    pub label: String,
    pub group: String,
    pub kind: Kind,
    /// Idle, Ok, Busy or Alert.
    pub state: String,
    /// ro, wo or rw.
    pub perm: String,
    /// OneOfMany, AtMostOne or AnyOfMany, for switches.
    pub rule: String,
    pub elems: Vec<Elem>,
}

impl Prop {
    pub fn elem(&self, name: &str) -> Option<&Elem> {
        self.elems.iter().find(|e| e.name == name)
    }

    /// The value of a number element.
    pub fn num(&self, name: &str) -> Option<f64> {
        self.elem(name).and_then(|e| parse_number(&e.value))
    }

    /// Whether a switch element is on.
    pub fn on(&self, name: &str) -> bool {
        self.elem(name).is_some_and(|e| e.value == "On")
    }

    /// The first switch element that is on.
    pub fn which_on(&self) -> Option<&Elem> {
        self.elems.iter().find(|e| e.value == "On")
    }

    pub fn writable(&self) -> bool {
        self.perm != "ro" && self.kind != Kind::Light
    }
}

#[derive(Debug, Clone, Default)]
pub struct Device {
    pub name: String,
    pub props: Vec<Prop>,
}

impl Device {
    pub fn prop(&self, name: &str) -> Option<&Prop> {
        self.props.iter().find(|p| p.name == name)
    }

    pub fn connected(&self) -> bool {
        self.prop("CONNECTION").is_some_and(|p| p.on("CONNECT"))
    }

    pub fn is_focuser(&self) -> bool {
        self.prop("ABS_FOCUS_POSITION").is_some() || self.prop("REL_FOCUS_POSITION").is_some()
    }

    pub fn is_camera(&self) -> bool {
        self.prop("CCD_EXPOSURE").is_some()
    }

    pub fn is_mount(&self) -> bool {
        self.prop("EQUATORIAL_EOD_COORD").is_some()
    }

    /// The property groups in the order the driver defined them.
    pub fn groups(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for p in &self.props {
            if !out.contains(&p.group) {
                out.push(p.group.clone());
            }
        }
        out
    }
}

/// Everything the server has told us.
#[derive(Default)]
pub struct Model {
    pub devices: Vec<Device>,
    /// The newest last: "device: text".
    pub messages: Vec<String>,
}

impl Model {
    pub fn device(&self, name: &str) -> Option<&Device> {
        self.devices.iter().find(|d| d.name == name)
    }

    fn device_mut(&mut self, name: &str) -> &mut Device {
        let i = match self.devices.iter().position(|d| d.name == name) {
            Some(i) => i,
            None => {
                self.devices.push(Device { name: name.to_string(), props: Vec::new() });
                self.devices.len() - 1
            }
        };
        &mut self.devices[i]
    }

    /// Take in one element from the server.
    pub fn apply(&mut self, el: &Element) {
        let device = el.attr("device").unwrap_or("").to_string();
        if let Some(msg) = el.attr("message").filter(|m| !m.is_empty()) {
            // "2026-11-18T20:00:00" shows as its time of day.
            let time = el.attr("timestamp").and_then(|t| t.get(11..19)).unwrap_or("");
            self.note(&device, msg, time);
        }
        let n = &el.name;
        if let Some(kind) = n.strip_prefix("def").and_then(|r| r.strip_suffix("Vector")).and_then(Kind::from_tag) {
            let Some(name) = el.attr("name") else { return };
            let prop = Prop {
                name: name.to_string(),
                label: el.attr("label").filter(|l| !l.is_empty()).unwrap_or(name).to_string(),
                group: el.attr("group").filter(|g| !g.is_empty()).unwrap_or("Main").to_string(),
                kind,
                state: el.attr("state").unwrap_or("Idle").to_string(),
                perm: el.attr("perm").unwrap_or(if kind == Kind::Light { "ro" } else { "rw" }).to_string(),
                rule: el.attr("rule").unwrap_or("").to_string(),
                elems: el.children.iter().filter_map(|c| {
                    let name = c.attr("name")?.to_string();
                    Some(Elem {
                        label: c.attr("label").filter(|l| !l.is_empty()).unwrap_or(&name).to_string(),
                        value: c.text.trim().to_string(),
                        format: c.attr("format").unwrap_or("").to_string(),
                        min: c.attr("min").and_then(parse_number).unwrap_or(0.0),
                        max: c.attr("max").and_then(parse_number).unwrap_or(0.0),
                        step: c.attr("step").and_then(parse_number).unwrap_or(0.0),
                        name,
                    })
                }).collect(),
            };
            let d = self.device_mut(&device);
            match d.props.iter_mut().find(|p| p.name == prop.name) {
                Some(p) => *p = prop,
                None => d.props.push(prop),
            }
        } else if n.starts_with("set") && n.ends_with("Vector") {
            let Some(name) = el.attr("name") else { return };
            let Some(d) = self.devices.iter_mut().find(|d| d.name == device) else { return };
            let Some(p) = d.props.iter_mut().find(|p| p.name == name) else { return };
            if let Some(s) = el.attr("state") {
                p.state = s.to_string();
            }
            for c in &el.children {
                let Some(e) = c.attr("name").and_then(|n| p.elems.iter_mut().find(|e| e.name == n)) else { continue };
                e.value = c.text.trim().to_string();
                if let Some(v) = c.attr("min").and_then(parse_number) {
                    e.min = v;
                }
                if let Some(v) = c.attr("max").and_then(parse_number) {
                    e.max = v;
                }
            }
        } else if n == "delProperty" {
            match el.attr("name") {
                Some(name) => {
                    if let Some(d) = self.devices.iter_mut().find(|d| d.name == device) {
                        d.props.retain(|p| p.name != name);
                    }
                }
                None => self.devices.retain(|d| d.name != device),
            }
        }
    }

    fn note(&mut self, device: &str, msg: &str, time: &str) {
        let line = if device.is_empty() { msg.to_string() } else { format!("{device}: {msg}") };
        let line = if time.is_empty() { line } else { format!("{time}  {line}") };
        self.messages.push(line);
        if self.messages.len() > 200 {
            self.messages.remove(0);
        }
    }
}

// ─────────────────────────── to the server ──────────────────────────

pub fn get_properties() -> String {
    "<getProperties version=\"1.7\"/>\n".to_string()
}

/// Ask the server to send a camera's pictures (its main chip, `CCD1`)
/// along with everything else.
pub fn enable_blob(device: &str) -> String {
    format!("<enableBLOB device=\"{}\" name=\"CCD1\">Also</enableBLOB>\n", escape(device))
}

/// A `new…Vector` that sets the named elements; elements not named keep
/// their values. A number or text property is always sent whole, since
/// some drivers read a missing element as zero (a go-to needs RA and Dec
/// together).
pub fn new_vector(device: &str, p: &Prop, values: &[(String, String)]) -> String {
    let tag = p.kind.tag();
    let mut out = format!("<new{tag}Vector device=\"{}\" name=\"{}\">\n", escape(device), escape(&p.name));
    let whole = p.kind != Kind::Switch;
    for e in &p.elems {
        let v = match values.iter().find(|(n, _)| *n == e.name) {
            Some((_, v)) => v.clone(),
            None if whole => e.value.clone(),
            None => continue,
        };
        out.push_str(&format!("  <one{tag} name=\"{}\">{}</one{tag}>\n", escape(&e.name), escape(&v)));
    }
    out.push_str(&format!("</new{tag}Vector>\n"));
    out
}

/// The switch values that pick element `chosen`, by the property's rule:
/// one of many turns the rest off, at most one turns an "on" switch off
/// again, any of many flips just that one.
pub fn pick(p: &Prop, chosen: &str) -> Vec<(String, String)> {
    let was = p.on(chosen);
    match p.rule.as_str() {
        "AnyOfMany" => vec![(chosen.to_string(), if was { "Off" } else { "On" }.to_string())],
        "AtMostOne" if was => vec![(chosen.to_string(), "Off".to_string())],
        _ => p.elems.iter()
            .map(|e| (e.name.clone(), if e.name == chosen { "On" } else { "Off" }.to_string()))
            .collect(),
    }
}

// ─────────────────────────── numbers ────────────────────────────────

/// A number as INDI writes it: "5000", "-1.5e3", or sexagesimal
/// "5:35:17.3", "-5 23 28" (hours or degrees, minutes, seconds).
pub fn parse_number(s: &str) -> Option<f64> {
    let s = s.trim();
    if let Ok(v) = s.parse::<f64>() {
        return Some(v);
    }
    let neg = s.starts_with('-');
    let parts: Vec<&str> = s.trim_start_matches(['-', '+']).split([':', ' ', ';']).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    let mut v = 0.0;
    for (i, p) in parts.iter().enumerate() {
        v += p.parse::<f64>().ok()? / 60f64.powi(i as i32);
    }
    Some(if neg { -v } else { v })
}

/// A number shown by its printf-style INDI format: "%g", "%.2f", "%5.0f",
/// or "%010.6m" for sexagesimal (6 gives seconds, 8 and 9 tenths and
/// hundredths of them, 5 tenths of a minute, 3 whole minutes).
pub fn show_number(v: f64, format: &str) -> String {
    let f = format.trim().trim_start_matches('%');
    let conv = f.chars().last().unwrap_or('g');
    let spec = f.trim_end_matches(conv).trim_start_matches(['0', '-', '+', ' ', '#']);
    // "%.f" is a precision of 0, as in C.
    let prec: Option<usize> = spec.split('.').nth(1).map(|p| p.parse().unwrap_or(0));
    match conv {
        'm' => sexagesimal(v, prec.unwrap_or(3)),
        'f' | 'F' => format!("{:.*}", prec.unwrap_or(6), v),
        'e' | 'E' => format!("{:.*e}", prec.unwrap_or(6), v),
        'd' | 'i' | 'u' => format!("{}", v.round() as i64),
        _ => {
            if v.fract() == 0.0 && v.abs() < 1e15 {
                format!("{}", v as i64)
            } else {
                let s = format!("{:.6}", v);
                s.trim_end_matches('0').trim_end_matches('.').to_string()
            }
        }
    }
}

fn sexagesimal(v: f64, prec: usize) -> String {
    // How many parts the whole number is cut into, as INDI's own
    // numberFormat does it.
    let base: f64 = match prec {
        9 => 360000.0,
        8 => 36000.0,
        6 => 3600.0,
        5 => 600.0,
        _ => 60.0,
    };
    let sign = if v < 0.0 { "-" } else { "" };
    let n = (v.abs() * base).round() as u64;
    let b = base as u64;
    let (d, rest) = (n / b, n % b);
    match prec {
        9 => format!("{sign}{d}:{:02}:{:02}.{:02}", rest / 6000, rest % 6000 / 100, rest % 100),
        8 => format!("{sign}{d}:{:02}:{:02}.{}", rest / 600, rest % 600 / 10, rest % 10),
        6 => format!("{sign}{d}:{:02}:{:02}", rest / 60, rest % 60),
        5 => format!("{sign}{d}:{:02}.{}", rest / 10, rest % 10),
        _ => format!("{sign}{d}:{:02}", rest),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FOCUSER: &str = r#"<defNumberVector device="ZWO EAF" name="ABS_FOCUS_POSITION" label="Absolute Position" group="Main Control" state="Ok" perm="rw" timeout="60">
    <defNumber name="FOCUS_ABSOLUTE_POSITION" label="Steps" format="%.f" min="0" max="60000" step="100">
5000
    </defNumber>
</defNumberVector>
<defSwitchVector device="ZWO EAF" name="CONNECTION" label="Connection" group="Main Control" state="Idle" perm="rw" rule="OneOfMany" timeout="60">
    <defSwitch name="CONNECT" label="Connect">Off</defSwitch>
    <defSwitch name="DISCONNECT" label="Disconnect">On</defSwitch>
</defSwitchVector>
"#;

    fn model(xml: &str) -> Model {
        let mut m = Model::default();
        for el in Parser::default().feed(xml.as_bytes()) {
            m.apply(&el);
        }
        m
    }

    #[test]
    fn a_definition_makes_a_device_with_its_properties() {
        let m = model(FOCUSER);
        let d = m.device("ZWO EAF").unwrap();
        assert!(d.is_focuser() && !d.is_mount() && !d.connected());
        let p = d.prop("ABS_FOCUS_POSITION").unwrap();
        assert_eq!(p.num("FOCUS_ABSOLUTE_POSITION"), Some(5000.0));
        assert_eq!(p.elems[0].max, 60000.0);
        assert_eq!(d.groups(), vec!["Main Control"]);
    }

    #[test]
    fn a_stream_cut_anywhere_gives_the_same_elements() {
        let whole = Parser::default().feed(FOCUSER.as_bytes());
        for cut in [1, 7, 50, 200, 333] {
            let mut p = Parser::default();
            let mut got = p.feed(&FOCUSER.as_bytes()[..cut]);
            got.extend(p.feed(&FOCUSER.as_bytes()[cut..]));
            assert_eq!(got, whole, "cut at {cut}");
        }
        assert_eq!(whole.len(), 2);
        let blob = br#"<setBLOBVector device="CCD" name="CCD1"><oneBLOB name="CCD1" size="5" format=".fits">SGVs
bG8s</oneBLOB></setBLOBVector>"#;
        let mut p = Parser::default();
        let mut got = Vec::new();
        for part in blob.chunks(9) {
            got.extend(p.feed(part));
        }
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].children[0].text, "SGVs\nbG8s");
    }

    #[test]
    fn set_and_delete_change_the_model() {
        let mut m = model(FOCUSER);
        let xml = r#"<setSwitchVector device="ZWO EAF" name="CONNECTION" state="Ok" message="Focuser is online &amp; ready">
<oneSwitch name="CONNECT">On</oneSwitch><oneSwitch name="DISCONNECT">Off</oneSwitch></setSwitchVector>
<message device="ZWO EAF" timestamp="2026-11-18T20:00:00" message="a &lt;note&gt; with &quot;quotes&quot; > here"/>"#;
        for el in Parser::default().feed(xml.as_bytes()) {
            m.apply(&el);
        }
        assert!(m.device("ZWO EAF").unwrap().connected());
        assert_eq!(m.messages, vec![
            "ZWO EAF: Focuser is online & ready".to_string(),
            "20:00:00  ZWO EAF: a <note> with \"quotes\" > here".to_string(),
        ]);
        for el in Parser::default().feed(br#"<delProperty device="ZWO EAF" name="CONNECTION"/>"#) {
            m.apply(&el);
        }
        assert!(m.device("ZWO EAF").unwrap().prop("CONNECTION").is_none());
        for el in Parser::default().feed(br#"<delProperty device="ZWO EAF"/>"#) {
            m.apply(&el);
        }
        assert!(m.devices.is_empty());
    }

    #[test]
    fn a_change_is_sent_whole_for_numbers_and_by_rule_for_switches() {
        let m = model(FOCUSER);
        let d = m.device("ZWO EAF").unwrap();
        let abs = d.prop("ABS_FOCUS_POSITION").unwrap();
        assert_eq!(
            new_vector(&d.name, abs, &[("FOCUS_ABSOLUTE_POSITION".into(), "5100".into())]),
            "<newNumberVector device=\"ZWO EAF\" name=\"ABS_FOCUS_POSITION\">\n  \
             <oneNumber name=\"FOCUS_ABSOLUTE_POSITION\">5100</oneNumber>\n</newNumberVector>\n"
        );
        let conn = d.prop("CONNECTION").unwrap();
        assert_eq!(pick(conn, "CONNECT"), vec![
            ("CONNECT".to_string(), "On".to_string()),
            ("DISCONNECT".to_string(), "Off".to_string()),
        ]);
        let mut any = conn.clone();
        any.rule = "AnyOfMany".into();
        assert_eq!(pick(&any, "DISCONNECT"), vec![("DISCONNECT".to_string(), "Off".to_string())]);
    }

    #[test]
    fn numbers_read_and_show_as_indi_writes_them() {
        assert_eq!(parse_number("5:35:17"), Some(5.0 + 35.0 / 60.0 + 17.0 / 3600.0));
        assert_eq!(parse_number("-5 23 28").map(|v| (v * 3600.0).round()), Some(-19408.0));
        assert_eq!(parse_number(" 42.5 "), Some(42.5));
        assert_eq!(parse_number("abc"), None);
        let ra = 5.0 + 35.0 / 60.0 + 17.3 / 3600.0;
        assert_eq!(show_number(ra, "%010.6m"), "5:35:17");
        assert_eq!(show_number(ra, "%010.9m"), "5:35:17.30");
        assert_eq!(show_number(-5.391, "%9.6m"), "-5:23:28");
        assert_eq!(show_number(-5.5, "%6.3m"), "-5:30");
        assert_eq!(show_number(5000.0, "%.f"), "5000");
        assert_eq!(show_number(4.25, "%.1f"), "4.2");
        assert_eq!(show_number(4.0, "%g"), "4");
        assert_eq!(show_number(0.125, "%g"), "0.125");
    }
}
