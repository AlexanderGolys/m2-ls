//! Inlining refactors: a `load` replaced by the file it runs, and a call
//! replaced by the body of the function it calls.
//!
//! ```m2
//! load "helpers.m2"      -- ⇒ the text of helpers.m2, the file left as it is
//! area = (w, h) -> w * h
//! a = area(2, n + 1)     -- ⇒ a = (h := n + 1; 2 * h)
//! ```
//!
//! An inline is offered only where the result reads every name as the
//! original did. Where it would not, the action is still offered, disabled,
//! with the [`Hazard`] as the reason. The hazards are the measure of
//! "safe": each names one way the inlined code could mean something else,
//! and an inline none of them applies to keeps every name's meaning, every
//! argument's evaluation count and order, and the scope of every local.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use m2_syn::nodes::{BinaryOperator, Expr, ExprFunction, ExprReturn, Symbol};
use m2_syn::visit::{self, Visit};
use tower_lsp::lsp_types::{
    CodeAction, CodeActionDisabled, CodeActionKind, Position, TextEdit, Url, WorkspaceEdit,
};

use crate::analysis::{Analysis, BindingId, Dispatch};
use crate::document::DocumentSnapshot;
use crate::file_load::{FileLoad, FileLookup};
use crate::macaulay2::Macaulay2Conventions;
use crate::node_metadata::ParsedCell;
use crate::source::{ByteRange, SourceNavigation, SyntaxSource};
use crate::syntax::{
    BindingReferences, BoundSymbols, ExpressionSyntax, LambdaSyntax, PackedArguments, QuotedNames,
};
use crate::util::TextRangeExt;

/// Why an inline would change what the program does.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Hazard {
    /// The loaded file reads a global naming the file it is read from.
    FileContext(String),
    /// A `:=` local of the loaded file would become a local of the loader,
    /// which uses that name too.
    FileLocal(String),
    /// The loaded file reads or assigns a global the loader hides with a local.
    ShadowedGlobal(String),
    /// A load in the loaded file names its file relative to it, and no name
    /// reaches that file from the loader.
    UnreachableLoad(String),
    /// The body reads a name that means another binding at the call.
    Captured(String),
    /// A `return` in the body would return from the caller.
    EarlyReturn,
    /// The body quotes a parameter or a local, `symbol x`, naming its symbol.
    QuotedLocal(String),
    /// The call passes another number of arguments than the function takes.
    Arity {
        /// Parameters of the function.
        expected: usize,
        /// Arguments of the call.
        given: usize,
    },
}

impl fmt::Display for Hazard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FileContext(name) => write!(f, "`{name}` would describe this file instead"),
            Self::FileLocal(name) => {
                write!(
                    f,
                    "the file's local `{name}` would collide with `{name}` here"
                )
            }
            Self::ShadowedGlobal(name) => {
                write!(
                    f,
                    "the file uses the global `{name}`, which a local hides here"
                )
            }
            Self::UnreachableLoad(name) => {
                write!(
                    f,
                    "the file's load of \"{name}\" cannot be written from here"
                )
            }
            Self::Captured(name) => write!(f, "`{name}` means another binding at the call"),
            Self::EarlyReturn => write!(f, "`return` would return from the caller"),
            Self::QuotedLocal(name) => write!(f, "the body quotes its own `{name}`"),
            Self::Arity { expected, given } => {
                write!(
                    f,
                    "the function takes {expected} arguments, the call passes {given}"
                )
            }
        }
    }
}

/// What inlining a load reads beyond the document: where `load` looks for
/// a file, and the current text of a file, its unsaved buffer when open.
pub struct LoadedFiles<'a> {
    /// Directories `load` searches after the loader's own, Macaulay2's `path`.
    pub search_path: &'a [PathBuf],
    /// The text of a file.
    pub text: &'a dyn Fn(&Url) -> Option<String>,
}

#[cfg(test)]
impl LoadedFiles<'static> {
    /// No file can be read, so no load is inlined.
    pub const NONE: Self = Self {
        search_path: &[],
        text: &|_| None,
    };
}

