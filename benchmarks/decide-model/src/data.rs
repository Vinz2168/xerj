//! The public labelled data: readers, splits, and the licence record.
//!
//! Sources are exactly `benchmarks/decisions-as-retrieval/load.py`'s, so the
//! head's numbers and the history-vote numbers read the same rows:
//!
//! * **Banking77** — `PolyAI-LDN/task-specific-datasets` `banking_data/`
//!   (`train.csv` 10,003 rows, `test.csv` 3,080 rows, 77 intents). The
//!   repository's `LICENSE` file is Creative Commons Attribution 4.0
//!   International; GitHub's licence detector reports the same
//!   (`license.key == "cc-by-4.0"`). Canonical paper: Casanueva et al.,
//!   *Efficient Intent Detection with Dual Sentence Encoders*, 2020
//!   (arXiv:2003.04807), which the repository asks to be cited.
//! * **SMS Spam Collection** — `sms.tsv` as mirrored by
//!   `justmarkham/pycon-2016-tutorial`, the file `load.py` fetches. The
//!   dataset is the UCI ML Repository's *SMS Spam Collection* (donated by
//!   Tiago Almeida and José Hidalgo, 2012; DOI 10.24432/C5CC84), licensed
//!   **CC BY 4.0** per the UCI dataset page.
//!
//! Both are CC BY 4.0: the trained weights are Apache-2.0 *from us*, and
//! these two statements ride alongside as training-data provenance with the
//! attribution the licence asks for — they are not a grant we make.

use std::path::Path;

use anyhow::{bail, Context, Result};

/// One labelled example.
#[derive(Debug, Clone)]
pub struct Item {
    pub text: String,
    pub label: String,
}

/// A dataset: its items plus its label vocabulary in first-seen order.
#[derive(Debug, Clone)]
pub struct Dataset {
    pub name: &'static str,
    pub items: Vec<Item>,
    /// Label vocabulary, first-seen order — the order `choice` scoring uses.
    pub labels: Vec<String>,
}

impl Dataset {
    /// Index of `label` in the vocabulary.
    pub fn label_index(&self, label: &str) -> Option<usize> {
        self.labels.iter().position(|l| l == label)
    }
}

/// Minimal CSV reader for these two files: no quoted-field complexity beyond
/// what they contain (Banking77 quotes fields that contain commas; RFC-4180
/// style). Deterministic, allocation-light, stdlib only.
fn read_csv_rows(path: &Path) -> Result<Vec<Vec<String>>> {
    let raw = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let mut rows = Vec::new();
    let mut field = String::new();
    let mut row: Vec<String> = Vec::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                row.push(std::mem::take(&mut field));
            }
            '\n' if !in_quotes => {
                row.push(std::mem::take(&mut field));
                if row.iter().any(|f| !f.trim().is_empty()) {
                    rows.push(std::mem::take(&mut row));
                } else {
                    row.clear();
                }
            }
            '\r' => {}
            other => field.push(other),
        }
    }
    if !field.is_empty() || row.len() > 1 {
        row.push(field);
        if row.iter().any(|f| !f.trim().is_empty()) {
            rows.push(row);
        }
    }
    Ok(rows)
}

/// Banking77 `train.csv`/`test.csv`: header `text,category`.
pub fn load_banking(path: &Path) -> Result<Dataset> {
    let rows = read_csv_rows(path)?;
    let (Some(header), rows) = (rows.first(), rows[1..].to_vec()) else {
        bail!("{} is empty", path.display());
    };
    let text_col = header
        .iter()
        .position(|h| h.trim() == "text")
        .context("banking csv has no text column")?;
    let label_col = header
        .iter()
        .position(|h| h.trim() == "category")
        .context("banking csv has no category column")?;
    let mut items = Vec::with_capacity(rows.len());
    let mut labels: Vec<String> = Vec::new();
    for row in &rows {
        let text = row
            .get(text_col)
            .with_context(|| format!("banking row missing text column: {row:?}"))?
            .trim()
            .to_string();
        let label = row
            .get(label_col)
            .with_context(|| format!("banking row missing category column: {row:?}"))?
            .trim()
            .to_string();
        if text.is_empty() || label.is_empty() {
            continue;
        }
        if !labels.contains(&label) {
            labels.push(label.clone());
        }
        items.push(Item { text, label });
    }
    Ok(Dataset {
        name: "banking77",
        items,
        labels,
    })
}

