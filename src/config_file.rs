//! The `m2-ls.toml` files settings are also read from.
//!
//! Besides what the editor sends, settings come from two files, written in
//! TOML with the same groups and keys:
//!
//! ```toml
//! [formatting]
//! preset = "macaulay2"
//! indent = 2
//!
//! [diagnostics]
//! disabled = ["unused-binding"]
//! ```
//!
//! - the user's file, `m2-ls/m2-ls.toml` in the user configuration directory
//!   (`$XDG_CONFIG_HOME`, normally `~/.config`; `%APPDATA%` on Windows);
//! - the project's file, `m2-ls.toml` or `.m2-ls.toml`, the nearest one in the
//!   workspace folder or a folder above it.
//!
//! They are layered lowest first: the defaults, the user's file, the editor's
//! settings, the project's file. The project's file wins so that everyone
//! formatting a project, in any editor or with `m2-fmt`, gets the same result.
//! A `.json` file holds the same settings as JSON.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::settings::{ParsedSettings, ServerSettings, SettingsLayer};

/// The names of a project's settings file, in the order looked for.
pub const PROJECT_FILE_NAMES: [&str; 2] = ["m2-ls.toml", ".m2-ls.toml"];

/// The name of the user's settings file inside its `m2-ls` directory.
const USER_FILE_NAME: &str = "m2-ls.toml";

/// Overrides the user's settings file; an empty value disables it.
const USER_FILE_ENV: &str = "M2_LS_CONFIG";

/// Which project settings file the editor's `configFile` setting selects.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum ConfigFileChoice {
    /// `true`, `false`: whether to look for `m2-ls.toml` at all.
    Discover(bool),
    /// A file to read instead, relative to the workspace folder.
    Path(PathBuf),
    /// Not set: look for it.
    #[default]
    #[serde(skip)]
    Default,
}

impl ConfigFileChoice {
    /// The choice the editor's settings `editor` make under `configFile`.
    pub fn of_editor_settings(editor: &Value) -> Self {
        ["m2-ls", "macaulay2"]
            .iter()
            .find_map(|name| editor.get(name))
            .unwrap_or(editor)
            .get("configFile")
            .and_then(|value| Self::deserialize(value).ok())
            .unwrap_or_default()
    }
}

/// A settings file read from disk.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigFile {
    /// Where it was read from.
    pub path: PathBuf,
    /// Its settings, as JSON.
    pub value: Value,
}

impl ConfigFile {
    /// Read the settings file at `path`: TOML, or JSON when it ends in
    /// `.json`. The error names the file.
    pub fn read(path: &Path) -> Result<Self, String> {
        let text =
            fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let value = if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            serde_json::from_str(&text).map_err(|error| error.to_string())
        } else {
            toml::from_str::<Value>(&text).map_err(|error| error.message().to_string())
        }
        .map_err(|error| format!("{}: {error}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            value,
        })
    }

    /// The layer this file's settings form.
    fn layer(&self) -> SettingsLayer {
        SettingsLayer {
            origin: Some(self.path.display().to_string()),
            value: self.value.clone(),
        }
    }

    /// The user's settings file, when there is one: `M2_LS_CONFIG`, or
    /// `m2-ls/m2-ls.toml` in the user configuration directory.
    pub fn user_path() -> Option<PathBuf> {
        if let Some(path) = env::var_os(USER_FILE_ENV) {
            return (!path.is_empty()).then(|| PathBuf::from(path));
        }
        #[cfg(target_os = "windows")]
        let directory = env::var_os("APPDATA").map(PathBuf::from);
        #[cfg(not(target_os = "windows"))]
        let directory = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
        Some(directory?.join("m2-ls").join(USER_FILE_NAME))
    }

    /// The nearest project settings file in `directory` or a directory
    /// above it.
    pub fn find_project_path(directory: &Path) -> Option<PathBuf> {
        directory.ancestors().find_map(|directory| {
            PROJECT_FILE_NAMES
                .iter()
                .map(|name| directory.join(name))
                .find(|path| path.is_file())
        })
    }

    /// Whether `path` has the name of a settings file, so a change to it
    /// changes the settings.
    pub fn is_settings_file_name(path: &Path) -> bool {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| PROJECT_FILE_NAMES.contains(&name))
    }
}

/// Every source of settings besides the defaults, read once.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SettingsSources {
    /// The user's settings file, when there is one.
    pub user: Option<ConfigFile>,
    /// What the editor sent.
    pub editor: Value,
    /// The project's settings file, when there is one.
    pub project: Option<ConfigFile>,
    /// Files that exist but could not be read, as messages naming them.
    pub unreadable: Vec<String>,
}

impl SettingsSources {
    /// Read the settings files for the editor settings `editor` and the
    /// project in `project_directory`: the user's file, and the project's
    /// file as the editor's `configFile` selects it.
    pub fn read(editor: Value, project_directory: Option<&Path>) -> Self {
        let mut unreadable = Vec::new();
        let mut read = |path: Option<PathBuf>, required: bool| {
            let path = path?;
            if !required && !path.is_file() {
                return None;
            }
            ConfigFile::read(&path)
                .map_err(|error| unreadable.push(error))
                .ok()
        };
        let user = read(ConfigFile::user_path(), false);
        let project = match ConfigFileChoice::of_editor_settings(&editor) {
            ConfigFileChoice::Discover(false) => None,
            ConfigFileChoice::Path(path) => {
                let path = expand_home(&path);
                let path = match project_directory {
                    Some(directory) if path.is_relative() => directory.join(path),
                    _ => path,
                };
                read(Some(path), true)
            }
            ConfigFileChoice::Discover(true) | ConfigFileChoice::Default => read(
                project_directory.and_then(ConfigFile::find_project_path),
                false,
            ),
        };
        Self {
            user,
            editor,
            project,
            unreadable,
        }
    }

