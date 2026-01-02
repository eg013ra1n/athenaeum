//! Siril script generator
//!
//! Generates Siril .ssf scripts from export data and configuration.
//!
//! Phase 4: Updated to use new ExportGroup structure with subgroups
//! and MasterCreationPlan for proper dependency ordering.

use crate::export::file_organizer::ExportFolders;
use crate::export::models::{
    CameraType, ExportConfig, ExportData, ExportGroup, FilterExportGroup, MasterCreationPlan,
    MasterInfo, SirilWorkflow,
};
use crate::export::siril::templates::{
    get_template, BIAS_SECTION_EMPTY, BIAS_SECTION_TEMPLATE, CALIBRATE_LIGHTS_DARK_ONLY,
    CALIBRATE_LIGHTS_FLAT_ONLY, CALIBRATE_LIGHTS_FULL, CALIBRATE_LIGHTS_NONE,
    DARK_SECTION_EMPTY, DARK_SECTION_TEMPLATE, DARK_SECTION_WITH_BIAS_TEMPLATE,
    FLAT_SECTION_BIAS_ONLY_TEMPLATE, FLAT_SECTION_EMPTY, FLAT_SECTION_TEMPLATE,
};
use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

/// Generate Siril scripts for the export
pub fn generate_scripts(config: &ExportConfig, data: &ExportData) -> Result<Vec<PathBuf>> {
    println!("📝 Generating Siril scripts for {:?}", config.workflow);
    println!("  Output dir: {:?}", config.output_dir);
    println!("  Filters: {}", data.filters.len());

    let folders = ExportFolders::new(config.output_dir.clone());
    let mut scripts = Vec::new();

    match config.workflow {
        SirilWorkflow::MonoPreprocessing => {
            // Generate one script per filter
            for filter_group in &data.filters {
                let script_path = generate_mono_script(config, &folders, filter_group)?;
                scripts.push(script_path);
            }
        }
        SirilWorkflow::OscPreprocessing => {
            // Generate single OSC script
            if let Some(first_group) = data.filters.first() {
                let script_path = generate_osc_script(config, &folders, first_group)?;
                scripts.push(script_path);
            }
        }
        SirilWorkflow::LrgbProcessing => {
            // Generate LRGB combination script (assumes individual channels already processed)
            let script_path = generate_lrgb_script(config, &folders)?;
            scripts.push(script_path);
        }
    }

    Ok(scripts)
}

