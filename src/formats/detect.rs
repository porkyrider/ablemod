use std::path::Path;

use crate::formats::base::Module;
use crate::formats::protracker;

fn supported_format(ext: &str) -> Option<&'static str> {
    match ext {
        "mod" => Some("protracker"),
        "xm" => Some("fasttracker2"),
        "s3m" => Some("screamtracker3"),
        _ => None,
    }
}

fn implemented(fmt: &str) -> bool {
    fmt == "protracker"
}

pub fn detect_format(path: &Path) -> Result<&'static str, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let fmt = supported_format(&ext)
        .ok_or_else(|| format!("Unrecognized module extension: '.{ext}' (expected .mod, .xm or .s3m)"))?;
    if !implemented(fmt) {
        return Err(format!(
            "{fmt} support (.{ext}) is not implemented yet — only ProTracker (.mod) is supported so far"
        ));
    }
    Ok(fmt)
}

pub fn load_module(path: &Path) -> Result<Module, String> {
    let fmt = detect_format(path)?;
    let data = std::fs::read(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    match fmt {
        "protracker" => Ok(protracker::parse(&data)),
        _ => unreachable!("unreachable: {fmt}"),
    }
}
