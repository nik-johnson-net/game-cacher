//! Valve KeyValues: the text form (PICS app info) and the binary form (PICS
//! package info). Values are kept as strings; lookups are case-insensitive like
//! SteamKit2's `KeyValue` indexer.

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, PartialEq)]
pub enum Kv {
    Str(String),
    Obj(Vec<(String, Kv)>),
}

impl Kv {
    /// First child named `key` (case-insensitive).
    pub fn get(&self, key: &str) -> Option<&Kv> {
        match self {
            Kv::Obj(children) => children
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v),
            Kv::Str(_) => None,
        }
    }

    pub fn path(&self, keys: &[&str]) -> Option<&Kv> {
        keys.iter().try_fold(self, |kv, k| kv.get(k))
    }

    /// String value at `keys`, if present and a leaf.
    pub fn str_at(&self, keys: &[&str]) -> Option<&str> {
        self.path(keys).and_then(Kv::as_str)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Kv::Str(s) => Some(s),
            Kv::Obj(_) => None,
        }
    }

    pub fn children(&self) -> &[(String, Kv)] {
        match self {
            Kv::Obj(c) => c,
            Kv::Str(_) => &[],
        }
    }
}

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

/// Parses text KeyValues and returns the root `(name, value)`.
pub fn parse_text(input: &str) -> Result<(String, Kv)> {
    let mut lexer = Lexer {
        chars: input.trim_end_matches('\0').chars().peekable(),
    };
    let name = match lexer.next_token()? {
        Some(Token::Str(s)) => s,
        other => bail!("expected root key, got {other:?}"),
    };
    match lexer.next_token()? {
        Some(Token::Open) => Ok((name, Kv::Obj(parse_object(&mut lexer)?))),
        Some(Token::Str(s)) => Ok((name, Kv::Str(s))),
        other => bail!("expected root value, got {other:?}"),
    }
}

fn parse_object(lexer: &mut Lexer) -> Result<Vec<(String, Kv)>> {
    let mut out = Vec::new();
    loop {
        let key = match lexer.next_token()? {
            Some(Token::Str(k)) => k,
            Some(Token::Close) => return Ok(out),
            other => bail!("expected key or '}}', got {other:?}"),
        };
        let value = match lexer.next_token()? {
            Some(Token::Str(v)) => Kv::Str(v),
            Some(Token::Open) => Kv::Obj(parse_object(lexer)?),
            other => bail!("expected value for {key:?}, got {other:?}"),
        };
        out.push((key, value));
    }
}

#[derive(Debug, PartialEq)]
enum Token {
    Str(String),
    Open,
    Close,
}

struct Lexer<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl Lexer<'_> {
    fn next_token(&mut self) -> Result<Option<Token>> {
        loop {
            let Some(&c) = self.chars.peek() else {
                return Ok(None);
            };
            match c {
                c if c.is_whitespace() => {
                    self.chars.next();
                }
                '/' => {
                    // `//` line comment
                    self.chars.next();
                    if self.chars.peek() != Some(&'/') {
                        bail!("unexpected '/'");
                    }
                    for c in self.chars.by_ref() {
                        if c == '\n' {
                            break;
                        }
                    }
                }
                '[' => {
                    // conditional like [$WIN32]; ignored
                    for c in self.chars.by_ref() {
                        if c == ']' {
                            break;
                        }
                    }
                }
                '{' => {
                    self.chars.next();
                    return Ok(Some(Token::Open));
                }
                '}' => {
                    self.chars.next();
                    return Ok(Some(Token::Close));
                }
                '"' => {
                    self.chars.next();
                    return self.quoted().map(|s| Some(Token::Str(s)));
                }
                _ => return Ok(Some(Token::Str(self.bare()))),
            }
        }
    }

    fn quoted(&mut self) -> Result<String> {
        let mut s = String::new();
        loop {
            match self.chars.next() {
                None => bail!("unterminated string"),
                Some('"') => return Ok(s),
                Some('\\') => match self.chars.next() {
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some('r') => s.push('\r'),
                    Some(c) => s.push(c),
                    None => bail!("unterminated escape"),
                },
                Some(c) => s.push(c),
            }
        }
    }

    fn bare(&mut self) -> String {
        let mut s = String::new();
        while let Some(&c) = self.chars.peek() {
            if c.is_whitespace() || matches!(c, '{' | '}' | '"') {
                break;
            }
            s.push(c);
            self.chars.next();
        }
        s
    }
}

// ---------------------------------------------------------------------------
// Binary
// ---------------------------------------------------------------------------

const T_OBJECT: u8 = 0;
const T_STRING: u8 = 1;
const T_INT32: u8 = 2;
const T_FLOAT32: u8 = 3;
const T_POINTER: u8 = 4;
const T_COLOR: u8 = 6;
const T_UINT64: u8 = 7;
const T_END: u8 = 8;
const T_INT64: u8 = 10;
const T_END_ALT: u8 = 11;

/// Parses binary KeyValues and returns the first top-level `(name, value)`.
pub fn parse_binary(data: &[u8]) -> Result<(String, Kv)> {
    let mut r = Reader { data, pos: 0 };
    let mut entries = read_entries(&mut r, 0)?;
    if entries.is_empty() {
        bail!("empty binary KeyValues");
    }
    Ok(entries.swap_remove(0))
}

