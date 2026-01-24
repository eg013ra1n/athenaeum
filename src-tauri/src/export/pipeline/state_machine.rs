//! State machine for export job management
//!
//! Handles job creation, status updates, and step transitions.

use anyhow::Result;
use rusqlite::Connection;
use chrono::Utc;

use super::models::{
    ExportJob, ExportJobStep, JobStatus, StepStatus, StepType, PipelinePlan,
};
use crate::export::models::ExportConfig;

// ============================================================================
// Job Creation
// ============================================================================

/// Create a new export job with all planned steps
pub fn create_export_job(
    conn: &Connection,
    frame_set_id: i64,
    output_dir: &str,
    config: &ExportConfig,
    plan: &PipelinePlan,
) -> Result<ExportJob> {
    let target = match config.target {
        crate::export::models::ExportTarget::Siril => "siril",
        crate::export::models::ExportTarget::PixInsightWBPP => "pixinsight_wbpp",
    };

    let config_json = serde_json::to_string(config)?;
    let now = Utc::now().to_rfc3339();
    let total_steps = plan.steps.len() as i32;

    // Insert the job
    conn.execute(
        "INSERT INTO export_jobs (frame_set_id, output_dir, target, config_json, status, created_at, total_steps, completed_steps)
         VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?6, 0)",
        rusqlite::params![frame_set_id, output_dir, target, config_json, now, total_steps],
    )?;

    let job_id = conn.last_insert_rowid();

    // Insert all steps
    for (idx, planned_step) in plan.steps.iter().enumerate() {
        let step_order = (idx + 1) as i32;
        let step_data_json = planned_step.step_data.as_ref()
            .map(|d| serde_json::to_string(d).ok())
            .flatten();

        conn.execute(
            "INSERT INTO export_job_steps (job_id, step_order, step_type, step_name, step_data_json, status, retry_count)
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending', 0)",
            rusqlite::params![
                job_id,
                step_order,
                planned_step.step_type.to_str(),
                planned_step.step_name,
                step_data_json,
            ],
        )?;
    }

    // Load and return the full job
    get_job_with_steps(conn, job_id)
}

// ============================================================================
// Job Queries
// ============================================================================

/// Get a job with all its steps
pub fn get_job_with_steps(conn: &Connection, job_id: i64) -> Result<ExportJob> {
    // Get the job
    let job = conn.query_row(
        "SELECT id, frame_set_id, output_dir, target, config_json, status,
                created_at, started_at, completed_at, error_message, total_steps, completed_steps
         FROM export_jobs WHERE id = ?1",
        [job_id],
        |row| {
            let config_json: String = row.get(4)?;
            let status_str: String = row.get(5)?;

            Ok(ExportJob {
                id: row.get(0)?,
                frame_set_id: row.get(1)?,
                output_dir: row.get(2)?,
                target: row.get(3)?,
                config: serde_json::from_str(&config_json).unwrap_or_default(),
                status: JobStatus::from_str(&status_str),
                created_at: row.get(6)?,
                started_at: row.get(7)?,
                completed_at: row.get(8)?,
                error_message: row.get(9)?,
                total_steps: row.get(10)?,
                completed_steps: row.get(11)?,
                steps: Vec::new(),
            })
        },
    )?;

    // Get all steps
    let mut stmt = conn.prepare(
        "SELECT id, job_id, step_order, step_type, step_name, step_data_json, status,
                started_at, completed_at, error_message, retry_count, output_files_json
         FROM export_job_steps WHERE job_id = ?1 ORDER BY step_order"
    )?;

    let steps: Vec<ExportJobStep> = stmt.query_map([job_id], |row| {
        let step_type_str: String = row.get(3)?;
        let step_data_json: Option<String> = row.get(5)?;
        let status_str: String = row.get(6)?;
        let output_files_json: Option<String> = row.get(11)?;

        // Deserialize step_data with error logging
        let step_data = step_data_json.and_then(|j| {
            match serde_json::from_str(&j) {
                Ok(data) => Some(data),
                Err(e) => {
                    eprintln!("⚠️ Failed to deserialize step_data for step {}: {}", row.get::<_, i32>(2).unwrap_or(0), e);
                    eprintln!("   JSON was: {}", j);
                    None
                }
            }
        });

        Ok(ExportJobStep {
            id: row.get(0)?,
            job_id: row.get(1)?,
            step_order: row.get(2)?,
            step_type: StepType::from_str(&step_type_str),
            step_name: row.get(4)?,
            step_data,
            status: StepStatus::from_str(&status_str),
            started_at: row.get(7)?,
            completed_at: row.get(8)?,
            error_message: row.get(9)?,
            retry_count: row.get(10)?,
            output_files: output_files_json
                .and_then(|j| serde_json::from_str(&j).ok())
                .unwrap_or_default(),
        })
    })?.filter_map(|r| r.ok()).collect();

    Ok(ExportJob { steps, ..job })
}