/// A source text with some of its byte ranges rewritten.
///
/// ```text
/// text "x + y", range 0..5, edits [0..1 => "(a)"]  ⇒  "(a) + y"
/// ```
struct TextRewrite {
    /// The rewritten part of the text.
    range: ByteRange,
    /// Disjoint replacements inside `range`.
    edits: Vec<(ByteRange, String)>,
}

impl TextRewrite {
    /// The `edits` inside `range` that no other of them contains.
    fn outermost(edits: &[(ByteRange, String)], range: &ByteRange) -> Vec<(ByteRange, String)> {
        let inside = |inner: &ByteRange, outer: &ByteRange| {
            outer.start <= inner.start && inner.end <= outer.end
        };
        edits
            .iter()
            .filter(|(bytes, _)| {
                inside(bytes, range)
                    && !edits.iter().any(|(other, _)| {
                        other != bytes && inside(bytes, other) && inside(other, range)
                    })
            })
            .cloned()
            .collect()
    }

    /// The text of `range` with the edits applied.
    fn render(mut self, text: &str) -> String {
        self.edits.sort_by_key(|(range, _)| range.start);
        let mut out = String::new();
        let mut at = self.range.start;
        for (range, replacement) in self.edits {
            out.push_str(&text[at..range.start]);
            out.push_str(&replacement);
            at = range.end;
        }
        out.push_str(&text[at..self.range.end]);
        out
    }
}

/// Every symbol a syntax tree reads as a binding, with its bytes.
struct Mentions<'ast>(Vec<(ByteRange, &'ast Symbol)>);

impl<'ast> Mentions<'ast> {
    /// The binding references collected, with their bytes in `document`.
    fn of(document: &DocumentSnapshot, references: BindingReferences<'ast>) -> Self {
        Self(
            references
                .0
                .into_iter()
                .filter_map(|symbol| Some((document.syntax_bytes(symbol)?, symbol)))
                .collect(),
        )
    }

    /// The binding references of the whole `document`.
    fn in_document(document: &'ast DocumentSnapshot) -> Self {
        let mut references = BindingReferences::default();
        document.syntax().visit(&mut references);
        Self::of(document, references)
    }

    /// The names mentioned, each once.
    fn names(&self) -> HashSet<&'ast str> {
        self.0.iter().map(|(_, symbol)| symbol.text()).collect()
    }
}

/// Whether a `return` is reached outside any nested function.
#[derive(Default)]
struct Returns(bool);

impl Visit<'_> for Returns {
    fn visit_expr_return(&mut self, _: &ExprReturn) {
        self.0 = true;
    }

    fn visit_expr_function(&mut self, _: &ExprFunction) {}
}

/// The calls `f x` and `f(x, y)` of one name, in source order.
struct Calls<'ast>(Vec<Call<'ast>>);

impl<'ast> Visit<'ast> for Calls<'ast> {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        if let Expr::Binary(call) = expr {
            if let (BinaryOperator::Space(_), Expr::Symbol(callee)) = (&call.op, &*call.left) {
                self.0.push(Call {
                    expr,
                    callee,
                    argument: &call.right,
                });
            }
        }
        visit::visit_expr(self, expr);
    }
}

/// A call of a named function: `f x`, `f(x, y)`.
#[derive(Clone, Copy)]
struct Call<'ast> {
    /// The whole call.
    expr: &'ast Expr,
    /// The name called.
    callee: &'ast Symbol,
    /// What it is applied to, `x` or `(x, y)`.
    argument: &'ast Expr,
}

/// What a name written in the inlined body refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Meaning {
    /// The parameter at this index.
    Parameter(usize),
    /// A local the body itself declares, in any nested scope.
    Internal(BindingId),
    /// Something outside the function: the local found from the body, or
    /// the global when `None`.
    Free(Option<BindingId>),
}