/// Generate a mono preprocessing script for a single filter
fn generate_mono_script(
    config: &ExportConfig,
    folders: &ExportFolders,
    filter_group: &FilterExportGroup,
) -> Result<PathBuf> {
    let filter_name = filter_group.filter.as_deref().unwrap_or("NoFilter");
    let template = get_template(&config.workflow);

    // Determine what calibrations are available
    let has_bias = filter_group
        .bias_sets
        .iter()
        .any(|s| !s.frames.is_empty())
        || filter_group.flat_sets.iter().any(|s| {
            s.sub_calibrations
                .iter()
                .any(|sub| sub.imagetyp == "Bias" && !sub.frames.is_empty())
        })
        || filter_group.dark_sets.iter().any(|s| {
            s.sub_calibrations
                .iter()
                .any(|sub| sub.imagetyp == "Bias" && !sub.frames.is_empty())
        });

    let has_dark = !filter_group.dark_sets.is_empty()
        && filter_group.dark_sets.iter().any(|s| !s.frames.is_empty());

    let has_flat = !filter_group.flat_sets.is_empty()
        && filter_group.flat_sets.iter().any(|s| !s.frames.is_empty());

    // Build script sections
    let bias_section = if has_bias {
        apply_placeholders(BIAS_SECTION_TEMPLATE, config, folders, filter_name)
    } else {
        BIAS_SECTION_EMPTY.to_string()
    };

    let dark_section = if has_dark {
        if has_bias && config.create_masters {
            apply_placeholders(DARK_SECTION_WITH_BIAS_TEMPLATE, config, folders, filter_name)
        } else {
            apply_placeholders(DARK_SECTION_TEMPLATE, config, folders, filter_name)
        }
    } else {
        DARK_SECTION_EMPTY.to_string()
    };

    let flat_section = if has_flat {
        if has_dark {
            apply_placeholders(FLAT_SECTION_TEMPLATE, config, folders, filter_name)
        } else if has_bias {
            apply_placeholders(FLAT_SECTION_BIAS_ONLY_TEMPLATE, config, folders, filter_name)
        } else {
            FLAT_SECTION_EMPTY.to_string()
        }
    } else {
        FLAT_SECTION_EMPTY.to_string()
    };

    let calibrate_cmd = if has_dark && has_flat {
        apply_placeholders(CALIBRATE_LIGHTS_FULL, config, folders, filter_name)
    } else if has_dark {
        apply_placeholders(CALIBRATE_LIGHTS_DARK_ONLY, config, folders, filter_name)
    } else if has_flat {
        apply_placeholders(CALIBRATE_LIGHTS_FLAT_ONLY, config, folders, filter_name)
    } else {
        CALIBRATE_LIGHTS_NONE.to_string()
    };

    // Apply all placeholders to the main template
    let mut script = template.template.to_string();
    script = apply_placeholders(&script, config, folders, filter_name);
    script = script.replace("{bias_section}", &bias_section);
    script = script.replace("{dark_section}", &dark_section);
    script = script.replace("{flat_section}", &flat_section);
    script = script.replace("{calibrate_lights_cmd}", &calibrate_cmd);

    // Write script to file
    let script_path = folders.root.join(format!("{}_preprocessing.ssf", filter_name));
    fs::write(&script_path, &script)
        .with_context(|| format!("Failed to write script to {:?}", script_path))?;

    Ok(script_path)
}

/// Generate an OSC preprocessing script
fn generate_osc_script(
    config: &ExportConfig,
    folders: &ExportFolders,
    filter_group: &FilterExportGroup,
) -> Result<PathBuf> {
    let template = get_template(&config.workflow);

    // Check available calibrations
    let has_bias = !filter_group.bias_sets.is_empty();
    let has_dark = !filter_group.dark_sets.is_empty();
    let has_flat = !filter_group.flat_sets.is_empty();

    // Build sections
    let bias_section = if has_bias {
        apply_placeholders(BIAS_SECTION_TEMPLATE, config, folders, "OSC")
    } else {
        BIAS_SECTION_EMPTY.to_string()
    };

    let dark_section = if has_dark {
        if has_bias && config.create_masters {
            apply_placeholders(DARK_SECTION_WITH_BIAS_TEMPLATE, config, folders, "OSC")
        } else {
            apply_placeholders(DARK_SECTION_TEMPLATE, config, folders, "OSC")
        }
    } else {
        DARK_SECTION_EMPTY.to_string()
    };

    let flat_section = if has_flat {
        if has_dark {
            apply_placeholders(FLAT_SECTION_TEMPLATE, config, folders, "OSC")
        } else if has_bias {
            apply_placeholders(FLAT_SECTION_BIAS_ONLY_TEMPLATE, config, folders, "OSC")
        } else {
            FLAT_SECTION_EMPTY.to_string()
        }
    } else {
        FLAT_SECTION_EMPTY.to_string()
    };

    let calibrate_cmd = if has_dark && has_flat {
        apply_placeholders(CALIBRATE_LIGHTS_FULL, config, folders, "OSC")
    } else if has_dark {
        apply_placeholders(CALIBRATE_LIGHTS_DARK_ONLY, config, folders, "OSC")
    } else if has_flat {
        apply_placeholders(CALIBRATE_LIGHTS_FLAT_ONLY, config, folders, "OSC")
    } else {
        CALIBRATE_LIGHTS_NONE.to_string()
    };

    let mut script = template.template.to_string();
    script = apply_placeholders(&script, config, folders, "OSC");
    script = script.replace("{bias_section}", &bias_section);
    script = script.replace("{dark_section}", &dark_section);
    script = script.replace("{flat_section}", &flat_section);
    script = script.replace("{calibrate_lights_cmd}", &calibrate_cmd);

    let script_path = folders.root.join("OSC_preprocessing.ssf");
    fs::write(&script_path, &script)
        .with_context(|| format!("Failed to write script to {:?}", script_path))?;

    Ok(script_path)
}

