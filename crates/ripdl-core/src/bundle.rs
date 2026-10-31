//! Multi-part bundles: turn a selection of direct links into **one** task that
//! owns a folder (`<dest>/<base name>/<parts...>`), the way a torrent owns
//! `<dest>/<name>/` and a plain HTTP task owns a single file.
//!
//! Everything here is pure: URL/filename text in, groups out. The daemon and
//! the extension's preview both call it, so the grouping rules exist once.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::engine::percent_decode_lossy;
use crate::names::{sanitize_component, suffixed};

/// One link as offered by the page (or the CLI).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub url: String,
    /// Filename the page already knows (link text, `download` attribute,
    /// hoster plugin). The URL tail is used when absent.
    pub filename: Option<String>,
}

impl Link {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            filename: None,
        }
    }

    pub fn named(url: impl Into<String>, filename: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            filename: Some(filename.into()),
        }
    }

    /// Single path segment used for this part on disk.
    pub fn part_filename(&self) -> String {
        let raw = match self.filename.as_deref() {
            Some(f) if !f.trim().is_empty() => f.to_string(),
            _ => url_tail(&self.url),
        };
        sanitize_component(&raw, "download")
    }
}

/// One part inside a group, in natural (part-number) order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    pub url: String,
    pub filename: String,
    /// 1-based position the numbering proves (`None` for a marker-less file).
    #[serde(default)]
    pub seq: Option<u32>,
}

/// A set of links that belong to one download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    /// Folder name (sanitized base name).
    pub name: String,
    pub parts: Vec<Part>,
    /// Lowest part number present (1 when the set carries no numbering).
    #[serde(default = "one")]
    pub first: u32,
    /// `Some(n)` only when the parts start at 1, which is what proves `n` is
    /// the real total. A selection starting at part 5 leaves this `None`
    /// instead of inventing a denominator.
    #[serde(default)]
    pub total: Option<u32>,
    /// Numbers missing inside `first..=last`.
    #[serde(default)]
    pub missing: Vec<u32>,
}

fn one() -> u32 {
    1
}

impl Group {
    /// A group of one is an ordinary single-file download.
    pub fn is_single(&self) -> bool {
        self.parts.len() == 1
    }

    /// "3 of 7 parts" / "3 parts from 5" / "1 part" - one wording for CLI and
    /// GUI.
    pub fn parts_summary(&self) -> String {
        match (self.parts.len(), self.total) {
            (1, _) => "1 part".to_string(),
            (n, Some(t)) => format!("{n} of {t} parts"),
            (n, None) => format!("{n} parts from {}", self.first),
        }
    }
}

/// Which numbering scheme a filename follows. Links only group with links of
/// the same family, so `x.zip` never absorbs an unrelated `x.mkv` that merely
/// shares a stem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Family {
    Part,
    Rar,
    Zip,
    Split,
    Disc,
}

/// Group a selection the way the user means it: by base name.
///
/// Identical URLs are dropped, links whose base name is unique stay single,
/// and links that share a base but not a scheme (`.part01.rar` next to
/// `.rar`) stay separate.
pub fn group(links: &[Link]) -> Vec<Group> {
    let mut order: Vec<(Option<Family>, String)> = Vec::new();
    let mut buckets: HashMap<(Option<Family>, String), Vec<Classified>> = HashMap::new();
    let mut seen: Vec<&str> = Vec::new();

    for link in links {
        let url = link.url.trim();
        if !is_http(url) || seen.contains(&url) {
            continue;
        }
        seen.push(url);

        let filename = link.part_filename();
        let (base, family, seq) = classify(&filename);
        // Key on lowercase so `X.part01.rar` and `x.part02.rar` still meet,
        // and keep marker-less links in their own namespace so a file whose
        // whole name equals another set's base never merges with it.
        let key = (family, base.to_lowercase());
        if !buckets.contains_key(&key) {
            order.push(key.clone());
        }
        buckets.entry(key).or_default().push(Classified {
            url: url.to_string(),
            filename,
            seq,
            base,
        });
    }

    let mut out = Vec::new();
    for key in order {
        let items = match buckets.remove(&key) {
            Some(v) => v,
            None => continue,
        };
        let base = match items.first() {
            Some(first) => first.base.clone(),
            None => continue,
        };
        for (n, mut parts) in split_mirrors(items).into_iter().enumerate() {
            parts.sort_by(|a, b| {
                (a.seq.unwrap_or(u32::MAX), a.filename.to_lowercase())
                    .cmp(&(b.seq.unwrap_or(u32::MAX), b.filename.to_lowercase()))
            });
            let seqs: Vec<Option<u32>> = parts.iter().map(|p| p.seq).collect();
            let (first, total, missing) = numbering(&seqs);
            out.push(Group {
                name: sanitize_component(&suffixed(&base, n as u32), "bundle"),
                parts: parts
                    .into_iter()
                    .map(|p| Part {
                        url: p.url,
                        filename: p.filename,
                        seq: p.seq,
                    })
                    .collect(),
                first,
                total,
                missing,
            });
        }
    }
    out
}

