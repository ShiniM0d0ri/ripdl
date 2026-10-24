//! File and folder name hygiene for names coming from the wild.
//!
//! Three callers need the same rules: the LAN inbox (an upload announces its
//! own name), torrent display names, and multi-part bundles (the part
//! filenames come from the page). Keeping one implementation means a name that
//! is safe in one path is safe in all of them.

/// One path component, no directories, no Windows-reserved characters.
///
/// Splits on both separators (a Windows path must not smuggle directories
/// into a Unix receiver, or the other way round), replaces illegal and
/// control characters, trims trailing dots/spaces, and prefixes Windows
/// device names (`CON.txt` cannot exist as a regular file). `fallback` is
/// returned when nothing usable survives.
pub fn sanitize_component(raw: &str, fallback: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut out = String::with_capacity(base.len());
    for ch in base.chars() {
        let bad = matches!(
            ch,
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' | '\0'
        ) || ch.is_control();
        out.push(if bad { '_' } else { ch });
    }
    let trimmed = out.trim_end_matches(['.', ' ']).to_string();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return fallback.to_string();
    }
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let stem = trimmed.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        return format!("_{trimmed}");
    }
    trimmed
}

/// `name.ext` with an optional ` (n)` suffix (`n == 0` returns `name`).
///
/// Used to claim a free name next to an existing file without inventing a
/// different extension for the copy.
pub fn suffixed(name: &str, n: u32) -> String {
    if n == 0 {
        return name.to_string();
    }
    let path = std::path::Path::new(name);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "received".into());
    match path.extension().map(|e| e.to_string_lossy().into_owned()) {
        Some(ext) if !ext.is_empty() => format!("{stem} ({n}).{ext}"),
        _ => format!("{stem} ({n})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_never_escape() {
        assert_eq!(sanitize_component("../../evil.sh", "f"), "evil.sh");
        assert_eq!(sanitize_component("..\\..\\evil.exe", "f"), "evil.exe");
        assert_eq!(sanitize_component("a/b/c.txt", "f"), "c.txt");
    }

    #[test]
    fn illegal_and_reserved_names_are_fixed() {
        assert_eq!(sanitize_component("bad:name?.txt", "f"), "bad_name_.txt");
        assert_eq!(sanitize_component("CON.txt", "f"), "_CON.txt");
        assert_eq!(sanitize_component("nul", "f"), "_nul");
        assert_eq!(sanitize_component("console.txt", "f"), "console.txt");
        assert_eq!(sanitize_component("trailing.  ", "f"), "trailing");
        assert_eq!(sanitize_component("..", "f"), "f");
        assert_eq!(sanitize_component("", "f"), "f");
    }

    #[test]
    fn suffix_keeps_the_extension() {
        assert_eq!(suffixed("a.bin", 0), "a.bin");
        assert_eq!(suffixed("a.bin", 1), "a (1).bin");
        assert_eq!(suffixed("a", 2), "a (2)");
        assert_eq!(suffixed("a.tar.gz", 1), "a.tar (1).gz");
    }
}
