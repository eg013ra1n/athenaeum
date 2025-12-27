//! Siril script generator
//!
//! Generates Siril .ssf scripts from export data and configuration.

use crate::export::file_organizer::ExportFolders;
use crate::export::models::{ExportConfig, ExportData, FilterExportGroup, SirilWorkflow};
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
                .any(|sub| sub.imagetyp == "BIAS" && !sub.frames.is_empty())
        })
        || filter_group.dark_sets.iter().any(|s| {
            s.sub_calibrations
                .iter()
                .any(|sub| sub.imagetyp == "BIAS" && !sub.frames.is_empty())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_placeholder_replacement() {
        let folders = ExportFolders::new(PathBuf::from("/tmp/export"));
        let config = ExportConfig {
            frame_set_id: 1,
            output_dir: PathBuf::from("/tmp/export"),
            mode: crate::export::models::ExportMode::OrganizeAndScript,
            workflow: SirilWorkflow::MonoPreprocessing,
            create_masters: true,
            rejection_low: 3.0,
            rejection_high: 3.0,
            use_symlinks: false,
        };

        let template = "cd {working_dir}\nfilter: {filter}\nrej: {rejection_low} {rejection_high}";
        let result = apply_placeholders(template, &config, &folders, "Ha");

        assert!(result.contains("cd /tmp/export"));
        assert!(result.contains("filter: Ha"));
        assert!(result.contains("rej: 3 3"));
    }
}
