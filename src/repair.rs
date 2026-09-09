use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    pub fn icon(&self) -> &'static str {
        match self {
            Severity::Info => "ℹ️ ",
            Severity::Warning => "⚠️ ",
            Severity::Error => "❌ ",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RepairIssue {
    pub severity: Severity,
    pub message: String,
    pub line_number: Option<u64>,
}

impl RepairIssue {
    pub fn warning(message: impl Into<String>, line_number: Option<u64>) -> Self {
        Self {
            severity: Severity::Warning,
            message: message.into(),
            line_number,
        }
    }

    pub fn error(message: impl Into<String>, line_number: Option<u64>) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            line_number,
        }
    }
}

pub struct RepairOptions {
    pub file_path: String,
}

pub struct RepairReport {
    pub issues: Vec<RepairIssue>,
    pub fixed_count: usize,
    pub remaining_issues: usize,
    pub success: bool,
}

fn report_issue(issue: RepairIssue) {
    let line_info = issue.line_number.map(|n| format!(" (line {})", n)).unwrap_or_default();
    eprintln!("{}{}{}", issue.severity.icon(), issue.message, line_info);
}

fn normalize_texture_path(texture_path: &str) -> String {
    // Normalize the path to use forward slashes and remove leading/trailing whitespace
    texture_path.trim().replace('\\', "/")
}

fn get_texture_filename(texture_path: &str) -> Option<String> {
    let normalized = normalize_texture_path(texture_path);
    let path = Path::new(&normalized);
    
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|s| s.to_string())
}

fn check_texture_file_exists(base_dir: &Path, texture_name: &str) -> bool {
    let full_path = base_dir.join(texture_name);
    full_path.exists()
}

fn check_texture_case_insensitive(base_dir: &Path, texture_name: &str) -> Option<String> {
    if let Ok(entries) = fs::read_dir(base_dir) {
        for entry in entries.filter_map(Result::ok) {
            if let Ok(file_name) = entry.file_name().into_string() {
                if file_name.eq_ignore_ascii_case(texture_name) {
                    return Some(file_name);
                }
            }
        }
    }
    None
}

fn check_texture_extension(base_dir: &Path, texture_name: &str, expected_extensions: &[&str]) -> Option<String> {
    let path = Path::new(texture_name);
    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
        for &ext in expected_extensions {
            let candidate = base_dir.join(format!("{}.{}", stem, ext));
            if candidate.exists() {
                return Some(ext.to_string());
            }
        }
    }
    None
}

pub fn run(opts: RepairOptions) -> Result<RepairReport, Box<dyn std::error::Error>> {
    let file_path = Path::new(&opts.file_path);
    let base_dir = file_path.parent().unwrap_or(Path::new("."));
    
    let file = File::open(&opts.file_path)
        .map_err(|e| format!("{}: {}", opts.file_path, e))?;
    let reader = BufReader::with_capacity(64 * 1024, file);

    let mut issues: Vec<RepairIssue> = Vec::new();
    let mut line_number = 0u64;
    
    // Common texture extensions to check for
    let common_extensions = vec!["png", "jpg", "jpeg", "tga", "bmp", "gif", "tif", "tiff"];
    
    for line_result in reader.lines() {
        line_number += 1;
        let line = line_result?;
        
        // Skip empty lines and comments
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }

        match parts[0] {
            "map_Kd" | "map_Ks" | "map_Ka" | "map_Bump" | "map_d" | "map_Ns" => {
                if parts.len() > 1 {
                    let texture_path = parts[1..].join(" ");
                    let normalized_path = normalize_texture_path(&texture_path);
                    
                    // Check if texture file exists
                    let texture_file = get_texture_filename(&normalized_path);
                    
                    if let Some(filename) = &texture_file {
                        // Check if file exists in the same directory as the OBJ
                        if !check_texture_file_exists(base_dir, filename) {
                            // Try to find with different case
                            if let Some(case_match) = check_texture_case_insensitive(base_dir, filename) {
                                issues.push(RepairIssue::warning(
                                    format!("Texture file '{}' found with different case: '{}'", filename, case_match),
                                    Some(line_number)
                                ));
                            } else {
                                // Check for common extensions
                                if let Some(found_ext) = check_texture_extension(base_dir, filename, &common_extensions) {
                                    issues.push(RepairIssue::warning(
                                        format!("Texture file '{}' found with extension '{}'", filename, found_ext),
                                        Some(line_number)
                                    ));
                                } else {
                                    issues.push(RepairIssue::error(
                                        format!("Texture file '{}' not found", filename),
                                        Some(line_number)
                                    ));
                                }
                            }
                        } else {
                            // File exists, but check for potential issues
                            let file_path = Path::new(&normalized_path);
                            if let Some(extension) = file_path.extension() {
                                let ext_lower = extension.to_string_lossy().to_lowercase();
                                if !common_extensions.contains(&ext_lower.as_str()) {
                                    issues.push(RepairIssue::warning(
                                        format!("Texture file '{}' has unusual extension '{}'", filename, ext_lower),
                                        Some(line_number)
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    
    // Count issues
    let fixed_issues = issues.iter().filter(|i| i.severity == Severity::Warning).count();
    let remaining_issues = issues.iter().filter(|i| i.severity != Severity::Info).count();
    
    let success = remaining_issues == 0;
    
    // Report issues
    for issue in &issues {
        report_issue(issue.clone());
    }
    
    Ok(RepairReport {
        issues,
        fixed_count: fixed_issues,
        remaining_issues,
        success,
    })
}