/// Generate an LRGB combination script
fn generate_lrgb_script(config: &ExportConfig, folders: &ExportFolders) -> Result<PathBuf> {
    let template = get_template(&config.workflow);
    let script = apply_placeholders(template.template, config, folders, "LRGB");

    let script_path = folders.root.join("LRGB_combine.ssf");
    fs::write(&script_path, &script)
        .with_context(|| format!("Failed to write script to {:?}", script_path))?;

    Ok(script_path)
}

/// Apply placeholder substitutions to a template string
fn apply_placeholders(
    template: &str,
    config: &ExportConfig,
    folders: &ExportFolders,
    filter_name: &str,
) -> String {
    let lights_dir = folders.lights_for_filter(Some(filter_name));
    let flats_dir = folders.flats_for_filter(Some(filter_name));

    template
        .replace("{working_dir}", &folders.root.to_string_lossy())
        .replace("{filter}", filter_name)
        .replace("{rejection_low}", &config.rejection_low.to_string())
        .replace("{rejection_high}", &config.rejection_high.to_string())
        .replace("{lights_dir}", &lights_dir.to_string_lossy())
        .replace("{darks_dir}", &folders.darks.to_string_lossy())
        .replace("{flats_dir}", &flats_dir.to_string_lossy())
        .replace("{bias_dir}", &folders.bias.to_string_lossy())
        .replace("{masters_dir}", &folders.masters.to_string_lossy())
        .replace("{process_dir}", &folders.process.to_string_lossy())
        .replace("{result_dir}", &folders.result.to_string_lossy())
}

// ============================================================================
// Phase 4: New Script Generation using ExportGroup Structure
// ============================================================================

/// Generate Siril scripts using the new ExportGroup structure (Phase 4)
///
/// This is the new entry point that uses:
/// - ExportData.groups (grouped by filter + camera type)
/// - ExportData.master_plan (topologically sorted master creation order)
pub fn generate_scripts_v2(config: &ExportConfig, data: &ExportData) -> Result<Vec<PathBuf>> {
    println!("📝 Generating Siril scripts (v2) for {:?}", config.workflow);
    println!("  Output dir: {:?}", config.output_dir);
    println!("  Export groups: {}", data.groups.len());
    println!("  Masters to create: {}", data.master_plan.masters.len());

    let folders = ExportFolders::new(config.output_dir.clone());
    let mut scripts = Vec::new();

    // Step 1: Generate master creation script (if there are masters to create)
    if !data.master_plan.masters.is_empty() && config.create_masters {
        let master_script = generate_master_creation_script(config, &folders, &data.master_plan)?;
        scripts.push(master_script);
    }

    // Step 2: Generate preprocessing scripts for each export group
    for group in &data.groups {
        let group_script = generate_group_preprocessing_script(config, &folders, group, &data.master_plan)?;
        scripts.push(group_script);
    }

    println!("  Generated {} scripts", scripts.len());
    Ok(scripts)
}

