//! A small property-list reader (XML and binary `bplist00`), enough for launchd job files and
//! bundle `Info.plist`s on macOS without a crate or a `plutil` call per file. Pure, so it is
//! tested on every host. Dates and data are kept only as markers.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

/// A parsed property-list value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Real(f64),
    Str(String),
    /// `<data>` or binary data; the bytes are not kept.
    Data,
    /// `<date>` or a binary date; the time is not kept.
    Date,
    Array(Vec<Value>),
    /// Entries in file order.
    Dict(Vec<(String, Value)>),
}

impl Value {
    /// The value of `key` when this is a dictionary.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(d) => d.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }
}

/// Parses a binary (`bplist00`) or XML property list.
pub fn parse(bytes: &[u8]) -> Option<Value> {
    if bytes.starts_with(b"bplist0") {
        Binary::new(bytes)?.object(None, 0)
    } else {
        parse_xml(std::str::from_utf8(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes)).ok()?)
    }
}

/// Deeper nesting than this is refused (and stops reference cycles in binary lists).
const MAX_DEPTH: usize = 64;

fn be(s: &[u8]) -> u64 {
    s.iter().fold(0, |a, &x| (a << 8) | x as u64)
}

struct Binary<'a> {
    b: &'a [u8],
    off_size: usize,
    ref_size: usize,
    count: usize,
    top: usize,
    table: usize,
}

impl<'a> Binary<'a> {
    fn new(b: &'a [u8]) -> Option<Self> {
        if b.len() < 8 + 32 {
            return None;
        }
        let t = &b[b.len() - 32..];
        let (off_size, ref_size) = (t[6] as usize, t[7] as usize);
        let (count, top, table) = (be(&t[8..16]) as usize, be(&t[16..24]) as usize, be(&t[24..32]) as usize);
        let ok = (1..=8).contains(&off_size) && (1..=8).contains(&ref_size) && top < count && count.checked_mul(off_size).and_then(|n| n.checked_add(table)).is_some_and(|end| end <= b.len() - 32);
        ok.then_some(Self { b, off_size, ref_size, count, top, table })
    }

    fn offset(&self, i: usize) -> Option<usize> {
        if i >= self.count {
            return None;
        }
        let at = self.table + i * self.off_size;
        Some(be(self.b.get(at..at + self.off_size)?) as usize)
    }

    /// The length nibble of the marker at `at`, or the int object after it; and where the payload starts.
    fn len(&self, at: usize, nibble: u8) -> Option<(usize, usize)> {
        if nibble != 0xF {
            return Some((nibble as usize, at + 1));
        }
        let m = *self.b.get(at + 1)?;
        if m >> 4 != 0x1 {
            return None;
        }
        let n = 1usize << (m & 0xF);
        Some((be(self.b.get(at + 2..at + 2 + n)?) as usize, at + 2 + n))
    }

    fn refs(&self, start: usize, n: usize) -> Option<Vec<usize>> {
        let bytes = self.b.get(start..start.checked_add(n.checked_mul(self.ref_size)?)?)?;
        Some(bytes.chunks(self.ref_size).map(|c| be(c) as usize).collect())
    }

