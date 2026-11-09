//! Lenient reading of a damaged JSON object (deep-audit round 2, R2-1): a file cut off by a
//! power cut, or garbage past some point, still gives back every member before the break.
//! A member cut off inside a nested object or array keeps the parts before the cut
//! (`"b.mp4":{"favourite":true,"dura` -> `{"favourite":true}`).
use serde_json::{Map, Value};

/// The members of the JSON object at the start of `bytes` that still parse, in order.
/// Empty when `bytes` does not start with `{` (NUL-filled, not JSON).
pub fn partial_object(bytes: &[u8]) -> Map<String, Value> {
    let mut out = Map::new();
    let at = skip_ws(bytes, 0);
    if bytes.get(at) == Some(&b'{') {
        object_body(bytes, at + 1, &mut out);
    }
    out
}

fn skip_ws(bytes: &[u8], mut at: usize) -> usize {
    while bytes.get(at).is_some_and(|b| b.is_ascii_whitespace()) {
        at += 1;
    }
    at
}

/// One whole JSON value at `at`, and the index after it.
fn value_at(bytes: &[u8], at: usize) -> Option<(Value, usize)> {
    let mut stream = serde_json::Deserializer::from_slice(bytes.get(at..)?).into_iter::<Value>();
    let value = stream.next()?.ok()?;
    Some((value, at + stream.byte_offset()))
}

/// What parsed of a value cut off at `at`: the members / elements before the cut.
fn partial_value(bytes: &[u8], at: usize) -> Option<Value> {
    match bytes.get(at)? {
        b'{' => {
            let mut map = Map::new();
            object_body(bytes, at + 1, &mut map);
            (!map.is_empty()).then_some(Value::Object(map))
        }
        b'[' => {
            let mut items = Vec::new();
            array_body(bytes, at + 1, &mut items);
            (!items.is_empty()).then_some(Value::Array(items))
        }
        _ => None,
    }
}

/// Members from `at` (just past `{`) into `out`, up to `}` or the first break.
fn object_body(bytes: &[u8], mut at: usize, out: &mut Map<String, Value>) {
    loop {
        at = skip_ws(bytes, at);
        match bytes.get(at) {
            Some(b',') => {
                at += 1;
                continue;
            }
            Some(b'"') => {}
            _ => return,
        }
        let Some((Value::String(key), next)) = value_at(bytes, at) else { return };
        at = skip_ws(bytes, next);
        if bytes.get(at) != Some(&b':') {
            return;
        }
        at = skip_ws(bytes, at + 1);
        match value_at(bytes, at) {
            Some((value, next)) => {
                out.insert(key, value);
                at = next;
            }
            None => {
                if let Some(value) = partial_value(bytes, at) {
                    out.insert(key, value);
                }
                return;
            }
        }
    }
}

/// Elements from `at` (just past `[`) into `out`, up to `]` or the first break.
fn array_body(bytes: &[u8], mut at: usize, out: &mut Vec<Value>) {
    loop {
        at = skip_ws(bytes, at);
        match bytes.get(at) {
            Some(b',') => {
                at += 1;
                continue;
            }
            Some(b']') | None => return,
            _ => {}
        }
        match value_at(bytes, at) {
            Some((value, next)) => {
                out.push(value);
                at = next;
            }
            None => {
                out.extend(partial_value(bytes, at));
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_cut_off_object_keeps_every_member_before_the_cut() {
        let got = partial_object(br#" {"a":{"favourite":true,"title":"A \"1\""}, "b":{"favourite":true,"dura"#);
        assert_eq!(Value::Object(got), json!({"a": {"favourite": true, "title": "A \"1\""}, "b": {"favourite": true}}));
        let got = partial_object(br#"{"imported":["D:\\Old","E:\\Cu"#);
        assert_eq!(Value::Object(got), json!({"imported": ["D:\\Old"]}));
    }

    #[test]
    fn garbage_stops_the_scan_and_non_objects_give_nothing() {
        assert!(partial_object(&[0u8; 64]).is_empty());
        assert!(partial_object(b"[1,2]").is_empty());
        let got = partial_object(b"{\"a\":1,\0\0\0\"b\":2}");
        assert_eq!(Value::Object(got), json!({"a": 1}));
        let whole = br#"{"a":1,"b":[true,null]}"#;
        assert_eq!(Value::Object(partial_object(whole)), serde_json::from_slice::<Value>(whole).unwrap());
    }
}
