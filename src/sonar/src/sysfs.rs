//! Reading sysfs, and the two parsers everything else here needs.
//!
//! Every read is fallible and none of them are errors. A missing attribute in
//! sysfs is the normal way the kernel says "not applicable to this device" —
//! `mem_info_vram_total` is absent on an integrated GPU rather than zero — so
//! the return type is `Option` throughout and the caller decides what absence
//! means. Treating absence as failure here would make an iGPU an error case.

use std::fs;
use std::path::Path;

/// Trimmed contents of a sysfs attribute, or `None` if it is absent or unreadable.
pub fn read(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

pub fn read_u64(path: impl AsRef<Path>) -> Option<u64> {
    read(path)?.parse().ok()
}

/// Parse a kernel cpulist: `0-1`, `4-7`, `0`, `0-3,8-11`.
///
/// Returns sorted and deduplicated. A malformed range contributes nothing
/// rather than poisoning the whole list — sysfs does not emit malformed
/// cpulists, and if it ever does, dropping the range is recoverable where
/// panicking in an inventory tool is not.
pub fn parse_cpulist(s: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.split_once('-') {
            Some((lo, hi)) => {
                if let (Ok(lo), Ok(hi)) = (lo.trim().parse::<u32>(), hi.trim().parse::<u32>()) {
                    if lo <= hi {
                        out.extend(lo..=hi);
                    }
                }
            }
            None => {
                if let Ok(n) = part.parse::<u32>() {
                    out.push(n);
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Parse a cache size as sysfs writes it: `1280K`, `2048K`, `12288K`, `16M`.
/// Returns kibibytes.
pub fn parse_cache_kb(s: &str) -> Option<u32> {
    let s = s.trim();
    let (digits, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u32 = digits.parse().ok()?;
    match unit.trim() {
        "K" | "KiB" | "" => Some(n),
        "M" | "MiB" => Some(n * 1024),
        // A unit this code has never seen is worth surfacing as absent rather
        // than guessed at — a cache size silently off by 1024 would make the
        // class grouping in cpu.rs wrong in a way nothing else would catch.
        _ => None,
    }
}

/// Human-readable byte count. Inventory output is read by people first.
pub fn fmt_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", b, UNITS[0])
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}

/// Minimal JSON string escaping — enough for sysfs-derived values, which are
/// driver names, PCI ids and paths.
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpulist_forms_seen_on_this_machine() {
        assert_eq!(parse_cpulist("0-1"), vec![0, 1]);
        assert_eq!(parse_cpulist("4"), vec![4]);
        assert_eq!(parse_cpulist("4-7"), vec![4, 5, 6, 7]);
        assert_eq!(parse_cpulist("0-3,8-11"), vec![0, 1, 2, 3, 8, 9, 10, 11]);
    }

    #[test]
    fn cpulist_degrades_rather_than_panics() {
        assert_eq!(parse_cpulist(""), Vec::<u32>::new());
        assert_eq!(parse_cpulist("garbage"), Vec::<u32>::new());
        // A reversed range is dropped, not expanded backwards into nothing and
        // not panicked on.
        assert_eq!(parse_cpulist("7-4"), Vec::<u32>::new());
        assert_eq!(parse_cpulist("0-1,junk,4"), vec![0, 1, 4]);
    }

    #[test]
    fn cache_sizes_seen_on_this_machine() {
        assert_eq!(parse_cache_kb("1280K"), Some(1280));
        assert_eq!(parse_cache_kb("2048K"), Some(2048));
        assert_eq!(parse_cache_kb("12288K"), Some(12288));
        assert_eq!(parse_cache_kb("16M"), Some(16384));
    }

    #[test]
    fn unknown_cache_unit_is_absent_not_guessed() {
        assert_eq!(parse_cache_kb("512G"), None);
        assert_eq!(parse_cache_kb(""), None);
    }

    #[test]
    fn bytes_format() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(16 * 1024 * 1024 * 1024), "16.0 GiB");
    }

    #[test]
    fn escaping() {
        assert_eq!(json_escape(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(json_escape("a\nb"), "a\\nb");
    }
}