/// Generate a master creation script from the MasterCreationPlan
///
/// Creates all master calibration frames in the correct dependency order.
fn generate_master_creation_script(
    config: &ExportConfig,
    folders: &ExportFolders,
    master_plan: &MasterCreationPlan,
) -> Result<PathBuf> {
    let mut script = String::new();

    // Header
    script.push_str(&format!(
        r#"############################################
# Siril Master Creation Script
# Generated by Athenaeum
# Total masters to create: {}
############################################

requires 1.2.0

# Set working directory
cd {}

"#,
        master_plan.masters.len(),
        folders.root.to_string_lossy()
    ));

    // Generate sections for each master in dependency order
    for (index, master) in master_plan.masters.iter().enumerate() {
        script.push_str(&format!(
            "# ========================================\n# Master {} of {}: {} (Set {})\n# ========================================\n",
            index + 1,
            master_plan.masters.len(),
            master.master_type,
            master.set_id
        ));

        script.push_str(&generate_master_section(config, folders, master, master_plan)?);
        script.push('\n');
    }

    script.push_str("close\n");

    // Write script to file
    let script_path = folders.root.join("00_create_masters.ssf");
    fs::write(&script_path, &script)
        .with_context(|| format!("Failed to write master script to {:?}", script_path))?;

    println!("  Created master script: {:?}", script_path);
    Ok(script_path)
}

/// Generate script section for a single master
fn generate_master_section(
    config: &ExportConfig,
    folders: &ExportFolders,
    master: &MasterInfo,
    master_plan: &MasterCreationPlan,
) -> Result<String> {
    let mut section = String::new();

    let master_type_lower = master.master_type.to_lowercase();
    let source_dir = get_calibration_source_dir(folders, &master.master_type, master.set_id);
    let output_path = folders.masters.join(&master.output_name);

    // Convert source frames (note: in real usage, files should already be organized)
    section.push_str(&format!(
        "cd {}\nconvert {} -out={}\ncd {}\n",
        source_dir.to_string_lossy(),
        master_type_lower,
        folders.process.to_string_lossy(),
        folders.process.to_string_lossy()
    ));

    // Apply calibrations if needed
    match master.master_type.as_str() {
        "Bias" => {
            // Bias frames are stacked without calibration
            section.push_str(&format!(
                "stack {} rej {} {} -nonorm -out={}\n",
                master_type_lower,
                config.rejection_low,
                config.rejection_high,
                output_path.to_string_lossy()
            ));
        }
        "Dark" => {
            // Dark frames may be calibrated with bias
            if let Some(bias_id) = master.apply_bias {
                if let Some(bias_path) = master_plan.master_paths.get(&bias_id) {
                    let bias_master = folders.masters.join(bias_path);
                    section.push_str(&format!(
                        "calibrate {} -bias={}\n",
                        master_type_lower,
                        bias_master.to_string_lossy()
                    ));
                    section.push_str(&format!(
                        "stack pp_{} rej {} {} -nonorm -out={}\n",
                        master_type_lower,
                        config.rejection_low,
                        config.rejection_high,
                        output_path.to_string_lossy()
                    ));
                } else {
                    section.push_str(&format!(
                        "stack {} rej {} {} -nonorm -out={}\n",
                        master_type_lower,
                        config.rejection_low,
                        config.rejection_high,
                        output_path.to_string_lossy()
                    ));
                }
            } else {
                section.push_str(&format!(
                    "stack {} rej {} {} -nonorm -out={}\n",
                    master_type_lower,
                    config.rejection_low,
                    config.rejection_high,
                    output_path.to_string_lossy()
                ));
            }
        }
        "DarkFlat" => {
            // DarkFlat is similar to Dark
            if let Some(bias_id) = master.apply_bias {
                if let Some(bias_path) = master_plan.master_paths.get(&bias_id) {
                    let bias_master = folders.masters.join(bias_path);
                    section.push_str(&format!(
                        "calibrate darkflat -bias={}\n",
                        bias_master.to_string_lossy()
                    ));
                    section.push_str(&format!(
                        "stack pp_darkflat rej {} {} -nonorm -out={}\n",
                        config.rejection_low,
                        config.rejection_high,
                        output_path.to_string_lossy()
                    ));
                } else {
                    section.push_str(&format!(
                        "stack darkflat rej {} {} -nonorm -out={}\n",
                        config.rejection_low,
                        config.rejection_high,
                        output_path.to_string_lossy()
                    ));
                }
            } else {
                section.push_str(&format!(
                    "stack darkflat rej {} {} -nonorm -out={}\n",
                    config.rejection_low,
                    config.rejection_high,
                    output_path.to_string_lossy()
                ));
            }
        }
        "Flat" => {
            // Flat frames are calibrated with dark (or darkflat) and optionally bias
            let mut calibrate_args = String::new();

            if let Some(dark_id) = master.apply_dark {
                if let Some(dark_path) = master_plan.master_paths.get(&dark_id) {
                    let dark_master = folders.masters.join(dark_path);
                    calibrate_args.push_str(&format!(" -dark={}", dark_master.to_string_lossy()));
                }
            }

            if let Some(bias_id) = master.apply_bias {
                if let Some(bias_path) = master_plan.master_paths.get(&bias_id) {
                    let bias_master = folders.masters.join(bias_path);
                    calibrate_args.push_str(&format!(" -bias={}", bias_master.to_string_lossy()));
                }
            }

            if !calibrate_args.is_empty() {
                section.push_str(&format!("calibrate flat{}\n", calibrate_args));
                section.push_str(&format!(
                    "stack pp_flat rej {} {} -norm=mul -out={}\n",
                    config.rejection_low,
                    config.rejection_high,
                    output_path.to_string_lossy()
                ));
            } else {
                section.push_str(&format!(
                    "stack flat rej {} {} -norm=mul -out={}\n",
                    config.rejection_low,
                    config.rejection_high,
                    output_path.to_string_lossy()
                ));
            }
        }
        _ => {
            // Unknown type, just stack
            section.push_str(&format!(
                "stack {} rej {} {} -nonorm -out={}\n",
                master_type_lower,
                config.rejection_low,
                config.rejection_high,
                output_path.to_string_lossy()
            ));
        }
    }

    Ok(section)
}

