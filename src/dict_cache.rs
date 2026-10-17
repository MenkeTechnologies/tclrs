//! The last dictionary a `dict` command read or produced, kept parsed.
//!
//! A dict here is its string form: a variable holds `k1 v1 k2 v2 ...`, so every
//! `dict get`, `dict exists` and `dict set` used to split the whole string and
//! rebuild its key index, and a loop over an n-key dict cost O(n^2) where tclsh,
//! whose dict is a hash table, costs O(n).
//!
//! The string is immutable, so its identity says whether it is the one parsed
//! last: the cache holds the `Arc` it parsed, which keeps that allocation alive
//! and its address from being reused by another string, and a pointer comparison
//! replaces the parse. This is the device the `lappend` and `lindex` caches in
//! [`crate::cmd_list`] use for lists.
//!
//! `dict set` and `dict incr` go further. A string they produced is canonical
//! (single spaces, each element quoted as [`crate::list::join`] quotes it), so a
//! new key is appended to it in place and an existing key's value replaced by
//! splicing its recorded span. Nothing already in the string is re-quoted.

use std::cell::RefCell;
use std::sync::Arc;

use fusevm::Value;

use crate::assoc::Dict;
use crate::list;
use crate::runtime::to_tcl_string;

/// A dictionary beside the string it was read from.
struct Cached {
    text: Arc<String>,
    dict: Dict,
    /// The byte span of each entry's value in `text`, in `dict` order. `Some`
    /// only when `text` is canonical, which is what makes splicing legal.
    spans: Option<Vec<(usize, usize)>>,
}

thread_local! {
    static LAST: RefCell<Option<Cached>> = const { RefCell::new(None) };
}

/// The string of a value used as a dict; an unset variable is the empty dict.
fn text_of(value: &Value) -> Arc<String> {
    match value {
        Value::Str(text) => Arc::clone(text),
        Value::Undef => Arc::new(String::new()),
        other => Arc::new(to_tcl_string(other)),
    }
}

/// The cache entry for `text` — removed from the cache, so that the string it
/// holds is not shared with it while the caller changes it — or a fresh parse.
fn take_for(text: &Arc<String>) -> Result<Cached, String> {
    let hit = LAST.with(|last| {
        let mut last = last.borrow_mut();
        match &*last {
            Some(c) if Arc::ptr_eq(&c.text, text) => last.take(),
            _ => None,
        }
    });
    match hit {
        Some(c) => Ok(c),
        None => Ok(Cached {
            text: Arc::clone(text),
            dict: Dict::parse(text)?,
            spans: None,
        }),
    }
}

fn keep(c: Cached) {
    LAST.with(|last| *last.borrow_mut() = Some(c));
}

/// Run `f` over the dictionary a value holds, parsing it only when it is not the
/// one parsed last.
fn with_dict<T>(
    container: &Value,
    f: impl FnOnce(&Dict) -> Result<T, String>,
) -> Result<T, String> {
    let text = text_of(container);
    let c = take_for(&text)?;
    let out = f(&c.dict);
    keep(c);
    out
}

fn not_known(key: &str) -> String {
    format!("key \"{key}\" not known in dictionary")
}

/// The value at the end of a key path, or the error `dict get` raises for the
/// first step that fails. `dict exists` is the same walk read as a boolean.
pub(crate) fn lookup(container: &Value, keys: &[String]) -> Result<String, String> {
    let (first, rest) = keys.split_first().expect("at least one key");
    with_dict(container, |d| {
        let mut value = d.get(first).ok_or_else(|| not_known(first))?.to_string();
        for key in rest {
            let inner = Dict::parse(&value)?;
            value = inner.get(key).ok_or_else(|| not_known(key))?.to_string();
        }
        Ok(value)
    })
}

/// `dict size`.
pub(crate) fn size(container: &Value) -> Result<usize, String> {
    with_dict(container, |d| Ok(d.len()))
}

/// The canonical string of `dict`, and the span of each value in it.
fn render(dict: &Dict) -> (String, Vec<(usize, usize)>) {
    let mut out = String::new();
    let mut spans = Vec::with_capacity(dict.entries.len());
    for (i, (key, value)) in dict.entries.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&list::quote(key, i == 0));
        out.push(' ');
        let start = out.len();
        out.push_str(&list::quote(value, false));
        spans.push((start, out.len()));
    }
    (out, spans)
}

/// Store `value` under `key` in a cache entry, keeping its string canonical.
fn put(c: &mut Cached, key: &str, value: &str) {
    let Some(spans) = c.spans.as_mut() else {
        c.dict.put(key.to_string(), value.to_string());
        let (text, spans) = render(&c.dict);
        c.text = Arc::new(text);
        c.spans = Some(spans);
        return;
    };
    let text = Arc::make_mut(&mut c.text);
    match c.dict.index.get(key).copied() {
        Some(i) => {
            let (start, end) = spans[i];
            let quoted = list::quote(value, false);
            text.replace_range(start..end, &quoted);
            let delta = quoted.len() as isize - (end - start) as isize;
            spans[i].1 = start + quoted.len();
            if delta != 0 {
                for span in &mut spans[i + 1..] {
                    span.0 = (span.0 as isize + delta) as usize;
                    span.1 = (span.1 as isize + delta) as usize;
                }
            }
            c.dict.entries[i].1 = value.to_string();
        }
        None => {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&list::quote(key, text.is_empty()));
            text.push(' ');
            let start = text.len();
            text.push_str(&list::quote(value, false));
            spans.push((start, text.len()));
            c.dict.put(key.to_string(), value.to_string());
        }
    }
}

/// `dict set` of a single key, on the dictionary `current` holds. On a malformed
/// dictionary the value is handed back with the message, unchanged.
pub(crate) fn set(current: Value, key: &str, value: &str) -> Result<Arc<String>, (Value, String)> {
    update(current, |_| Ok((key.to_string(), value.to_string())))
}

/// `dict incr` of a single key: the key's value, or 0, plus `by`.
pub(crate) fn incr(current: Value, key: &str, by: &str) -> Result<Arc<String>, (Value, String)> {
    update(current, |dict| {
        let sum = crate::runtime::incr_text(dict.get(key).unwrap_or("0"), by)?;
        Ok((key.to_string(), sum))
    })
}

/// Apply the one-key change `change` computes from the parsed dictionary.
fn update(
    current: Value,
    change: impl FnOnce(&Dict) -> Result<(String, String), String>,
) -> Result<Arc<String>, (Value, String)> {
    let text = text_of(&current);
    let mut c = take_for(&text).map_err(|msg| (current.clone(), msg))?;
    // Every other reference to the string must go, or the edit below copies it.
    drop(text);
    let original = Value::Str(Arc::clone(&c.text));
    drop(current);
    let (key, value) = match change(&c.dict) {
        Ok(pair) => pair,
        Err(msg) => {
            keep(c);
            return Err((original, msg));
        }
    };
    drop(original);
    put(&mut c, &key, &value);
    let out = Arc::clone(&c.text);
    keep(c);
    Ok(out)
}
