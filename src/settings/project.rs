//! Global and project TOML settings, merged before explicit client settings.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{ParsedSettings, ServerSettings, SettingIssue, SettingProblem};

/// Conventional configuration locations shared by the server and CLI tools.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProjectConfiguration;

impl ProjectConfiguration {
    /// Distinctive project configuration file, also usable as an editor root marker.
    pub const FILE_NAME: &str = "M2LS.toml";
    /// Directory under the user's home holding global configuration.
    pub const GLOBAL_DIRECTORY: &str = ".m2-ls";

    /// Merges global settings, the root's project file, and explicit editor
    /// settings, in that order. Arrays replace previous arrays; tables merge.
    /// Missing files are optional; read and parse failures become setting problems.
    pub fn load(root: Option<&Path>, client: &Value) -> ParsedSettings {
        let global = Self::global_path();
        let project = root.map(|root| root.join(Self::FILE_NAME));
        Self::load_paths(global.as_deref(), project.as_deref(), client)
    }

    /// Global settings path supplied by the operating system's user profile.
    /// An unavailable home directory leaves only project and explicit settings.
    pub fn global_path() -> Option<PathBuf> {
        std::env::home_dir().map(|home| home.join(Self::GLOBAL_DIRECTORY).join(Self::FILE_NAME))
    }

    /// Loads explicitly located files, keeping configuration tests independent
    /// of the current user's home and editor installation.
    pub fn load_paths(
        global: Option<&Path>,
        project: Option<&Path>,
        client: &Value,
    ) -> ParsedSettings {
        let mut value = Value::Object(Default::default());
        let mut problems = Vec::new();
        for path in [global, project].into_iter().flatten() {
            match fs::read_to_string(path) {
                Ok(text) => match toml::from_str::<Value>(&text) {
                    Ok(settings) => Self::merge(&mut value, ServerSettings::section(&settings)),
                    Err(error) => problems.push(SettingProblem {
                        path: path.display().to_string(),
                        issue: SettingIssue::Invalid(error.to_string()),
                    }),
                },
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => problems.push(SettingProblem {
                    path: path.display().to_string(),
                    issue: SettingIssue::Invalid(error.to_string()),
                }),
            }
        }
        if !client.is_null() {
            Self::merge(&mut value, ServerSettings::section(client));
        }
        let mut parsed = ServerSettings::from_value(&value);
        problems.append(&mut parsed.problems);
        parsed.problems = problems;
        parsed
    }

    /// Nearest local project file for standalone tools, which have no editor
    /// workspace root. A file path searches from its containing directory.
    pub fn find_local(path: &Path) -> Option<PathBuf> {
        let directory = if path.is_dir() { path } else { path.parent()? };
        directory
            .ancestors()
            .map(|directory| directory.join(Self::FILE_NAME))
            .find(|candidate| candidate.is_file())
    }

    /// Recursive object overlay; scalar and list values replace the old value.
    fn merge(base: &mut Value, overlay: &Value) {
        if let (Some(base), Some(overlay)) = (base.as_object_mut(), overlay.as_object()) {
            for (key, value) in overlay {
                Self::merge(base.entry(key.clone()).or_insert(Value::Null), value);
            }
        } else {
            *base = overlay.clone();
        }
    }
}
