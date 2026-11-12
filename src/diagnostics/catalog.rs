//! The registration table: every diagnostic m2-ls can report, as one
//! [`DiagnosticRule`] value with the code, name, severity, and tags it is
//! published under.
//!
//! Each entry's documentation shows Macaulay2 source that triggers it; the
//! checks that spawn it run on the occasions in [`super::checks`].

use tower_lsp::lsp_types::{DiagnosticSeverity, DiagnosticTag};

use super::{DiagnosticCode, DiagnosticRule};

/// Declares one `static` rule per entry, documented by the entry's doc comment,
/// and lists every rule in registration order.
macro_rules! register_diagnostics {
    ($(
        $(#[doc = $doc:literal])*
        $rule:ident {
            code: $code:literal,
            name: $name:literal,
            severity: $severity:ident,
            $(tags: [$($tag:ident),+],)?
        }
    )+) => {
        $(
            $(#[doc = $doc])*
            pub static $rule: DiagnosticRule = DiagnosticRule {
                code: DiagnosticCode($code),
                name: $name,
                severity: DiagnosticSeverity::$severity,
                tags: &[$($(DiagnosticTag::$tag),+)?],
            };
        )+

        /// Every registered rule, in registration order.
        pub static RULES: &[&DiagnosticRule] = &[$(&$rule),+];
    };
}

register_diagnostics! {
    /// Source the grammar cannot parse.
    ///
    /// ```m2
    /// x = (1 + ]
    /// ```
SYNTAX_ERROR {
        code: "X01",
        name: "syntax-error",
        severity: ERROR,
    }

    /// A token the grammar expected but did not find, such as an `if` that
    /// ends after `then`.
    ///
    /// ```m2
    /// y = if true then
    /// ```
MISSING_NODE {
        code: "X02",
        name: "missing-node",
        severity: ERROR,
    }

    /// `L.1` reads as `L` applied to the float `.1`, not as member access;
    /// the fix suggests `L#1`.
    ///
    /// ```m2
    /// L = {10, 20}
    /// second = L.1
    /// ```
AMBIGUOUS_FLOAT_MEMBER_ACCESS {
        code: "X03",
        name: "ambiguous-float-member-access",
        severity: WARNING,
    }

    /// A parallel assignment target M2 cannot assign. Targets are assigned
    /// component-wise and may be symbols, nested lists, parts such as `x#i`
    /// (with `=`), or installation targets such as `T + T` or `f ZZ`; under
    /// `:=` an operator target must be an installation, so a literal operand
    /// rules it out.
    ///
    /// ```m2
    /// (a, 1) = (2, 3)
    /// ```
MULTIPLE_ASSIGNMENT_TARGETS {
        code: "X04",
        name: "multiple-assignment-targets",
        severity: ERROR,
    }

    /// `:=` used to assign a part; parts are assigned with `=`.
    ///
    /// ```m2
    /// L = new MutableList from {1, 2}
    /// L#0 := 5
    /// ```
COLON_EQUAL_PART_ASSIGNMENT {
        code: "X05",
        name: "colon-equal-part-assignment",
        severity: ERROR,
    }

    /// A parallel assignment whose two sides list different numbers of
    /// elements.
    ///
    /// ```m2
    /// (a, b) = (1, 2, 3)
    /// ```
PARALLEL_ASSIGNMENT_ARITY {
        code: "X06",
        name: "parallel-assignment-arity",
        severity: ERROR,
    }

    /// An option name or unbound symbol default in an option declaration that
    /// does not use PascalCase. Ordinary hash table entries are not checked.
    ///
    /// ```m2
    /// f = method(Options => {verbose => false})
    /// g = {limit => 10} >> o -> x -> x
    /// ```
OPTION_KEY_CONVENTION {
        code: "S01",
        name: "option-key-convention",
        severity: HINT,
    }

    /// A floating-point value used as a key or index. Approximate values are
    /// unsuitable for reliable key equality, even where Macaulay2 accepts them.
    ///
    /// ```m2
    /// h = hashTable {1.5 => "value"}
    /// ```
FLOAT_KEY {
        code: "E21",
        name: "float-key",
        severity: ERROR,
    }

    /// A required callable, type, receiver, or condition has no binding in the
    /// declared environment. An intentional symbol is valid outside these slots.
    ///
    /// ```m2
    /// missingFunction 1
    /// ```
UNRESOLVED_VALUE {
        code: "E22",
        name: "unresolved-value",
        severity: ERROR,
    }

    /// A type or a declared optional argument does not use PascalCase or
    /// uppercase. Functions and locals are not constrained.
    ///
    /// ```m2
    /// badType = new Type of HashTable
    /// ```
NAMING_CONVENTION {
        code: "S08",
        name: "naming-convention",
        severity: HINT,
    }

    /// An option uses a source-bound name for a key.
    /// Its binding is evaluated instead of supplying the name as a symbol.
    ///
    /// ```m2
    /// Limit = 3
    /// f = {Limit => 1} >> opts -> x -> x
    /// ```
BOUND_OPTION_KEY {
        code: "S09",
        name: "bound-option-key",
        severity: WARNING,
    }

    /// A conversion method named after a known type should declare a typical
    /// value, shared or per installation, naming that type or a variant of it.
    ///
    /// ```m2
    /// toWidget = method()
    /// toWidget ZZ := n -> new Widget
    /// ```
CONSTRUCTOR_TYPICAL_VALUE {
        code: "S10",
        name: "constructor-typical-value",
        severity: HINT,
    }

    /// A local binding that is never read. Names starting with `_` are exempt.
    ///
    /// ```m2
    /// f = () -> (
    ///     unused := 1;
    ///     2
    ///     )
    /// ```
UNUSED_BINDING {
        code: "S02",
        name: "unused-binding",
        severity: WARNING,
        tags: [UNNECESSARY],
    }

    /// Parentheses around a condition, `try` value, or loop range that the
    /// keyword already delimits.
    ///
    /// ```m2
    /// f = n -> if (n > 0) then 1 else 2
    /// ```
REDUNDANT_CONTROL_PARENTHESES {
        code: "S03",
        name: "redundant-control-parentheses",
        severity: HINT,
    }

    /// A null test that `??` expresses directly: here `x ?? 0`, and
    /// `typeInfo ??= class value` for an `if` that only assigns the fallback.
    ///
    /// ```m2
    /// f = x -> if x === null then 0 else x
    /// ```
    ///
    /// ```m2
    /// if typeInfo === null then typeInfo = class value;
    /// ```
PREFER_COALESCENCE {
        code: "S04",
        name: "prefer-coalescence",
        severity: HINT,
    }

    /// A local bound only to be tested against `null`, which `??` folds away
    /// together with the test: `y = g x ?? 0`.
    ///
    /// ```m2
    /// f = x -> (v := g x; if v =!= null then y = v else y = 0; y)
    /// ```
INLINE_NULL_BINDING {
        code: "S24",
        name: "inline-null-binding",
        severity: HINT,
    }

    /// An assignment repeating its target as the left operand of the value,
    /// which the augmented assignment writes once.
    ///
    /// ```m2
    /// f = text -> (prefix = "x"; while match(prefix, text) do prefix = prefix | "X"; prefix)
    /// ```
PREFER_AUGMENTED_ASSIGNMENT {
        code: "S25",
        name: "prefer-augmented-assignment",
        severity: HINT,
    }

    /// An `if` whose redundant null branch can be removed.
    ///
    /// ```m2
    /// if ready then value else null
    /// ```
UNNECESSARY_NULL_BRANCH {
        code: "S05",
        name: "unnecessary-null-branch",
        severity: HINT,
    }

    /// An unused null guard that returns early, expressible with `??`.
    ///
    /// ```m2
    /// f = x -> (if x === null then return 0; x)
    /// ```
NULL_RETURN_GUARD {
        code: "S11",
        name: "null-return-guard",
        severity: HINT,
    }

    /// A mapped callback expressible as a composition of named functions.
    ///
    /// ```m2
    /// apply(rows, r -> blocksOf(contents r))
    /// ```
MAPPED_COMPOSITION {
        code: "S12",
        name: "mapped-composition",
        severity: HINT,
    }

    /// A known Boolean strictly compared with `true` or `false`.
    ///
    /// ```m2
    /// linked = true
    /// linked === true
    /// ```
REDUNDANT_BOOLEAN_COMPARISON {
        code: "S13",
        name: "redundant-boolean-comparison",
        severity: HINT,
    }

    /// A callback whose sole effect is forwarding its parameter to a function.
    ///
    /// ```m2
    /// c -> inlineOf c
    /// ```
FORWARDING_LAMBDA {
        code: "S14",
        name: "forwarding-lambda",
        severity: HINT,
    }

    /// A `try` with a repeated success value or redundant null fallback.
    ///
    /// ```m2
    /// try value then value
    /// ```
SIMPLIFIABLE_TRY {
        code: "S15",
        name: "simplifiable-try",
        severity: HINT,
    }

    /// An `if` condition whose negation can be inverted or cancelled.
    ///
    /// ```m2
    /// if not (left == right) then value
    /// ```
SIMPLIFIABLE_IF_CONDITION {
        code: "S16",
        name: "simplifiable-if-condition",
        severity: HINT,
    }

    /// Nested conditionals that can be flattened into an `else if` chain.
    ///
    /// ```m2
    /// if outer then one else (if inner then two else three)
    /// ```
FLATTENABLE_IF_CHAIN {
        code: "S17",
        name: "flattenable-if-chain",
        severity: HINT,
    }

    /// A callback forwarding its parameter after a fixed first argument.
    ///
    /// ```m2
    /// y -> f(x, y)
    /// ```
PARTIAL_APPLICATION_LAMBDA {
        code: "S18",
        name: "partial-application-lambda",
        severity: HINT,
    }

    /// A self-initializing type can be called directly to initialize an instance.
    ///
    /// ```m2
    /// Node = new SelfInitializingType of List
    /// new Node from {1, 2}
    /// ```
SELF_INITIALIZING_CONSTRUCTOR {
        code: "S19",
        name: "self-initializing-constructor",
        severity: HINT,
    }

    /// A header type accepts list or sequence initialization through call syntax.
    ///
    /// ```m2
    /// Node = new HeaderType of List
    /// new Node from {1, 2}
    /// ```
HEADER_CONSTRUCTOR {
        code: "S20",
        name: "header-constructor",
        severity: HINT,
    }

    /// A wrapper type accepts list or sequence initialization through call syntax.
    /// Scalar shorthand instead wraps the input and is not the same rewrite.
    ///
    /// ```m2
    /// Node = new WrapperType of List
    /// new Node from {1, 2}
    /// ```
WRAPPER_CONSTRUCTOR {
        code: "S21",
        name: "wrapper-constructor",
        severity: HINT,
    }

    /// A predicate, a method named `is` and an uppercase letter such as
    /// `isPrime`, whose results are not always Boolean: its `method()`
    /// declaration does not set `TypicalValue => Boolean`, or one of its
    /// installations annotates another result type.
    ///
    /// ```m2
    /// isGood = method()
    /// ```
    ///
    /// ```m2
    /// isGood = method(TypicalValue => Boolean)
    /// isGood ZZ := ZZ => n -> n
    /// ```
PREDICATE_TYPICAL_VALUE {
        code: "S22",
        name: "predicate-typical-value",
        severity: WARNING,
    }

    /// A predicate defined as a function closure, which cannot dispatch on its
    /// arguments or declare a typical value, rather than as a method.
    ///
    /// ```m2
    /// isGood = n -> n > 0
    /// ```
PREDICATE_FUNCTION_CLOSURE {
        code: "S23",
        name: "predicate-function-closure",
        severity: HINT,
    }

    /// A multi-character ring variable such as `x0`, which is a separate
    /// symbol rather than the indexed variable `x_0`.
    ///
    /// ```m2
    /// R = QQ[x0, x1]
    /// ```
RING_VARIABLE_NAMING {
        code: "S06",
        name: "ring-variable-naming",
        severity: HINT,
    }

    /// A method installed on something that does not dispatch to it: a plain
    /// function rather than a `method()`, the binary form of `??`, a `<-`
    /// method for anything but one non-symbol type, or an operator the
    /// interpreter evaluates itself for the operand classes. M2 evaluates
    /// `x <- v` by assigning a symbol `x` directly and otherwise calling the
    /// one-type method of `class x`, yet `installMethod` accepts every other
    /// domain. Likewise it computes arithmetic and comparison on native numbers
    /// (`ZZ`, `QQ`, `RR`, `RRi`, `CC`, `CCi`), `//`, `..` and `..<` on two
    /// integers, `and`, `or` and `not` on Booleans, and the equality and
    /// ordering of strings, symbols and nets, before it looks up any method.
    /// Install the method for a type of your own instead, for example one
    /// wrapping the value.
    ///
    /// ```m2
    /// f = x -> x
    /// f ZZ := n -> n
    /// ```
    ///
    /// ```m2
    /// installMethod(symbol <-, Symbol, Symbol, (x, y, v) -> v)
    /// ```
    ///
    /// ```m2
    /// ZZ + QQ := (a, b) -> a
    /// ```
INSTALL_NO_EFFECT {
        code: "E01",
        name: "install-no-effect",
        severity: WARNING,
    }

    /// A method installed on an operator form Macaulay2 does not let users
    /// extend.
    ///
    /// ```m2
    /// ZZ === ZZ := (a, b) -> true
    /// ```
OPERATOR_NOT_FLEXIBLE {
        code: "E02",
        name: "operator-not-flexible",
        severity: ERROR,
    }

    /// A method function whose parameter count differs from the installed
    /// domain.
    ///
    /// ```m2
    /// g = method()
    /// g(ZZ, ZZ) := (a, b, c) -> a
    /// ```
INSTALL_ARITY {
        code: "E03",
        name: "install-arity",
        severity: ERROR,
    }

    /// An installed function whose form does not fit how the method passes
    /// options: a method with an option table (`Options => {…}`) installs
    /// `o -> x -> …`, and one forwarding its options (`Options => true`)
    /// installs a function declaring them, `{…} >> o -> x -> …`.
    ///
    /// ```m2
    /// f = method(Options => {Limit => 1})
    /// f ZZ := print
    /// f QQ := {} >> o -> x -> x
    /// g = method(Options => true)
    /// g ZZ := o -> x -> x
    /// ```
INSTALL_OPTION_FORM {
        code: "E11",
        name: "install-option-form",
        severity: ERROR,
    }

    /// A function declaring options with `>>` installed on a method without
    /// options, which cannot pass any, so the declared defaults always apply.
    ///
    /// ```m2
    /// f = method()
    /// f ZZ := {Limit => 1} >> o -> x -> x
    /// ```
INSTALL_UNREACHABLE_OPTIONS {
        code: "E12",
        name: "install-unreachable-options",
        severity: WARNING,
    }

    /// A method installed with `=`, which Macaulay2 rejects; installations use
    /// `:=`.
    ///
    /// ```m2
    /// g = method()
    /// g ZZ = n -> n
    /// ```
INSTALL_NEEDS_COLON_EQUALS {
        code: "E04",
        name: "install-needs-colon-equals",
        severity: ERROR,
    }

    /// `protect` applied to an expression, so the protected symbol is only
    /// known at run time.
    ///
    /// ```m2
    /// protect getSymbol "x"
    /// ```
PROTECT_COMPUTED_SYMBOL {
        code: "E06",
        name: "protect-computed-symbol",
        severity: WARNING,
    }

    /// `return`, `break`, or `continue` outside the function or loop it
    /// needs.
    ///
    /// ```m2
    /// f = n -> (if n > 0 then break 1; n)
    /// ```
INVALID_CONTROL_TRANSFER {
        code: "E08",
        name: "invalid-control-transfer",
        severity: ERROR,
    }

    /// `x <- y` used as an assignment target, alone or inside a parallel
    /// assignment. That never installs a callable method, so it is always an
    /// error; `<-` is overloaded only with
    /// `installMethod(symbol <-, Type, function)`.
    ///
    /// ```m2
    /// (x <- ZZ) := y -> y
    /// ```
EXPLICIT_INSTALL_REQUIRED {
        code: "E09",
        name: "explicit-install-required",
        severity: ERROR,
    }

    /// Reassignment of a protected indexed symbol, regardless of whether its
    /// current value is mutable.
    ///
    /// ```m2
    /// ZZ = 7
    /// ```
PROTECTED_ASSIGNMENT {
        code: "E13",
        name: "protected-assignment",
        severity: ERROR,
    }

    /// A sum, difference or product of matrices whose dimensions the program
    /// determines and which do not combine: Macaulay2 rejects them when it
    /// evaluates the expression.
    ///
    /// ```m2
    /// A = matrix {{1, 2, 3}, {4, 5, 6}}
    /// A * A
    /// ```
MATRIX_SHAPE_MISMATCH {
        code: "E23",
        name: "matrix-shape-mismatch",
        severity: ERROR,
    }

    /// A division of a number by an exact zero written out, which
    /// Macaulay2 rejects as a division by zero; `1/0.` is infinity.
    ///
    /// ```m2
    /// 1/0
    /// ```
DIVISION_BY_ZERO {
        code: "E24",
        name: "division-by-zero",
        severity: ERROR,
    }

    /// An `if` or `while` condition that is a computation with one value,
    /// such as a comparison of numbers, so that one branch never runs.
    ///
    /// ```m2
    /// if 2^10 > 1000 then 1 else 2
    /// ```
CONSTANT_CONDITION {
        code: "S26",
        name: "constant-condition",
        severity: HINT,
    }

    /// A subscript past the variables of a ring the program writes out,
    /// which Macaulay2 rejects; over a polynomial ring it writes out,
    /// subscripts go on into that ring's variables.
    ///
    /// ```m2
    /// S = RR_53[x]
    /// S_1
    /// ```
RING_INDEX_OUT_OF_BOUNDS {
        code: "E14",
        name: "ring-index-out-of-bounds",
        severity: ERROR,
    }

    /// A lookup by name of a variable a ring the program writes out does
    /// not have, by a string or an indexed variable. A ring element is
    /// promoted when its name is missing, which may succeed, so it is not
    /// reported.
    ///
    /// ```m2
    /// A = QQ[x, y_1]
    /// "y"_A
    /// ```
RING_VARIABLE_NOT_FOUND {
        code: "E15",
        name: "ring-variable-not-found",
        severity: ERROR,
    }

    /// A parallel assignment whose right side has a type that cannot supply
    /// several values.
    ///
    /// ```m2
    /// (a, b) = 5
    /// ```
PARALLEL_ASSIGNMENT_TYPE {
        code: "T01",
        name: "parallel-assignment-type",
        severity: ERROR,
    }

    /// An `if` or `while` condition whose type is known not to be `Boolean`.
    ///
    /// ```m2
    /// f = n -> if #n then 1 else 2
    /// ```
CONDITION_TYPE {
        code: "T02",
        name: "condition-type",
        severity: WARNING,
    }

    /// A method installation without a codomain where inference can deduce
    /// one; the fix inserts `ZZ =>` here.
    ///
    /// ```m2
    /// g = method()
    /// g ZZ := n -> n + 1
    /// ```
INSTALL_CODOMAIN_MISSING {
        code: "T03",
        name: "install-codomain-missing",
        severity: HINT,
    }

    /// A method installation whose declared codomain contradicts the inferred
    /// result type.
    ///
    /// ```m2
    /// g = method()
    /// g ZZ := String => n -> n + 1
    /// ```
INSTALL_CODOMAIN_MISMATCH {
        code: "T04",
        name: "install-codomain-mismatch",
        severity: WARNING,
    }

    /// An option name the called function does not accept; Macaulay2 rejects
    /// the call.
    ///
    /// ```m2
    /// f = method(Options => {Verbose => false})
    /// f ZZ := o -> n -> n
    /// f(1, Verbos => true)
    /// ```
UNKNOWN_OPTION {
        code: "E16",
        name: "unknown-option",
        severity: ERROR,
    }

    /// A file a `load`, `needs` or `input` call cannot run: nothing where
    /// `load` looks (the directory of the loading file, then, for a name
    /// without a leading `/`, `~/`, `./` or `../`, `path`), a directory, a
    /// file it may not read, or an empty name.
    ///
    /// ```m2
    /// load "no-such-file.m2"
    /// ```
UNLOADABLE_FILE {
        code: "E17",
        name: "unloadable-file",
        severity: WARNING,
    }

    /// An `initFile` directive selecting a file that cannot be read: it is
    /// not there, may not be read, or is no UTF-8 text. The document is
    /// analyzed without an init file.
    ///
    /// ```m2
    /// -- m2-ls: initFile no-such-init.m2
    /// ```
UNAVAILABLE_INIT_FILE {
        code: "E18",
        name: "unavailable-init-file",
        severity: WARNING,
    }

    /// Three or more inheritance tests of the same function parameter anywhere
    /// in its body, including compound conditions and validation guards.
    /// Prefer method specializations so additional classes can add behavior.
    ///
    /// ```m2
    /// renderLabel = x -> if instance(x, String) then x else if instance(x, ZZ) then toString x else if instance(x, List) then toString x else "other"
    /// ```
MANUAL_TYPE_DISPATCH {
        code: "E19",
        name: "manual-type-dispatch",
        severity: WARNING,
    }

    /// A list-family inheritance test combined with a fixed length above one.
    /// Named hash-table fields better describe a record's structure.
    ///
    /// ```m2
    /// validateTask = task -> if not instance(task, VisibleList) or #task != 2 then error "task needs two fields"
    /// ```
LIST_RECORD_SHAPE {
        code: "E20",
        name: "list-record-shape",
        severity: WARNING,
    }

    /// More than three quarters of a local method's installations explicitly
    /// annotate one result type, with at least five effective installations.
    /// Prefer one `TypicalValue` in the constructor and retain differing overrides.
    ///
    /// ```m2
    /// renderLabel = method()
    /// renderLabel String := String => x -> x
    /// renderLabel ZZ := String => x -> toString x
    /// renderLabel List := String => x -> toString x
    /// renderLabel Boolean := String => x -> toString x
    /// renderLabel Nothing := String => x -> ""
    /// ```
SHARED_METHOD_CODOMAIN {
        code: "S07",
        name: "shared-method-codomain",
        severity: HINT,
    }

    /// A method call whose argument types cannot match any known domain.
    ///
    /// ```m2
    /// f = method()
    /// f ZZ := n -> n
    /// f "text"
    /// ```
CALL_DOMAIN_MISMATCH {
        code: "T05",
        name: "call-domain-mismatch",
        severity: WARNING,
    }

    /// An option a call passes whose value is none of the types the option is
    /// documented to take, or a symbol outside the values it accepts.
    ///
    /// ```m2
    /// gb(ideal x, ChangeMatrix => 3)
    /// ```
OPTION_VALUE_MISMATCH {
        code: "T06",
        name: "option-value-mismatch",
        severity: WARNING,
    }

    /// A SimpleDoc keyword or `document` option that does not exist where it
    /// is written. In SimpleDoc, a content line indented no deeper than the
    /// keyword above it reads as a keyword too; `document` has no
    /// `Description` option, its description being the list's other elements.
    ///
    /// ```m2
    /// doc ///
    /// Key
    ///   f
    /// Headlines
    ///   a function
    /// ///
    /// ```
UNKNOWN_DOCUMENTATION_SECTION {
        code: "D01",
        name: "unknown-documentation-section",
        severity: ERROR,
    }

    /// Documentation `document` or SimpleDoc rejects: a section given twice,
    /// a section missing its content, a headline over several lines or over
    /// 200 characters, an `Inputs` item without `:` or `=>`, `Inputs` without
    /// `Usage`, an unmatched `@`, an expression that does not parse, or
    /// options passed outside the `document` list.
    ///
    /// ```m2
    /// doc ///
    /// Key
    ///   f
    /// Inputs
    ///   n ZZ
    /// ///
    /// ```
MALFORMED_DOCUMENTATION {
        code: "D02",
        name: "malformed-documentation",
        severity: ERROR,
    }

    /// A documentation page without a `Key`.
    ///
    /// ```m2
    /// doc ///
    /// Headline
    ///   a function
    /// ///
    /// ```
MISSING_DOCUMENTATION_KEY {
        code: "D03",
        name: "missing-documentation-key",
        severity: ERROR,
    }

    /// A headline longer than the 100 characters `document` allows without a
    /// warning.
    ///
    /// ```m2
    /// document {Key => f, Headline => "a headline that goes on and on, well past the hundred characters Macaulay2 accepts quietly"}
    /// ```
LONG_HEADLINE {
        code: "D04",
        name: "long-headline",
        severity: WARNING,
    }

    /// A name documentation evaluates — a key, an `@TO …@` link, a `SeeAlso`
    /// entry — that neither the package nor a package it loads defines.
    /// Macaulay2 creates it as a new symbol, which `endPackage` rejects as a
    /// "mutable unexported unset symbol".
    ///
    /// ```m2
    /// newPackage "P"
    /// export {"foo"}
    /// foo = x -> x
    /// beginDocumentation()
    /// doc ///
    /// Key
    ///   foo
    /// SeeAlso
    ///   bar
    /// ///
    /// ```
UNDEFINED_DOCUMENTATION_KEY {
        code: "D05",
        name: "undefined-documentation-key",
        severity: ERROR,
    }

    /// A key documented twice, or both documented and listed in
    /// `undocumented`.
    ///
    /// ```m2
    /// doc ///
    /// Key
    ///   foo
    /// ///
    /// document {Key => foo}
    /// ```
DUPLICATE_DOCUMENTATION {
        code: "D06",
        name: "duplicate-documentation",
        severity: ERROR,
    }

    /// An exported symbol without documentation in a package whose
    /// documentation is in the same file; `installPackage` warns about each.
    /// List deliberately undocumented symbols in `undocumented {…}`.
    ///
    /// ```m2
    /// newPackage "P"
    /// export {"foo", "bar"}
    /// foo = x -> x
    /// bar = x -> x
    /// beginDocumentation()
    /// doc ///
    /// Key
    ///   foo
    /// ///
    /// ```
UNDOCUMENTED_EXPORT {
        code: "D07",
        name: "undocumented-export",
        severity: WARNING,
    }

    /// Documentation outside every package section of a file that declares
    /// packages, where no package is open to hold it.
    ///
    /// ```m2
    /// newPackage "P"
    /// endPackage "P"
    /// document {Key => "P"}
    /// ```
DOCUMENTATION_OUTSIDE_PACKAGE {
        code: "D08",
        name: "documentation-outside-package",
        severity: ERROR,
    }

    /// `doc` called in a package before `beginDocumentation()`, which loads
    /// the SimpleDoc package defining it; until then `doc` is an unset
    /// symbol.
    ///
    /// ```m2
    /// newPackage "P"
    /// doc ///
    /// Key
    ///   P
    /// ///
    /// ```
DOCUMENTATION_BEFORE_BEGIN {
        code: "D09",
        name: "documentation-before-begin",
        severity: ERROR,
    }
}