/// One bundle the user asked for by name (popup "combine", CLI `--name`):
/// grouping rules do not apply, every link is a part in the given order.
pub fn make_group(name: &str, links: &[Link]) -> Group {
    let mut parts = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for link in links {
        let url = link.url.trim();
        if !is_http(url) || seen.contains(&url) {
            continue;
        }
        seen.push(url);
        let filename = link.part_filename();
        let (_, _, seq) = classify(&filename);
        parts.push(Part {
            url: url.to_string(),
            filename,
            seq,
        });
    }
    parts.sort_by(|a, b| {
        (a.seq.unwrap_or(u32::MAX), a.filename.to_lowercase())
            .cmp(&(b.seq.unwrap_or(u32::MAX), b.filename.to_lowercase()))
    });
    Group {
        name: sanitize_component(name, "bundle"),
        parts,
        first: 1,
        total: None,
        missing: Vec::new(),
    }
}

/// Numbering facts for a set of parts: `(first, total, missing)`.
///
/// `total` is only set when the parts start at 1, which is what proves the
/// count; a selection starting at part 5 leaves it `None` instead of inventing
/// a denominator. Used by grouping and by the daemon's list/details, so both
/// say the same thing.
pub fn numbering(seqs: &[Option<u32>]) -> (u32, Option<u32>, Vec<u32>) {
    let present: Vec<u32> = seqs.iter().filter_map(|s| *s).collect();
    if present.is_empty() {
        return (1, None, Vec::new());
    }
    let first = *present.iter().min().unwrap();
    let last = *present.iter().max().unwrap();
    let numbered = seqs.iter().all(|s| s.is_some());
    let total = (numbered && first == 1).then_some(last);
    let missing = (first..=last).filter(|n| !present.contains(n)).collect();
    (first, total, missing)
}

/// `<dir>/<name>`, with ` (n)` appended until the folder does not exist yet.
pub fn free_dir(dir: &Path, name: &str) -> PathBuf {
    let safe = sanitize_component(name, "bundle");
    let mut candidate = dir.join(&safe);
    let mut n = 1u32;
    while candidate.exists() && n < 10_000 {
        candidate = dir.join(suffixed(&safe, n));
        n += 1;
    }
    candidate
}

#[derive(Debug)]
struct Classified {
    url: String,
    filename: String,
    seq: Option<u32>,
    base: String,
}

/// Same filename from a different URL is a mirror of one part, not a second
/// copy of it: mirrors split into their own group so they can never overwrite
/// each other inside one folder.
fn split_mirrors(items: Vec<Classified>) -> Vec<Vec<Classified>> {
    let mut groups: Vec<Vec<Classified>> = Vec::new();
    for item in items {
        match groups.iter_mut().find(|g| {
            !g.iter()
                .any(|i| i.filename.eq_ignore_ascii_case(&item.filename))
        }) {
            Some(g) => g.push(item),
            None => groups.push(vec![item]),
        }
    }
    groups
}

