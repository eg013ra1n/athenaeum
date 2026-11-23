//! The canonical `ROWORDER` spellings, which a writer stamps when it has to state the row order explicitly.

/// The canonical `ROWORDER` spellings, so a writer that has to state the
/// order explicitly (the XISF writer) spells it the same way the row-order
/// rule reads it.
pub const ROW_ORDER_TOP_DOWN: &str = "TOP-DOWN";
/// The spelling of a bottom-up row order (row 0 is the bottom row of the sky image), the `ROWORDER` card value.
pub const ROW_ORDER_BOTTOM_UP: &str = "BOTTOM-UP";