fn read_entries(r: &mut Reader, depth: usize) -> Result<Vec<(String, Kv)>> {
    if depth > 64 {
        bail!("binary KeyValues nested too deeply");
    }
    let mut out = Vec::new();
    loop {
        let Some(ty) = r.u8_opt() else {
            return Ok(out); // tolerate a missing trailing end marker
        };
        if ty == T_END || ty == T_END_ALT {
            return Ok(out);
        }
        let name = r.cstr()?;
        let value = match ty {
            T_OBJECT => Kv::Obj(read_entries(r, depth + 1)?),
            T_STRING => Kv::Str(r.cstr()?),
            T_INT32 => Kv::Str(i32::from_le_bytes(r.take()?).to_string()),
            T_FLOAT32 => Kv::Str(f32::from_le_bytes(r.take()?).to_string()),
            T_POINTER | T_COLOR => Kv::Str(u32::from_le_bytes(r.take()?).to_string()),
            T_UINT64 => Kv::Str(u64::from_le_bytes(r.take()?).to_string()),
            T_INT64 => Kv::Str(i64::from_le_bytes(r.take()?).to_string()),
            other => bail!("unsupported binary KeyValues type {other} for {name:?}"),
        };
        out.push((name, value));
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn u8_opt(&mut self) -> Option<u8> {
        let b = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        let bytes = self
            .data
            .get(self.pos..self.pos + N)
            .context("truncated binary KeyValues")?;
        self.pos += N;
        Ok(bytes.try_into().expect("slice length checked"))
    }

    fn cstr(&mut self) -> Result<String> {
        let rest = &self.data[self.pos..];
        let end = rest
            .iter()
            .position(|&b| b == 0)
            .context("unterminated string in binary KeyValues")?;
        self.pos += end + 1;
        Ok(String::from_utf8_lossy(&rest[..end]).into_owned())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Encodes `kv` as binary KeyValues (test helper; ints are written as strings).
    pub fn encode_binary(name: &str, kv: &Kv, out: &mut Vec<u8>) {
        match kv {
            Kv::Str(s) => {
                out.push(T_STRING);
                out.extend_from_slice(name.as_bytes());
                out.push(0);
                out.extend_from_slice(s.as_bytes());
                out.push(0);
            }
            Kv::Obj(children) => {
                out.push(T_OBJECT);
                out.extend_from_slice(name.as_bytes());
                out.push(0);
                for (k, v) in children {
                    encode_binary(k, v, out);
                }
                out.push(T_END);
            }
        }
    }

    #[test]
    fn text_parsing() {
        let text = r#"
"appinfo"
{
    // comment
    "appid"     "730"
    "common" { "name" "Counter-Strike \"2\"" "Type" "Game" }
    "depots"
    {
        "731" { "manifests" { "public" { "gid" "123" } } }
        bare value
    }
}
"#;
        let (name, kv) = parse_text(&format!("{text}\0")).unwrap();
        assert_eq!(name, "appinfo");
        assert_eq!(kv.str_at(&["appid"]), Some("730"));
        assert_eq!(kv.str_at(&["COMMON", "name"]), Some("Counter-Strike \"2\""));
        assert_eq!(kv.str_at(&["common", "type"]), Some("Game"));
        assert_eq!(
            kv.str_at(&["depots", "731", "manifests", "public", "gid"]),
            Some("123")
        );
        assert_eq!(kv.str_at(&["depots", "bare"]), Some("value"));
        assert!(kv.get("missing").is_none());
        assert_eq!(kv.get("depots").unwrap().children().len(), 2);
    }

    #[test]
    fn text_errors() {
        assert!(parse_text(r#""a" { "b" "c" "#).is_err());
        assert!(parse_text(r#""a" { "b" }"#).is_err());
        assert!(parse_text(r#""a" { "b" "unterminated }"#).is_err());
        assert!(parse_text("").is_err());
    }

    #[test]
    fn binary_parsing() {
        let mut data = vec![T_OBJECT];
        data.extend_from_slice(b"42\0");
        data.push(T_INT32);
        data.extend_from_slice(b"packageid\0");
        data.extend_from_slice(&42i32.to_le_bytes());
        data.push(T_OBJECT);
        data.extend_from_slice(b"appids\0");
        data.push(T_INT32);
        data.extend_from_slice(b"0\0");
        data.extend_from_slice(&730i32.to_le_bytes());
        data.push(T_UINT64);
        data.extend_from_slice(b"1\0");
        data.extend_from_slice(&u64::MAX.to_le_bytes());
        data.push(T_END);
        data.push(T_STRING);
        data.extend_from_slice(b"name\0pkg\0");
        data.push(T_END);
        data.push(T_END);

        let (name, kv) = parse_binary(&data).unwrap();
        assert_eq!(name, "42");
        assert_eq!(kv.str_at(&["packageid"]), Some("42"));
        assert_eq!(kv.str_at(&["appids", "0"]), Some("730"));
        assert_eq!(kv.str_at(&["appids", "1"]), Some(&*u64::MAX.to_string()));
        assert_eq!(kv.str_at(&["name"]), Some("pkg"));
    }

    #[test]
    fn binary_roundtrip_and_errors() {
        let kv = Kv::Obj(vec![("a".into(), Kv::Str("b".into()))]);
        let mut data = Vec::new();
        encode_binary("root", &kv, &mut data);
        assert_eq!(parse_binary(&data).unwrap(), ("root".into(), kv));

        assert!(parse_binary(&[]).is_err());
        assert!(parse_binary(&[T_INT32, b'x', 0, 1]).is_err()); // truncated int
        assert!(parse_binary(&[T_STRING, b'x']).is_err()); // unterminated name
        assert!(parse_binary(&[5, b'x', 0]).is_err()); // wide string unsupported
    }
}
