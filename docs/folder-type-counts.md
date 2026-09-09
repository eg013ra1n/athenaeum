# Folder frame-type counts

File Manager → Folders → select a monitored or role folder. **Frame types by
folder** shows This folder only, Including subfolders, and each immediate child
subtree. Click a child to drill down; Back one folder returns. Refresh counts
rereads the catalog, and scan-state changes refresh the panel automatically.

Columns: Total, Lights, Darks, Flats, Bias, Dark flats, Masters and Unknown.
Masters means explicitly typed MasterLight/MasterDark/etc; processing-stage
assessment is a separate concept. A file counts once even if multiple frame
rows reference it. Confirmed exposure versions remain separate file counts.
Unknown includes files without frame metadata or recognized type.

Counts reflect scanned catalog paths, including offline/missing files. No disk
traversal or symlink following is performed. Noncataloged empty directories are
not listed as children. Scan new files to include them; Refresh counts alone is
not a filesystem scan. Subtree matching treats percent/underscore literally and
excludes similarly named siblings. Desktop and web share the same handler.