/// One name written in the inlined body.
struct Occurrence<'ast> {
    /// Where the name is written.
    bytes: ByteRange,
    /// The name as written.
    symbol: &'ast Symbol,
    /// What it refers to inside the function.
    meaning: Meaning,
}

/// How a parameter reaches the inlined body.
enum Passing {
    /// Its argument's text replaces each use: the argument is a literal or a
    /// name the body never mentions, and the body never assigns the parameter.
    Substituted(String),
    /// A local `name` is bound to the argument's `value` before the body.
    Bound {
        /// The local, the parameter's name or a fresh one where that clashes.
        name: String,
        /// The argument's text.
        value: String,
    },
}

/// A function bound once to a name, `f = (x, y) -> body`, as inlining a call
/// of it sees it.
struct InlinedFunction<'a> {
    /// The document defining and calling the function.
    document: &'a DocumentSnapshot,
    /// The name the function is bound to.
    name: &'a str,
    /// The binding of that name.
    binding: BindingId,
    /// The function as written.
    function: &'a ExprFunction,
    /// Where the function is written; calls inside it are not inlined.
    bytes: ByteRange,
    /// The names written in the body.
    occurrences: Vec<Occurrence<'a>>,
    /// Every name the document reads, where a local the inline introduces
    /// could change what one means.
    mentions: Mentions<'a>,
    /// The hazards of the body itself, whatever the call.
    hazard: Option<Hazard>,
}

impl<'a> InlinedFunction<'a> {
    /// What an empty argument slot, `f(x, , y)`, passes.
    const NULL: &'static str = "null";

    /// The function the name `symbol` is bound to, when that binding is
    /// assigned exactly once, with a function written in place.
    fn of(document: &'a DocumentSnapshot, symbol: &'a Symbol) -> Option<Self> {
        let analysis = document.analysis();
        let name = symbol.text();
        let binding = analysis.binding_id_at(name, document.position_for_syntax(symbol)?)?;
        let [state] = analysis.binding_with_id(binding)?.states.as_slice() else {
            return None;
        };
        let value = state.value_range?;
        let function = document
            .enclosing_expressions(value.start)
            .into_iter()
            .find(|expr| document.range_for_syntax(*expr) == Some(value))?
            .assigned_function()?;
        let scope = analysis.scope_with_range(document.range_for_syntax(function)?)?;
        let parameters: Vec<_> = function
            .bound_symbols()
            .into_iter()
            .map(|parameter| {
                analysis.binding_id_at(parameter.text(), document.position_for_syntax(parameter)?)
            })
            .collect::<Option<_>>()?;
        let meaning = |symbol: &Symbol, at: Position| {
            let id = analysis.binding_id_at(symbol.text(), at);
            if let Some(index) = parameters
                .iter()
                .position(|parameter| Some(*parameter) == id)
            {
                return Meaning::Parameter(index);
            }
            match id.and_then(|id| Some((id, analysis.binding_with_id(id)?))) {
                Some((id, info))
                    if info.is_local() && analysis.scope_lies_within(info.scope_idx, scope) =>
                {
                    Meaning::Internal(id)
                }
                _ => Meaning::Free(analysis.local_binding_at(symbol.text(), at)),
            }
        };
        let mut references = BindingReferences::default();
        references.visit_expr(&function.body);
        let occurrences = Mentions::of(document, references)
            .0
            .into_iter()
            .map(|(bytes, symbol)| {
                let meaning = meaning(symbol, document.position_for_byte(bytes.start));
                Occurrence {
                    bytes,
                    symbol,
                    meaning,
                }
            })
            .collect::<Vec<_>>();
        let mut returns = Returns::default();
        returns.visit_expr(&function.body);
        let mut quoted = QuotedNames::default();
        quoted.visit_expr(&function.body);
        let hazard = if returns.0 {
            Some(Hazard::EarlyReturn)
        } else {
            quoted.0.into_iter().find_map(|symbol| {
                let at = document.position_for_syntax(symbol)?;
                matches!(
                    meaning(symbol, at),
                    Meaning::Parameter(_) | Meaning::Internal(_)
                )
                .then(|| Hazard::QuotedLocal(symbol.text().to_string()))
            })
        };
        Some(Self {
            document,
            name,
            binding,
            bytes: document.syntax_bytes(function)?,
            function,
            occurrences,
            mentions: Mentions::in_document(document),
            hazard,
        })
    }

