//! Glob matching for `switch -glob`, ported from `Tcl_StringCaseMatch` in
//! tclsh 9.0.4's `generic/tclUtil.c` (case-sensitive half only — `-nocase` is
//! refused at compile time rather than approximated).
//!
//! The pattern language is smaller than a regular expression and differs from
//! a POSIX character class in one way worth stating: `^` inside `[...]` is an
//! ordinary character, not a negation, so `[^b]` matches a caret or a `b`.

/// True when `text` matches the glob `pattern`.
pub fn string_match(text: &str, pattern: &str) -> bool {
    matches(
        &text.chars().collect::<Vec<_>>(),
        &pattern.chars().collect::<Vec<_>>(),
    )
}

fn matches(str_: &[char], pat: &[char]) -> bool {
    let (mut s, mut p) = (0usize, 0usize);
    loop {
        // The end of the pattern matches only the end of the string.
        if p >= pat.len() {
            return s >= str_.len();
        }
        if s >= str_.len() && pat[p] != '*' {
            return false;
        }

        if pat[p] == '*' {
            while p < pat.len() && pat[p] == '*' {
                p += 1;
            }
            if p >= pat.len() {
                return true;
            }
            let next = pat[p];
            loop {
                // Skip ahead to the first position that could start a match
                // when the pattern character after the star is ordinary.
                if next != '[' && next != '?' && next != '\\' {
                    while s < str_.len() && str_[s] != next {
                        s += 1;
                    }
                }
                if matches(&str_[s..], &pat[p..]) {
                    return true;
                }
                if s >= str_.len() {
                    return false;
                }
                s += 1;
            }
        }

        if pat[p] == '?' {
            p += 1;
            s += 1;
            continue;
        }

        if pat[p] == '[' {
            p += 1;
            let ch = str_[s];
            s += 1;
            loop {
                if p >= pat.len() || pat[p] == ']' {
                    return false;
                }
                let start = pat[p];
                p += 1;
                if p < pat.len() && pat[p] == '-' {
                    p += 1;
                    if p >= pat.len() {
                        return false;
                    }
                    let end = pat[p];
                    p += 1;
                    // Both `[a-z]` and the reversed `[z-a]` name the range.
                    if (start <= ch && ch <= end) || (end <= ch && ch <= start) {
                        break;
                    }
                } else if start == ch {
                    break;
                }
            }
            // Step past the rest of the class. An unterminated class matches
            // only if the string ended with it.
            while p < pat.len() && pat[p] != ']' {
                p += 1;
            }
            if p >= pat.len() {
                return s >= str_.len();
            }
            p += 1;
            continue;
        }

        // A backslash makes the next pattern character literal.
        if pat[p] == '\\' {
            p += 1;
            if p >= pat.len() {
                return false;
            }
        }
        if str_[s] != pat[p] {
            return false;
        }
        s += 1;
        p += 1;
    }
}