    /// The settings these sources give, layered: the user's file, the
    /// editor's settings, the project's file.
    pub fn parse(&self) -> ParsedSettings {
        ServerSettings::from_layers(
            self.user
                .iter()
                .map(ConfigFile::layer)
                .chain([SettingsLayer::editor(self.editor.clone())])
                .chain(self.project.iter().map(ConfigFile::layer)),
        )
    }
}

/// `path` with a leading `~` standing for the home directory.
fn expand_home(path: &Path) -> PathBuf {
    let home = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE"));
    match (path.strip_prefix("~"), home) {
        (Ok(relative), Some(home)) => PathBuf::from(home).join(relative),
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::formatting::ControlFlowLayout;

    fn write(directory: &Path, name: &str, text: &str) -> PathBuf {
        let path = directory.join(name);
        fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn toml_files_hold_the_same_settings_as_the_editor_sends() {
        let directory = tempfile::tempdir().unwrap();
        let path = write(
            directory.path(),
            "m2-ls.toml",
            "[formatting]\nindent = 2\ncontrolFlowLayout = \"multiline\"\n\n[diagnostics]\ndisabled = [\"unused-binding\"]\n",
        );
        let file = ConfigFile::read(&path).unwrap();
        let parsed = ServerSettings::from_layers([file.layer()]);
        assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
        assert_eq!(parsed.settings.formatting.indent, Some(2));
        assert_eq!(
            parsed.settings.formatting.control_flow_layout,
            ControlFlowLayout::Multiline
        );
    }

    #[test]
    fn the_nearest_project_file_above_a_directory_is_found() {
        let directory = tempfile::tempdir().unwrap();
        let nested = directory.path().join("a/b");
        fs::create_dir_all(&nested).unwrap();
        let outer = write(directory.path(), "m2-ls.toml", "");
        assert_eq!(ConfigFile::find_project_path(&nested), Some(outer));
        let inner = write(&directory.path().join("a"), ".m2-ls.toml", "");
        assert_eq!(ConfigFile::find_project_path(&nested), Some(inner));
    }

    #[test]
    fn the_project_file_wins_over_the_editor_and_the_editor_over_the_user_file() {
        let user = ConfigFile {
            path: PathBuf::from("user.toml"),
            value: serde_json::json!({"formatting": {"indent": 8, "useTabs": true, "hardLineWidth": 70}}),
        };
        let project = ConfigFile {
            path: PathBuf::from("m2-ls.toml"),
            value: serde_json::json!({"formatting": {"indent": 2}}),
        };
        let sources = SettingsSources {
            user: Some(user),
            editor: serde_json::json!({"macaulay2": {"formatting": {"indent": 4, "useTabs": false}}}),
            project: Some(project),
            unreadable: Vec::new(),
        };
        let formatting = sources.parse().settings.formatting;
        assert_eq!(formatting.indent, Some(2));
        assert_eq!(formatting.use_tabs, Some(false));
        assert_eq!(formatting.hard_line_width, Some(70));
    }

    #[test]
    fn a_bad_value_in_a_file_names_the_file_and_keeps_the_layer_below() {
        let sources = SettingsSources {
            editor: serde_json::json!({"formatting": {"indent": 4}}),
            project: Some(ConfigFile {
                path: PathBuf::from("m2-ls.toml"),
                value: serde_json::json!({"formatting": {"indent": "two"}}),
            }),
            ..SettingsSources::default()
        };
        let parsed = sources.parse();
        assert_eq!(parsed.settings.formatting.indent, Some(4));
        assert_eq!(parsed.problems.len(), 1);
        assert!(parsed.problems[0]
            .to_string()
            .starts_with("m2-ls.toml: `formatting.indent`"));
    }

    #[test]
    fn a_preset_in_the_project_file_overrides_the_editors_formatting() {
        let sources = SettingsSources {
            editor: serde_json::json!({"formatting": {"controlFlowLayout": "compact", "indent": 3}}),
            project: Some(ConfigFile {
                path: PathBuf::from("m2-ls.toml"),
                value: serde_json::json!({"formatting": {"preset": "lua", "indent": 2}}),
            }),
            ..SettingsSources::default()
        };
        let formatting = sources.parse().settings.formatting;
        assert_eq!(formatting.control_flow_layout, ControlFlowLayout::Multiline);
        assert_eq!(formatting.indent, Some(2));
        assert_eq!(formatting.space_in_braces, Some(true));
    }

    #[test]
    fn the_editor_can_turn_the_project_file_off_or_name_another() {
        let directory = tempfile::tempdir().unwrap();
        write(directory.path(), "m2-ls.toml", "[formatting]\nindent = 2\n");
        write(directory.path(), "team.toml", "[formatting]\nindent = 6\n");
        let indent = |editor: Value| {
            SettingsSources::read(editor, Some(directory.path()))
                .parse()
                .settings
                .formatting
                .indent
        };
        assert_eq!(indent(serde_json::json!({})), Some(2));
        assert_eq!(indent(serde_json::json!({"configFile": false})), None);
        assert_eq!(
            indent(serde_json::json!({"configFile": "team.toml"})),
            Some(6)
        );
        let missing = SettingsSources::read(
            serde_json::json!({"configFile": "missing.toml"}),
            Some(directory.path()),
        );
        assert_eq!(missing.unreadable.len(), 1);
    }
}
