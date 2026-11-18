//! What a program may assign to the library globals Macaulay2 leaves
//! rebindable. Each keeps the class the index records for its value; a few
//! integer settings also accept only a range, which
//! [`LibraryGlobals::FILE`] lists.

use std::collections::HashMap;
use std::fmt::{self, Display, Formatter};
use std::sync::LazyLock;

use serde::Deserialize;

use crate::object_registry::ObjectName;

/// The integers a library global accepts, bounded on either side or both:
/// `allowableThreads` takes `{ min = 1 }`, `errorDepth` takes
/// `{ min = 0, max = 255 }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegerRange {
    /// The least accepted value.
    pub min: Option<i128>,
    /// The greatest accepted value.
    pub max: Option<i128>,
}

impl IntegerRange {
    /// Whether `value` lies in the range.
    pub fn contains(&self, value: i128) -> bool {
        self.min.is_none_or(|min| min <= value) && self.max.is_none_or(|max| value <= max)
    }
}

impl Display for IntegerRange {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match (self.min, self.max) {
            (Some(min), Some(max)) => write!(formatter, "an integer in {min} .. {max}"),
            (Some(min), None) => write!(formatter, "an integer of at least {min}"),
            (None, Some(max)) => write!(formatter, "an integer of at most {max}"),
            (None, None) => formatter.write_str("an integer"),
        }
    }
}

/// The ranges of [`LibraryGlobals::FILE`], by global.
#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub struct LibraryGlobals(HashMap<ObjectName, IntegerRange>);

impl LibraryGlobals {
    /// The compiled-in range table.
    pub const FILE: &str = include_str!("data/library_globals.toml");

    /// The table [`Self::FILE`] gives.
    pub fn get() -> &'static Self {
        static TABLE: LazyLock<LibraryGlobals> = LazyLock::new(|| {
            toml::from_str(LibraryGlobals::FILE)
                .unwrap_or_else(|error| panic!("the library globals file: {error}"))
        });
        &TABLE
    }

    /// The integers the global `name` accepts, if Macaulay2 restricts them.
    pub fn integer_range(&self, name: &ObjectName) -> Option<&IntegerRange> {
        self.0.get(name)
    }
}