    /// The analysis of the document.
    fn analysis(&self) -> &'a Analysis {
        self.document.analysis()
    }

    /// The calls of this function outside its own body.
    fn calls(&self) -> Vec<Call<'a>> {
        let mut calls = Calls(Vec::new());
        self.document.syntax().visit(&mut calls);
        calls
            .0
            .into_iter()
            .filter(|call| self.calls_this(call))
            .collect()
    }

    /// Whether `call` calls this function, from outside its own body.
    fn calls_this(&self, call: &Call<'_>) -> bool {
        let Some(bytes) = self.document.syntax_bytes(call.expr) else {
            return false;
        };
        call.callee.text() == self.name
            && !(self.bytes.start <= bytes.start && bytes.end <= self.bytes.end)
            && self
                .document
                .position_for_syntax(call.callee)
                .and_then(|at| self.analysis().binding_id_at(self.name, at))
                == Some(self.binding)
    }

    /// The arguments the call passes to the parameters, `None` for an empty
    /// slot, which passes `null`.
    fn arguments(&self, call: &Call<'a>) -> Result<Vec<Option<&'a Expr>>, Hazard> {
        let given = call.argument.arguments();
        match self.function.dispatch() {
            Dispatch::Variadic if given.len() == 1 => Ok(given),
            Dispatch::Variadic => Ok(vec![Some(call.argument)]),
            Dispatch::Fixed(expected) if expected == given.len() => Ok(given),
            Dispatch::Fixed(expected) => Err(Hazard::Arity {
                expected,
                given: given.len(),
            }),
        }
    }

    /// Whether `argument` may be written at each use of a parameter: its
    /// value is fixed and evaluating it any number of times does nothing.
    fn is_substitutable(&self, argument: Option<&Expr>) -> bool {
        match argument {
            None | Some(Expr::Literal(_)) => true,
            Some(Expr::Symbol(symbol)) => !self
                .occurrences
                .iter()
                .any(|occurrence| occurrence.symbol.text() == symbol.text()),
            Some(collection) => collection.collection_elements().is_some_and(|elements| {
                elements
                    .into_iter()
                    .all(|element| self.is_substitutable(element))
            }),
        }
    }

    /// The text replacing `call`, its arguments written as `argument_text`
    /// gives them.
    fn inline(
        &self,
        call: &Call<'a>,
        argument_text: impl Fn(&Expr) -> Option<String>,
    ) -> Option<Result<String, Hazard>> {
        if let Some(hazard) = &self.hazard {
            return Some(Err(hazard.clone()));
        }
        let document = self.document;
        let analysis = self.analysis();
        let at = document.position_for_syntax(call.expr)?;
        if let Some(captured) = self.occurrences.iter().find(|occurrence| {
            matches!(occurrence.meaning, Meaning::Free(local)
                if local != analysis.local_binding_at(occurrence.symbol.text(), at))
        }) {
            return Some(Err(Hazard::Captured(captured.symbol.text().to_string())));
        }
        let arguments = match self.arguments(call) {
            Ok(arguments) => arguments,
            Err(hazard) => return Some(Err(hazard)),
        };
        let dictionary = analysis.dictionary_at(at)?;
        let extent = analysis.dictionary_range(dictionary)?;
        let used_here: HashSet<&str> = self
            .mentions
            .0
            .iter()
            .filter(|(bytes, symbol)| {
                let written = document.position_for_byte(bytes.start);
                extent.contains_position(written)
                    && !analysis.is_local_inside(symbol.text(), written, dictionary)
            })
            .map(|(_, symbol)| symbol.text())
            .collect();
        let taken: HashSet<&str> = used_here
            .iter()
            .copied()
            .chain(
                self.occurrences
                    .iter()
                    .map(|occurrence| occurrence.symbol.text()),
            )
            .collect();
        let mut fresh_names = Vec::new();
        let mut fresh = |name: &str| {
            if !used_here.contains(name) {
                return name.to_string();
            }
            let mut candidate = name.to_string();
            while taken.contains(candidate.as_str()) || fresh_names.contains(&candidate) {
                candidate.push(Macaulay2Conventions::PRIME);
            }
            fresh_names.push(candidate.clone());
            candidate
        };
        let mut passing = Vec::new();
        for (parameter, argument) in self.function.bound_symbols().into_iter().zip(arguments) {
            let value = match argument {
                None => Self::NULL.to_string(),
                Some(argument) => argument_text(argument)?,
            };
            let assigned = analysis
                .binding_id_at(parameter.text(), document.position_for_syntax(parameter)?)
                .and_then(|id| analysis.binding_with_id(id))
                .is_some_and(|info| info.states.len() > 1);
            passing.push(if assigned || !self.is_substitutable(argument) {
                Passing::Bound {
                    name: fresh(parameter.text()),
                    value,
                }
            } else if argument.is_some_and(|argument| !Self::is_atomic(argument)) {
                Passing::Substituted(format!("({value})"))
            } else {
                Passing::Substituted(value)
            });
        }
        let mut renamed = HashMap::new();
        for occurrence in &self.occurrences {
            if let Meaning::Internal(id) = occurrence.meaning {
                renamed
                    .entry(id)
                    .or_insert_with(|| fresh(occurrence.symbol.text()));
            }
        }
        let edits = self
            .occurrences
            .iter()
            .filter_map(|occurrence| {
                let text = match occurrence.meaning {
                    Meaning::Parameter(index) => match &passing[index] {
                        Passing::Substituted(text) | Passing::Bound { name: text, .. } => text,
                    },
                    Meaning::Internal(id) => &renamed[&id],
                    Meaning::Free(_) => return None,
                };
                (text != occurrence.symbol.text()).then(|| (occurrence.bytes.clone(), text.clone()))
            })
            .collect();
        let body = &*self.function.body;
        let body_bytes = document.syntax_bytes(body)?;
        let bindings: Vec<_> = passing
            .iter()
            .filter_map(|passing| match passing {
                Passing::Bound { name, value } => Some(format!("{name} := {value}")),
                Passing::Substituted(_) => None,
            })
            .collect();
        let inner = match body {
            Expr::Paren(_) if !bindings.is_empty() => body_bytes.start + 1..body_bytes.end - 1,
            _ => body_bytes,
        };
        let text = TextRewrite {
            range: inner,
            edits,
        }
        .render(document.text());
        Some(Ok(if !bindings.is_empty() {
            format!("({}; {text})", bindings.join("; "))
        } else if Self::is_atomic(body) {
            text
        } else {
            format!("({text})")
        }))
    }

    /// The edit inlining one call, its arguments as written.
    fn inline_one(&self, call: &Call<'a>) -> Option<Result<Vec<TextEdit>, String>> {
        let document = self.document;
        let text = self.inline(call, |argument| {
            document.text_for_syntax(argument).map(str::to_string)
        })?;
        Some(
            text.map(|new_text| {
                document
                    .range_for_syntax(call.expr)
                    .map(|range| TextEdit { range, new_text })
                    .into_iter()
                    .collect()
            })
            .map_err(|hazard| hazard.to_string()),
        )
    }

    /// The edits inlining every call, a call in the argument of another
    /// inlined inside it: innermost first, so each argument is written with
    /// the calls in it already replaced.
    fn inline_all(&self, calls: &[Call<'a>]) -> Option<Result<Vec<TextEdit>, String>> {
        let document = self.document;
        let mut ordered = calls
            .iter()
            .map(|call| Some((document.syntax_bytes(call.expr)?, call)))
            .collect::<Option<Vec<_>>>()?;
        ordered.sort_by_key(|(bytes, _)| bytes.len());
        let mut inlined: Vec<(ByteRange, String)> = Vec::new();
        for (bytes, call) in ordered {
            let text = self.inline(call, |argument| {
                let range = document.syntax_bytes(argument)?;
                let edits = TextRewrite::outermost(&inlined, &range);
                Some(TextRewrite { range, edits }.render(document.text()))
            })?;
            match text {
                Ok(text) => inlined.push((bytes, text)),
                Err(hazard) => {
                    let line = document.position_for_byte(bytes.start).line + 1;
                    return Some(Err(format!("line {line}: {hazard}")));
                }
            }
        }
        let whole = 0..document.text().len();
        Some(Ok(TextRewrite::outermost(&inlined, &whole)
            .into_iter()
            .map(|(bytes, new_text)| TextEdit {
                range: document.range_for_bytes(bytes),
                new_text,
            })
            .collect()))
    }

    /// Whether `expr` needs no parentheses to stand as an operand.
    fn is_atomic(expr: &Expr) -> bool {
        matches!(
            expr,
            Expr::Symbol(_)
                | Expr::Literal(_)
                | Expr::Paren(_)
                | Expr::Sequence(_)
                | Expr::List(_)
                | Expr::Array(_)
                | Expr::AngleBar(_)
        )
    }
}

/// The inline actions offered at one position of a document.
pub struct Inlining<'a> {
    /// The document the actions edit.
    document: &'a DocumentSnapshot,
    /// Where it is saved, the target of the edits.
    uri: &'a Url,
    /// The position the actions are requested at.
    position: Position,
}

