//! Debian policy version ordering, implemented without libapt-pkg or libdpkg.
use anyhow::{bail, Result};
use std::cmp::Ordering;

fn split(version: &str) -> Result<(&str, &str, &str)> {
    if version.is_empty() || !version.is_ascii() || version.bytes().any(|b| b.is_ascii_whitespace() || b.is_ascii_control()) {
        bail!("invalid Debian version");
    }
    let (epoch, rest) = match version.split_once(':') {
        Some((e, rest)) if !e.is_empty() && e.bytes().all(|x| x.is_ascii_digit()) => (e, rest),
        Some(_) => bail!("invalid Debian version epoch"),
        None => ("0", version),
    };
    let (upstream, revision) = rest.rsplit_once('-').unwrap_or((rest, "0"));
    if upstream.is_empty() || revision.is_empty() || !upstream.as_bytes()[0].is_ascii_digit() {
        bail!("invalid Debian version");
    }
    if !upstream.bytes().all(|b| b.is_ascii_alphanumeric() || b".+~:-".contains(&b))
        || !revision.bytes().all(|b| b.is_ascii_alphanumeric() || b".+~".contains(&b)) {
        bail!("invalid Debian version characters");
    }
    Ok((epoch, upstream, revision))
}

fn order(c: Option<u8>) -> i32 {
    match c {
        Some(b'~') => -1,
        None => 0,
        Some(b) if b.is_ascii_alphabetic() => b as i32,
        Some(b) => b as i32 + 256,
    }
}

fn numeric_compare(a: &[u8], b: &[u8]) -> Ordering {
    let a = a.iter().copied().skip_while(|x| *x == b'0').collect::<Vec<_>>();
    let b = b.iter().copied().skip_while(|x| *x == b'0').collect::<Vec<_>>();
    a.len().cmp(&b.len()).then_with(|| a.cmp(&b))
}

fn part_compare(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        loop {
            let x = a.get(i).copied();
            let y = b.get(j).copied();
            let a_nd = x.is_some_and(|v| !v.is_ascii_digit());
            let b_nd = y.is_some_and(|v| !v.is_ascii_digit());
            if !a_nd && !b_nd { break; }
            let comparison = order(if a_nd { x } else { None })
                .cmp(&order(if b_nd { y } else { None }));
            if comparison != Ordering::Equal { return comparison; }
            if a_nd { i += 1; }
            if b_nd { j += 1; }
        }
        let ai = i;
        let bj = j;
        while i < a.len() && a[i].is_ascii_digit() { i += 1; }
        while j < b.len() && b[j].is_ascii_digit() { j += 1; }
        let cmp = numeric_compare(&a[ai..i], &b[bj..j]);
        if cmp != Ordering::Equal { return cmp; }
    }
    Ordering::Equal
}

/// Compare two Debian versions, including epochs, tildes and revisions.
pub fn compare(a: &str, b: &str) -> Result<Ordering> {
    let (ae, av, ar) = split(a)?;
    let (be, bv, br) = split(b)?;
    let epoch = numeric_compare(ae.as_bytes(), be.as_bytes());
    Ok(epoch.then_with(|| part_compare(av, bv)).then_with(|| part_compare(ar, br)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_examples() {
        let pairs = [
            ("1.0~rc1", "1.0", Ordering::Less),
            ("1.0", "1.0-1", Ordering::Less),
            ("1.0-1", "1.0-2", Ordering::Less),
            ("1:1.0", "2.0", Ordering::Greater),
            ("001:1.0", "1:1.0", Ordering::Equal),
            ("1.9", "1.10", Ordering::Less),
            ("1.001", "1.1", Ordering::Equal),
            ("1.0+git", "1.0", Ordering::Greater),
            ("1.0~~", "1.0~", Ordering::Less),
            ("1.0-0", "1.0", Ordering::Equal),
            ("1.0a", "1.0+", Ordering::Less),
        ];
        for (a, b, expected) in pairs {
            assert_eq!(compare(a, b).unwrap(), expected, "{a} vs {b}");
            assert_eq!(compare(b, a).unwrap(), expected.reverse(), "{b} vs {a}");
        }
    }
    #[test]
    fn invalid_versions() {
        for v in ["", "abc", "a b", ":1.2", "1:x", "1.0-", "1.0_1"] {
            assert!(compare(v, "1").is_err(), "should reject {v}");
        }
    }
}
