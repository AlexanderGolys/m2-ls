//! Conversion of indexed builtin records into LSP-facing hover and symbol data.

use std::collections::HashSet;

use m2_syn::Token;
use tower_lsp::lsp_types::{Hover, SymbolKind};

use crate::analysis::CallSignatureFacts;
use crate::builtin_index::{MethodSignature, OperatorInfo, Record};
use crate::capabilities::hover::layout::{HoverLayout, SectionContent};
use crate::macaulay2::Macaulay2Conventions;
use crate::node_metadata::matches_token;
use crate::node_metadata::SyntaxParser;
use crate::object_registry::{
    ObjectId, ObjectKnowledge, ObjectName, ObjectRegistry, ObjectRegistryView, TypeId,
};
use crate::typesystem::TypeKnowledge;

/// The most callables a hover names among those an option belongs to, or
/// whose option a value is.
const OPTION_ROLE_LIMIT: usize = 8;

/// The list items of a published documentation page stating facts a hover
/// shows above it.
const PUBLISHED_FACT_PREFIXES: [&str; 2] = ["- **Package:**", "- **Class:**"];

/// The list item of a published documentation page naming an object's
/// aliases.
const ALIASES_PREFIX: &str = "- **Aliases:**";

/// The languages a published documentation page tags Macaulay2 code with.
const PUBLISHED_CODE_LANGUAGES: [&str; 2] = ["macaulay2", "m2"];

/// One callable signature after indexed type and documentation facts are
/// prepared for LSP presentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSignature {
    /// Input type names forming this displayed method signature.
    pub signature: Vec<ObjectName>,
    /// Known result types attached to the displayed signature.
    pub output_types: Vec<ObjectName>,
}

/// Indexed queries used by hover, completion, navigation, and signature help.
pub trait LspKnowledge: TypeKnowledge {
    /// Looks up a visible object together with its owning package name.
    fn get_record_with_package(&self, name: &ObjectName) -> Option<(String, &Record)>;

    /// Visible object names matching a prefix up to the requested result count.
    fn records_with_prefix(&self, prefix: &str, limit: usize) -> Vec<(String, &Record)>;

    /// Known package names matching a prefix up to the result limit.
    fn package_names_with_prefix(&self, prefix: &str, limit: usize) -> Vec<String>;

    /// Resolved method signatures supplied by the object's documentation.
    fn documented_signatures(&self, record: &Record) -> Vec<ResolvedSignature>;

    /// Installed signatures not already represented by documented methods.
    fn undocumented_installed_methods(&self, record: &Record) -> Vec<MethodSignature>;

    /// Callable names whose documentation mentions the specified option.
    fn option_usage_names(&self, option_name: &str, limit: usize) -> Vec<String>;

    /// Callable names documenting the specified option value.
    fn option_value_usage_names(&self, value_name: &str, limit: usize) -> Vec<String>;

    /// Rendered documentation for a visible object, when available.
    fn doc_markdown(&self, name: &ObjectName) -> Option<String>;

    /// Packages currently visible through this knowledge view.
    fn loaded_packages(&self) -> Vec<ObjectName>;
}

/// Package-addressed record lookup used by static type-hierarchy navigation.
pub trait PartitionedTypeKnowledge {
    /// Looks up an object specifically within its owning package partition.
    fn get_record_from_package(&self, package: &str, name: &ObjectName) -> Option<&Record>;

    /// Type record and package associated with a resolved runtime class identity.
    fn get_type_by_id(&self, type_id: &TypeId) -> Option<(String, &Record)>;

    /// Runtime class identity of the supplied canonical object, when it denotes a type.
    fn type_id(&self, object_id: &ObjectId) -> Option<TypeId>;

    /// Visible type records whose immediate parent is the requested class.
    fn direct_subtypes(&self, type_id: &TypeId) -> Vec<(String, &Record)>;
}

impl PartitionedTypeKnowledge for ObjectRegistry {
    fn get_record_from_package(&self, package: &str, name: &ObjectName) -> Option<&Record> {
        let package = self.catalog_package_id(&ObjectName::new(package))?;
        let object = self.package_objects(package)?.objects_by_name.get(name)?;
        self.object(object)
    }