    /// Object `i` (the top object when None).
    fn object(&self, i: Option<usize>, depth: usize) -> Option<Value> {
        if depth > MAX_DEPTH {
            return None;
        }
        let at = self.offset(i.unwrap_or(self.top))?;
        let m = *self.b.get(at)?;
        let (kind, nib) = (m >> 4, m & 0xF);
        Some(match kind {
            0x0 => match m {
                0x08 => Value::Bool(false),
                0x09 => Value::Bool(true),
                _ => Value::Data,
            },
            0x1 => {
                let n = 1usize << nib;
                let raw = self.b.get(at + 1..at + 1 + n)?;
                // 16-byte ints carry the value in their low 8 bytes.
                Value::Int(be(&raw[n.saturating_sub(8)..]) as i64)
            }
            0x2 => match nib {
                2 => Value::Real(f32::from_bits(be(self.b.get(at + 1..at + 5)?) as u32) as f64),
                3 => Value::Real(f64::from_bits(be(self.b.get(at + 1..at + 9)?))),
                _ => return None,
            },
            0x3 => Value::Date,
            0x4 => Value::Data,
            0x5 | 0x7 => {
                let (n, start) = self.len(at, nib)?;
                Value::Str(String::from_utf8_lossy(self.b.get(start..start.checked_add(n)?)?).into_owned())
            }
            0x6 => {
                let (n, start) = self.len(at, nib)?;
                let raw = self.b.get(start..start.checked_add(n.checked_mul(2)?)?)?;
                Value::Str(String::from_utf16_lossy(&raw.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect::<Vec<_>>()))
            }
            0x8 => Value::Int(be(self.b.get(at + 1..at + 2 + nib as usize)?) as i64),
            0xA => {
                let (n, start) = self.len(at, nib)?;
                Value::Array(self.refs(start, n)?.into_iter().map(|r| self.object(Some(r), depth + 1)).collect::<Option<_>>()?)
            }
            0xD => {
                let (n, start) = self.len(at, nib)?;
                let keys = self.refs(start, n)?;
                let vals = self.refs(start + n * self.ref_size, n)?;
                let mut out = Vec::with_capacity(n);
                for (k, v) in keys.into_iter().zip(vals) {
                    let Value::Str(k) = self.object(Some(k), depth + 1)? else { return None };
                    out.push((k, self.object(Some(v), depth + 1)?));
                }
                Value::Dict(out)
            }
            _ => return None,
        })
    }
}

#[derive(Debug, PartialEq)]
enum Tok<'a> {
    Open(&'a str),
    Close(&'a str),
    Empty(&'a str),
    Text(String),
}

/// Tags and text of an XML document; declarations, the doctype and comments are skipped.
fn tokens(s: &str) -> Option<Vec<Tok<'_>>> {
    let mut out = Vec::new();
    let mut rest = s;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            push_text(&mut out, rest);
            break;
        };
        push_text(&mut out, &rest[..lt]);
        rest = &rest[lt..];
        if let Some(r) = rest.strip_prefix("<!--") {
            rest = &r[r.find("-->")? + 3..];
            continue;
        }
        if let Some(r) = rest.strip_prefix("<![CDATA[") {
            let end = r.find("]]>")?;
            out.push(Tok::Text(r[..end].to_string()));
            rest = &r[end + 3..];
            continue;
        }
        let gt = rest.find('>')?;
        let tag = &rest[1..gt];
        rest = &rest[gt + 1..];
        if tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }
        if let Some(t) = tag.strip_prefix('/') {
            out.push(Tok::Close(tag_name(t)));
        } else if let Some(t) = tag.strip_suffix('/') {
            out.push(Tok::Empty(tag_name(t)));
        } else {
            out.push(Tok::Open(tag_name(tag)));
        }
    }
    Some(out)
}

/// The element name of a tag's inside (`plist version="1.0"` -> `plist`).
fn tag_name(t: &str) -> &str {
    t.split_whitespace().next().unwrap_or("")
}

fn push_text(out: &mut Vec<Tok<'_>>, t: &str) {
    if !t.is_empty() {
        out.push(Tok::Text(unescape(t)));
    }
}

