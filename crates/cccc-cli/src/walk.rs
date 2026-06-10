//! Discover the source files to analyze from the given paths.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;

fn has_ext(path: &Path, exts: &[String]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

/// Compile `--exclude` glob patterns into a single matcher.
///
/// Returns `Ok(None)` when no patterns are given (the common case, so callers
/// can skip matching entirely). `literal_separator(true)` makes `*` stop at `/`,
/// so `**` is the way to span directories — matching the intuition from
/// `.gitignore`. An invalid pattern is surfaced as an error rather than silently
/// ignored.
pub fn build_exclude_set(patterns: &[String]) -> Result<Option<GlobSet>, globset::Error> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        builder.add(GlobBuilder::new(p).literal_separator(true).build()?);
    }
    Ok(Some(builder.build()?))
}

/// True if `path` should be skipped per the exclude set. Patterns are matched
/// against the path **relative to its walk root** (so `dist/**` is anchored at
/// the directory the user passed, regardless of whether that root was absolute),
/// and additionally against the file name alone (so `*.test.ts` matches at any
/// depth without needing a `**/` prefix).
fn is_excluded(path: &Path, base: Option<&Path>, exclude: Option<&GlobSet>) -> bool {
    let Some(set) = exclude else { return false };
    let rel = base
        .and_then(|b| path.strip_prefix(b).ok())
        // No walk root (explicit file argument): just drop a leading `./`.
        .unwrap_or_else(|| path.strip_prefix(".").unwrap_or(path));
    if set.is_match(rel) {
        return true;
    }
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| set.is_match(name))
}

/// Collect matching files from `paths`. Explicit file arguments are included
/// regardless of extension; directories are walked (respecting ignore files
/// unless `no_ignore`) and filtered by `exts`. `node_modules` is always skipped.
/// Any file matching `exclude` is dropped, whether named explicitly or found by
/// walking.
pub fn collect_files(
    paths: &[PathBuf],
    exts: &[String],
    no_ignore: bool,
    exclude: Option<&GlobSet>,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for root in paths {
        if root.is_file() {
            if !is_excluded(root, None, exclude) {
                push_unique(root, &mut out, &mut seen);
            }
            continue;
        }

        let mut builder = WalkBuilder::new(root);
        builder
            .git_ignore(!no_ignore)
            .git_global(!no_ignore)
            .git_exclude(!no_ignore)
            .ignore(!no_ignore)
            .hidden(false)
            .filter_entry(|entry| entry.file_name() != "node_modules");

        for result in builder.build() {
            let Ok(entry) = result else { continue };
            let path = entry.path();
            if path.is_file() && has_ext(path, exts) && !is_excluded(path, Some(root), exclude) {
                push_unique(path, &mut out, &mut seen);
            }
        }
    }

    out
}

fn push_unique(path: &Path, out: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>) {
    let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if seen.insert(key) {
        out.push(path.to_path_buf());
    }
}