    fn get_type_by_id(&self, type_id: &TypeId) -> Option<(String, &Record)> {
        let record = self.object(type_id.object())?;
        record.type_info()?;
        Some((self.package_name(&record.package)?.to_string(), record))
    }

    fn type_id(&self, object_id: &ObjectId) -> Option<TypeId> {
        ObjectKnowledge::type_id(self, object_id)
    }

    fn direct_subtypes(&self, type_id: &TypeId) -> Vec<(String, &Record)> {
        self.catalog_records()
            .iter()
            .filter_map(|record| {
                record
                    .type_info()
                    .and_then(|type_info| type_info.parent.as_ref())
                    .filter(|parent| *parent == type_id)
                    .filter(|_| &record.id != type_id.object())
                    .and_then(|_| Some((self.package_name(&record.package)?.to_string(), record)))
            })
            .collect()
    }
}

/// The installed signatures that apply at one call site: the one the
/// argument types pin, or those they leave possible. Those the argument types
/// rule out do not describe the call.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SignatureUsage {
    /// The single applicable signature, when selection is unambiguous.
    pub pinned: Option<ResolvedSignature>,
    /// Candidates remaining compatible with the available evidence.
    pub possible: Vec<ResolvedSignature>,
}

impl ObjectRegistry {
    /// Rendered documentation for a visible object, when available.
    pub fn doc_markdown(&self, name: &ObjectName) -> Option<&str> {
        self.get_record(name)?.markdown()
    }

    /// Callable names whose documentation mentions the specified option.
    pub fn option_usage_names(&self, option_name: &str, limit: usize) -> Vec<String> {
        option_usage_names(self, self.records_by_precedence(), option_name, limit)
    }

    /// Callable names documenting the specified option value.
    pub fn option_value_usage_names(&self, value_name: &str, limit: usize) -> Vec<String> {
        if limit == 0 {
            return Vec::new();
        }

        let value_name = self
            .get_record(&ObjectName::new(value_name))
            .map_or_else(|| ObjectName::new(value_name), |record| record.name.clone());
        self.option_facts()
            .option_value_usages
            .get(&value_name)
            .into_iter()
            .flat_map(|usages| usages.iter())
            .filter(|usage| self.get_record(&usage.callable).is_some())
            .map(|usage| format!("{}.{}", usage.callable, usage.option))
            .take(limit)
            .collect()
    }

    /// Resolved method signatures supplied by the object's documentation.
    pub fn documented_signatures(&self, record: &Record) -> Vec<ResolvedSignature> {
        documented_signatures(self, record)
    }

    /// Installed signatures not already represented by documented methods.
    pub fn undocumented_installed_methods(&self, record: &Record) -> Vec<MethodSignature> {
        undocumented_installed_methods(record)
    }
}

/// Usage from facts used to render the resolved callable signature.
pub fn signature_usage_from_facts(
    knowledge: &(impl TypeKnowledge + ?Sized),
    record: &Record,
    facts: &CallSignatureFacts,
) -> Option<SignatureUsage> {
    let callable = record.callable()?;
    let mut possible = facts
        .possible
        .iter()
        .filter_map(|method| resolved_method_signature(knowledge, record, callable, method))
        .collect::<Vec<_>>();
    dedup_resolved_signatures(&mut possible);
    let pinned = facts
        .pinned
        .as_ref()
        .and_then(|method| resolved_method_signature(knowledge, record, callable, method));

    (pinned.is_some() || !possible.is_empty()).then_some(SignatureUsage { pinned, possible })
}

