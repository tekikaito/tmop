//! Pure parsers for the text formats the Linux and macOS inventory back ends read:
//! `KEY=value` files (os-release), XDG `.desktop` entries, XML property lists, human sizes.
//!
//! Compiled and tested on every host; only the readers in the per-OS modules are cfg-gated.

/// `KEY=value` lines with shell-style quoting (os-release, `/etc/default/*`). Comments and lines
/// without `=` are skipped; later keys do not replace earlier ones' order.
pub fn parse_env_file(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        let value = if let Some(inner) = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            let mut s = String::with_capacity(inner.len());
            let mut chars = inner.chars();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => s.extend(chars.next()),
                    c => s.push(c),
                }
            }
            s
        } else if let Some(inner) = v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
            inner.to_string()
        } else {
            v.to_string()
        };
        out.push((k.trim().to_string(), value));
    }
    out
}

/// The value of `key` in [`parse_env_file`] output, empty when missing.
pub fn env_value(pairs: &[(String, String)], key: &str) -> String {
    pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).unwrap_or_default()
}

/// The `[Desktop Entry]` group of an XDG `.desktop` file (unlocalized keys only).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DesktopEntry {
    pub kind: String,
    pub name: String,
    pub comment: String,
    pub exec: String,
    pub try_exec: String,
    pub hidden: bool,
    pub no_display: bool,
    /// `X-GNOME-Autostart-enabled`, true when absent.
    pub autostart_enabled: bool,
    pub only_show_in: Vec<String>,
    pub not_show_in: Vec<String>,
}

/// Parses a `.desktop` file; None when it has no `[Desktop Entry]` group.
pub fn parse_desktop_entry(text: &str) -> Option<DesktopEntry> {
    let mut e = DesktopEntry { autostart_enabled: true, ..Default::default() };
    let mut seen = false;
    let mut inside = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            inside = line == "[Desktop Entry]";
            seen |= inside;
            continue;
        }
        if !inside || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = desktop_unescape(v.trim());
        let yes = v.eq_ignore_ascii_case("true");
        let list = |v: &str| v.split(';').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect::<Vec<_>>();
        match k.trim() {
            "Type" => e.kind = v,
            "Name" => e.name = v,
            "Comment" => e.comment = v,
            "Exec" => e.exec = v,
            "TryExec" => e.try_exec = v,
            "Hidden" => e.hidden = yes,
            "NoDisplay" => e.no_display = yes,
            "X-GNOME-Autostart-enabled" => e.autostart_enabled = !v.eq_ignore_ascii_case("false"),
            "OnlyShowIn" => e.only_show_in = list(&v),
            "NotShowIn" => e.not_show_in = list(&v),
            _ => {}
        }
    }
    seen.then_some(e)
}

/// `\s`, `\n`, `\t`, `\r` and `\\` in a `.desktop` string value.
fn desktop_unescape(v: &str) -> String {
    let mut s = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            s.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => s.push(' '),
            Some('n') => s.push('\n'),
            Some('t') => s.push('\t'),
            Some('r') => s.push('\r'),
            Some(o) => s.push(o),
            None => s.push('\\'),
        }
    }
    s
}

/// True when an entry is meant for one of the desktops in `$XDG_CURRENT_DESKTOP` (colon-separated;
/// an empty value matches everything).
pub fn shows_in(e: &DesktopEntry, current: &str) -> bool {
    let current: Vec<&str> = current.split(':').map(str::trim).filter(|s| !s.is_empty()).collect();
    if current.is_empty() {
        return true;
    }
    let hit = |list: &[String]| list.iter().any(|d| current.iter().any(|c| c.eq_ignore_ascii_case(d)));
    (e.only_show_in.is_empty() || hit(&e.only_show_in)) && !hit(&e.not_show_in)
}

