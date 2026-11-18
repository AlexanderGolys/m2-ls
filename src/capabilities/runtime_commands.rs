//! The commands that run Macaulay2 on request, and the code lenses offering
//! them in any client that renders lenses.
//!
//! ```m2
//! -- ▶ Run file
//! newPackage "Tiny"     -- ▶ Run tests, when the package registers TESTs
//! ```

use serde_json::json;
use strum::{EnumIter, EnumString, IntoStaticStr};
use tower_lsp::lsp_types::{CodeLens, Command, Position, Range, Url};

use crate::document::DocumentSnapshot;

/// A `workspace/executeCommand` that starts Macaulay2. Each takes one object
/// argument carrying the `uri` of the file it concerns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, EnumString, IntoStaticStr)]
#[non_exhaustive]
pub enum RuntimeCommand {
    /// Runs the saved file with `input`.
    #[strum(serialize = "m2-ls.executeFile")]
    ExecuteFile,
    /// Evaluates the `code` the client supplies.
    #[strum(serialize = "m2-ls.executeCell")]
    ExecuteCell,
    /// Loads the `package` from the file and runs its TESTs with `check`.
    #[strum(serialize = "m2-ls.runTests")]
    RunTests,
}

impl RuntimeCommand {
    /// The command identifier sent over the protocol.
    pub fn name(self) -> &'static str {
        self.into()
    }

    /// The label of the lens offering the command; a cell has no lens, as
    /// the client chooses its code.
    fn title(self) -> &'static str {
        match self {
            Self::ExecuteFile => "▶ Run file",
            Self::ExecuteCell => "▶ Run cell",
            Self::RunTests => "▶ Run tests",
        }
    }

    /// A lens at `line` invoking the command with `argument`.
    fn lens(self, line: u32, argument: serde_json::Value) -> CodeLens {
        let start = Position::new(line, 0);
        CodeLens {
            range: Range::new(start, start),
            command: Some(Command::new(
                self.title().into(),
                self.name().into(),
                Some(vec![argument]),
            )),
            data: None,
        }
    }

    /// The lenses of `document`: running the file on its first line, and
    /// running the tests of each declared package that registers any on its
    /// `newPackage` line.
    pub fn code_lenses(document: &DocumentSnapshot, uri: &Url) -> Vec<CodeLens> {
        let packages = document.packages();
        let tested = packages.declarations().iter().filter(|declaration| {
            packages
                .tests()
                .iter()
                .any(|test| test.package.as_ref() == Some(&declaration.name))
        });
        std::iter::once(Self::ExecuteFile.lens(0, json!({ "uri": uri })))
            .chain(tested.map(|declaration| {
                Self::RunTests.lens(
                    declaration.section.start.line,
                    json!({ "uri": uri, "package": declaration.name.name() }),
                )
            }))
            .collect()
    }
}