/// Method signature after resolving the available source and catalog identities.
fn resolved_method_signature(
    knowledge: &(impl TypeKnowledge + ?Sized),
    record: &Record,
    callable: &crate::builtin_index::CallableInfo,
    method: &MethodSignature,
) -> Option<ResolvedSignature> {
    let mut signature = Vec::with_capacity(method.domain.len() + 1);
    signature.push(record.name.clone());
    signature.extend(
        method
            .domain
            .iter()
            .map(|object_id| {
                knowledge
                    .object(object_id)
                    .map(|record| record.name.clone())
            })
            .collect::<Option<Vec<_>>>()?,
    );
    let output_types = callable
        .effective_codomain(method)
        .and_then(|(codomain, _)| Some(vec![knowledge.type_name(codomain)?.clone()]))
        .unwrap_or_default();
    Some(ResolvedSignature {
        signature,
        output_types,
    })
}

/// Resolve a record's method signatures without consulting a name index.
fn documented_signatures(
    knowledge: &(impl TypeKnowledge + ?Sized),
    record: &Record,
) -> Vec<ResolvedSignature> {
    let Some(callable) = record.callable() else {
        return Vec::new();
    };
    let mut signatures = callable
        .methods
        .iter()
        .filter_map(|method| resolved_method_signature(knowledge, record, callable, method))
        .filter(|signature| !signature.output_types.is_empty())
        .collect::<Vec<_>>();

    if signatures.is_empty() {
        if let Some(codomain) = callable
            .typical_value
            .as_ref()
            .and_then(|type_id| knowledge.type_name(type_id))
        {
            signatures.push(ResolvedSignature {
                signature: vec![record.name.clone()],
                output_types: vec![codomain.clone()],
            });
        }
    }

    signatures
}

impl LspKnowledge for ObjectRegistry {
    fn get_record_with_package(&self, name: &ObjectName) -> Option<(String, &Record)> {
        let record = ObjectRegistry::get_record(self, name)?;
        let package = self.package_name(&record.package)?.to_string();
        Some((package, record))
    }

    fn records_with_prefix(&self, prefix: &str, limit: usize) -> Vec<(String, &Record)> {
        visible_records(self.records_by_precedence(), prefix, limit)
            .into_iter()
            .map(|record| {
                let package = self.package_name(&record.package).unwrap_or("Core");
                (package.to_string(), record)
            })
            .collect()
    }

    fn package_names_with_prefix(&self, prefix: &str, limit: usize) -> Vec<String> {
        ObjectRegistry::package_names_with_prefix(self, prefix, limit)
    }

    fn documented_signatures(&self, record: &Record) -> Vec<ResolvedSignature> {
        ObjectRegistry::documented_signatures(self, record)
    }

    fn undocumented_installed_methods(&self, record: &Record) -> Vec<MethodSignature> {
        ObjectRegistry::undocumented_installed_methods(self, record)
    }

    fn option_usage_names(&self, option_name: &str, limit: usize) -> Vec<String> {
        ObjectRegistry::option_usage_names(self, option_name, limit)
    }

    fn option_value_usage_names(&self, value_name: &str, limit: usize) -> Vec<String> {
        ObjectRegistry::option_value_usage_names(self, value_name, limit)
    }

    fn doc_markdown(&self, name: &ObjectName) -> Option<String> {
        ObjectRegistry::doc_markdown(self, name).map(str::to_string)
    }

    fn loaded_packages(&self) -> Vec<ObjectName> {
        self.loaded_packages_at(pos_max!())
    }
}

