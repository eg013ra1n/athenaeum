// Diffs the ts-rs-generated TS declarations against the checked-in files
// under src/types/. Regenerate with:
//   TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract
use std::path::Path;

#[test]
fn ts_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src/types");
    let write = std::env::var("TS_RS_WRITE").is_ok();
    let mut stale: Vec<String> = Vec::new();
    for (rel, content) in athenaeum_core::ts_export::generated_files() {
        let path = root.join(rel);
        if write {
            std::fs::write(&path, &content).unwrap();
            continue;
        }
        let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
        if on_disk != content {
            stale.push(rel.to_string());
        }
    }
    assert!(
        stale.is_empty(),
        "stale generated TS files: {stale:?}\nRegenerate: TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract"
    );
}
