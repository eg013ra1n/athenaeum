//! Link-mode placement, with the fallback Windows needs.
//!
//! Windows refuses a symbolic link with os error 1314 ("A required privilege is not held by the
//! client") unless Developer Mode is on or the process is elevated. Link mode then makes a hard link,
//! which needs no privilege on NTFS when the source and the export folder share a volume, and copies
//! when that fails too (another volume, FAT/exFAT, a network share). Every replacement is counted in
//! [`LinkFallbackCounts`] so the export result can say the links were replaced.
//!
//! Every way here CREATES the destination; none opens an existing file for writing. The callers clear
//! a link at the destination first (or place onto a fresh temp name), so nothing is written through one.
use std::io;
use std::path::Path;

use anyhow::{Context, Result};

use crate::export::models::LinkFallbackCounts;

/// Windows `ERROR_PRIVILEGE_NOT_HELD`: a symbolic link without Developer Mode or elevation.
const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;

/// How one link-mode placement landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Symlink,
    HardLink,
    Copy,
}

/// One export run's link placer. After the first refused symbolic link the rest of the run goes
/// straight to a hard link; `counts` holds the run's replacements.
#[derive(Debug, Default)]
pub struct Linker {
    symlinks_refused: bool,
    hard_link_failed: bool,
    pub counts: LinkFallbackCounts,
}

impl Linker {
    /// Link `dest` to `src`. `copy` is the caller's own copy of `src` to the final destination, run only
    /// when neither link can be made.
    pub fn link(
        &mut self,
        src: &Path,
        dest: &Path,
        copy: impl FnOnce() -> Result<()>,
    ) -> Result<LinkKind> {
        self.link_with(src, dest, os_symlink, |s, d| std::fs::hard_link(s, d), copy)
    }

    fn link_with(
        &mut self,
        src: &Path,
        dest: &Path,
        symlink: impl FnOnce(&Path, &Path) -> io::Result<()>,
        hard_link: impl FnOnce(&Path, &Path) -> io::Result<()>,
        copy: impl FnOnce() -> Result<()>,
    ) -> Result<LinkKind> {
        if !self.symlinks_refused {
            match symlink(src, dest) {
                Ok(()) => return Ok(LinkKind::Symlink),
                Err(e) if e.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD) => {
                    tracing::info!(
                        error = %e,
                        "export: symbolic links not allowed; hard links or copies instead"
                    );
                    self.symlinks_refused = true;
                }
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!("Failed to symlink {} -> {}", src.display(), dest.display())
                    })
                }
            }
        }
        match hard_link(src, dest) {
            Ok(()) => {
                self.counts.hard_links += 1;
                Ok(LinkKind::HardLink)
            }
            Err(e) => {
                // The first failure says why; the rest of a run on the same volume would repeat it.
                if self.hard_link_failed {
                    tracing::debug!(src = %src.display(), error = %e, "export: hard link failed; copying");
                } else {
                    tracing::info!(src = %src.display(), error = %e, "export: hard link failed; copying");
                    self.hard_link_failed = true;
                }
                copy()?;
                self.counts.copies += 1;
                Ok(LinkKind::Copy)
            }
        }
    }
}

fn os_symlink(src: &Path, dest: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(src, dest)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(src, dest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn refused() -> io::Result<()> {
        Err(io::Error::from_raw_os_error(ERROR_PRIVILEGE_NOT_HELD))
    }

    fn other_error() -> io::Error {
        io::Error::new(io::ErrorKind::Unsupported, "no links here")
    }

    #[test]
    fn a_symbolic_link_that_works_is_not_counted() {
        let mut l = Linker::default();
        let kind = l
            .link_with(
                Path::new("a"),
                Path::new("b"),
                |_, _| Ok(()),
                |_, _| panic!("no hard link after a symbolic link"),
                || panic!("no copy after a symbolic link"),
            )
            .unwrap();
        assert_eq!(kind, LinkKind::Symlink);
        assert_eq!(l.counts, LinkFallbackCounts::default());
    }

    #[test]
    fn refused_symbolic_link_makes_a_hard_link_and_is_not_tried_again() {
        let mut l = Linker::default();
        let symlink_calls = Cell::new(0);
        for _ in 0..3 {
            let kind = l
                .link_with(
                    Path::new("a"),
                    Path::new("b"),
                    |_, _| {
                        symlink_calls.set(symlink_calls.get() + 1);
                        refused()
                    },
                    |_, _| Ok(()),
                    || panic!("no copy after a hard link"),
                )
                .unwrap();
            assert_eq!(kind, LinkKind::HardLink);
        }
        assert_eq!(symlink_calls.get(), 1, "the refusal is remembered for the run");
        assert_eq!(l.counts, LinkFallbackCounts { hard_links: 3, copies: 0 });
    }

    #[test]
    fn a_failed_hard_link_copies() {
        let mut l = Linker::default();
        let copied = Cell::new(false);
        let kind = l
            .link_with(
                Path::new("a"),
                Path::new("b"),
                |_, _| refused(),
                |_, _| Err(other_error()),
                || {
                    copied.set(true);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(kind, LinkKind::Copy);
        assert!(copied.get());
        assert_eq!(l.counts, LinkFallbackCounts { hard_links: 0, copies: 1 });
    }

    #[test]
    fn another_symbolic_link_error_is_an_error() {
        let mut l = Linker::default();
        let err = l
            .link_with(
                Path::new("a"),
                Path::new("b"),
                |_, _| Err(other_error()),
                |_, _| panic!("only a refusal falls back"),
                || panic!("only a refusal falls back"),
            )
            .unwrap_err();
        assert!(format!("{err:#}").contains("Failed to symlink"), "{err:#}");
        assert_eq!(l.counts, LinkFallbackCounts::default());
    }

    #[test]
    fn a_failed_copy_is_an_error_and_not_counted() {
        let mut l = Linker::default();
        let err = l
            .link_with(
                Path::new("a"),
                Path::new("b"),
                |_, _| refused(),
                |_, _| Err(other_error()),
                || Err(anyhow::anyhow!("disk full")),
            )
            .unwrap_err();
        assert!(format!("{err:#}").contains("disk full"));
        assert_eq!(l.counts, LinkFallbackCounts::default());
    }

    /// The fallback on a real disk: a refused symbolic link becomes a real hard link with the source's
    /// bytes, and replacing it later (temp + rename, as both organizers do) leaves the source untouched.
    #[test]
    fn refused_symbolic_link_on_a_real_disk_is_a_hard_link() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("light_1.fits");
        let dest = tmp.path().join("export_light_1.fits");
        std::fs::write(&src, b"light-bytes").unwrap();
        let mut l = Linker::default();
        let kind = l
            .link_with(
                &src,
                &dest,
                |_, _| refused(),
                |s, d| std::fs::hard_link(s, d),
                || panic!("same volume: no copy"),
            )
            .unwrap();
        assert_eq!(kind, LinkKind::HardLink);
        assert!(!std::fs::symlink_metadata(&dest).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read(&dest).unwrap(), b"light-bytes");

        let tmp_name = tmp.path().join("export_light_1.fits.tmp");
        std::fs::write(&tmp_name, b"other-bytes").unwrap();
        std::fs::rename(&tmp_name, &dest).unwrap();
        assert_eq!(std::fs::read(&src).unwrap(), b"light-bytes", "the source is untouched");
    }
}
