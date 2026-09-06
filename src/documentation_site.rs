//! mdBook generation for structurally attached source documentation.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::document::DocumentSnapshot;
use crate::documentation::{DocumentationBlock, DocumentationTarget};
use crate::object_registry::ObjectRegistry;

/// A semantic page name used independently from its filesystem-safe slug.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DocumentationPageName(String);

/// The source-local identity of one generated page.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DocumentationPageId {
    source: PathBuf,
    name: DocumentationPageName,
}

/// One generated page, containing only explicitly attached documentation.
#[derive(Debug)]
struct DocumentationPage {
    id: DocumentationPageId,
    title: String,
    slug: String,
    blocks: Vec<DocumentationBlock>,
}

/// The result of generating an mdBook source tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneratedDocumentation {
    pub pages: usize,
}

/// Generate an mdBook whose pages correspond only to structurally documented
/// packages and items.
pub fn generate_documentation_book(
    input: &Path,
    output: &Path,
    registry: &ObjectRegistry,
) -> io::Result<GeneratedDocumentation> {
    let source_root = if input.is_dir() {
        input.to_path_buf()
    } else {
        input
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    };
    let mut source_files = Vec::new();
    collect_source_files(input, &mut source_files)?;
    source_files.sort();

    let mut pages = Vec::new();
    for source_file in source_files {
        let text = fs::read_to_string(&source_file)?;
        let Some(document) = DocumentSnapshot::from_text(text, registry) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("could not parse {}", source_file.display()),
            ));
        };
        let relative_source = source_file
            .strip_prefix(&source_root)
            .unwrap_or(&source_file)
            .to_path_buf();
        let package_name = source_file
            .file_stem()
            .and_then(OsStr::to_str)
            .unwrap_or("Package");
        for block in document.documentation_blocks() {
            let mut block_names = Vec::new();
            for target in block.targets() {
                let name = match target {
                    DocumentationTarget::Package => package_name,
                    target => match target.name() {
                        Some(name) => name,
                        None => continue,
                    },
                };
                if block_names.contains(&name) {
                    continue;
                }
                block_names.push(name);
                let id = DocumentationPageId {
                    source: relative_source.clone(),
                    name: DocumentationPageName(name.to_string()),
                };
                if let Some(page) = pages
                    .iter_mut()
                    .find(|page: &&mut DocumentationPage| page.id == id)
                {
                    page.blocks.push(block.clone());
                } else {
                    pages.push(DocumentationPage {
                        slug: page_slug(&relative_source, name),
                        id,
                        title: name.to_string(),
                        blocks: vec![block.clone()],
                    });
                }
            }
        }
    }

    fs::create_dir_all(output.join("src/items"))?;
    write_book_configuration(output)?;
    write_summary(output, &pages)?;
    for page in &pages {
        write_page(output, page, &pages)?;
    }
    copy_tree_sitter_assets(output)?;

    Ok(GeneratedDocumentation { pages: pages.len() })
}

/// Build a generated documentation book with the local mdBook toolchain.
pub fn build_documentation_book(output: &Path) -> io::Result<()> {
    let status = Command::new("mdbook").arg("build").arg(output).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "mdbook build exited with {status}"
        )))
    }
}

fn collect_source_files(path: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    if path.is_file() {
        if path.extension() == Some(OsStr::new("m2")) {
            files.push(path.to_path_buf());
        }
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if matches!(
                path.file_name().and_then(OsStr::to_str),
                Some(".git" | "target" | "book")
            ) {
                continue;
            }
            collect_source_files(&path, files)?;
        } else if path.extension() == Some(OsStr::new("m2")) {
            files.push(path);
        }
    }
    Ok(())
}