/// Get source directory for a specific calibration set
/// V2: Returns per-set folder path (e.g., Calibration/Darks/set_46)
fn get_calibration_source_dir(folders: &ExportFolders, master_type: &str, set_id: i64) -> PathBuf {
    folders.calibration_set_folder(master_type, set_id)
}

/// Generate a preprocessing script for an ExportGroup
fn generate_group_preprocessing_script(
    config: &ExportConfig,
    folders: &ExportFolders,
    group: &ExportGroup,
    master_plan: &MasterCreationPlan,
) -> Result<PathBuf> {
    let group_name = &group.display_name;
    let filter_name = group.filter.as_deref().unwrap_or("Unfiltered");

    // Determine workflow based on camera type
    let is_osc = group.camera_type == CameraType::Osc;
    let has_multiple_subgroups = group.subgroups.len() > 1;

    let mut script = String::new();

    // Header
    script.push_str(&format!(
        r#"############################################
# Siril Preprocessing Script - {}
# Camera Type: {}
# Total Frames: {}
# Total Exposure: {:.1}s
# Subgroups: {}
# Generated by Athenaeum
############################################

requires 1.2.0

# Set working directory
cd {}

"#,
        group_name,
        group.camera_type.display_name(),
        group.total_frames,
        group.total_exposure,
        group.subgroups.len(),
        folders.root.to_string_lossy()
    ));

    if has_multiple_subgroups {
        // Multiple subgroups: process each separately then combine
        script.push_str("# Multiple calibration subgroups detected\n");
        script.push_str("# Each subgroup will be calibrated with its specific masters\n\n");

        let mut registered_sequences = Vec::new();

        for (idx, subgroup) in group.subgroups.iter().enumerate() {
            let subgroup_num = idx + 1;
            let seq_name = format!("subgroup_{}", subgroup_num);

            script.push_str(&format!(
                "# ========================================\n# Subgroup {} of {} ({})\n# ========================================\n",
                subgroup_num,
                group.subgroups.len(),
                subgroup.display_name
            ));

            // Get calibration masters for this subgroup
            let flat_master = subgroup
                .flat
                .as_ref()
                .and_then(|f| master_plan.master_paths.get(&f.set_id))
                .map(|p| folders.masters.join(p));

            let dark_master = subgroup
                .dark
                .as_ref()
                .and_then(|d| master_plan.master_paths.get(&d.set_id))
                .map(|p| folders.masters.join(p));

            // Lights are in subgroup folder
            let lights_dir = folders.lights_for_filter(Some(filter_name))
                .join(format!("subgroup_{}", subgroup_num));

            script.push_str(&format!(
                "cd {}\nconvert lights -out={}\ncd {}\n",
                lights_dir.to_string_lossy(),
                folders.process.to_string_lossy(),
                folders.process.to_string_lossy()
            ));

            // Build calibrate command
            let mut calibrate_args = Vec::new();
            if let Some(ref dark) = dark_master {
                calibrate_args.push(format!("-dark={}", dark.to_string_lossy()));
            }
            if let Some(ref flat) = flat_master {
                calibrate_args.push(format!("-flat={}", flat.to_string_lossy()));
            }
            if dark_master.is_some() {
                calibrate_args.push("-cc=dark".to_string());
            }

            if !calibrate_args.is_empty() {
                script.push_str(&format!("calibrate lights {}\n", calibrate_args.join(" ")));
            }

            // OSC debayer
            if is_osc {
                script.push_str("preprocess pp_lights -debayer\n");
            }

            // Register with unique prefix for this subgroup
            let registered_prefix = format!("{}_r_", seq_name);
            script.push_str(&format!(
                "register pp_lights -prefix={}\n\n",
                registered_prefix
            ));

            // The registered sequence will be named {prefix}pp_lights
            registered_sequences.push(format!("{}pp_lights", registered_prefix));
        }

        // Combine all subgroup sequences
        script.push_str("# ========================================\n");
        script.push_str("# Combine all subgroups\n");
        script.push_str("# ========================================\n");
        script.push_str(&format!(
            "# Sequences to combine: {}\n",
            registered_sequences.join(", ")
        ));

        // Use merge command to combine sequences
        script.push_str(&format!(
            "merge {} combined_lights\n",
            registered_sequences.join(" ")
        ));

        // Stack combined
        let output_name = format!("{}_stacked", group.group_key);
        let stack_options = if is_osc {
            format!("rej {} {} -norm=addscale -output_norm -rgb_equal", config.rejection_low, config.rejection_high)
        } else {
            format!("rej {} {} -norm=addscale -output_norm", config.rejection_low, config.rejection_high)
        };

        script.push_str(&format!(
            "\n# ========================================\n# Stack Combined Light Frames\n# ========================================\nstack combined_lights {} -out={}/{}\n\nclose\n",
            stack_options,
            folders.result.to_string_lossy(),
            output_name
        ));
    } else {
        // Single subgroup: original behavior
        let primary_subgroup = group.subgroups.first();

        let (flat_master, dark_master) = if let Some(subgroup) = primary_subgroup {
            let flat = subgroup
                .flat
                .as_ref()
                .and_then(|f| master_plan.master_paths.get(&f.set_id))
                .map(|p| folders.masters.join(p));

            let dark = subgroup
                .dark
                .as_ref()
                .and_then(|d| master_plan.master_paths.get(&d.set_id))
                .map(|p| folders.masters.join(p));

            (flat, dark)
        } else {
            (None, None)
        };

        // Step: Convert and calibrate light frames
        let lights_dir = folders.lights_for_filter(Some(filter_name));

        script.push_str(&format!(
            r#"# ========================================
# Calibrate Light Frames
# ========================================
cd {}
convert lights -out={}
cd {}
"#,
            lights_dir.to_string_lossy(),
            folders.process.to_string_lossy(),
            folders.process.to_string_lossy()
        ));

        // Build calibrate command
        let mut calibrate_args = Vec::new();

        if let Some(ref dark) = dark_master {
            calibrate_args.push(format!("-dark={}", dark.to_string_lossy()));
        }

        if let Some(ref flat) = flat_master {
            calibrate_args.push(format!("-flat={}", flat.to_string_lossy()));
        }

        if dark_master.is_some() {
            calibrate_args.push("-cc=dark".to_string());
        }

        if !calibrate_args.is_empty() {
            script.push_str(&format!(
                "calibrate lights {}\n\n",
                calibrate_args.join(" ")
            ));
        } else {
            script.push_str("# No calibration masters available - lights not calibrated\n\n");
        }

        // OSC-specific: Debayer step
        if is_osc {
            script.push_str(
                r#"# ========================================
# Debayer (OSC cameras)
# ========================================
# Note: Siril auto-detects Bayer pattern from FITS header
preprocess pp_lights -debayer

"#,
            );
        }

        // Registration
        script.push_str(
            r#"# ========================================
# Register Light Frames
# ========================================
register pp_lights

"#,
        );

        // Stacking
        let output_name = format!("{}_stacked", group.group_key);
        let stack_options = if is_osc {
            format!(
                "rej {} {} -norm=addscale -output_norm -rgb_equal",
                config.rejection_low, config.rejection_high
            )
        } else {
            format!(
                "rej {} {} -norm=addscale -output_norm",
                config.rejection_low, config.rejection_high
            )
        };

        script.push_str(&format!(
            r#"# ========================================
# Stack Light Frames
# ========================================
stack r_pp_lights {} -out={}/{}

close
"#,
        stack_options,
        folders.result.to_string_lossy(),
        output_name
    ));
    }

    // Write script to file
    let safe_group_name = group.group_key.replace(['/', '\\', ' '], "_");
    let script_path = folders.root.join(format!("{}_preprocessing.ssf", safe_group_name));
    fs::write(&script_path, &script)
        .with_context(|| format!("Failed to write script to {:?}", script_path))?;

    println!("  Created group script: {:?}", script_path);
    Ok(script_path)
}

