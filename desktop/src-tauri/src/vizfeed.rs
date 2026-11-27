//! The native miner's visualisation feed, checked at the boundary (`automine --viz`, pow/miner/src/rrt/vizio.rs):
//!
//! * up: [`parse_frame`] accepts only well-formed `\x01VIZ` frames on the miner's stdout (known kind, numeric worker, short
//!   printable key, base64 payload where bytes are expected); anything else that starts with the mark is dropped, never shown
//!   as a log line or forwarded. [`read_line_capped`] bounds a line's size so a runaway line cannot exhaust memory, and does
//!   not stop at invalid UTF-8 (a stopped reader would leave the miner blocked on a full pipe).
//! * down: [`clean_mail`] rebuilds the page's mailbox from the fields the search reads (child.rs `hybrid`, vizio.rs
//!   `requested`) with their types checked and sizes bounded; nothing else reaches the miner's stdin.
use serde_json::{Map, Value};
use std::io::BufRead;

/// The mark in front of a visualisation frame on the miner's stdout: `MARK worker TAB kind TAB key TAB payload`.
pub const VIZ_MARK: &str = "\u{1}VIZ\t";

/// The longest line read from the miner: a SaveState as base64 is ~11.4 MB, a hybrid node state as hex ~17 MB.
pub const MAX_LINE: usize = 40 << 20;

/// The browser's mailbox is 2 MB (lib/webminer/board.js); the same bound here.
pub const MAX_MAIL: usize = 2 << 20;

const KINDS: &[&str] = &["viz", "state", "edges", "pert", "path", "hybrid_out"];

#[derive(Debug, PartialEq)]
pub struct Frame<'a> {
    pub w: u32,
    pub kind: &'a str,
    pub key: &'a str,
    pub data: &'a str,
}

/// A `\x01VIZ` frame, checked; `None` for anything malformed.
pub fn parse_frame(line: &str) -> Option<Frame<'_>> {
    let rest = line.strip_prefix(VIZ_MARK)?;
    let mut p = rest.splitn(4, '\t');
    let (w, kind, key, data) = (p.next()?, p.next()?, p.next()?, p.next()?);
    if w.is_empty() || w.len() > 4 || !w.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let w: u32 = w.parse().ok()?;
    if !KINDS.contains(&kind) || key.len() > 256 || !key.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return None;
    }
    let ok = match kind {
        "state" | "edges" => data.len() % 4 == 0 && data.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'='),
        // JSON payloads: the page parses them (in a try); here only that they look like one object.
        _ => data.starts_with('{') && data.ends_with('}'),
    };
    ok.then_some(Frame { w, kind, key, data })
}

/// One line of at most `cap` bytes into `buf` (without the newline). `Ok(None)` at the end of the stream; `Ok(Some(false))`
/// when the line was longer than `cap` (its rest is skipped and `buf` holds only the first `cap` bytes).
pub fn read_line_capped(r: &mut impl BufRead, buf: &mut Vec<u8>, cap: usize) -> std::io::Result<Option<bool>> {
    buf.clear();
    let mut fits = true;
    let mut any = false;
    loop {
        let chunk = match r.fill_buf() {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if chunk.is_empty() {
            return Ok(any.then_some(fits));
        }
        any = true;
        let (take, done) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (i, true),
            None => (chunk.len(), false),
        };
        let room = cap.saturating_sub(buf.len());
        if take > room {
            fits = false;
        }
        buf.extend_from_slice(&chunk[..take.min(room)]);
        r.consume(if done { take + 1 } else { take });
        if done {
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
            return Ok(Some(fits));
        }
    }
}

fn str_field(c: &Map<String, Value>, k: &str, max: usize) -> Result<Option<Value>, String> {
    match c.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.len() <= max => Ok(Some(Value::String(s.clone()))),
        Some(_) => Err(format!("{k}: a string of at most {max} bytes")),
    }
}

fn uint_field(c: &Map<String, Value>, k: &str, max: u64) -> Result<Option<Value>, String> {
    match c.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().filter(|&n| n <= max).map(|n| Some(Value::from(n))).ok_or_else(|| format!("{k}: a whole number up to {max}")),
    }
}

fn num_field(c: &Map<String, Value>, k: &str, lo: f64, hi: f64) -> Result<Option<Value>, String> {
    match c.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().filter(|x| x.is_finite() && (lo..=hi).contains(x)).map(|x| Some(Value::from(x))).ok_or_else(|| format!("{k}: a number in {lo}..{hi}")),
    }
}

fn put(out: &mut Map<String, Value>, k: &str, v: Option<Value>) {
    if let Some(v) = v {
        out.insert(k.into(), v);
    }
}

/// Command and request ids stay below 2^40 (vizio.rs `requested` marks node requests with bit 40).
const MAX_ID: u64 = (1 << 40) - 1;

