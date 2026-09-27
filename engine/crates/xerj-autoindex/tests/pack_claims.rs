//! The rust-vulns pack's public numbers must be numbers a real build produced.
//!
//! Same discipline as `security_tooling_claims.rs` (issue #207's lesson): a
//! README that advertises record counts nobody measured is a worse failure
//! than admitting the gap. `tools/packs/rust-vulns/pack-stats.json` is the
//! committed record of a measured build (`provenance: "measured"`); these
//! tests fail when the README, the stats and the recipe disagree — so a
//! rebuild with different numbers forces the docs to move with them, and a
//! recipe edit without a rebuild fails the sha pin.

use std::path::{Path, PathBuf};

/// Walk up from this crate to the repository root. Panics rather than
/// skipping: a claims check that turns into a no-op when it cannot find its
/// inputs is the tautological pass this file exists to prevent.
fn repo_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if dir.join(".github/workflows/ci.yml").is_file() {
            return dir;
        }
        assert!(
            dir.pop(),
            "no .github/workflows/ci.yml above {} — this test must run from a \
             repository checkout, and must not pass by failing to look",
            env!("CARGO_MANIFEST_DIR"),
        );
    }
}

fn pack_dir(root: &Path) -> PathBuf {
    root.join("tools/packs/rust-vulns")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn stats(root: &Path) -> serde_json::Value {
    let dir = pack_dir(root);
    let text = read(&dir.join("pack-stats.json"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("pack-stats.json is not valid JSON: {e}"))
}

/// The README's table row for `label` must carry exactly the number in
/// pack-stats.json under `key`.
fn readme_row_pins(root: &Path, label: &str, key: &str) {
    let readme = read(&pack_dir(root).join("README.md"));
    let want = stats(root)
        .get(key)
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| panic!("pack-stats.json has no numeric '{key}'"));
    // rows look like `| … label … | 1950 |` or `| … label … | 270 (707 …) |`,
    // so pin on maximal digit runs, not whole cells
    let needle = label.to_string();
    let row = readme
        .lines()
        .find(|l| l.contains(&needle))
        .unwrap_or_else(|| panic!("README has no row containing '{needle}'"));
    let numbers: Vec<String> = row
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    assert!(
        numbers.contains(&want.to_string()),
        "README row '{needle}' does not carry {want} (pack-stats.{key}); rebuild \
         the pack and move the docs with the numbers",
    );
}

#[test]
fn pack_stats_are_measured_not_asserted() {
    let root = repo_root();
    let s = stats(&root);
    assert_eq!(
        s["provenance"], "measured",
        "pack-stats.json must record provenance 'measured' — 'estimated' numbers \
         are exactly what this file exists to keep out of the README"
    );
    for key in [
        "envelopes",
        "records",
        "merged_from_both_sources",
        "records_with_functions",
        "function_paths_distinct",
        "function_entries_total",
        "records_withdrawn",
        "packages",
    ] {
        assert!(
            s.get(key).and_then(|v| v.as_u64()).is_some_and(|v| v > 0),
            "pack-stats.json lacks a positive numeric '{key}'"
        );
    }
    // internal consistency: the parts cannot exceed the whole
    let records = s["records"].as_u64().unwrap();
    let merged = s["merged_from_both_sources"].as_u64().unwrap();
    let with_fn = s["records_with_functions"].as_u64().unwrap();
    assert!(merged <= records, "more merged records than records");
    assert!(with_fn <= records, "more function records than records");
}

#[test]
fn readme_counts_match_the_measured_stats() {
    let root = repo_root();
    readme_row_pins(&root, "Source records harvested", "envelopes");
    readme_row_pins(&root, "Records after identity resolution", "records");
    readme_row_pins(
        &root,
        "…merged from both sources",
        "merged_from_both_sources",
    );
    readme_row_pins(
        &root,
        "Records with affected-function paths",
        "records_with_functions",
    );
    readme_row_pins(
        &root,
        "Records with affected-function paths",
        "function_paths_distinct",
    );
    readme_row_pins(&root, "Records withdrawn", "records_withdrawn");
    readme_row_pins(&root, "Packages covered", "packages");
}

#[test]
fn the_recipe_is_the_one_the_stats_measured() {
    let root = repo_root();
    let dir = pack_dir(&root);
    let recipe = read(&dir.join("recipe.toml"));
    let mut hasher = sha2::Sha256::new();
    use sha2::Digest;
    hasher.update(recipe.as_bytes());
    let got = format!("{:x}", hasher.finalize());
    let want = stats(&root)["recipe_sha256"]
        .as_str()
        .expect("pack-stats.json pins recipe_sha256")
        .to_string();
    assert_eq!(
        got, want,
        "recipe.toml changed since pack-stats.json was measured — rebuild the \
         pack and refresh pack-stats.json (and the README numbers with it)"
    );

    // and the stats' source table names exactly the recipe's sources
    for slug in ["rustsec", "osv"] {
        assert!(
            recipe.contains(&format!("slug = \"{slug}\"")),
            "recipe no longer declares source '{slug}'"
        );
        assert!(
            stats(&root)["sources"][slug].is_object(),
            "pack-stats.json no longer records source '{slug}'"
        );
    }
    // every licence in the recipe is CC0; the README's licence claims must
    // not silently grow a restricted term
    assert!(
        !recipe.contains("CC-BY") && !recipe.contains("BUSL") && !recipe.contains("GPL"),
        "the pack README says all record data is CC0; a new source with a \
         different licence must move that claim too"
    );
}