/// Generate a combined preprocessing script for all groups (convenience function)
#[allow(dead_code)]
pub fn generate_combined_script(
    config: &ExportConfig,
    data: &ExportData,
) -> Result<PathBuf> {
    let folders = ExportFolders::new(config.output_dir.clone());

    let mut script = String::new();

    script.push_str(&format!(
        r#"############################################
# Siril Combined Preprocessing Script
# Generated by Athenaeum
#
# Groups: {}
# Total Frames: {}
# Total Exposure: {:.1}s
############################################

requires 1.2.0

cd {}

"#,
        data.groups.len(),
        data.total_light_frames,
        data.total_exposure_seconds,
        folders.root.to_string_lossy()
    ));

    // List all scripts to run
    script.push_str("# Run scripts in order:\n");

    if !data.master_plan.masters.is_empty() && config.create_masters {
        script.push_str("# 1. Run 00_create_masters.ssf first\n");
    }

    for (i, group) in data.groups.iter().enumerate() {
        let safe_name = group.group_key.replace(['/', '\\', ' '], "_");
        script.push_str(&format!(
            "# {}. Run {}_preprocessing.ssf\n",
            i + 2,
            safe_name
        ));
    }

    script.push_str("\nclose\n");

    let script_path = folders.root.join("README.ssf");
    fs::write(&script_path, &script)
        .with_context(|| format!("Failed to write combined script to {:?}", script_path))?;

    Ok(script_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::models::{CalibrationSubgroup, ExportMode};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn make_test_config() -> ExportConfig {
        ExportConfig {
            frame_set_id: 1,
            output_dir: PathBuf::from("/tmp/export"),
            mode: ExportMode::OrganizeAndScript,
            workflow: SirilWorkflow::MonoPreprocessing,
            create_masters: true,
            rejection_low: 3.0,
            rejection_high: 3.0,
            use_symlinks: false,
        }
    }

    #[test]
    fn test_placeholder_replacement() {
        let folders = ExportFolders::new(PathBuf::from("/tmp/export"));
        let config = make_test_config();

        let template = "cd {working_dir}\nfilter: {filter}\nrej: {rejection_low} {rejection_high}";
        let result = apply_placeholders(template, &config, &folders, "Ha");

        assert!(result.contains("cd /tmp/export"));
        assert!(result.contains("filter: Ha"));
        assert!(result.contains("rej: 3 3"));
    }

    #[test]
    fn test_get_calibration_source_dir() {
        let folders = ExportFolders::new(PathBuf::from("/tmp/export"));

        // V2: Now returns per-set folder paths
        assert_eq!(
            get_calibration_source_dir(&folders, "Bias", 55),
            PathBuf::from("/tmp/export/Calibration/Bias/set_55")
        );
        assert_eq!(
            get_calibration_source_dir(&folders, "Dark", 46),
            PathBuf::from("/tmp/export/Calibration/Darks/set_46")
        );
        assert_eq!(
            get_calibration_source_dir(&folders, "DarkFlat", 100),
            PathBuf::from("/tmp/export/Calibration/DarkFlats/set_100")
        );
        assert_eq!(
            get_calibration_source_dir(&folders, "Flat", 38),
            PathBuf::from("/tmp/export/Calibration/Flats/set_38")
        );
    }

    #[test]
    fn test_master_section_bias() {
        let folders = ExportFolders::new(PathBuf::from("/tmp/export"));
        let config = make_test_config();

        let master = MasterInfo {
            set_id: 1,
            master_type: "Bias".to_string(),
            output_name: "master_bias_1.fit".to_string(),
            source_frames: vec![],
            depends_on: vec![],
            apply_bias: None,
            apply_dark: None,
        };

        let master_plan = MasterCreationPlan {
            masters: vec![master.clone()],
            master_paths: HashMap::new(),
        };

        let section = generate_master_section(&config, &folders, &master, &master_plan).unwrap();

        assert!(section.contains("cd /tmp/export/Calibration/Bias"));
        assert!(section.contains("convert bias"));
        assert!(section.contains("stack bias rej 3 3"));
        assert!(section.contains("master_bias_1.fit"));
    }

    #[test]
    fn test_export_group_script_mono() {
        let group = ExportGroup {
            group_key: "Ha_Mono".to_string(),
            filter: Some("Ha".to_string()),
            camera_type: CameraType::Mono,
            display_name: "Ha (Mono)".to_string(),
            subgroups: vec![CalibrationSubgroup {
                subgroup_key: "f1_d2_b3".to_string(),
                display_name: "Default".to_string(),
                frames: vec![],
                flat: None,
                dark: None,
                bias: None,
                warnings: vec![],
            }],
            total_frames: 10,
            total_exposure: 600.0,
            warnings: vec![],
        };

        // Verify group properties
        assert_eq!(group.camera_type, CameraType::Mono);
        assert!(!group.subgroups.is_empty());
    }

    #[test]
    fn test_export_group_script_osc() {
        let group = ExportGroup {
            group_key: "Unfiltered_OSC".to_string(),
            filter: None,
            camera_type: CameraType::Osc,
            display_name: "Luminance (OSC)".to_string(),
            subgroups: vec![CalibrationSubgroup {
                subgroup_key: "f1_d2_b3".to_string(),
                display_name: "Default".to_string(),
                frames: vec![],
                flat: None,
                dark: None,
                bias: None,
                warnings: vec![],
            }],
            total_frames: 20,
            total_exposure: 1200.0,
            warnings: vec![],
        };

        // Verify OSC detection
        assert_eq!(group.camera_type, CameraType::Osc);
    }
}