/// An `Exec` line without its field codes (`%U`, `%f`, ...); `%%` becomes `%`.
pub fn exec_command(exec: &str) -> String {
    let mut out = String::with_capacity(exec.len());
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            if chars.next() == Some('%') {
                out.push('%');
            }
        } else {
            out.push(c);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A property-list value. Dates and data keep no payload beyond the date text.
#[derive(Clone, Debug, PartialEq)]
pub enum Plist {
    Str(String),
    Int(i64),
    Real(f64),
    Bool(bool),
    Date(String),
    Data,
    Array(Vec<Plist>),
    Dict(Vec<(String, Plist)>),
}

impl Plist {
    /// A dictionary member.
    pub fn get(&self, key: &str) -> Option<&Plist> {
        match self {
            Plist::Dict(d) => d.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn str(&self) -> Option<&str> {
        match self {
            Plist::Str(s) => Some(s),
            _ => None,
        }
    }

    /// A non-empty trimmed string member.
    pub fn text(&self, key: &str) -> Option<String> {
        self.get(key).and_then(Plist::str).map(str::trim).filter(|s| !s.is_empty()).map(String::from)
    }

    pub fn int(&self) -> Option<i64> {
        match self {
            Plist::Int(i) => Some(*i),
            Plist::Real(r) => Some(*r as i64),
            _ => None,
        }
    }

    pub fn bool(&self) -> Option<bool> {
        match self {
            Plist::Bool(b) => Some(*b),
            Plist::Int(i) => Some(*i != 0),
            _ => None,
        }
    }

    pub fn array(&self) -> &[Plist] {
        match self {
            Plist::Array(a) => a,
            _ => &[],
        }
    }
}

enum Tok<'a> {
    Open(&'a str),
    Close(&'a str),
    Empty(&'a str),
    Text(String),
}

struct Xml<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> Xml<'a> {
    fn skip_to(&mut self, end: &str) {
        self.i = match self.s[self.i..].find(end) {
            Some(p) => self.i + p + end.len(),
            None => self.s.len(),
        };
    }

    fn next(&mut self) -> Option<Tok<'a>> {
        loop {
            let rest = self.s.get(self.i..).filter(|r| !r.is_empty())?;
            if rest.starts_with("<!--") {
                self.skip_to("-->");
            } else if let Some(cdata) = rest.strip_prefix("<![CDATA[") {
                let end = cdata.find("]]>").unwrap_or(cdata.len());
                let text = cdata[..end].to_string();
                self.skip_to("]]>");
                return Some(Tok::Text(text));
            } else if rest.starts_with("<?") {
                self.skip_to("?>");
            } else if rest.starts_with("<!") {
                self.skip_to(">");
            } else if rest.starts_with('<') {
                let end = rest.find('>')?;
                let inner = &rest[1..end];
                self.i += end + 1;
                let name = |s: &'a str| s.split_whitespace().next().unwrap_or("");
                return Some(if let Some(n) = inner.strip_prefix('/') {
                    Tok::Close(name(n))
                } else if let Some(n) = inner.strip_suffix('/') {
                    Tok::Empty(name(n))
                } else {
                    Tok::Open(name(inner))
                });
            } else {
                let end = rest.find('<').unwrap_or(rest.len());
                self.i += end;
                return Some(Tok::Text(xml_unescape(&rest[..end])));
            }
        }
    }

    /// The next token that is not whitespace-only text.
    fn next_tag(&mut self) -> Option<Tok<'a>> {
        loop {
            match self.next()? {
                Tok::Text(t) if t.trim().is_empty() => {}
                t => return Some(t),
            }
        }
    }

    /// Text up to `</name>`.
    fn text_until(&mut self, name: &str) -> Option<String> {
        let mut s = String::new();
        loop {
            match self.next()? {
                Tok::Text(t) => s.push_str(&t),
                Tok::Close(n) if n == name => return Some(s),
                _ => return None,
            }
        }
    }

    fn value(&mut self, tok: Tok<'a>) -> Option<Plist> {
        Some(match tok {
            Tok::Empty("true") => Plist::Bool(true),
            Tok::Empty("false") => Plist::Bool(false),
            Tok::Empty("string") => Plist::Str(String::new()),
            Tok::Empty("dict") => Plist::Dict(Vec::new()),
            Tok::Empty("array") => Plist::Array(Vec::new()),
            Tok::Empty("data") => Plist::Data,
            Tok::Open(n @ ("true" | "false")) => {
                self.text_until(n)?;
                Plist::Bool(n == "true")
            }
            Tok::Open("string") => Plist::Str(self.text_until("string")?),
            Tok::Open("integer") => {
                let t = self.text_until("integer")?;
                let t = t.trim();
                // ioreg writes negative values as their unsigned 64-bit pattern.
                Plist::Int(t.parse::<i64>().ok().or_else(|| t.parse::<u64>().ok().map(|u| u as i64)).unwrap_or(0))
            }
            Tok::Open("real") => Plist::Real(self.text_until("real")?.trim().parse().unwrap_or(0.0)),
            Tok::Open("date") => Plist::Date(self.text_until("date")?.trim().to_string()),
            Tok::Open("data") => {
                self.text_until("data")?;
                Plist::Data
            }
            Tok::Open("array") => {
                let mut items = Vec::new();
                loop {
                    match self.next_tag()? {
                        Tok::Close("array") => break,
                        t => items.push(self.value(t)?),
                    }
                }
                Plist::Array(items)
            }
            Tok::Open("dict") => {
                let mut items = Vec::new();
                loop {
                    match self.next_tag()? {
                        Tok::Close("dict") => break,
                        Tok::Open("key") => {
                            let k = self.text_until("key")?;
                            let t = self.next_tag()?;
                            items.push((k, self.value(t)?));
                        }
                        _ => return None,
                    }
                }
                Plist::Dict(items)
            }
            _ => return None,
        })
    }
}

/// `&lt;`, `&gt;`, `&amp;`, `&quot;`, `&apos;` and numeric references.
fn xml_unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        let Some(end) = rest.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..end];
        let c = match ent {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .or_else(|| ent.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
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

/// Parses an XML property list (`<plist>` with one root value). None for anything else,
/// including binary plists (`bplist00`).
pub fn parse_plist_xml(text: &str) -> Option<Plist> {
    let mut x = Xml { s: text, i: 0 };
    loop {
        match x.next_tag()? {
            Tok::Open("plist") => break,
            Tok::Text(_) => return None,
            _ => {}
        }
    }
    let t = x.next_tag()?;
    x.value(t)
}

/// Reads a property list file: XML directly, binary ones through `plutil -convert xml1`.
#[cfg(target_os = "macos")]
pub fn read_plist(path: &std::path::Path) -> Option<Plist> {
    let bytes = std::fs::read(path).ok()?;
    if !bytes.starts_with(b"bplist") {
        return parse_plist_xml(&String::from_utf8_lossy(&bytes));
    }
    let p = path.to_str()?;
    let xml = crate::platform::run_capture("plutil", &["-convert", "xml1", "-o", "-", p], std::time::Duration::from_secs(5)).ok()?;
    parse_plist_xml(&xml)
}

/// `"1.2 GB"`, `"512 kB"`, `"3 MiB"`, `"800 bytes"` to bytes (SI units decimal, IEC binary).
pub fn parse_human_size(s: &str) -> Option<u64> {
    let s = s.trim().replace('\u{a0}', " ");
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',')).unwrap_or(s.len());
    let num: f64 = s[..split].replace(',', ".").parse().ok()?;
    let unit = s[split..].trim().to_ascii_lowercase();
    let mul = match unit.as_str() {
        "" | "b" | "byte" | "bytes" => 1.0,
        "kb" | "k" => 1e3,
        "mb" | "m" => 1e6,
        "gb" | "g" => 1e9,
        "tb" | "t" => 1e12,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0f64.powi(4),
        _ => return None,
    };
    Some((num * mul).round() as u64)
}

/// The holder in a copyright line: `"Copyright © 2009-2024 Google LLC. All rights reserved."`
/// gives `"Google LLC"`. Empty when nothing name-like is left.
pub fn copyright_holder(s: &str) -> String {
    let mut rest = s.trim();
    loop {
        let before = rest.len();
        for p in ["Copyright", "copyright", "COPYRIGHT", "\u{a9}", "(c)", "(C)", "by "] {
            if let Some(r) = rest.strip_prefix(p) {
                rest = r.trim_start();
            }
        }
        rest = rest.trim_start_matches(|c: char| c.is_ascii_digit() || matches!(c, '-' | '\u{2013}' | ',' | ' ' | ':'));
        if rest.len() == before {
            break;
        }
    }
    for cut in ["All rights reserved", "all rights reserved", "All Rights Reserved", ";", "\n"] {
        if let Some(p) = rest.find(cut) {
            rest = &rest[..p];
        }
    }
    let mut name = rest.trim().trim_end_matches([',', ' ']).to_string();
    let keeps_dot = ["Inc.", "Ltd.", "Co.", "Corp."].iter().any(|s| name.ends_with(s));
    if name.ends_with('.') && !keeps_dot {
        name.pop();
    }
    if name.chars().count() > 60 || !name.chars().any(char::is_alphabetic) {
        return String::new();
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    const OS_RELEASE: &str = r#"PRETTY_NAME="Ubuntu 24.04.5 LTS"
NAME="Ubuntu"
VERSION_ID="24.04"
VERSION="24.04.5 LTS (Noble Numbat)"
# a comment
ID=ubuntu
ID_LIKE=debian
HOME_URL='https://www.ubuntu.com/'
ESCAPED="say \"hi\""
"#;

    #[test]
    fn os_release() {
        let kv = parse_env_file(OS_RELEASE);
        assert_eq!(env_value(&kv, "PRETTY_NAME"), "Ubuntu 24.04.5 LTS");
        assert_eq!(env_value(&kv, "VERSION"), "24.04.5 LTS (Noble Numbat)");
        assert_eq!(env_value(&kv, "ID"), "ubuntu");
        assert_eq!(env_value(&kv, "HOME_URL"), "https://www.ubuntu.com/");
        assert_eq!(env_value(&kv, "ESCAPED"), "say \"hi\"");
        assert_eq!(env_value(&kv, "MISSING"), "");
    }

    const DESKTOP: &str = "[Desktop Entry]
Type=Application
Name=Evolution Alarm Notify
Name[de]=Evolution Erinnerungen
Comment=Calendar\\sevent notifications
Exec=/usr/libexec/evolution-data-server/evolution-alarm-notify %U --flag=100%%
OnlyShowIn=GNOME;Unity;
X-GNOME-Autostart-enabled=false

[Desktop Action New]
Name=Should not win
Exec=nope
";

    #[test]
    fn desktop_entries() {
        let e = parse_desktop_entry(DESKTOP).unwrap();
        assert_eq!(e.kind, "Application");
        assert_eq!(e.name, "Evolution Alarm Notify");
        assert_eq!(e.comment, "Calendar event notifications");
        assert!(!e.autostart_enabled && !e.hidden);
        assert_eq!(e.only_show_in, ["GNOME", "Unity"]);
        assert_eq!(exec_command(&e.exec), "/usr/libexec/evolution-data-server/evolution-alarm-notify --flag=100%");
        assert!(shows_in(&e, "ubuntu:GNOME"));
        assert!(!shows_in(&e, "KDE"));
        assert!(shows_in(&e, ""));
        let not = DesktopEntry { not_show_in: vec!["KDE".into()], ..Default::default() };
        assert!(!shows_in(&not, "KDE") && shows_in(&not, "XFCE"));
        assert!(parse_desktop_entry("[Other]\nName=x\n").is_none());
        let hidden = parse_desktop_entry("[Desktop Entry]\nName=H\nHidden=true\n").unwrap();
        assert!(hidden.hidden && hidden.autostart_enabled);
    }

    const PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>com.example.agent</string>
	<!-- a comment -->
	<key>ProgramArguments</key>
	<array>
		<string>/Applications/Example.app/Contents/MacOS/agent</string>
		<string>--mode=a&amp;b</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>Disabled</key>
	<false/>
	<key>Interval</key>
	<integer>3600</integer>
	<key>Amperage</key>
	<integer>18446744073709550616</integer>
	<key>Ratio</key>
	<real>0.5</real>
	<key>Empty</key>
	<string/>
	<key>When</key>
	<date>2024-01-31T10:00:00Z</date>
	<key>Blob</key>
	<data>AAEC</data>
	<key>Nested</key>
	<dict><key>A</key><array/></dict>
</dict>
</plist>
"#;

    #[test]
    fn xml_plists() {
        let p = parse_plist_xml(PLIST).unwrap();
        assert_eq!(p.text("Label").as_deref(), Some("com.example.agent"));
        let args: Vec<&str> = p.get("ProgramArguments").unwrap().array().iter().filter_map(Plist::str).collect();
        assert_eq!(args, ["/Applications/Example.app/Contents/MacOS/agent", "--mode=a&b"]);
        assert_eq!(p.get("RunAtLoad").and_then(Plist::bool), Some(true));
        assert_eq!(p.get("Disabled").and_then(Plist::bool), Some(false));
        assert_eq!(p.get("Interval").and_then(Plist::int), Some(3600));
        assert_eq!(p.get("Amperage").and_then(Plist::int), Some(-1000));
        assert_eq!(p.get("Ratio"), Some(&Plist::Real(0.5)));
        assert_eq!(p.get("Empty").and_then(Plist::str), Some(""));
        assert_eq!(p.text("Empty"), None);
        assert_eq!(p.get("When"), Some(&Plist::Date("2024-01-31T10:00:00Z".into())));
        assert_eq!(p.get("Blob"), Some(&Plist::Data));
        assert_eq!(p.get("Nested").and_then(|n| n.get("A")), Some(&Plist::Array(vec![])));
        assert!(parse_plist_xml("bplist00\u{0}\u{1}").is_none());
        assert!(parse_plist_xml("<plist><dict><key>x</key></dict></plist>").is_none());
        assert_eq!(xml_unescape("a &lt;b&gt; &#65;&#x42; & c"), "a <b> AB & c");
    }

    #[test]
    fn human_sizes() {
        assert_eq!(parse_human_size("1.2 GB"), Some(1_200_000_000));
        assert_eq!(parse_human_size("512\u{a0}kB"), Some(512_000));
        assert_eq!(parse_human_size("3 MiB"), Some(3 * 1024 * 1024));
        assert_eq!(parse_human_size("800 bytes"), Some(800));
        assert_eq!(parse_human_size("1,5 GB"), Some(1_500_000_000));
        assert_eq!(parse_human_size("lots"), None);
        assert_eq!(parse_human_size("4 parsecs"), None);
    }

    #[test]
    fn copyright_holders() {
        assert_eq!(copyright_holder("Copyright \u{a9} 2009-2024 Google LLC. All rights reserved."), "Google LLC");
        assert_eq!(copyright_holder("\u{a9} 2024 Apple Inc."), "Apple Inc.");
        assert_eq!(copyright_holder("Copyright (c) 2023, Mozilla Corporation"), "Mozilla Corporation");
        assert_eq!(copyright_holder("2019"), "");
        assert_eq!(copyright_holder(""), "");
    }
}
