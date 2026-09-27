//! Debian version comparison (`dpkg --compare-versions`, Debian Policy
//! §5.6.12), without shelling out: `apply --locked` and `verify` compare a
//! locked version with the installed one while building a plan.

use std::cmp::Ordering;

/// Compare two Debian versions `[epoch:]upstream[-revision]`.
pub fn compare(a: &str, b: &str) -> Ordering {
    let (ea, ua, ra) = split(a);
    let (eb, ub, rb) = split(b);
    ea.cmp(&eb)
        .then_with(|| verrevcmp(ua.as_bytes(), ub.as_bytes()))
        .then_with(|| verrevcmp(ra.as_bytes(), rb.as_bytes()))
}

fn split(v: &str) -> (u64, &str, &str) {
    let v = v.trim();
    let (epoch, rest) = match v.split_once(':') {
        Some((e, rest)) if e.bytes().all(|b| b.is_ascii_digit()) && !e.is_empty() => {
            (e.parse().unwrap_or(0), rest)
        }
        _ => (0, v),
    };
    match rest.rsplit_once('-') {
        Some((upstream, revision)) => (epoch, upstream, revision),
        None => (epoch, rest, ""),
    }
}

/// Sort weight of one character in a non-digit run: `~` sorts before
/// everything (even the end of the string), letters before other symbols.
fn order(c: Option<u8>) -> i32 {
    match c {
        None => 0,
        Some(b'~') => -1,
        Some(c) if c.is_ascii_digit() => 0,
        Some(c) if c.is_ascii_alphabetic() => c as i32,
        Some(c) => c as i32 + 256,
    }
}

/// dpkg's `verrevcmp`: alternate non-digit runs (compared by [`order`]) and
/// digit runs (compared numerically).
fn verrevcmp(a: &[u8], b: &[u8]) -> Ordering {
    let digit = |s: &[u8], i: usize| s.get(i).is_some_and(|c| c.is_ascii_digit());
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        while (i < a.len() && !digit(a, i)) || (j < b.len() && !digit(b, j)) {
            let (ac, bc) = (order(a.get(i).copied()), order(b.get(j).copied()));
            if ac != bc {
                return ac.cmp(&bc);
            }
            i += 1;
            j += 1;
        }
        while a.get(i) == Some(&b'0') {
            i += 1;
        }
        while b.get(j) == Some(&b'0') {
            j += 1;
        }
        let mut first_diff = 0i32;
        while digit(a, i) && digit(b, j) {
            if first_diff == 0 {
                first_diff = a[i] as i32 - b[j] as i32;
            }
            i += 1;
            j += 1;
        }
        if digit(a, i) {
            return Ordering::Greater;
        }
        if digit(b, j) {
            return Ordering::Less;
        }
        if first_diff != 0 {
            return first_diff.cmp(&0);
        }
    }
    Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::compare;
    use std::cmp::Ordering::{Equal, Greater, Less};

    #[test]
    fn compares_like_dpkg() {
        for (a, b, want) in [
            ("1.0", "1.0", Equal),
            ("1.0", "1.1", Less),
            ("1.10", "1.9", Greater),
            ("1.0~rc1", "1.0", Less),
            ("1.0", "1.0+b1", Less),
            ("1:0.1", "2.0", Greater),
            ("7.81.0-1ubuntu1.29", "7.81.0-1ubuntu1.25", Greater),
            ("7.81.0-1ubuntu1.4", "7.81.0-1ubuntu1.15", Less),
            ("0.27.0-1", "0.26.0-1", Greater),
            (
                "16.0.19-1jammy.20260808.123456",
                "16.0.19-1jammy.20260907.010101",
                Less,
            ),
            ("1.0-1", "1.0", Greater),
            ("1.0a", "1.0", Greater),
            ("1.0.a", "1.0a", Greater),
            ("001", "1", Equal),
        ] {
            assert_eq!(compare(a, b), want, "{a} vs {b}");
            assert_eq!(compare(b, a), want.reverse(), "{b} vs {a}");
        }
    }
}