impl LspKnowledge for ObjectRegistryView<'_> {
    fn get_record_with_package(&self, name: &ObjectName) -> Option<(String, &Record)> {
        let record = self.get_record(name)?;
        let package = self.package_name(&record.package)?.to_string();
        Some((package, record))
    }

    fn records_with_prefix(&self, prefix: &str, limit: usize) -> Vec<(String, &Record)> {
        visible_records(self.records_by_precedence(), prefix, limit)
            .into_iter()
            .map(|record| {
                let package = self.package_name(&record.package).unwrap_or("Core");
                (package.to_string(), record)
            })
            .collect()
    }

    fn package_names_with_prefix(&self, prefix: &str, limit: usize) -> Vec<String> {
        ObjectRegistryView::package_names_with_prefix(self, prefix, limit)
    }

    fn documented_signatures(&self, record: &Record) -> Vec<ResolvedSignature> {
        documented_signatures(self, record)
    }

    fn undocumented_installed_methods(&self, record: &Record) -> Vec<MethodSignature> {
        undocumented_installed_methods(record)
    }

    fn option_usage_names(&self, option_name: &str, limit: usize) -> Vec<String> {
        option_usage_names(self, self.records_by_precedence(), option_name, limit)
    }

    fn option_value_usage_names(&self, value_name: &str, limit: usize) -> Vec<String> {
        let value = self
            .get_record(&ObjectName::new(value_name))
            .map_or_else(|| ObjectName::new(value_name), |record| record.name.clone());
        self.option_facts()
            .option_value_usages
            .get(&value)
            .into_iter()
            .flat_map(|usages| usages.iter())
            .filter(|usage| self.get_record(&usage.callable).is_some())
            .map(|usage| format!("{}.{}", usage.callable, usage.option))
            .take(limit)
            .collect()
    }

    fn doc_markdown(&self, name: &ObjectName) -> Option<String> {
        self.get_record(name)?.markdown().map(str::to_string)
    }

    fn loaded_packages(&self) -> Vec<ObjectName> {
        ObjectRegistryView::loaded_packages(self)
    }
}

/// Records available under the current source position and import context.
fn visible_records<'a>(
    records: impl Iterator<Item = &'a Record>,
    prefix: &str,
    limit: usize,
) -> Vec<&'a Record> {
    visible_records_matching(records, prefix, limit, true)
}

/// Records matching available under the current source position and import context.
fn visible_records_matching<'a>(
    records: impl Iterator<Item = &'a Record>,
    query: &str,
    limit: usize,
    prefix: bool,
) -> Vec<&'a Record> {
    if limit == 0 {
        return Vec::new();
    }
    let folded = query.to_lowercase();
    let mut seen = HashSet::new();
    records
        .filter(|record| {
            let name = record.name.name();
            if prefix {
                name.starts_with(query)
            } else {
                name.to_lowercase().contains(&folded)
            }
        })
        .filter(|record| seen.insert(record.name.name()))
        .take(limit)
        .collect()
}

/// Callable names whose documentation mentions the specified option.
fn option_usage_names<'a>(
    knowledge: &(impl TypeKnowledge + ?Sized),
    records: impl Iterator<Item = &'a Record>,
    option_name: &str,
    limit: usize,
) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }
    let option_name = knowledge
        .get_record(&ObjectName::new(option_name))
        .map_or_else(
            || ObjectName::new(option_name),
            |record| record.name.clone(),
        );
    records
        .filter(|record| {
            record.callable().is_some_and(|callable| {
                callable
                    .options
                    .iter()
                    .any(|option| option.name == option_name)
            })
        })
        .map(|record| record.name.name().to_string())
        .take(limit)
        .collect()
}

/// Installed signatures not already represented by documented methods.
fn undocumented_installed_methods(record: &Record) -> Vec<MethodSignature> {
    let Some(callable) = record.callable() else {
        return Vec::new();
    };
    callable
        .methods
        .iter()
        .filter(|method| callable.effective_codomain(method).is_none())
        .cloned()
        .collect()
}

/// Removes duplicate resolved signatures while preserving presentation order.
fn dedup_resolved_signatures(signatures: &mut Vec<ResolvedSignature>) {
    let mut seen = HashSet::new();
    signatures.retain(|signature| {
        seen.insert((signature.signature.clone(), signature.output_types.clone()))
    });
}

/// Editor symbol category appropriate for this catalog record.
pub fn record_symbol_kind(record: &Record) -> SymbolKind {
    if record.type_info().is_some() {
        return SymbolKind::CLASS;
    }

    if record.callable().is_some() {
        return SymbolKind::FUNCTION;
    }

    match record.class.0.as_str() {
        "Package" => SymbolKind::NAMESPACE,
        "Type" => SymbolKind::CLASS,
        "Option" => SymbolKind::PROPERTY,
        _ => SymbolKind::VARIABLE,
    }
}

