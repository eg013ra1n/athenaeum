//! Round-trip + validation tests for the `package` module (task A3).
//!
//! Payloads are tiny but *real* FITS files: fabricated in-test via `fits_writer`
//! (BITPIX=-32 primary HDU), the sanctioned in-repo way to make a valid FITS
//! without committing a binary fixture.

use std::path::Path;

use tempfile::tempdir;

use crate::fits_writer::{write_fits_f32, Card, CardValue};
use crate::models::Frame;

use super::manifest::MANIFEST_VERSION;
use super::{
    read_manifest, validate_package, validate_package_id, validate_rel_path, write_package,
    write_package_with_root_hash, xxh3_full_file, ManifestRecord, PayloadKind, MANIFEST_FILENAME,
};

/// Fabricate a tiny valid FITS (4x4 float image) at `path`.
fn write_fixture_fits(path: &Path) {
    let data: Vec<f32> = (0..16).map(|i| i as f32 * 0.5).collect();
    let cards = vec![
        Card::new("IMAGETYP", CardValue::Str("Light Frame".into())).unwrap(),
        Card::new("EXPTIME", CardValue::Real(120.0)).unwrap(),
    ];
    write_fits_f32(path, 4, 4, 1, &data, &cards).unwrap();
}

/// Build a fully-formed manifest record for a source file, mirroring what a
/// producer would supply (byte_size + full-content xxh3 computed from disk).
fn sample_record(src: &Path, rel_path: &str) -> ManifestRecord {
    let byte_size = std::fs::metadata(src).unwrap().len();
    let xxh3 = xxh3_full_file(src).unwrap();
    let frame = Frame {
        object: Some("M42".to_string()),
        ..Frame::default()
    };
    ManifestRecord {
        v: MANIFEST_VERSION,
        frame_uuid: "frame-uuid-0001".to_string(),
        origin_catalog_uuid: "catalog-uuid-0001".to_string(),
        origin_device: "ab".repeat(32), // plausible 64-char hex NodeId
        payload_kind: PayloadKind::RawFrame,
        rel_path: rel_path.to_string(),
        byte_size,
        xxh3,
        frame_meta: serde_json::to_value(&frame).unwrap(),
        analysis: None,
        app_version: "0.4.0".to_string(),
        project: None,
    }
}

#[test]
fn package_roundtrip_manifest_matches() {
    let src_dir = tempdir().unwrap();
    let src = src_dir.path().join("light_0001.fits");
    write_fixture_fits(&src);

    let rel_path = "frames/light_0001.fits";
    let record = sample_record(&src, rel_path);

    let dest = tempdir().unwrap();
    let announce = write_package(dest.path(), vec![(src.clone(), record.clone())]).unwrap();

    // Payload copied to its rel_path under the package dir.
    let copied = dest.path().join(rel_path);
    assert!(copied.exists(), "payload copied to rel_path");

    // Announce reflects the package.
    assert_eq!(announce.frame_count, 1);
    assert_eq!(announce.byte_size, record.byte_size);
    assert!(!announce.root_hash.is_empty(), "root_hash produced");

    // Manifest reads back byte-for-byte equal to what we wrote.
    let read = read_manifest(dest.path()).unwrap();
    assert_eq!(read, vec![record.clone()]);

    // xxh3 recomputes on the copied file.
    assert_eq!(xxh3_full_file(&copied).unwrap(), record.xxh3);

    // Clean package validates.
    validate_package(dest.path()).unwrap();
}

#[test]
fn validate_catches_corruption() {
    let src_dir = tempdir().unwrap();
    let src = src_dir.path().join("light.fits");
    write_fixture_fits(&src);
    let rel_path = "frames/light.fits";
    let record = sample_record(&src, rel_path);

    let dest = tempdir().unwrap();
    write_package(dest.path(), vec![(src.clone(), record)]).unwrap();
    validate_package(dest.path()).unwrap(); // healthy first

    // Flip one byte in the copied payload — length unchanged, content differs.
    let copied = dest.path().join(rel_path);
    let mut bytes = std::fs::read(&copied).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    std::fs::write(&copied, &bytes).unwrap();

    let err = validate_package(dest.path()).unwrap_err();
    assert!(
        err.to_string().contains(rel_path),
        "error must name the corrupt rel_path, got: {err}"
    );
}

