//! Prepared-frame bookkeeping for reviewed publishing (spec 2026-10-01).

use crate::services::ServiceContext;

/// Delete prepared rows and files of frames withheld while a run was active
/// (plan W3). Filled in by the withhold task; a no-op until then.
pub(crate) fn drop_withheld_prepared(ctx: &ServiceContext, project_id: &str) {
    let _ = (ctx, project_id);
}
