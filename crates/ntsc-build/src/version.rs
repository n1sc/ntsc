//! Version requirements for `[package].compiler`.
//!
//! The syntax matches the package manager's dependency requirements
//! (`crates/ntsc-pkg/src/modules/semver.nt`): comma-separated clauses, each
//! one of `^`, `~`, `>=`, `<=`, `>`, `<`, `=`, a bare partial version, or `*`.
//! A bare `1.2` accepts any `1.2.x` and `1` accepts any `1.x.y`.

use std::cmp::Ordering;

/// A parsed semantic version. Prerelease identifiers are kept verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    prerelease: String,
}

/// Whether `spec` is a requirement this module can evaluate.
pub fn is_valid_requirement(spec: &str) -> bool {
    let text = spec.trim();
    !text.is_empty() && text.split(',').all(|clause| is_valid_clause(clause.trim()))
}

/// Whether `version` satisfies `spec`.
pub fn satisfies(version: &str, spec: &str) -> bool {
    let Some(version) = parse(version) else {
        return false;
    };
    let text = spec.trim();
    if text.is_empty() || text == "*" {
        return version.prerelease.is_empty();
    }
    // A prerelease only matches a range that names one, so `2.0.0-beta.1`
    // never sneaks into `>=1.0.0, <2.0.0`.
    if !version.prerelease.is_empty() && !text.contains('-') {
        return false;
    }
    text.split(',')
        .all(|clause| clause_satisfies(&version, clause.trim()))
}

/// Whether `compiler_version` satisfies the requirement `spec`.
///
/// The compiler's own prerelease suffix is ignored: a `0.2.0-beta.1` compiler
/// satisfies `>=0.1.0`, otherwise every prerelease build would reject every
/// project. Requirement clauses that name a prerelease still compare exactly.
pub fn compiler_satisfies(spec: &str, compiler_version: &str) -> bool {
    let core = core_of(&normalize(compiler_version)).to_string();
    satisfies(&core, spec)
}

fn clause_satisfies(version: &Version, spec: &str) -> bool {
    if spec.is_empty() || spec == "*" || spec == "x" || spec == "X" {
        return true;
    }
    if let Some(rest) = spec.strip_prefix('^') {
        return caret_satisfies(version, rest);
    }
    if let Some(rest) = spec.strip_prefix('~') {
        return tilde_satisfies(version, rest);
    }
    if let Some(rest) = spec.strip_prefix(">=") {
        return compare_with(version, rest).is_some_and(|order| order != Ordering::Less);
    }
    if let Some(rest) = spec.strip_prefix("<=") {
        return compare_with(version, rest).is_some_and(|order| order != Ordering::Greater);
    }
    if let Some(rest) = spec.strip_prefix('>') {
        return compare_with(version, rest) == Some(Ordering::Greater);
    }
    if let Some(rest) = spec.strip_prefix('<') {
        return compare_with(version, rest) == Some(Ordering::Less);
    }
    if let Some(rest) = spec.strip_prefix('=') {
        return partial_satisfies(version, rest);
    }
    partial_satisfies(version, spec)
}

fn is_valid_clause(spec: &str) -> bool {
    if spec.is_empty() {
        return false;
    }
    if spec == "*" || spec == "x" || spec == "X" {
        return true;
    }
    let version = ["^", "~", ">=", "<=", ">", "<", "="]
        .iter()
        .find_map(|prefix| spec.strip_prefix(prefix))
        .unwrap_or(spec);
    parse(version).is_some()
}

fn compare_with(version: &Version, spec: &str) -> Option<Ordering> {
    parse(spec).map(|target| compare(version, &target))
}

/// A bare version is exact when it names a patch, a minor range when it names
/// `x.y`, and a major range when it names only `x`.
fn partial_satisfies(version: &Version, spec: &str) -> bool {
    let Some(target) = parse(spec) else {
        return false;
    };
    if !target.prerelease.is_empty() {
        return compare(version, &target) == Ordering::Equal;
    }
    match component_count(spec) {
        Some(3) => compare(version, &target) == Ordering::Equal,
        Some(2) => version.major == target.major && version.minor == target.minor,
        _ => version.major == target.major,
    }
}

/// `^1.2.3` -> `>=1.2.3, <2.0.0`; below 1.0.0 the leftmost non-zero component
/// is the one that may not change.
fn caret_satisfies(version: &Version, spec: &str) -> bool {
    let Some(target) = parse(spec) else {
        return false;
    };
    if compare(version, &target) == Ordering::Less {
        return false;
    }
    if component_count(spec) == Some(1) {
        return version.major == target.major;
    }
    if target.major > 0 {
        return version.major == target.major;
    }
    if component_count(spec) == Some(2) || target.minor > 0 {
        return version.minor == target.minor;
    }
    version.patch == target.patch
}

/// `~1.2.3` -> `>=1.2.3, <1.3.0`; `~1` keeps the major version.
fn tilde_satisfies(version: &Version, spec: &str) -> bool {
    let Some(target) = parse(spec) else {
        return false;
    };
    if compare(version, &target) == Ordering::Less {
        return false;
    }
    if component_count(spec) == Some(1) {
        return version.major == target.major;
    }
    version.major == target.major && version.minor == target.minor
}

fn compare(left: &Version, right: &Version) -> Ordering {
    (left.major, left.minor, left.patch)
        .cmp(&(right.major, right.minor, right.patch))
        .then_with(|| compare_prerelease(&left.prerelease, &right.prerelease))
}