#[test]
fn root_hash_provider_overrides_placeholder() {
    let src_dir = tempdir().unwrap();
    let src = src_dir.path().join("light.fits");
    write_fixture_fits(&src);
    let record = sample_record(&src, "frames/light.fits");

    let dest = tempdir().unwrap();
    // The provider sees the fully-written package dir (manifest present) and
    // supplies the opaque root_hash — this is the seam A5's iroh transport uses
    // to inject the collection hash.
    let provider = |dir: &std::path::Path| {
        assert!(dir.join(MANIFEST_FILENAME).exists(), "manifest written before provider runs");
        Ok("collection-hash-stub".to_string())
    };
    let announce =
        write_package_with_root_hash(dest.path(), vec![(src, record)], Some(&provider)).unwrap();

    assert_eq!(announce.root_hash, "collection-hash-stub");
    // The package still validates — the provider only changes the announce field.
    validate_package(dest.path()).unwrap();
}

// ── path-safety guards (C1 / L1) ─────────────────────────────────────────────

#[test]
fn validate_package_id_accepts_uuids_and_simple_ids() {
    // The sender always mints a v4 UUID; that (and plain alphanumeric/`-`/`_`
    // ids) must pass.
    validate_package_id("550e8400-e29b-41d4-a716-446655440000").unwrap();
    validate_package_id("pkg_42").unwrap();
    validate_package_id("ABCdef0123").unwrap();
}

#[test]
fn validate_package_id_rejects_path_components_and_traversal() {
    // A peer-supplied package_id is used to build the receiver's staging dir
    // (receiver.rs). It must never be allowed to carry a path separator, a
    // parent-dir escape, an absolute root, or a Windows drive/UNC prefix — any
    // of which would let a malicious announce place the fetched package outside
    // the staging root (arbitrary file write / RCE, finding C1).
    for bad in [
        "",
        ".",
        "..",
        "a/b",
        "../../etc/cron.d",
        "/Users/victim/Library/LaunchAgents",
        "..\\..\\windows",
        "C:\\windows\\system32",
        "\\\\host\\share",
        "pkg\0null",
    ] {
        assert!(
            validate_package_id(bad).is_err(),
            "package_id must be rejected as unsafe: {bad:?}"
        );
    }
}

#[test]
fn validate_rel_path_rejects_backslash_and_drive_letters_cross_platform() {
    // The guard runs on the receiver's own platform, but a Unix receiver parses
    // `..\..\x` and `C:\x` as a single Normal component, so a Windows-style
    // traversal would slip through if we only relied on `Path::components`.
    // Reject backslashes, drive-letter, and UNC prefixes independent of host
    // (finding L1).
    for bad in [
        "..\\..\\secret",
        "C:\\Windows\\System32\\x",
        "\\\\host\\share\\x",
        "a\\b",
    ] {
        assert!(
            validate_rel_path(bad).is_err(),
            "rel_path with a Windows separator/prefix must be rejected: {bad:?}"
        );
    }
    // Forward-slash relative paths (the wire format) still pass.
    validate_rel_path("frames/light_0001.fits").unwrap();
}

#[test]
fn validate_package_rejects_traversal_rel_path() {
    // `validate_package` is the "verify an untrusted package" helper; it must
    // refuse a manifest record whose rel_path escapes the package dir rather
    // than stat/hash an arbitrary file on disk (finding L1, latent hardening).
    let dir = tempdir().unwrap();
    let placeholder = dir.path().join("placeholder.fits");
    write_fixture_fits(&placeholder);
    let record = ManifestRecord {
        rel_path: "../../../../etc/hosts".to_string(),
        ..sample_record(&placeholder, "unused")
    };
    let line = serde_json::to_string(&record).unwrap();
    std::fs::write(dir.path().join(MANIFEST_FILENAME), format!("{line}\n")).unwrap();

    let err = validate_package(dir.path()).expect_err("traversal rel_path must be rejected");
    assert!(
        format!("{err:#}").contains("rel_path"),
        "error should name the rel_path guard: {err:#}"
    );
}

#[test]
fn manifest_forward_compat_unknown_field_ok() {
    let src_dir = tempdir().unwrap();
    let src = src_dir.path().join("light.fits");
    write_fixture_fits(&src);
    let record = sample_record(&src, "frames/light.fits");

    // Serialize the record, then inject an unknown key a future schema might add.
    let mut value = serde_json::to_value(&record).unwrap();
    value.as_object_mut().unwrap().insert(
        "futureField".to_string(),
        serde_json::json!({ "nested": [1, 2, 3] }),
    );
    let line = serde_json::to_string(&value).unwrap();

    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join(MANIFEST_FILENAME), format!("{line}\n")).unwrap();

    let read = read_manifest(dir.path()).unwrap();
    assert_eq!(
        read,
        vec![record],
        "unknown fields ignored; known fields intact"
    );
}