/// `sms.tsv`: `label\ttext`, no header, `ham`/`spam`.
pub fn load_sms(path: &Path) -> Result<Dataset> {
    let raw = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let mut items = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let Some((label, body)) = line.split_once('\t') else {
            continue;
        };
        let label = label.trim().to_string();
        let body = body.trim().to_string();
        if body.is_empty() {
            continue;
        }
        if !labels.contains(&label) {
            labels.push(label.clone());
        }
        items.push(Item { text: body, label });
    }
    // The history-vote benchmark indexed the FIRST 4,000 rows of the
    // seed-7 shuffle and held the rest out; mirror that split so both
    // surfaces are judged on the same rows. Shuffling happens here, in the
    // loader, from a fixed seed — not at the call site.
    let mut order: Vec<usize> = (0..items.len()).collect();
    let items = {
        let mut rng = crate::rng::Rng::new(7);
        rng.shuffle(&mut order);
        order
            .into_iter()
            .map(|i| items[i].clone())
            .collect::<Vec<_>>()
    };
    Ok(Dataset {
        name: "sms",
        items,
        labels,
    })
}

/// The SMS train/test split `load.py` established: first 4,000 shuffled rows
/// are the history index (our training rows), the rest are the held-out test.
pub const SMS_TRAIN_ROWS: usize = 4_000;

/// Both datasets' expected sha256 — the fetch step verifies, so a silently
/// changed upstream file fails the run instead of training on unknown data.
pub const EXPECTED_SHA256: &[(&str, &str)] = &[
    (
        "b77_train.csv",
        "b06e26ac675513959a63135f11b94ea7786ed02da65db93a5650d8838cbc664b",
    ),
    (
        "b77_test.csv",
        "d12d6e3bc4c3103966ae786dc435913c0c563dfa328f5a3646d0e62cfeeb474d",
    ),
    (
        "sms.tsv",
        "7d039a24a6083ed9ef0f806ebad56bbb976e3aeb8de05669173bfdc4996c239d",
    ),
];

/// sha256 hex of a file, lowercase.
pub fn sha256_file(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

/// Verify every expected file against its recorded digest.
pub fn verify_data_dir(dir: &Path) -> Result<Vec<String>> {
    let mut checked = Vec::new();
    for (name, want) in EXPECTED_SHA256 {
        let path = dir.join(name);
        let got = sha256_file(&path).with_context(|| {
            format!("{} is missing (run scripts/fetch_data.sh)", path.display())
        })?;
        if &got != want {
            bail!(
                "{name} sha256 {got} != recorded {want}: upstream changed or the download is \
                 partial; re-verify the source before training on it"
            );
        }
        checked.push(format!("{name} {want}"));
    }
    Ok(checked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_handles_quoted_commas_and_newlines() {
        let dir = tempfile_dir();
        let path = dir.join("q.csv");
        std::fs::write(
            &path,
            "text,category\n\"yes, but\",a\nplain,b\n\"multi\nline\",c\n",
        )
        .unwrap();
        let rows = read_csv_rows(&path).unwrap();
        assert_eq!(rows.len(), 4, "header + 3 rows: {rows:?}");
        assert_eq!(rows[1][0], "yes, but");
        assert_eq!(rows[2][0], "plain");
        assert_eq!(rows[3][0], "multi\nline");
    }

    #[test]
    fn sms_rows_are_label_then_text() {
        let dir = tempfile_dir();
        let path = dir.join("s.tsv");
        std::fs::write(&path, "ham\thi there\nspam\twin now\r\n").unwrap();
        let ds = load_sms(&path).unwrap();
        // `load_sms` shuffles with the seed-7 stream (mirroring load.py), so
        // assert the row↔label pairing, not the order the shuffle produced.
        assert_eq!(ds.items.len(), 2);
        for item in &ds.items {
            let expected = if item.label == "ham" {
                "hi there"
            } else {
                "win now"
            };
            assert_eq!(item.text, expected, "pairing broken for {}", item.label);
        }
        assert_eq!(ds.labels.len(), 2);
        assert!(ds.labels.contains(&"ham".to_string()));
        assert!(ds.labels.contains(&"spam".to_string()));
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("decide-model-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
