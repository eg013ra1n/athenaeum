/// Module for computing header fingerprints for file relinking
///
/// This module provides functionality to create unique fingerprints from FITS header data.
/// The fingerprints are used to match files when directories are moved or renamed.

use xxhash_rust::xxh3::xxh3_64;

/// Compute a fingerprint hash from a FITS header JSON string.
///
/// Uses XXH3_64 to generate a unique 64-bit hash of the complete header content.
/// This fingerprint is used to match files when their locations change but their
/// content remains the same.
///
/// # Arguments
///
/// * `header_json` - The complete FITS header as a JSON string
///
/// # Returns
///
/// A hexadecimal string representation of the 64-bit hash (16 characters)
///
/// # Example
///
/// ```ignore
/// let header = r#"{"SIMPLE": true, "BITPIX": -32, "NAXIS": 2}"#;
/// let fingerprint = compute_header_fingerprint(header);
/// println!("Fingerprint: {}", fingerprint);
/// ```
pub fn compute_header_fingerprint(header_json: &str) -> String {
    let hash = xxh3_64(header_json.as_bytes());
    format!("{:016x}", hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_header_fingerprint() {
        let header = r#"{"SIMPLE": true, "BITPIX": -32}"#;
        let fingerprint = compute_header_fingerprint(header);

        // Should return a 16-character hex string
        assert_eq!(fingerprint.len(), 16);
        assert!(fingerprint.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_fingerprint_consistency() {
        let header = r#"{"SIMPLE": true, "BITPIX": -32}"#;
        let fp1 = compute_header_fingerprint(header);
        let fp2 = compute_header_fingerprint(header);

        // Same input should always produce same fingerprint
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn test_different_headers_different_fingerprints() {
        let header1 = r#"{"SIMPLE": true, "BITPIX": -32}"#;
        let header2 = r#"{"SIMPLE": true, "BITPIX": -64}"#;

        let fp1 = compute_header_fingerprint(header1);
        let fp2 = compute_header_fingerprint(header2);

        // Different headers should produce different fingerprints
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn test_empty_header() {
        let header = "";
        let fingerprint = compute_header_fingerprint(header);

        // Should still produce valid fingerprint for empty string
        assert_eq!(fingerprint.len(), 16);
    }
}