fn compare_prerelease(left: &str, right: &str) -> Ordering {
    match (left.is_empty(), right.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => compare_identifiers(left, right),
    }
}

/// Numeric identifiers compare numerically and rank below alphanumeric ones,
/// as semver requires.
fn compare_identifiers(left: &str, right: &str) -> Ordering {
    let left: Vec<&str> = left.split('.').collect();
    let right: Vec<&str> = right.split('.').collect();
    for (left, right) in left.iter().zip(right.iter()) {
        let order = compare_identifier(left, right);
        if order != Ordering::Equal {
            return order;
        }
    }
    left.len().cmp(&right.len())
}

fn compare_identifier(left: &str, right: &str) -> Ordering {
    match (numeric(left), numeric(right)) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => left.cmp(right),
    }
}

fn numeric(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Strip surrounding whitespace, a leading `v`, and build metadata (`+sha`).
fn normalize(text: &str) -> String {
    let text = text.trim();
    let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
    match text.find('+') {
        Some(index) => text[..index].to_string(),
        None => text.to_string(),
    }
}

/// The `x.y.z` part of a version, without any prerelease suffix.
fn core_of(text: &str) -> &str {
    match text.find('-') {
        Some(index) => &text[..index],
        None => text,
    }
}

fn prerelease_of(text: &str) -> &str {
    match text.find('-') {
        Some(index) => &text[index + 1..],
        None => "",
    }
}

fn component_count(text: &str) -> Option<usize> {
    let count = core_of(text).split('.').count();
    (1..=3).contains(&count).then_some(count)
}

fn parse(text: &str) -> Option<Version> {
    let text = normalize(text);
    let core = core_of(&text);
    let mut components = core.split('.');
    let major = numeric(components.next()?)?;
    let minor = match components.next() {
        Some(component) => numeric(component)?,
        None => 0,
    };
    let patch = match components.next() {
        Some(component) => numeric(component)?,
        None => 0,
    };
    if components.next().is_some() {
        return None;
    }
    Some(Version {
        major,
        minor,
        patch,
        prerelease: prerelease_of(&text).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_versions_are_ranges_over_the_named_components() {
        assert!(satisfies("1.2.3", "1.2.3"));
        assert!(!satisfies("1.2.4", "1.2.3"));
        assert!(satisfies("1.2.9", "1.2"));
        assert!(!satisfies("1.3.0", "1.2"));
        assert!(satisfies("1.9.9", "1"));
        assert!(!satisfies("2.0.0", "1"));
    }

    #[test]
    fn caret_keeps_the_leftmost_non_zero_component() {
        assert!(satisfies("1.9.0", "^1.2.3"));
        assert!(!satisfies("2.0.0", "^1.2.3"));
        assert!(satisfies("0.1.9", "^0.1.0"));
        assert!(!satisfies("0.2.0", "^0.1.0"));
        assert!(satisfies("0.0.3", "^0.0.3"));
        assert!(!satisfies("0.0.4", "^0.0.3"));
    }

    #[test]
    fn tilde_keeps_the_minor_component() {
        assert!(satisfies("1.2.9", "~1.2.3"));
        assert!(!satisfies("1.3.0", "~1.2.3"));
        assert!(satisfies("1.9.0", "~1"));
        assert!(!satisfies("2.0.0", "~1"));
    }

    #[test]
    fn comparison_clauses_combine() {
        assert!(satisfies("0.1.0", ">=0.1.0"));
        assert!(!satisfies("0.0.9", ">=0.1.0"));
        assert!(satisfies("0.1.5", ">=0.1.0, <0.2.0"));
        assert!(!satisfies("0.2.0", ">=0.1.0, <0.2.0"));
        assert!(satisfies("1.0.0", "*"));
    }

    #[test]
    fn prereleases_only_match_ranges_that_name_one() {
        assert!(!satisfies("2.0.0-beta.1", ">=1.0.0, <2.0.0"));
        assert!(satisfies("2.0.0-beta.1", ">=2.0.0-beta.1"));
        assert!(satisfies("2.0.0-beta.2", ">2.0.0-beta.1"));
        assert!(satisfies("2.0.0", ">=2.0.0-beta.1"));
        assert!(!satisfies("2.0.0-alpha.1", ">=2.0.0-beta.1"));
    }

    #[test]
    fn build_metadata_and_v_prefix_are_ignored() {
        assert!(satisfies("v1.2.3+build.7", "1.2.3"));
    }

    #[test]
    fn the_compilers_own_prerelease_does_not_exclude_ranges() {
        assert!(compiler_satisfies(">=0.1.0", "0.2.0-beta.1"));
        assert!(compiler_satisfies("^0.2", "0.2.0-beta.1"));
        assert!(!compiler_satisfies(">=0.3.0", "0.2.0-beta.1"));
    }

    #[test]
    fn requirements_are_validated() {
        for valid in [
            "*",
            "^0.1",
            "~0.1.2",
            ">=0.1.0",
            ">=0.1, <0.2",
            "0.1.0",
            "=1.2",
        ] {
            assert!(is_valid_requirement(valid), "{valid}");
        }
        for invalid in ["", " ", "^", "abc", ">=0.1,", "1.2.3.4", "0.x.1"] {
            assert!(!is_valid_requirement(invalid), "{invalid}");
        }
    }

    #[test]
    fn malformed_requirements_never_match() {
        assert!(!satisfies("1.0.0", "abc"));
        assert!(!satisfies("1.0.0", ">=x"));
        assert!(!satisfies("1.0.0", "1.2.3.4"));
        assert!(satisfies("1.0.0", ""));
    }
}