fn is_http(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Last path segment, percent-decoded, query and fragment dropped.
pub fn url_tail(url: &str) -> String {
    let no_frag = url.split('#').next().unwrap_or(url);
    let no_query = no_frag.split('?').next().unwrap_or(no_frag);
    let tail = no_query.rsplit('/').next().unwrap_or(no_query);
    percent_decode_lossy(tail)
}

/// `name.ext` -> (`name`, `ext`); no dot (or a leading dot) means no extension.
fn split_ext(f: &str) -> (&str, &str) {
    match f.rfind('.') {
        Some(i) if i > 0 && i + 1 < f.len() => (&f[..i], &f[i + 1..]),
        _ => (f, ""),
    }
}

/// `stem` ending in `sep?keyword<digits>` -> (`base`, number).
///
/// `x.part01` -> (`x`, 1); `x_part01` -> (`x`, 1); `report.part2` ->
/// (`report`, 2). The keyword must be a separate token, so `my.partly.txt`
/// does not match.
fn trailing_index(stem: &str, keyword: &str) -> Option<(String, u32)> {
    let lower = stem.to_ascii_lowercase();
    let pos = lower.rfind(keyword)?;
    if pos > 0 {
        let prev = lower[..pos].chars().next_back()?;
        if !matches!(prev, '.' | '_' | '-' | ' ') {
            return None;
        }
    }
    let digits = &stem[pos + keyword.len()..];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u32 = digits.parse().ok()?;
    let base = stem[..pos].trim_end_matches(['.', '_', '-', ' ']);
    if base.is_empty() {
        return None;
    }
    Some((base.to_string(), n))
}

/// `r00`/`z01`-style extension -> the number in it.
fn numeric_suffix(ext: &str, lead: char) -> Option<u32> {
    let rest = ext.strip_prefix(lead)?;
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

/// Drop a container extension left in front of a split marker
/// (`x.7z.001` -> `x`, `x.zip.001` -> `x`).
fn strip_container(stem: &str) -> String {
    const CONTAINERS: &[&str] = &["7z", "zip", "rar", "tar", "gz", "bz2", "xz"];
    let lower = stem.to_ascii_lowercase();
    for c in CONTAINERS {
        if let Some(rest) = lower.strip_suffix(&format!(".{c}")) {
            if !rest.is_empty() {
                return stem[..rest.len()].to_string();
            }
        }
    }
    stem.to_string()
}

/// (base name, numbering scheme, part number) for one filename.
fn classify(f: &str) -> (String, Option<Family>, Option<u32>) {
    let (stem, ext) = split_ext(f);
    let ext_l = ext.to_ascii_lowercase();

    // x.part01[.rar] - try the whole name first so a part marker that *is*
    // the extension (`x.part01`) still matches.
    if let Some((base, n)) = trailing_index(f, "part") {
        return (base, Some(Family::Part), Some(n));
    }
    if let Some((base, n)) = trailing_index(stem, "part") {
        return (base, Some(Family::Part), Some(n));
    }
    // x.r00 ... x.rar
    if let Some(n) = numeric_suffix(&ext_l, 'r') {
        return (stem.to_string(), Some(Family::Rar), Some(n + 2));
    }
    // x.z01 ... x.zip
    if let Some(n) = numeric_suffix(&ext_l, 'z') {
        return (stem.to_string(), Some(Family::Zip), Some(n + 1));
    }
    // x CD1 / Disc2 / DVD3. Checked before the plain container extensions,
    // or `Some.Movie.CD1.rar` would become its own one-part set.
    for kw in ["disc", "dvd", "cd"] {
        if let Some((base, n)) = trailing_index(stem, kw) {
            return (base, Some(Family::Disc), Some(n));
        }
    }
    if ext_l == "rar" {
        return (stem.to_string(), Some(Family::Rar), Some(1));
    }
    if ext_l == "zip" {
        return (stem.to_string(), Some(Family::Zip), Some(1));
    }
    // x[.7z].001
    if ext.len() >= 2 && ext.bytes().all(|b| b.is_ascii_digit()) {
        let seq = ext.parse().ok();
        return (strip_container(stem), Some(Family::Split), seq);
    }
    (f.to_string(), None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(v: &[&str]) -> Vec<Link> {
        v.iter().map(|u| Link::new(*u)).collect()
    }

    fn names(g: &Group) -> Vec<String> {
        g.parts.iter().map(|p| p.filename.clone()).collect()
    }

    #[test]
    fn rar_parts_group_in_number_order() {
        let links = urls(&[
            "https://h/x.part10.rar",
            "https://h/x.part02.rar",
            "https://h/x.part01.rar",
        ]);
        let groups = group(&links);
        assert_eq!(groups.len(), 1);
        let g = &groups[0];
        assert_eq!(g.name, "x");
        assert_eq!(names(g), ["x.part01.rar", "x.part02.rar", "x.part10.rar"]);
        assert_eq!(g.total, Some(10));
        assert_eq!(g.missing.len(), 7);
        assert_eq!(g.parts_summary(), "3 of 10 parts");
    }

    #[test]
    fn rar_volumes_include_the_first_volume() {
        let groups = group(&urls(&[
            "https://h/x.r01",
            "https://h/x.rar",
            "https://h/x.r00",
        ]));
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].total, Some(3));
        assert_eq!(groups[0].missing, Vec::<u32>::new());
        assert_eq!(names(&groups[0]), ["x.rar", "x.r00", "x.r01"]);
    }

    #[test]
    fn zip_volumes_include_the_first_volume() {
        let groups = group(&urls(&["https://h/x.z01", "https://h/x.zip"]));
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].total, Some(2));
    }

    #[test]
    fn seven_zip_splits_strip_the_container() {
        let groups = group(&urls(&["https://h/x.7z.002", "https://h/x.7z.001"]));
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "x");
        assert_eq!(groups[0].total, Some(2));
        let zip = group(&urls(&["https://h/y.zip.001", "https://h/y.zip.002"]));
        assert_eq!(zip[0].name, "y");
    }

    #[test]
    fn gaps_are_reported_but_never_refused() {
        let groups = group(&urls(&[
            "https://h/x.part01.rar",
            "https://h/x.part02.rar",
            "https://h/x.part05.rar",
        ]));
        assert_eq!(groups[0].total, Some(5));
        assert_eq!(groups[0].missing, vec![3, 4]);
    }

    #[test]
    fn an_unanchored_selection_does_not_invent_a_total() {
        let groups = group(&urls(&["https://h/x.part05.rar", "https://h/x.part07.rar"]));
        assert_eq!(groups[0].total, None);
        assert_eq!(groups[0].first, 5);
        assert_eq!(groups[0].missing, vec![6]);
        assert_eq!(groups[0].parts_summary(), "2 parts from 5");
    }

    #[test]
    fn a_lone_link_is_a_single_download() {
        let groups = group(&urls(&["https://h/solo.part01.rar"]));
        assert_eq!(groups.len(), 1);
        assert!(groups[0].is_single());
    }

    #[test]
    fn same_stem_without_a_marker_stays_apart() {
        let groups = group(&urls(&["https://h/x.mkv", "https://h/x.srt"]));
        assert_eq!(groups.len(), 2);
        assert!(groups.iter().all(|g| g.is_single()));
    }

    #[test]
    fn mirrors_split_instead_of_overwriting() {
        let groups = group(&urls(&[
            "https://h/x.part01.rar",
            "https://h/x.part02.rar",
            "https://m/x.part01.rar",
        ]));
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].name, "x");
        assert_eq!(groups[0].parts.len(), 2);
        assert_eq!(groups[1].name, "x (1)");
        assert_eq!(names(&groups[1]), ["x.part01.rar"]);
    }

    #[test]
    fn the_same_url_twice_is_one_part() {
        let groups = group(&urls(&[
            "https://h/x.part01.rar",
            "https://h/x.part01.rar",
            "https://h/x.part02.rar",
        ]));
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].parts.len(), 2);
    }

    #[test]
    fn query_strings_and_escapes_do_not_confuse_the_base_name() {
        let groups = group(&urls(&[
            "https://h/My%20File.part01.rar?token=abc#frag",
            "https://h/My%20File.part02.rar?token=def",
        ]));
        assert_eq!(groups[0].name, "My File");
        assert_eq!(groups[0].parts[0].filename, "My File.part01.rar");
    }

    #[test]
    fn disc_sets_group_by_the_release_name() {
        let groups = group(&urls(&[
            "https://h/Some.Movie.CD2.rar",
            "https://h/Some.Movie.CD1.rar",
        ]));
        assert_eq!(groups[0].name, "Some.Movie");
        assert_eq!(groups[0].total, Some(2));
    }

    #[test]
    fn schemes_never_bleed_into_each_other() {
        let groups = group(&urls(&[
            "https://h/x.part01.rar",
            "https://h/x.part02.rar",
            "https://h/x.rar",
        ]));
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].parts.len(), 2);
        assert_eq!(groups[1].parts.len(), 1);
    }

    #[test]
    fn folder_names_are_safe() {
        // A `?` in the URL is a query string, not part of the name.
        let groups = group(&urls(&[
            "https://h/a:b?.part01.rar",
            "https://h/a:b?.part02.rar",
        ]));
        assert_eq!(groups[0].name, "a_b");
        // A plugin-supplied name can carry anything.
        let hinted = group(&[
            Link::named("https://h/dl/1", "a:b?.part01.rar"),
            Link::named("https://h/dl/2", "a:b?.part02.rar"),
        ]);
        assert_eq!(hinted[0].name, "a_b");
        let made = make_group("CON", &urls(&["https://h/a.bin"]));
        assert_eq!(made.name, "_CON");
    }

    #[test]
    fn explicit_groups_keep_every_link() {
        let g = make_group(
            "My Release",
            &urls(&["https://h/a.mkv", "https://h/b.srt", "https://h/a.mkv"]),
        );
        assert_eq!(g.name, "My Release");
        assert_eq!(g.parts.len(), 2);
        assert_eq!(g.total, None);
    }

    #[test]
    fn free_dir_steps_past_existing_folders() {
        let dir = std::env::temp_dir().join(format!("ripdl-bundle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(free_dir(&dir, "x"), dir.join("x"));
        std::fs::create_dir_all(dir.join("x")).unwrap();
        assert_eq!(free_dir(&dir, "x"), dir.join("x (1)"));
        std::fs::create_dir_all(dir.join("x (1)")).unwrap();
        assert_eq!(free_dir(&dir, "x"), dir.join("x (2)"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