fn clean_goal(v: &Value) -> Result<Value, String> {
    let Some(g) = v.as_object() else { return Ok(Value::Null) };
    let mut out = Map::new();
    put(&mut out, "level", Some(uint_field(g, "level", u16::MAX as u64)?.ok_or("goal.level is missing")?));
    put(&mut out, "area", Some(uint_field(g, "area", u8::MAX as u64)?.ok_or("goal.area is missing")?));
    let pos = g.get("pos").and_then(|p| p.as_array()).filter(|p| p.len() == 3 && p.iter().all(|x| x.as_f64().is_some_and(|x| x.is_finite() && x.abs() < 1e6))).ok_or("goal.pos: three numbers")?;
    out.insert("pos".into(), Value::Array(pos.clone()));
    put(&mut out, "radius", num_field(g, "radius", 0.0, 1e6)?);
    put(&mut out, "share", num_field(g, "share", 0.0, 1.0)?);
    Ok(Value::Object(out))
}

fn clean_cmd(v: &Value) -> Result<Value, String> {
    let c = v.as_object().ok_or("a command is an object")?;
    let mut out = Map::new();
    put(&mut out, "id", Some(uint_field(c, "id", MAX_ID)?.ok_or("cmd.id is missing")?));
    put(&mut out, "slot", uint_field(c, "slot", 1024)?);
    put(&mut out, "at", uint_field(c, "at", u64::MAX)?);
    match c.get("kind").and_then(|k| k.as_str()) {
        Some("state") => {
            out.insert("kind".into(), "state".into());
            put(&mut out, "nid", Some(uint_field(c, "nid", u32::MAX as u64)?.ok_or("cmd.nid is missing")?));
            put(&mut out, "vkey", str_field(c, "vkey", 256)?);
        }
        Some("insert") => {
            out.insert("kind".into(), "insert".into());
            put(&mut out, "tag", str_field(c, "tag", 512)?);
            put(&mut out, "path", Some(str_field(c, "path", MAX_MAIL)?.ok_or("cmd.path is missing")?));
            put(&mut out, "seg", Some(str_field(c, "seg", MAX_MAIL)?.ok_or("cmd.seg is missing")?));
            put(&mut out, "weight", num_field(c, "weight", 0.0, 1000.0)?);
        }
        _ => return Err("cmd.kind: state or insert".into()),
    }
    Ok(Value::Object(out))
}

fn clean_req(v: &Value) -> Result<Value, String> {
    let r = v.as_object().ok_or("a request is an object")?;
    let mut out = Map::new();
    put(&mut out, "id", Some(uint_field(r, "id", MAX_ID)?.ok_or("req.id is missing")?));
    put(&mut out, "nid", Some(uint_field(r, "nid", u32::MAX as u64)?.ok_or("req.nid is missing")?));
    put(&mut out, "key", Some(str_field(r, "key", 256)?.ok_or("req.key is missing")?));
    put(&mut out, "slot", uint_field(r, "slot", 1024)?);
    Ok(Value::Object(out))
}

fn list(v: &Value, max: usize, f: fn(&Value) -> Result<Value, String>) -> Result<Value, String> {
    let a = v.as_array().ok_or("a list")?;
    if a.len() > max {
        return Err(format!("at most {max} entries"));
    }
    a.iter().map(f).collect::<Result<Vec<_>, _>>().map(Value::Array)
}