fn unescape(t: &str) -> String {
    if !t.contains('&') {
        return t.to_string();
    }
    let mut out = String::with_capacity(t.len());
    let mut rest = t;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..end];
        let ch = match ent {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent.strip_prefix("#x").or_else(|| ent.strip_prefix("#X")).and_then(|h| u32::from_str_radix(h, 16).ok()).or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok())).and_then(char::from_u32),
        };
        match ch {
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

fn parse_xml(s: &str) -> Option<Value> {
    let toks = tokens(s)?;
    let mut i = 0;
    // Skip to the first value inside <plist> (or the first value at all).
    while i < toks.len() && !matches!(&toks[i], Tok::Open(n) | Tok::Empty(n) if *n != "plist") {
        i += 1;
    }
    xml_value(&toks, &mut i, 0)
}

/// Skips whitespace text between tags.
fn skip_text(toks: &[Tok<'_>], i: &mut usize) {
    while matches!(toks.get(*i), Some(Tok::Text(_))) {
        *i += 1;
    }
}

/// The text up to the closing `name` tag.
fn text_until(toks: &[Tok<'_>], i: &mut usize, name: &str) -> Option<String> {
    let mut s = String::new();
    loop {
        match toks.get(*i)? {
            Tok::Text(t) => s.push_str(t),
            Tok::Close(n) if *n == name => {
                *i += 1;
                return Some(s);
            }
            _ => return None,
        }
        *i += 1;
    }
}

fn xml_value(toks: &[Tok<'_>], i: &mut usize, depth: usize) -> Option<Value> {
    if depth > MAX_DEPTH {
        return None;
    }
    skip_text(toks, i);
    let tok = toks.get(*i)?;
    *i += 1;
    Some(match tok {
        Tok::Empty("true") => Value::Bool(true),
        Tok::Empty("false") => Value::Bool(false),
        Tok::Empty("string") | Tok::Empty("ustring") => Value::Str(String::new()),
        Tok::Empty("dict") => Value::Dict(Vec::new()),
        Tok::Empty("array") => Value::Array(Vec::new()),
        Tok::Empty("data") => Value::Data,
        Tok::Open(n @ ("true" | "false")) => {
            text_until(toks, i, n)?;
            Value::Bool(*n == "true")
        }
        Tok::Open(n @ ("string" | "ustring")) => Value::Str(text_until(toks, i, n)?),
        Tok::Open("integer") => {
            let t = text_until(toks, i, "integer")?;
            let t = t.trim();
            Value::Int(t.strip_prefix("0x").map(|h| i64::from_str_radix(h, 16).ok()).unwrap_or_else(|| t.parse().ok())?)
        }
        Tok::Open("real") => Value::Real(text_until(toks, i, "real")?.trim().parse().ok()?),
        Tok::Open("date") => {
            text_until(toks, i, "date")?;
            Value::Date
        }
        Tok::Open("data") => {
            text_until(toks, i, "data")?;
            Value::Data
        }
        Tok::Open("array") => {
            let mut out = Vec::new();
            loop {
                skip_text(toks, i);
                if matches!(toks.get(*i)?, Tok::Close("array")) {
                    *i += 1;
                    break Value::Array(out);
                }
                out.push(xml_value(toks, i, depth + 1)?);
            }
        }
        Tok::Open("dict") => {
            let mut out = Vec::new();
            loop {
                skip_text(toks, i);
                match toks.get(*i)? {
                    Tok::Close("dict") => {
                        *i += 1;
                        break Value::Dict(out);
                    }
                    Tok::Open("key") => {
                        *i += 1;
                        let k = text_until(toks, i, "key")?;
                        out.push((k, xml_value(toks, i, depth + 1)?));
                    }
                    Tok::Empty("key") => {
                        *i += 1;
                        out.push((String::new(), xml_value(toks, i, depth + 1)?));
                    }
                    _ => return None,
                }
            }
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>com.example.agent</string>
	<!-- a comment -->
	<key>ProgramArguments</key>
	<array>
		<string>/usr/local/bin/agent</string>
		<string>--flag &amp; &lt;x&gt; &#x41;</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>Disabled</key>
	<false/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>Nice</key>
	<integer>-5</integer>
	<key>Empty</key>
	<string></string>
	<key>Blob</key>
	<data>AAEC</data>
</dict>
</plist>
"#;

    #[test]
    fn reads_xml() {
        let v = parse(XML.as_bytes()).expect("parses");
        assert_eq!(v.get("Label").and_then(Value::as_str), Some("com.example.agent"));
        let args = v.get("ProgramArguments").and_then(Value::as_array).unwrap();
        assert_eq!(args[0].as_str(), Some("/usr/local/bin/agent"));
        assert_eq!(args[1].as_str(), Some("--flag & <x> A"));
        assert_eq!(v.get("RunAtLoad").and_then(Value::as_bool), Some(true));
        assert_eq!(v.get("Disabled").and_then(Value::as_bool), Some(false));
        assert!(matches!(v.get("KeepAlive"), Some(Value::Dict(d)) if d.len() == 1));
        assert_eq!(v.get("Nice"), Some(&Value::Int(-5)));
        assert_eq!(v.get("Empty").and_then(Value::as_str), Some(""));
        assert_eq!(v.get("Blob"), Some(&Value::Data));
        assert_eq!(v.get("Missing"), None);
    }

    #[test]
    fn refuses_broken_xml() {
        assert_eq!(parse(b"<plist><dict><key>a</key></dict></plist>"), None);
        assert_eq!(parse(b"<plist><dict><key>a</key><string>x</dict>"), None);
        assert_eq!(parse(b"not a plist"), None);
    }

    /// Writes a binary plist of `objs` (top object 0) the way CFBinaryPList lays it out.
    fn bplist(objs: &[Vec<u8>]) -> Vec<u8> {
        let mut b = b"bplist00".to_vec();
        let mut offsets = Vec::new();
        for o in objs {
            offsets.push(b.len());
            b.extend_from_slice(o);
        }
        let table = b.len();
        for o in &offsets {
            b.extend_from_slice(&(*o as u16).to_be_bytes());
        }
        b.extend_from_slice(&[0, 0, 0, 0, 0, 0, 2, 1]);
        b.extend_from_slice(&(objs.len() as u64).to_be_bytes());
        b.extend_from_slice(&0u64.to_be_bytes());
        b.extend_from_slice(&(table as u64).to_be_bytes());
        b
    }

    fn ascii(s: &str) -> Vec<u8> {
        let mut v = if s.len() < 15 { vec![0x50 | s.len() as u8] } else { vec![0x5F, 0x10, s.len() as u8] };
        v.extend_from_slice(s.as_bytes());
        v
    }

    #[test]
    fn reads_binary() {
        // 0: dict {1: 3, 2: 4, 5: 6, 7: 8}
        let utf16: Vec<u8> = std::iter::once(0x62u8).chain("h\u{e9}".encode_utf16().flat_map(|u| u.to_be_bytes())).collect();
        let objs = vec![
            vec![0xD4, 1, 2, 5, 7, 3, 4, 6, 8],
            ascii("Label"),
            ascii("RunAtLoad"),
            ascii("com.apple.something.longer.than.fifteen"),
            vec![0x09],
            ascii("ProgramArguments"),
            vec![0xA2, 9, 10],
            ascii("Nice"),
            vec![0x11, 0xFF, 0xFE],
            ascii("/usr/libexec/x"),
            utf16,
        ];
        let v = parse(&bplist(&objs)).expect("parses");
        assert_eq!(v.get("Label").and_then(Value::as_str), Some("com.apple.something.longer.than.fifteen"));
        assert_eq!(v.get("RunAtLoad").and_then(Value::as_bool), Some(true));
        let args = v.get("ProgramArguments").and_then(Value::as_array).unwrap();
        assert_eq!((args[0].as_str(), args[1].as_str()), (Some("/usr/libexec/x"), Some("h\u{e9}")));
        assert_eq!(v.get("Nice"), Some(&Value::Int(0xFFFE)));
    }

    #[test]
    fn refuses_broken_binary() {
        // A dict that contains itself, a truncated file and an out-of-range reference.
        assert_eq!(parse(&bplist(&[vec![0xD1, 1, 0], ascii("k")])), None);
        let ok = bplist(&[vec![0xA1, 1], ascii("x")]);
        assert!(parse(&ok).is_some());
        assert_eq!(parse(&ok[..ok.len() - 3]), None);
        assert_eq!(parse(&bplist(&[vec![0xA1, 7], ascii("x")])), None);
    }
}
