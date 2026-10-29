//! The location occasion: diagnostics spawned once the document is known to
//! be at a place on disk.

use std::io::ErrorKind;

use super::{DiagnosticSink, LocationContext};
use crate::diagnostics::{catalog, Finding};
use crate::file_load::{FileLoad, FileLookup, LoadResolution};

/// Spawns every diagnostic that depends on where the document is.
pub(super) fn spawn(context: &LocationContext, sink: &mut DiagnosticSink) {
    unloadable_file(context, sink);
    unavailable_init_file(context, sink);
}

/// Flags each `load`, `needs` and `input` call naming a file Macaulay2 cannot
/// run from where the document is.
fn unloadable_file(context: &LocationContext, sink: &mut DiagnosticSink) {
    for load in context.document.file_loads() {
        let resolution = load.resolve(context.uri, context.sources.path());
        if let Some(message) = unloadable_file_message(load, &resolution) {
            sink.spawn(
                &catalog::UNLOADABLE_FILE,
                Finding::new(load.literal, message),
            );
        }
    }
}

/// Why `load` cannot run what `resolution` found, and what to do about it;
/// `None` when it can, or when the server cannot tell.
fn unloadable_file_message(load: &FileLoad, resolution: &LoadResolution) -> Option<String> {
    let name = &load.name;
    let message = match resolution {
        LoadResolution::File(_) | LoadResolution::Undetermined => return None,
        LoadResolution::EmptyName => {
            "An empty file name loads nothing: name the `.m2` file to run.".to_string()
        }
        LoadResolution::Directory(_) => format!(
            "`{name}` is a directory, not a file: name a `.m2` file inside it, \
             such as `{}/file.m2`.",
            name.trim_end_matches('/')
        ),
        LoadResolution::Inaccessible { reason, .. } if *reason == ErrorKind::PermissionDenied => {
            format!(
                "`{name}` cannot be opened: permission denied. Macaulay2 fails to \
                 load it until the file is readable and its directories searchable."
            )
        }
        LoadResolution::Inaccessible { reason, .. } => {
            format!("`{name}` cannot be opened: {reason}.")
        }
        LoadResolution::Missing => match load.lookup() {
            FileLookup::Absolute => format!("No file `{name}` to load."),
            FileLookup::HomeRelative => {
                format!("No file `{name}` to load in the home directory.")
            }
            FileLookup::BesideLoader => format!(
                "No file `{name}` to load: a name starting with `./` or `../` is \
                 looked up only in the directory of this file (`currentFileDirectory`)."
            ),
            FileLookup::BesideLoaderThenPath | FileLookup::NotAFile => format!(
                "No file `{name}` to load, neither in the directory of this file \
                 nor along `path`; a relative name is looked up from this file's \
                 directory first."
            ),
        },
    };
    Some(message)
}

/// Flags an `initFile` directive whose file cannot be read: the document
/// then starts without an init file.
fn unavailable_init_file(context: &LocationContext, sink: &mut DiagnosticSink) {
    let (Some(directive), Some(problem)) = (
        context.document.init_file_directive(),
        context.document.object_registry().init_file().problem(),
    ) else {
        return;
    };
    sink.spawn(
        &catalog::UNAVAILABLE_INIT_FILE,
        Finding::new(
            directive.range,
            format!(
                "{problem}, so this file is analyzed without an init file. Point \
                 the directive at a readable file, relative to this file's \
                 directory, or write `initFile none` to start without one."
            ),
        ),
    );
}