/// The page's mailbox, rebuilt from the known fields only: `goal` (object or null), `cmds` (hybrid commands), `reqs` (node
/// requests). Any other key, a wrong type, or a mailbox over [`MAX_MAIL`] is refused.
pub fn clean_mail(mail: &Map<String, Value>) -> Result<Map<String, Value>, String> {
    let mut out = Map::new();
    for (k, v) in mail {
        let x = match k.as_str() {
            "goal" => clean_goal(v),
            "cmds" => list(v, 256, clean_cmd),
            "reqs" => list(v, 64, clean_req),
            _ => Err("unknown key".into()),
        }
        .map_err(|e| format!("mail.{k}: {e}"))?;
        out.insert(k.clone(), x);
    }
    let size = serde_json::to_string(&out).map_or(usize::MAX, |s| s.len());
    if size > MAX_MAIL {
        return Err(format!("the mailbox is {size} bytes (at most {MAX_MAIL})"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn frames_are_checked() {
        let f = parse_frame("\u{1}VIZ\t0\tstate\tk:1\tZm9v").unwrap();
        assert_eq!(f, Frame { w: 0, kind: "state", key: "k:1", data: "Zm9v" });
        assert_eq!(parse_frame("\u{1}VIZ\t0\tviz\t\t{\"a\":1}").unwrap().kind, "viz");
        assert_eq!(parse_frame("\u{1}VIZ\t3\tpath\tk\t{\"id\":1,\"flat\":[]}").unwrap().w, 3);
        // malformed: not a frame
        for bad in [
            "\u{1}VIZ",
            "\u{1}VIZ\t0\tstate\tk",
            "\u{1}VIZnope",
            "\u{1}VIZ\tx\tviz\t\t{}",
            "\u{1}VIZ\t-1\tviz\t\t{}",
            "\u{1}VIZ\t99999\tviz\t\t{}",
            "\u{1}VIZ\t0\texec\t\t{}",
            "\u{1}VIZ\t0\tstate\tk\tnot base64!",
            "\u{1}VIZ\t0\tstate\tk\tZm9",
            "\u{1}VIZ\t0\tviz\t\tplain text",
            "\u{1}VIZ\t0\tviz\tbad\u{7}key\t{}",
            "  step 1: castle",
        ] {
            assert!(parse_frame(bad).is_none(), "{bad:?}");
        }
        assert!(parse_frame(&format!("\u{1}VIZ\t0\tviz\t{}\t{{}}", "k".repeat(300))).is_none());
    }

    #[test]
    fn lines_are_capped_and_lossy_bytes_kept_going() {
        let data = b"short\nthis line is too long\n\xff\xfe bad utf8\nlast";
        let mut r = std::io::BufReader::with_capacity(4, &data[..]);
        let mut buf = Vec::new();
        assert_eq!(read_line_capped(&mut r, &mut buf, 10).unwrap(), Some(true));
        assert_eq!(buf, b"short");
        assert_eq!(read_line_capped(&mut r, &mut buf, 10).unwrap(), Some(false));
        assert_eq!(buf, b"this line ");
        assert_eq!(read_line_capped(&mut r, &mut buf, 64).unwrap(), Some(true));
        assert_eq!(String::from_utf8_lossy(&buf), "\u{fffd}\u{fffd} bad utf8");
        assert_eq!(read_line_capped(&mut r, &mut buf, 64).unwrap(), Some(true));
        assert_eq!(buf, b"last");
        assert_eq!(read_line_capped(&mut r, &mut buf, 64).unwrap(), None);
    }

    #[test]
    fn mail_keeps_only_the_schema() {
        let m = json!({
            "goal": { "level": 9, "area": 1, "pos": [1.0, 2.0, 3.0], "radius": 300, "share": 0.35, "evil": 1 },
            "cmds": [
                { "id": 1, "kind": "state", "slot": 0, "nid": 5, "vkey": "1:2:9:1:Star", "at": 1700000000000u64 },
                { "id": 2, "kind": "insert", "tag": "t", "path": "AAAA", "seg": "BBBB", "weight": 8, "extra": "x" }
            ],
            "reqs": [{ "id": 3, "slot": 0, "key": "k", "nid": 7 }]
        });
        let c = clean_mail(m.as_object().unwrap()).unwrap();
        assert!(c["goal"].get("evil").is_none());
        assert!(c["cmds"][1].get("extra").is_none());
        assert_eq!(c["cmds"][0]["nid"], 5);
        assert_eq!(c["reqs"][0]["nid"], 7);
        assert_eq!(clean_mail(json!({ "goal": null }).as_object().unwrap()).unwrap()["goal"], Value::Null);
        for bad in [
            json!({ "viz_on": true }),
            json!({ "exec": "rm -rf /" }),
            json!({ "cmds": [{ "id": 1, "kind": "shell" }] }),
            json!({ "cmds": [{ "id": -1, "kind": "state", "nid": 1 }] }),
            json!({ "cmds": [{ "id": 1u64 << 41, "kind": "state", "nid": 1 }] }),
            json!({ "cmds": [{ "id": 1, "kind": "insert", "path": "A" }] }),
            json!({ "cmds": [{ "id": 1, "kind": "insert", "path": "A", "seg": "B", "weight": 1e9 }] }),
            json!({ "cmds": "x" }),
            json!({ "goal": { "level": 9, "area": 1, "pos": [1, 2] } }),
            json!({ "goal": { "level": 9, "area": 1, "pos": [1, 2, 3], "share": 2 } }),
            json!({ "reqs": [{ "id": 1, "nid": 1 }] }),
            json!({ "reqs": (0..65).map(|i| json!({ "id": i, "nid": 1, "key": "k" })).collect::<Vec<_>>() }),
            json!({ "cmds": (0..3).map(|i| json!({ "id": i, "kind": "insert", "path": "A".repeat(1 << 20), "seg": "B" })).collect::<Vec<_>>() }),
        ] {
            assert!(clean_mail(bad.as_object().unwrap()).is_err(), "{bad}");
        }
    }
}
