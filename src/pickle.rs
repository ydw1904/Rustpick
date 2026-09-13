//! Minimal Python `pickle` reader: just enough opcodes to walk the structure
//! aiohttp's `CookieJar.save` pickles into upstream's `cookies.jar`
//! (`{domain: {path: SimpleCookie{name: Morsel}}}`) and dig out cookie values.
//! Unknown classes become opaque dicts instead of being instantiated.

// some variants are only built, never inspected - they still shape the pickle stream
#[allow(dead_code)]
#[derive(Debug, Clone)]
enum Py {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    List(Vec<Py>),
    /// A dict, or an opaque instance of a Python class we can't build
    /// (BUILD state and dict items both collect into `items`).
    Dict { class: Option<String>, items: Vec<(Py, Py)> },
}

impl Py {
    fn as_str(&self) -> Option<&str> {
        match self {
            Py::Str(s) => Some(s),
            _ => None,
        }
    }
}

struct Unpickler<'a> {
    data: &'a [u8],
    pos: usize,
    memo: Vec<Py>,
    stack: Vec<Py>,
    marks: Vec<usize>,
}

type R<T> = Result<T, String>;

impl<'a> Unpickler<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0, memo: Vec::new(), stack: Vec::new(), marks: Vec::new() }
    }

    fn byte(&mut self) -> R<u8> {
        let b = *self.data.get(self.pos).ok_or("unexpected end of pickle")?;
        self.pos += 1;
        Ok(b)
    }

    fn number<const N: usize>(&mut self) -> R<u64> {
        if self.pos + N > self.data.len() {
            return Err("unexpected end of pickle".into());
        }
        let mut n = [0u8; 8];
        n[..N].copy_from_slice(&self.data[self.pos..self.pos + N]);
        self.pos += N;
        Ok(u64::from_le_bytes(n))
    }

    fn line(&mut self) -> R<&'a str> {
        let start = self.pos;
        let end = self.data[start..]
            .iter()
            .position(|&b| b == b'\n')
            .ok_or("unterminated string")?
            + start;
        self.pos = end + 1;
        std::str::from_utf8(&self.data[start..end]).map_err(|_| "invalid utf8".into())
    }

    fn unicode(&mut self, len: usize) -> R<String> {
        if self.pos + len > self.data.len() {
            return Err("unexpected end of pickle".into());
        }
        let s = String::from_utf8_lossy(&self.data[self.pos..self.pos + len]).into_owned();
        self.pos += len;
        Ok(s)
    }

    fn mark_pop(&mut self) -> Vec<Py> {
        let n = self.marks.pop().unwrap_or(0);
        if self.stack.len() > n {
            self.stack.split_off(n)
        } else {
            Vec::new()
        }
    }

    /// Classes we can't instantiate (Morsel, SimpleCookie, defaultdict, ...) become
    /// opaque dicts; the ones we know as containers become their container.
    fn instance(&self, class: &str) -> Py {
        match class {
            "list" | "tuple" => Py::List(Vec::new()),
            _ => Py::Dict { class: Some(class.into()), items: Vec::new() },
        }
    }

    fn push_instance(&mut self, class: &str) {
        self.stack.push(self.instance(class));
    }

    fn step(&mut self) -> R<bool> {
        let op = self.byte()?;
        match op {
            b'(' => self.marks.push(self.stack.len()),
            b'.' => return Ok(true), // STOP
            b'0' => {
                self.stack.pop();
            }
            b'1' => {
                let n = self.marks.pop().unwrap_or(0);
                self.stack.truncate(n);
            }
            b'}' => self.stack.push(Py::Dict { class: None, items: Vec::new() }),
            b']' | b')' => self.stack.push(Py::List(Vec::new())),
            b'N' => self.stack.push(Py::None),
            b'\x88' => self.stack.push(Py::Bool(true)),
            b'\x89' => self.stack.push(Py::Bool(false)),
            b'K' => {
                let b = self.byte()?;
                self.stack.push(Py::Int(b as i64));
            }
            b'M' => {
                let n = self.number::<2>()?;
                self.stack.push(Py::Int(n as i64));
            }
            b'J' => {
                let n = self.number::<4>()?;
                self.stack.push(Py::Int(n as u32 as i64));
            }
            b'G' => {
                let n = self.number::<8>()?;
                self.stack.push(Py::Float(f64::from_bits(n)));
            }
            b'X' => {
                let len = self.number::<4>()? as usize;
                let s = self.unicode(len)?;
                self.stack.push(Py::Str(s));
            }
            b'\x8c' => {
                let len = self.byte()? as usize;
                let s = self.unicode(len)?;
                self.stack.push(Py::Str(s));
            }
            b'\x8d' => {
                let len = self.number::<8>()? as usize;
                let s = self.unicode(len)?;
                self.stack.push(Py::Str(s));
            }
            b'C' => {
                let len = self.byte()? as usize;
                if self.pos + len > self.data.len() {
                    return Err("unexpected end of pickle".into());
                }
                self.stack.push(Py::Bytes(self.data[self.pos..self.pos + len].to_vec()));
                self.pos += len;
            }
            b'I' => {
                let line = self.line()?;
                self.stack.push(Py::Int(line.trim().parse().map_err(|_| "bad INT")?));
            }
            b'L' => {
                let line = self.line()?;
                self.stack
                    .push(Py::Int(line.trim_end_matches('L').parse().map_err(|_| "bad LONG")?));
            }
            b'\x8a' => {
                let len = self.byte()? as usize;
                if self.pos + len > self.data.len() {
                    return Err("unexpected end of pickle".into());
                }
                let mut bytes = [0u8; 8];
                bytes[..len].copy_from_slice(&self.data[self.pos..self.pos + len]);
                self.pos += len;
                if len < 8 && bytes[len - 1] & 0x80 != 0 {
                    bytes[len..].fill(0xff); // sign-extend
                }
                self.stack.push(Py::Int(i64::from_le_bytes(bytes)));
            }
            b't' | b'l' => {
                let items = self.mark_pop();
                self.stack.push(Py::List(items));
            }
            b'd' => {
                let items = self.mark_pop();
                let mut pairs = Vec::new();
                let mut it = items.into_iter();
                while let (Some(k), Some(v)) = (it.next(), it.next()) {
                    pairs.push((k, v));
                }
                self.stack.push(Py::Dict { class: None, items: pairs });
            }
            b'\x85' => {
                let a = self.stack.pop().ok_or("stack underflow")?;
                self.stack.push(Py::List(vec![a]));
            }
            b'\x86' => {
                let b = self.stack.pop().ok_or("stack underflow")?;
                let a = self.stack.pop().ok_or("stack underflow")?;
                self.stack.push(Py::List(vec![a, b]));
            }
            b'\x87' => {
                let c = self.stack.pop().ok_or("stack underflow")?;
                let b = self.stack.pop().ok_or("stack underflow")?;
                let a = self.stack.pop().ok_or("stack underflow")?;
                self.stack.push(Py::List(vec![a, b, c]));
            }
            b'a' => {
                let v = self.stack.pop().ok_or("stack underflow")?;
                if let Some(target) = self.stack.last_mut() {
                    match target {
                        Py::List(l) => l.push(v),
                        Py::Dict { items, .. } => items.push((Py::Int(items.len() as i64), v)),
                        _ => {} // shape we don't model - drop the value
                    }
                }
            }
            b'e' => {
                let items = self.mark_pop();
                if let Some(target) = self.stack.last_mut() {
                    match target {
                        Py::List(l) => l.extend(items),
                        Py::Dict { items: d, .. } => {
                            let mut it = items.into_iter();
                            while let (Some(k), Some(v)) = (it.next(), it.next()) {
                                d.push((k, v));
                            }
                        }
                        _ => {}
                    }
                }
            }
            b's' => {
                let v = self.stack.pop().ok_or("stack underflow")?;
                let k = self.stack.pop().ok_or("stack underflow")?;
                if let Some(Py::Dict { items, .. }) = self.stack.last_mut() {
                    items.push((k, v));
                }
            }
            b'u' => {
                let items = self.mark_pop();
                if let Some(Py::Dict { items: d, .. }) = self.stack.last_mut() {
                    let mut it = items.into_iter();
                    while let (Some(k), Some(v)) = (it.next(), it.next()) {
                        d.push((k, v));
                    }
                }
            }
            b'c' => {
                let module = self.line()?;
                let name = self.line()?;
                self.push_instance(&format!("{module}.{name}"));
            }
            b'\x93' => {
                let name = self.stack.pop().and_then(|p| p.as_str().map(String::from)).unwrap_or_default();
                let module = self.stack.pop().and_then(|p| p.as_str().map(String::from)).unwrap_or_default();
                self.push_instance(&format!("{module}.{name}"));
            }
            b'R' | b'\x81' | b'\x82' => {
                let _args = self.stack.pop().ok_or("stack underflow")?;
                let func = self.stack.pop().ok_or("stack underflow")?;
                let class = match &func {
                    Py::Dict { class: Some(c), .. } => c.clone(),
                    _ => String::new(),
                };
                self.push_instance(&class);
            }
            b'b' => {
                // BUILD: fold the state dict into the object below it
                let state = self.stack.pop().ok_or("stack underflow")?;
                if let (Py::Dict { items: state_items, .. }, Some(Py::Dict { items, .. })) =
                    (&state, self.stack.last_mut())
                {
                    items.extend(state_items.iter().cloned());
                }
            }
            b'q' | b'\x94' => {
                let id = if op == b'\x94' { self.memo.len() } else { self.byte()? as usize };
                let v = self.stack.last().cloned().ok_or("stack underflow")?;
                if id >= self.memo.len() {
                    self.memo.resize(id + 1, Py::None);
                }
                self.memo[id] = v;
            }
            b'r' => {
                let id = self.number::<4>()? as usize;
                let v = self.stack.last().cloned().ok_or("stack underflow")?;
                if id >= self.memo.len() {
                    self.memo.resize(id + 1, Py::None);
                }
                self.memo[id] = v;
            }
            b'p' => {
                self.line()?;
            }
            b'g' | b'h' => {
                let id: usize = if op == b'g' {
                    self.line()?.trim().parse().map_err(|_| "bad GET")?
                } else {
                    self.byte()? as usize
                };
                let v = self.memo.get(id).cloned().ok_or("bad memo index")?;
                self.stack.push(v);
            }
            b'j' => {
                let id = self.number::<4>()? as usize;
                let v = self.memo.get(id).cloned().ok_or("bad memo index")?;
                self.stack.push(v);
            }
            b'\x80' => {
                self.byte()?; // protocol version
            }
            b'\x95' => {
                self.number::<8>()?; // frame length
            }
            other => return Err(format!("unsupported pickle opcode 0x{other:02x}")),
        }
        Ok(false)
    }

    fn run(mut self) -> R<Vec<Py>> {
        for _ in 0..1_000_000 {
            if self.step()? {
                return Ok(self.stack);
            }
        }
        Err("pickle too large".into())
    }
}

