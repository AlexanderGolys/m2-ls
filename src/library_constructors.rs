//! The library functions that make a value of the type their name names,
//! whatever they are given, as [`LibraryConstructors::FILE`] lists them.

use std::collections::HashSet;
use std::sync::LazyLock;

use serde::Deserialize;

use crate::object_registry::ObjectName;

/// The functions of [`LibraryConstructors::FILE`]: `ideal`, which makes an
/// `Ideal` of a list, a matrix or a ring alike.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryConstructors {
    /// The functions' names.
    functions: HashSet<ObjectName>,
}

impl LibraryConstructors {
    /// The compiled-in constructor list.
    pub const FILE: &str = include_str!("data/library_constructors.toml");

    /// The list [`Self::FILE`] gives.
    pub fn get() -> &'static Self {
        static TABLE: LazyLock<LibraryConstructors> = LazyLock::new(|| {
            toml::from_str(LibraryConstructors::FILE)
                .unwrap_or_else(|error| panic!("the library constructors file: {error}"))
        });
        &TABLE
    }

    /// The type the function `name` makes, its name Pascal-cased: `Ideal`
    /// for `ideal`; `None` unless it is a constructor.
    pub fn made_type(&self, name: &ObjectName) -> Option<ObjectName> {
        self.functions.contains(name).then(|| {
            let (first, rest) = name.name().split_at(1);
            ObjectName::new(first.to_uppercase() + rest)
        })
    }
}