/// The hover of `record`, an object of `package`: its signature at the
/// hovered call `usage` (or its name), its class, package, and roles, the
/// signatures still possible for the call, and its documentation as
/// Macaulay2 publishes it — which lists every signature itself, so the hover
/// lists them only for an object without documentation.
pub fn record_hover_with_package_and_usage(
    record: &Record,
    package: Option<&str>,
    knowledge: &(impl LspKnowledge + ?Sized),
    usage: Option<&SignatureUsage>,
) -> Hover {
    let operator_info = record.operator_info();
    let pinned = usage.and_then(|usage| usage.pinned.as_ref());
    let code = pinned.map_or_else(
        || record.name.name().to_string(),
        |signature| signature_code(signature, operator_info),
    );
    let mut layout = HoverLayout::new(code).with_fact(format!("`{}`", record.class.0));
    if let Some(package) = package {
        layout = layout.with_fact(format!("package `{package}`"));
    }
    if let Some(typical_value) =
        record_typical_value(record, knowledge).filter(|_| pinned.is_none())
    {
        layout = layout.with_fact(format!("returns `{typical_value}`"));
    }
    let possible = usage
        .map(|usage| {
            usage
                .possible
                .iter()
                .map(|signature| signature_code(signature, operator_info))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let documentation = knowledge
        .doc_markdown(&record.name)
        .map(|markdown| published_documentation(&markdown));
    let all_signatures = if usage.is_none() && documentation.is_none() {
        record_signature_codes(record, knowledge)
    } else {
        Vec::new()
    };
    layout
        .with_section(
            "Option of",
            SectionContent::Names,
            knowledge.option_usage_names(record.name.name(), OPTION_ROLE_LIMIT),
        )
        .with_section(
            "Option value of",
            SectionContent::Names,
            knowledge.option_value_usage_names(record.name.name(), OPTION_ROLE_LIMIT),
        )
        .with_section(
            if pinned.is_some() {
                "Also possible for this call"
            } else {
                "Possible for this call"
            },
            SectionContent::Code,
            possible,
        )
        .with_section("Signatures", SectionContent::Code, all_signatures)
        .with_documentation(documentation)
        .into_hover()
}

/// Every signature of `record` as Macaulay2 code: the documented ones, with
/// their codomains, then the installed methods documentation does not cover.
fn record_signature_codes(
    record: &Record,
    knowledge: &(impl LspKnowledge + ?Sized),
) -> Vec<String> {
    if record.callable().is_none() {
        return Vec::new();
    }
    let mut codes = knowledge
        .documented_signatures(record)
        .iter()
        .map(|signature| signature_code(signature, record.operator_info()))
        .collect::<Vec<_>>();
    for method in knowledge.undocumented_installed_methods(record) {
        let mut signature = Vec::with_capacity(method.domain.len() + 1);
        signature.push(record.name.clone());
        signature.extend(method.domain.iter().filter_map(|object_id| {
            knowledge
                .object(object_id)
                .map(|record| record.name.clone())
        }));
        let code = signature_code(
            &ResolvedSignature {
                signature,
                output_types: Vec::new(),
            },
            record.operator_info(),
        );
        if !codes.contains(&code) {
            codes.push(code);
        }
    }
    codes
}

/// The documentation page `markdown` of an object as hover shows it: as
/// Macaulay2 publishes it, without the title and the facts the hover states
/// above it, its code fenced as Macaulay2, and its aliases only those
/// Macaulay2 can read back.
fn published_documentation(markdown: &str) -> String {
    let mut parser = SyntaxParser::new().expect("the Macaulay2 grammar loads");
    let mut lines = Vec::new();
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if line.starts_with("# ")
            || line.contains("_undocumented_")
            || PUBLISHED_FACT_PREFIXES
                .iter()
                .any(|prefix| trimmed.starts_with(prefix))
        {
            continue;
        }
        if let Some(aliases) = trimmed.strip_prefix(ALIASES_PREFIX) {
            let readable = aliases
                .split(',')
                .map(str::trim)
                .filter(|alias| {
                    let code = alias.trim_matches('`');
                    parser.parse(code).is_some_and(|root| !root.has_error())
                })
                .collect::<Vec<_>>();
            if !readable.is_empty() {
                lines.push(format!("{ALIASES_PREFIX} {}", readable.join(", ")));
            }
            continue;
        }
        match trimmed.strip_prefix("```") {
            Some(language)
                if PUBLISHED_CODE_LANGUAGES
                    .contains(&language.trim().to_ascii_lowercase().as_str()) =>
            {
                lines.push(format!(
                    "{}```{}",
                    &line[..line.len() - trimmed.len()],
                    Macaulay2Conventions::MARKDOWN_CODE_LANGUAGE
                ));
            }
            _ => lines.push(line.to_string()),
        }
    }
    lines.join("\n").trim().to_string()
}

/// Known result class attached to a callable catalog record.
fn record_typical_value(
    record: &Record,
    knowledge: &(impl TypeKnowledge + ?Sized),
) -> Option<String> {
    record
        .callable()
        .and_then(|info| info.typical_value.as_ref())
        .and_then(|type_id| knowledge.type_name(type_id))
        .map(ObjectName::name)
        .map(str::to_string)
}

/// `signature` as the Macaulay2 code of a call: `f(ZZ, QQ) -> ZZ`, or the
/// operator written between its operands, `String | String -> String`.
fn signature_code(signature: &ResolvedSignature, operator_info: Option<&OperatorInfo>) -> String {
    let label = signature_label(signature, operator_info);
    if label.starts_with('(') {
        format!("{}{label}", signature.signature[0].name())
    } else {
        label
    }
}

/// Human-readable call signature, accounting for operator presentation.
fn signature_label(signature: &ResolvedSignature, operator_info: Option<&OperatorInfo>) -> String {
    let domain_parts = signature
        .signature
        .iter()
        .skip(1)
        .map(|s| s.0.as_str())
        .collect::<Vec<_>>();
    let domain = operator_signature_domain_label(signature, operator_info, &domain_parts)
        .unwrap_or_else(|| format!("({})", domain_parts.join(", ")));
    let outputs = signature
        .output_types
        .iter()
        .map(|s| s.0.as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    if outputs.is_empty() {
        domain
    } else {
        format!("{domain} -> {outputs}")
    }
}

/// Signature domain label derived from the operator spelling and its runtime form.
fn operator_signature_domain_label(
    signature: &ResolvedSignature,
    operator_info: Option<&OperatorInfo>,
    domain_parts: &[&str],
) -> Option<String> {
    let operator_info = operator_info?;
    let method_key = signature.signature.first()?.0.as_str();
    let (operator, is_assignment) = operator_method_key(method_key)?;
    if operator != operator_info.method_symbol.0 {
        return None;
    }

    match domain_parts {
        [left, right]
            if is_assignment && operator_info.forms.iter().any(|form| form == "Binary") =>
        {
            Some(format!("{left} {operator} {right} = ..."))
        }
        [operand] if is_assignment && operator_info.forms.iter().any(|form| form == "Prefix") => {
            Some(format!("{operator} {operand} = ..."))
        }
        [operand] if is_assignment && operator_info.forms.iter().any(|form| form == "Postfix") => {
            Some(format!("{operand}{operator} = ..."))
        }
        [left, right]
            if !is_assignment && operator_info.forms.iter().any(|form| form == "Binary") =>
        {
            Some(format!("{left} {operator} {right}"))
        }
        [operand] if !is_assignment && operator_info.forms.iter().any(|form| form == "Prefix") => {
            Some(format!("{operator} {operand}"))
        }
        [operand] if !is_assignment && operator_info.forms.iter().any(|form| form == "Postfix") => {
            Some(format!("{operand}{operator}"))
        }
        _ => None,
    }
}

/// Operator spelling and binary-form flag decoded from a method key.
fn operator_method_key(method_key: &str) -> Option<(&str, bool)> {
    if let Some(inner) = method_key
        .strip_prefix('(')
        .and_then(|key| key.strip_suffix(')'))
    {
        let (operator, suffix) = inner.split_once(',')?;
        return matches_token::<Token![=]>(suffix).then_some((operator, true));
    }

    Some((method_key, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_search_does_not_require_sorted_names() {
        let corpus = concat!(
            "{\"kind\":\"type\",\"name\":\"ZZ\"}\n",
            "{\"kind\":\"symbol\",\"name\":\"about\"}\n",
            "{\"kind\":\"type\",\"name\":\"Ring\"}\n",
            "{\"kind\":\"methodFunction\",\"name\":\"coefficient\"}\n",
        );
        let builtins = ObjectRegistry::load(corpus);
        let names = |prefix| {
            builtins
                .records_with_prefix(prefix, 8)
                .into_iter()
                .map(|(_, record)| record.name.name())
                .collect::<Vec<_>>()
        };

        assert_eq!(names("ab"), vec!["about"]);
        assert_eq!(names("co"), vec!["coefficient"]);
        assert_eq!(names("R"), vec!["Ring"]);
        assert_eq!(names("Z"), vec!["ZZ"]);
    }
    use crate::object_registry::ObjectName;
    use crate::object_registry::ObjectRegistry;
    use tower_lsp::lsp_types::HoverContents;

    fn hover_markdown(hover: Hover) -> String {
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("record hover should use markdown");
        };
        markup.value
    }

    #[test]
    fn record_hover_shows_the_published_documentation_below_its_facts() {
        let corpus = concat!(
            "{\"kind\":\"methodFunction\",\"name\":\"scan\",",
            "\"package\":\"$Core$Core\",\"class\":\"$Core$CompiledFunction\",",
            "\"methods\":[",
            "{\"domain\":[\"ZZ\",\"Function\"],\"typicalValue\":\"Number\"}],",
            "\"markdown\":\"# scan\\nApply a function to each element\\n\\n",
            "- **Package:** [Core](Core.md)\\n- **Class:** `CompiledFunction`\\n",
            "- **Aliases:** `Core$scan`, `Core$|`\\n",
            "- **Returns:** _undocumented_\\n\\n## Description\\nUseful details.\\n\\n",
            "## Methods\\n- [`scan(ZZ,Function)`](#method)\\n\\n",
            "## Examples\\n```m2\\nscan(3, print)\\n```\\n\\n",
            "## See also\\n- `apply`\"}\n",
        );
        let knowledge = ObjectRegistry::load(corpus);
        let record = knowledge
            .get_record(&ObjectName::new("scan"))
            .expect("scan should deserialize");
        let markdown = hover_markdown(record_hover_with_package_and_usage(
            record,
            Some("Core"),
            &knowledge,
            None,
        ));

        assert!(
            markdown.starts_with(
                "```macaulay2\nscan\n```\n`CompiledFunction` · package `Core`\n\n---\n\nApply"
            ),
            "{markdown}"
        );
        assert!(
            markdown.contains("- **Aliases:** `Core$scan`\n"),
            "{markdown}"
        );
        for hidden in [
            "# scan",
            "_undocumented_",
            "**Package:**",
            "**Class:**",
            "**Signatures**",
        ] {
            assert!(!markdown.contains(hidden), "{hidden} in {markdown}");
        }
        for shown in [
            "## Description\nUseful details.",
            "## Methods\n- [`scan(ZZ,Function)`]",
            "```macaulay2\nscan(3, print)\n```",
            "## See also\n- `apply`",
        ] {
            assert!(markdown.contains(shown), "{shown} in {markdown}");
        }
    }

    #[test]
    fn option_roles_name_the_callables_they_belong_to() {
        let corpus = concat!(
            "{\"kind\":\"function\",\"name\":\"compute\",\"options\":[",
            "{\"key\":\"Strategy\",\"possibleValues\":[\"Fast\"]}]}\n",
            "{\"kind\":\"symbol\",\"name\":\"Strategy\",\"class\":\"$Core$Symbol\"}\n",
            "{\"kind\":\"symbol\",\"name\":\"Fast\",\"class\":\"$Core$Symbol\"}\n",
        );
        let knowledge = ObjectRegistry::load(corpus);
        let hover_of = |name: &str| {
            let record = knowledge
                .get_record(&ObjectName::new(name))
                .expect("the symbol should load");
            hover_markdown(record_hover_with_package_and_usage(
                record, None, &knowledge, None,
            ))
        };

        assert!(hover_of("Strategy").contains("**Option of** `compute`"));
        assert!(hover_of("Fast").contains("**Option value of** `compute.Strategy`"));
    }

    #[test]
    fn a_pinned_call_heads_the_hover_with_its_signature_alone() {
        let knowledge = ObjectRegistry::load(concat!(
            "{\"kind\":\"methodFunction\",\"name\":\"f\",\"methods\":[",
            "{\"domain\":[\"$Core$String\"],\"typicalValue\":\"$Core$File\"},",
            "{\"domain\":[\"$Core$ZZ\"],\"typicalValue\":\"$Core$Ring\"}",
            "]}\n",
        ));
        let record = knowledge
            .get_record(&ObjectName::new("f"))
            .expect("f should have builtin metadata");
        let callable = record.callable().expect("f should be callable");
        let usage = signature_usage_from_facts(
            &knowledge,
            record,
            &CallSignatureFacts {
                pinned: callable.methods.first().cloned(),
                possible: Vec::new(),
                excluded: callable.methods.iter().skip(1).cloned().collect(),
            },
        )
        .expect("f String facts should render as signature usage");

        let markdown = hover_markdown(record_hover_with_package_and_usage(
            record,
            Some("Core"),
            &knowledge,
            Some(&usage),
        ));

        assert!(
            markdown.starts_with("```macaulay2\nf(String) -> File\n```"),
            "{markdown}"
        );
        assert!(!markdown.contains("f(ZZ)"), "{markdown}");
    }

    #[test]
    fn undocumented_records_list_their_signatures_as_code() {
        let knowledge = ObjectRegistry::load(concat!(
            "{\"kind\":\"methodFunction\",\"name\":\"kernel\",",
            "\"methods\":[{\"domain\":[\"RingMap\"],\"typicalValue\":\"Ideal\"}]}\n",
            "{\"kind\":\"operator\",\"name\":\"=>\",\"operator\":{\"forms\":[\"binary\"]},",
            "\"methods\":[{\"domain\":[\"Thing\",\"Thing\"],\"typicalValue\":\"Option\"}]}\n",
        ));
        let hover_of = |name: &str| {
            let record = knowledge
                .get_record(&ObjectName::new(name))
                .expect("the record should load");
            hover_markdown(record_hover_with_package_and_usage(
                record,
                Some("Core"),
                &knowledge,
                None,
            ))
        };

        assert!(hover_of("kernel")
            .contains("**Signatures**\n```macaulay2\nkernel(RingMap) -> Ideal\n```"));
        assert!(hover_of("=>").contains("Thing => Thing -> Option"));
    }

    #[test]
    fn record_hover_states_the_global_typical_value() {
        let knowledge = ObjectRegistry::load(
            "{\"kind\":\"methodFunction\",\"name\":\"method\",\"typical_value\":\"MethodFunction\"}\n",
        );
        let record = knowledge
            .get_record(&ObjectName::new("method"))
            .expect("method should deserialize");

        let markdown = hover_markdown(record_hover_with_package_and_usage(
            record,
            Some("Core"),
            &knowledge,
            None,
        ));

        assert!(markdown.contains("returns `MethodFunction`"), "{markdown}");
    }

    #[test]
    fn option_value_usage_lookup_resolves_from_possible_values() {
        let corpus = concat!(
            "{\"kind\":\"symbol\",\"name\":\"LongPolynomial\",\"class\":\"Symbol\"}\n",
            "{\"kind\":\"methodFunction\",\"name\":\"gb\",\"options\":[{\"key\":\"Strategy\",\"possibleValues\":[\"LongPolynomial\"]}]}\n",
        );
        let knowledge = ObjectRegistry::load(corpus);

        assert_eq!(
            knowledge.option_value_usage_names("LongPolynomial", 8),
            vec!["gb.Strategy"],
        );
    }
}
