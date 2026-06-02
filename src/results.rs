use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub title: String,
    pub level: ResultLevel,
    pub summary: String,
    pub details: Vec<String>,
}

impl ToolResult {
    pub fn info(
        title: impl Into<String>,
        summary: impl Into<String>,
        details: Vec<String>,
    ) -> Self {
        Self {
            title: title.into(),
            level: ResultLevel::Info,
            summary: summary.into(),
            details,
        }
    }

    pub fn warning(
        title: impl Into<String>,
        summary: impl Into<String>,
        details: Vec<String>,
    ) -> Self {
        Self {
            title: title.into(),
            level: ResultLevel::Warning,
            summary: summary.into(),
            details,
        }
    }

    pub fn error(
        title: impl Into<String>,
        summary: impl Into<String>,
        details: Vec<String>,
    ) -> Self {
        Self {
            title: title.into(),
            level: ResultLevel::Error,
            summary: summary.into(),
            details,
        }
    }
}

pub fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub fn display_paths(paths: &[PathBuf]) -> Vec<String> {
    paths.iter().map(|path| display_path(path)).collect()
}