impl<'a> Inlining<'a> {
    /// The inlines offered at `position` of `document`, saved at `uri`.
    pub fn new(document: &'a DocumentSnapshot, uri: &'a Url, position: Position) -> Self {
        Self {
            document,
            uri,
            position,
        }
    }

    /// Inlining the function named at the position, bound once to a
    /// function written in place: into the call there, and into every call.
    /// The definition stays, as other files or values may still use it.
    pub fn function_actions(&self) -> Vec<CodeAction> {
        let cursor = self.document.cursor_syntax(self.position);
        let Some(symbol) = cursor.symbol.filter(|_| !cursor.member_key) else {
            return Vec::new();
        };
        let Some(function) = InlinedFunction::of(self.document, symbol) else {
            return Vec::new();
        };
        let calls = function.calls();
        let name = function.name;
        let this_call = calls
            .iter()
            .find(|call| std::ptr::eq(call.callee, symbol))
            .and_then(|call| function.inline_one(call))
            .map(|edits| self.action(format!("Inline this call of `{name}`"), edits));
        let every_call = (!calls.is_empty())
            .then(|| function.inline_all(&calls))
            .flatten()
            .map(|edits| self.action(format!("Inline every call of `{name}`"), edits));
        this_call.into_iter().chain(every_call).collect()
    }