fn write_book_configuration(output: &Path) -> io::Result<()> {
    fs::write(
        output.join("book.toml"),
        concat!(
            "[book]\n",
            "title = \"Macaulay2 package documentation\"\n",
            "src = \"src\"\n",
            "\n",
            "[preprocessor.tsitter]\n",
            "after = [\"links\"]\n",
            "\n",
            "[preprocessor.tsitter.languages.macaulay2]\n",
            "library = \"parsers/macaulay2.so\"\n",
            "highlights = \"queries/macaulay2/highlights.scm\"\n",
            "aliases = [\"m2\", \"macaulay2\"]\n",
            "\n",
            "[output.html]\n",
            "additional-css = [\"theme/treesitter.css\"]\n",
        ),
    )
}

fn write_summary(output: &Path, pages: &[DocumentationPage]) -> io::Result<()> {
    let mut summary = String::from("# Summary\n\n");
    for page in pages {
        summary.push_str(&format!("- [{}](items/{}.md)\n", page.title, page.slug));
    }
    fs::write(output.join("src/SUMMARY.md"), summary)
}

fn write_page(
    output: &Path,
    page: &DocumentationPage,
    pages: &[DocumentationPage],
) -> io::Result<()> {
    let mut markdown = format!("# {}\n\n", page.title);
    for (index, block) in page.blocks.iter().enumerate() {
        if index > 0 {
            markdown.push_str(&format!(
                "\n\n## Additional documentation at line {}\n\n",
                block.source_span().range().start.line + 1
            ));
        }
        markdown.push_str(&block.render_markdown(|name| {
            resolve_page(pages, &page.id.source, name).map(|target| format!("{}.md", target.slug))
        }));
        markdown.push('\n');
    }
    fs::write(
        output.join("src/items").join(format!("{}.md", page.slug)),
        markdown,
    )
}

fn resolve_page<'a>(
    pages: &'a [DocumentationPage],
    source: &Path,
    name: &str,
) -> Option<&'a DocumentationPage> {
    pages
        .iter()
        .find(|page| page.id.source == source && page.id.name.0 == name)
        .or_else(|| {
            let mut matches = pages.iter().filter(|page| page.id.name.0 == name);
            let page = matches.next()?;
            matches.next().is_none().then_some(page)
        })
}

fn page_slug(source: &Path, name: &str) -> String {
    let source = source.with_extension("").to_string_lossy().to_string();
    format!("{}--{}", slug_component(&source), slug_component(name))
}

fn slug_component(value: &str) -> String {
    let mut slug = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() {
            slug.push(char::from(byte));
        } else {
            slug.push_str(&format!("_{byte:02x}"));
        }
    }
    slug
}

/// Where the Tree-sitter assets a generated book needs are found.
///
/// `M2_LS_DOC_ASSETS` wins, so a packager can point at an installed location.
/// Otherwise they are looked for beside the executable and then in the source
/// tree. A bare relative `docs` would resolve against the working directory,
/// which makes the command work only when run from the repository root — the
/// one place a user documenting their own package is not standing.
fn documentation_asset_root() -> PathBuf {
    if let Some(configured) = std::env::var_os("M2_LS_DOC_ASSETS") {
        return PathBuf::from(configured);
    }
    std::env::current_exe()
        .ok()
        .and_then(|executable| executable.parent().map(|directory| directory.join("docs")))
        .filter(|candidate| candidate.is_dir())
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/docs")))
}

fn copy_tree_sitter_assets(output: &Path) -> io::Result<()> {
    let assets = documentation_asset_root();
    let copies = [
        ("parsers/macaulay2.so", "parsers/macaulay2.so"),
        (
            "queries/macaulay2/highlights.scm",
            "queries/macaulay2/highlights.scm",
        ),
        ("theme/treesitter.css", "theme/treesitter.css"),
    ];
    for (source, destination) in copies {
        let source = assets.join(source);
        if !source.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "missing documentation asset {}; set M2_LS_DOC_ASSETS to the m2-ls docs directory",
                    source.display()
                ),
            ));
        }
        let destination = output.join(destination);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, destination)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_preserve_names_without_exposing_path_syntax() {
        assert_eq!(
            page_slug(Path::new("Package/File.m2"), "ideal(ZZ)"),
            "Package_2fFile--ideal_28ZZ_29"
        );
    }
}
