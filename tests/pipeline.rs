//! Full-process language-server integration tests.

mod pipeline {
    mod analysis;
    mod coalescing_update;
    mod counted_loop;
    mod hierarchies;
    mod init_file;
    mod lsp;
    mod package_documentation;
    mod workspace_files;
}
mod support;