    /// Replacing the top-level `load "f.m2"` at the position with the text
    /// of the file it runs. The file itself is left as it is.
    pub fn load_action(&self, files: &LoadedFiles<'_>) -> Option<CodeAction> {
        let document = self.document;
        let byte = document.byte_for_position(self.position)?;
        let cell = document
            .syntax()
            .cells_intersecting(byte..byte)
            .iter()
            // At the start of a line the previous cell, ending there, touches
            // the position too.
            .rev()
            .find_map(ParsedCell::syntax)?;
        let [Some(statement)] = cell.elems.arguments()[..] else {
            return None;
        };
        let Expr::Binary(call) = statement else {
            return None;
        };
        if !matches!(call.op, BinaryOperator::Space(_))
            || call.left.symbol_name() != Some(Macaulay2Conventions::LOAD_FUNCTION)
        {
            return None;
        }
        let start = document.position_for_syntax(statement)?;
        let load = document
            .file_loads()
            .iter()
            .find(|load| load.call == start)?;
        let file = load.resolve(self.uri, files.search_path).file()?.clone();
        let loaded = DocumentSnapshot::from_text((files.text)(&file)?, document.object_registry())?;
        let mut bytes = document.syntax_bytes(statement)?;
        if let Some(semicolon) = cell
            .semi_token
            .as_ref()
            .and_then(|semi| document.syntax_bytes(semi))
        {
            bytes.end = semicolon.end;
        }
        let edits = self
            .loaded_text(load, &file, &loaded)
            .map(|new_text| {
                vec![TextEdit {
                    range: document.range_for_bytes(bytes),
                    new_text,
                }]
            })
            .map_err(|hazard| hazard.to_string());
        Some(self.action(format!("Inline \"{}\"", load.name), edits))
    }