/// Walk the parsed pickle looking for a Morsel-shaped mapping holding `name`,
/// and return the cookie's value. Morsel state keys moved over Python versions:
/// old ones carry `_key`/`_value`, Python 3.13+ carries `key`/`value`.
pub fn cookie_value(data: &[u8], name: &str) -> Option<String> {
    let stack = Unpickler::new(data).run().ok()?;
    fn walk(v: &Py, name: &str, out: &mut Vec<String>) {
        if let Py::Dict { items, .. } = v {
            let get = |k: &str| items.iter().find(|(key, _)| key.as_str() == Some(k));
            let is_named = |k: &str| get(k).and_then(|(_, v)| v.as_str()) == Some(name);
            if (is_named("_key") || is_named("key"))
                && let Some((_, v)) = get("_value").or_else(|| get("value"))
                && let Some(s) = v.as_str()
            {
                out.push(s.to_owned());
            }
            for (k, val) in items {
                walk(k, name, out);
                walk(val, name, out);
            }
        } else if let Py::List(l) = v {
            for item in l {
                walk(item, name, out);
            }
        }
    }
    let mut out = Vec::new();
    for v in &stack {
        walk(v, name, &mut out);
    }
    out.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    const JAR: &[u8] = include_bytes!("testdata/cookies_jar.bin");

    #[test]
    fn finds_cookies_in_real_pickled_jar() {
        assert_eq!(cookie_value(JAR, "auth-token").as_deref(), Some("tok3nfromtdminstall"));
        assert_eq!(
            cookie_value(JAR, "unique_id").as_deref(),
            Some("abcdef0123456789abcdef0123456789")
        );
        assert_eq!(cookie_value(JAR, "persistent").as_deref(), Some("12345678"));
        assert_eq!(cookie_value(JAR, "nonexistent"), None);
    }

    #[test]
    fn hand_built_morsel() {
        // {"_key": "auth-token", "_value": "xyz"} with proto 5: PROTO, EMPTY_DICT,
        // MEMOIZE, MARK, k1, v1, k2, v2, SETITEMS, STOP
        let p: Vec<u8> = [
            &[0x80u8, 0x05, 0x7d, 0x94, 0x28][..],
            b"\x8c\x04_key",
            b"\x8c\x0aauth-token",
            b"\x8c\x06_value",
            b"\x8c\x03xyz",
            &[b'u', b'.'][..],
        ]
        .concat();
        assert_eq!(cookie_value(&p, "auth-token").as_deref(), Some("xyz"));
        assert_eq!(cookie_value(&p, "missing"), None);
    }

    #[test]
    fn garbage_is_none() {
        assert_eq!(cookie_value(b"not a pickle", "auth-token"), None);
        assert_eq!(cookie_value(&[], "auth-token"), None);
    }
}
