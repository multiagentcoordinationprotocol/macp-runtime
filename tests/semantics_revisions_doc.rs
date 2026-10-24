//! `docs/semantics-revisions.md` is the embedder-facing table of
//! `CURRENT_SEMANTICS_REV` values. Its rows must be exactly 0..=current, so a
//! revision bump that forgets the doc fails here rather than shipping an
//! embedder contract that is one revision behind.

use macp_runtime::macp_core::session::CURRENT_SEMANTICS_REV;
use std::path::Path;

#[test]
fn revision_table_lists_every_revision_up_to_current() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/semantics-revisions.md");
    let doc = std::fs::read_to_string(&path).expect("read docs/semantics-revisions.md");

    // A table row is `| <rev> | <release> | <gates> |`.
    let revs: Vec<u32> = doc
        .lines()
        .filter_map(|line| line.strip_prefix("| ")?.split(" |").next()?.parse().ok())
        .collect();

    let expected: Vec<u32> = (0..=CURRENT_SEMANTICS_REV).collect();
    assert_eq!(
        revs, expected,
        "docs/semantics-revisions.md must have one row per revision, in order, \
         up to CURRENT_SEMANTICS_REV = {CURRENT_SEMANTICS_REV}"
    );
}