    /// The text of `loaded`, run by `load` from this document, as code of
    /// this document: its loads written to reach the same files from here.
    fn loaded_text(
        &self,
        load: &FileLoad,
        file: &Url,
        loaded: &DocumentSnapshot,
    ) -> Result<String, Hazard> {
        let analysis = loaded.analysis();
        let here = Mentions::in_document(self.document);
        let names_here = here.names();
        let mentions = Mentions::in_document(loaded);
        for (bytes, symbol) in &mentions.0 {
            let name = symbol.text();
            if analysis
                .local_binding_at(name, loaded.position_for_byte(bytes.start))
                .is_some()
            {
                continue;
            }
            if Macaulay2Conventions::FILE_CONTEXT_NAMES.contains(&name) {
                return Err(Hazard::FileContext(name.to_string()));
            }
            if self
                .document
                .analysis()
                .local_binding_at(name, load.call)
                .is_some()
            {
                return Err(Hazard::ShadowedGlobal(name.to_string()));
            }
        }
        if let Some(local) = analysis.bindings().find(|binding| {
            binding.scope_idx == 0 && binding.is_local() && names_here.contains(binding.name.name())
        }) {
            return Err(Hazard::FileLocal(local.name.name().to_string()));
        }
        let directory = |url: &Url| {
            url.to_file_path()
                .ok()
                .and_then(|path| path.parent().map(Path::to_path_buf))
        };
        let mut edits = Vec::new();
        if directory(file) != directory(self.uri) {
            for nested in loaded.file_loads() {
                if !matches!(
                    nested.lookup(),
                    FileLookup::BesideLoader | FileLookup::BesideLoaderThenPath
                ) {
                    continue;
                }
                let unreachable = || Hazard::UnreachableLoad(nested.name.clone());
                let exists = |url: Option<Url>| {
                    url.and_then(|url| url.to_file_path().ok())
                        .is_some_and(|path| path.exists())
                };
                let target = nested.beside(file).ok_or_else(unreachable)?;
                if nested.lookup() == FileLookup::BesideLoaderThenPath
                    && !exists(Some(target.clone()))
                {
                    if exists(nested.beside(self.uri)) {
                        return Err(unreachable());
                    }
                    continue;
                }
                let literal = nested
                    .name_for(self.uri, &target)
                    .and_then(|name| nested.literal_for(&name))
                    .ok_or_else(unreachable)?;
                edits.push((
                    loaded
                        .bytes_for_range(nested.literal)
                        .ok_or_else(unreachable)?,
                    literal,
                ));
            }
        }
        let range = 0..loaded.text().len();
        Ok(TextRewrite { range, edits }
            .render(loaded.text())
            .trim_end()
            .to_string())
    }

    /// An inline action applying `edits` to this document, or disabled with
    /// the reason it would change the program.
    fn action(&self, title: String, edits: Result<Vec<TextEdit>, String>) -> CodeAction {
        let (edit, disabled) = match edits {
            Ok(edits) => (
                Some(WorkspaceEdit {
                    changes: Some(HashMap::from([(self.uri.clone(), edits)])),
                    ..WorkspaceEdit::default()
                }),
                None,
            ),
            Err(reason) => (None, Some(CodeActionDisabled { reason })),
        };
        CodeAction {
            title,
            kind: Some(CodeActionKind::REFACTOR_INLINE),
            edit,
            disabled,
            ..CodeAction::default()
        }
    }
}