/// Get all jobs for a frame set
pub fn get_jobs_for_frame_set(conn: &Connection, frame_set_id: Option<i64>) -> Result<Vec<ExportJob>> {
    let job_ids: Vec<i64> = match frame_set_id {
        Some(fs_id) => {
            let mut stmt = conn.prepare(
                "SELECT id FROM export_jobs WHERE frame_set_id = ?1 ORDER BY created_at DESC"
            )?;
            let ids: Vec<i64> = stmt.query_map([fs_id], |row| row.get(0))?
                .filter_map(|r| r.ok())
                .collect();
            ids
        }
        None => {
            let mut stmt = conn.prepare(
                "SELECT id FROM export_jobs ORDER BY created_at DESC"
            )?;
            let ids: Vec<i64> = stmt.query_map([], |row| row.get(0))?
                .filter_map(|r| r.ok())
                .collect();
            ids
        }
    };

    let mut jobs = Vec::new();
    for job_id in job_ids {
        if let Ok(job) = get_job_with_steps(conn, job_id) {
            jobs.push(job);
        }
    }

    Ok(jobs)
}

/// Get recent job history
pub fn get_job_history(conn: &Connection, limit: Option<i32>) -> Result<Vec<ExportJob>> {
    let limit = limit.unwrap_or(50);

    let mut stmt = conn.prepare(
        "SELECT id FROM export_jobs ORDER BY created_at DESC LIMIT ?1"
    )?;

    let job_ids: Vec<i64> = stmt.query_map([limit], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    let mut jobs = Vec::new();
    for job_id in job_ids {
        if let Ok(job) = get_job_with_steps(conn, job_id) {
            jobs.push(job);
        }
    }

    Ok(jobs)
}

// ============================================================================
// Job Status Updates
// ============================================================================

/// Update job status
pub fn update_job_status(
    conn: &Connection,
    job_id: i64,
    status: JobStatus,
    error_message: Option<&str>,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();

    match status {
        JobStatus::Running => {
            conn.execute(
                "UPDATE export_jobs SET status = ?2, started_at = ?3 WHERE id = ?1",
                rusqlite::params![job_id, status.to_str(), now],
            )?;
        }
        JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled => {
            conn.execute(
                "UPDATE export_jobs SET status = ?2, completed_at = ?3, error_message = ?4 WHERE id = ?1",
                rusqlite::params![job_id, status.to_str(), now, error_message],
            )?;
        }
        _ => {
            conn.execute(
                "UPDATE export_jobs SET status = ?2 WHERE id = ?1",
                rusqlite::params![job_id, status.to_str()],
            )?;
        }
    }

    Ok(())
}

/// Update completed steps count
pub fn update_completed_steps(conn: &Connection, job_id: i64) -> Result<i32> {
    let completed: i32 = conn.query_row(
        "SELECT COUNT(*) FROM export_job_steps WHERE job_id = ?1 AND status IN ('completed', 'skipped')",
        [job_id],
        |row| row.get(0),
    )?;

    conn.execute(
        "UPDATE export_jobs SET completed_steps = ?2 WHERE id = ?1",
        [job_id, completed as i64],
    )?;

    Ok(completed)
}

// ============================================================================
// Step Status Updates
// ============================================================================

/// Update step status
pub fn update_step_status(
    conn: &Connection,
    step_id: i64,
    status: StepStatus,
    error_message: Option<&str>,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();

    match status {
        StepStatus::Running => {
            conn.execute(
                "UPDATE export_job_steps SET status = ?2, started_at = ?3 WHERE id = ?1",
                rusqlite::params![step_id, status.to_str(), now],
            )?;
        }
        StepStatus::Completed | StepStatus::Failed | StepStatus::Skipped => {
            conn.execute(
                "UPDATE export_job_steps SET status = ?2, completed_at = ?3, error_message = ?4 WHERE id = ?1",
                rusqlite::params![step_id, status.to_str(), now, error_message],
            )?;
        }
        _ => {
            conn.execute(
                "UPDATE export_job_steps SET status = ?2 WHERE id = ?1",
                rusqlite::params![step_id, status.to_str()],
            )?;
        }
    }

    Ok(())
}

/// Increment retry count for a step
pub fn increment_step_retry(conn: &Connection, step_id: i64) -> Result<i32> {
    conn.execute(
        "UPDATE export_job_steps SET retry_count = retry_count + 1, status = 'pending', error_message = NULL WHERE id = ?1",
        [step_id],
    )?;

    let retry_count: i32 = conn.query_row(
        "SELECT retry_count FROM export_job_steps WHERE id = ?1",
        [step_id],
        |row| row.get(0),
    )?;

    Ok(retry_count)
}

/// Update step output files
pub fn update_step_output_files(
    conn: &Connection,
    step_id: i64,
    output_files: &[String],
) -> Result<()> {
    let output_json = serde_json::to_string(output_files)?;

    conn.execute(
        "UPDATE export_job_steps SET output_files_json = ?2 WHERE id = ?1",
        rusqlite::params![step_id, output_json],
    )?;

    Ok(())
}

// ============================================================================
// Step Queries
// ============================================================================

/// Get the next pending step for a job
pub fn get_next_pending_step(conn: &Connection, job_id: i64) -> Result<Option<ExportJobStep>> {
    let result = conn.query_row(
        "SELECT id, job_id, step_order, step_type, step_name, step_data_json, status,
                started_at, completed_at, error_message, retry_count, output_files_json
         FROM export_job_steps
         WHERE job_id = ?1 AND status = 'pending'
         ORDER BY step_order LIMIT 1",
        [job_id],
        |row| {
            let step_type_str: String = row.get(3)?;
            let step_data_json: Option<String> = row.get(5)?;
            let status_str: String = row.get(6)?;
            let output_files_json: Option<String> = row.get(11)?;

            Ok(ExportJobStep {
                id: row.get(0)?,
                job_id: row.get(1)?,
                step_order: row.get(2)?,
                step_type: StepType::from_str(&step_type_str),
                step_name: row.get(4)?,
                step_data: step_data_json.and_then(|j| serde_json::from_str(&j).ok()),
                status: StepStatus::from_str(&status_str),
                started_at: row.get(7)?,
                completed_at: row.get(8)?,
                error_message: row.get(9)?,
                retry_count: row.get(10)?,
                output_files: output_files_json
                    .and_then(|j| serde_json::from_str(&j).ok())
                    .unwrap_or_default(),
            })
        },
    );

    match result {
        Ok(step) => Ok(Some(step)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Get a specific step by ID
pub fn get_step(conn: &Connection, step_id: i64) -> Result<ExportJobStep> {
    conn.query_row(
        "SELECT id, job_id, step_order, step_type, step_name, step_data_json, status,
                started_at, completed_at, error_message, retry_count, output_files_json
         FROM export_job_steps WHERE id = ?1",
        [step_id],
        |row| {
            let step_type_str: String = row.get(3)?;
            let step_data_json: Option<String> = row.get(5)?;
            let status_str: String = row.get(6)?;
            let output_files_json: Option<String> = row.get(11)?;

            Ok(ExportJobStep {
                id: row.get(0)?,
                job_id: row.get(1)?,
                step_order: row.get(2)?,
                step_type: StepType::from_str(&step_type_str),
                step_name: row.get(4)?,
                step_data: step_data_json.and_then(|j| serde_json::from_str(&j).ok()),
                status: StepStatus::from_str(&status_str),
                started_at: row.get(7)?,
                completed_at: row.get(8)?,
                error_message: row.get(9)?,
                retry_count: row.get(10)?,
                output_files: output_files_json
                    .and_then(|j| serde_json::from_str(&j).ok())
                    .unwrap_or_default(),
            })
        },
    ).map_err(|e| e.into())
}

// ============================================================================
// Job Deletion
// ============================================================================

/// Delete a job and all its steps (CASCADE)
pub fn delete_job(conn: &Connection, job_id: i64) -> Result<()> {
    conn.execute("DELETE FROM export_jobs WHERE id = ?1", [job_id])?;
    Ok(())
}
