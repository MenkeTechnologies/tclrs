//! `lset` of one element, edited in place.
//!
//! A list is its string, so `lset l $i x` used to split the whole string,
//! replace one element and join it again, twice over for the copy the command
//! returns: a loop assigning n elements was quadratic in n where tclsh, whose
//! list holds its elements, replaces one in constant time.
//!
//! The string `lset` produces is canonical (single spaces, each element quoted
//! as [`crate::list::join`] quotes it). This module remembers the last such
//! string together with the byte span of every element, keyed on the identity of
//! the `Arc` that holds it, as [`crate::dict_cache`] does for dictionaries. The
//! next `lset` on that same string splices the one element's span and shifts the
//! spans after it; nothing already in the string is re-quoted.

use std::cell::RefCell;
use std::sync::Arc;

use crate::list;

/// A canonical list string and where each of its elements sits in it.
struct Spans {
    text: Arc<String>,
    spans: Vec<(usize, usize)>,
}

thread_local! {
    static LAST: RefCell<Option<Spans>> = const { RefCell::new(None) };
}

/// The canonical string of `items` and the span of each element in it.
fn render(items: &[String]) -> (String, Vec<(usize, usize)>) {
    let mut out = String::new();
    let mut spans = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let start = out.len();
        out.push_str(&list::quote(item, i == 0));
        spans.push((start, out.len()));
    }
    (out, spans)
}

/// The remembered spans of `text` — taken out of the memory, so that the string
/// is not shared with it while it is edited — or a fresh parse of it.
fn take_for(text: &Arc<String>) -> Result<Spans, String> {
    let hit = LAST.with(|last| {
        let mut last = last.borrow_mut();
        match &*last {
            Some(s) if Arc::ptr_eq(&s.text, text) => last.take(),
            _ => None,
        }
    });
    if let Some(spans) = hit {
        return Ok(spans);
    }
    let (canonical, spans) = render(&list::split(text)?);
    // A string already in canonical form stays the allocation it is, so a
    // value another variable shares is not copied for nothing.
    let text = if *canonical == **text {
        Arc::clone(text)
    } else {
        Arc::new(canonical)
    };
    Ok(Spans { text, spans })
}

/// `lset` of the element `index` names in the list `current` holds, which is
/// one past the end to append. On a refusal the list is handed back unchanged
/// with the message.
pub(crate) fn set_element(
    current: Arc<String>,
    index: &str,
    replacement: &str,
) -> Result<Arc<String>, (Arc<String>, String)> {
    let mut entry = take_for(&current).map_err(|msg| (Arc::clone(&current), msg))?;
    let count = entry.spans.len() as i64;
    let at = match list::index(index, count - 1) {
        Ok(at) if (0..=count).contains(&at) => at as usize,
        Ok(_) => {
            let msg = format!("index \"{index}\" out of range");
            return Err(refuse(current, entry, msg));
        }
        Err(msg) => return Err(refuse(current, entry, msg)),
    };
    // Every other reference to the string must go, or the edit copies it.
    drop(current);
    let text = Arc::make_mut(&mut entry.text);
    if at == entry.spans.len() {
        if !text.is_empty() {
            text.push(' ');
        }
        let start = text.len();
        text.push_str(&list::quote(replacement, text.is_empty()));
        entry.spans.push((start, text.len()));
    } else {
        let (start, end) = entry.spans[at];
        let quoted = list::quote(replacement, at == 0);
        text.replace_range(start..end, &quoted);
        let delta = quoted.len() as isize - (end - start) as isize;
        entry.spans[at].1 = start + quoted.len();
        if delta != 0 {
            for span in &mut entry.spans[at + 1..] {
                span.0 = (span.0 as isize + delta) as usize;
                span.1 = (span.1 as isize + delta) as usize;
            }
        }
    }
    let out = Arc::clone(&entry.text);
    LAST.with(|last| *last.borrow_mut() = Some(entry));
    Ok(out)
}

/// A refused edit: remember what was learned of the string, and hand the
/// variable's value back as it came.
fn refuse(current: Arc<String>, entry: Spans, msg: String) -> (Arc<String>, String) {
    LAST.with(|last| *last.borrow_mut() = Some(entry));
    (current, msg)
}
