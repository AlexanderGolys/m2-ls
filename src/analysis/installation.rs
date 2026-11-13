//! Method installations read from typed syntax.
//!
//! An installation is an assignment or `installMethod` call that adds a method
//! to a method function or operator. This module recognizes the installation
//! forms, the function each installation stores, and the `method(…)` calls
//! deciding how installed functions receive options. Every operation follows
//! generated fields; none searches enclosing syntax.

use m2_syn::nodes::{
    Expr, ExprAssign, ExprCollection, ExprLambda, ExprNew, ExprOp, ExprOpBin, ExprOpBinAdj,
    ExprOpBinRegular, List,
};
use m2_syn::{Span, Spanned, Token};

use super::inference::syntax::{InfixSyntax, MethodConstructor};
use super::syntax_roles::QuotedName;
use super::*;
use crate::macaulay2::Macaulay2Conventions;
use crate::source::SyntaxSource;
use crate::syntax::{
    AssignmentOperator, AssignmentSyntax, AssignmentTarget, ClauseSyntax, LambdaSyntax,
};

/// The callable that installs a method by a call, `installMethod(f, T, g)`.
const INSTALL_METHOD_FUNCTION: &str = "installMethod";

/// The callable creating a method function, `method(Options => …)`.
const METHOD_CONSTRUCTOR: &str = "method";

/// The function an installation stores, read from its syntax once grouping
/// parentheses and a `Codomain =>` annotation are removed.
#[derive(Debug, Clone, Copy)]
pub(super) enum InstalledFunction<'ast> {
    /// `x -> …`, or `o -> x -> …` receiving an option table.
    Lambda(&'ast ExprLambda),
    /// `{…} >> o -> …`: a function declaring its own options, whose handler
    /// `o -> …` receives them.
    OptionDeclaring {
        /// The whole `{…} >> o -> …` expression.
        declaration: &'ast ExprOpBinRegular,
        /// Function receiving the option table before ordinary call arguments.
        handler: &'ast ExprLambda,
    },
    /// Any other value, such as a named function.
    Other(&'ast Expr),
}

impl<'ast> InstalledFunction<'ast> {
    /// Recognizes the stored function, or `None` when grouping parentheses
    /// hold no value.
    pub(super) fn of(value: &'ast Expr) -> Option<Self> {
        let value = value.unparenthesized()?;
        Some(match value {
            Expr::Lambda(lambda) => Self::Lambda(lambda),
            Expr::Option(annotated) => return Self::of(&annotated.value),
            Expr::Op(ExprOp::Bin(ExprOpBin::Regular(declaration))) => {
                match Self::option_declaring_handler(declaration) {
                    Some(handler) => Self::OptionDeclaring {
                        declaration,
                        handler,
                    },
                    None => Self::Other(value),
                }
            }
            _ => Self::Other(value),
        })
    }

    /// The handler `o -> …` of `{…} >> o -> …`.
    fn option_declaring_handler(declaration: &'ast ExprOpBinRegular) -> Option<&'ast ExprLambda> {
        if declaration.operator.spelling() != <Token![>>]>::SPELLING {
            return None;
        }
        match declaration.right_operand.unparenthesized()? {
            Expr::Lambda(handler) => Some(handler),
            _ => None,
        }
    }

    /// The whole installed function, before which a codomain annotation goes.
    pub(super) fn syntax(self) -> &'ast dyn Spanned {
        match self {
            Self::Lambda(lambda) => lambda,
            Self::OptionDeclaring { declaration, .. } => declaration,
            Self::Other(value) => value,
        }
    }
}

/// The lambda a lambda returns directly, as `x -> …` in `o -> x -> …`.
fn returned_lambda(lambda: &ExprLambda) -> Option<&ExprLambda> {
    match lambda.body().unparenthesized()? {
        Expr::Lambda(returned) => Some(returned),
        _ => None,
    }
}

impl MethodOptions {
    /// The lambda that receives the method's arguments in an installed
    /// function of the form these options expect, or `None` when the form
    /// does not fit. With unrecorded options a lambda returning another lambda
    /// could be either form, so only one that does not is taken.
    pub(super) fn argument_lambda(self, installed: InstalledFunction<'_>) -> Option<&ExprLambda> {
        match (self, installed) {
            (Self::Absent, InstalledFunction::Lambda(lambda)) => Some(lambda),
            (Self::Unrecorded, InstalledFunction::Lambda(lambda)) => {
                returned_lambda(lambda).is_none().then_some(lambda)
            }
            (Self::Table | Self::TableOrForwarded, InstalledFunction::Lambda(handler))
            | (
                Self::Absent | Self::Forwarded | Self::TableOrForwarded | Self::Unrecorded,
                InstalledFunction::OptionDeclaring { handler, .. },
            ) => returned_lambda(handler),
            _ => None,
        }
    }
}

/// The options of Macaulay2's `method` that shape the method function it creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MethodDeclarationOption {
    /// Selects binary method dispatch behavior.
    Binary,
    /// Infer the result from applicable callable method domains.
    Dispatch,
    /// Selects the callable's option-passing policy.
    Options,
    /// Selects the callable's representative return type.
    TypicalValue,
}

impl MethodDeclarationOption {
    /// Name used to identify this value in source and catalog lookups.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Binary => "Binary",
            Self::Dispatch => "Dispatch",
            Self::Options => "Options",
            Self::TypicalValue => "TypicalValue",
        }
    }
}

/// A `method(...)` call, which creates a method function.
///
/// Its options are read from the call's own arguments only, so an option list
/// such as `Options => {Binary => true}` does not configure the method itself.
#[derive(Debug, Clone, Copy)]
pub(super) struct MethodDeclaration<'ast> {
    /// Call supplying the declaration's options.
    call: &'ast ExprOpBinAdj,
}

impl<'ast> MethodDeclaration<'ast> {
    /// Recognizes a `method(...)` call.
    pub(super) fn of(value: &'ast Expr) -> Option<Self> {
        match value {
            Expr::Op(ExprOp::Bin(ExprOpBin::Adj(call)))
                if call.left_operand.symbol_name() == Some(METHOD_CONSTRUCTOR) =>
            {
                Some(Self { call })
            }
            _ => None,
        }
    }

    /// Value given for the requested option.
    fn option(self, option: MethodDeclarationOption) -> Option<&'ast Expr> {
        self.call.method_option(option)
    }

    /// Declared representative result class, when the declaration names one.
    pub(super) fn typical_value(self) -> Option<ObjectName> {
        self.option(MethodDeclarationOption::TypicalValue)
            .and_then(ExpressionSyntax::symbol_name)
            .map(ObjectName::new)
    }

    /// The options of a method declaring a literal option list. `Options =>
    /// false` passes any option a call names, so it declares no list.
    pub(super) fn declared_options(self) -> Option<Vec<CallableOption>> {
        if self.options() != MethodOptions::Table {
            return None;
        }
        OptionDeclaration::declared_options(self.option(MethodDeclarationOption::Options)?)
    }

    /// `method` takes options exactly when `Options` is given and not `null`;
    /// `true` forwards them, any other value (an empty list and `false`
    /// included) passes them as a table.
    pub(super) fn options(self) -> MethodOptions {
        match self
            .option(MethodDeclarationOption::Options)
            .map(|options| options.symbol_name())
        {
            None | Some(Some("null")) => MethodOptions::Absent,
            Some(Some("true")) => MethodOptions::Forwarded,
            Some(_) => MethodOptions::Table,
        }
    }

    /// The runtime class `method` picks: `Binary => true` first, then a single
    /// `Dispatch` of `Thing` or `Type`, then whether there are options.
    pub(super) fn runtime_class(self) -> TypeRole {
        self.call.method_class().unwrap_or(TypeRole::MethodFunction)
    }
}

impl OptionDeclaration {
    /// The options a literal `{key => default, …}` list declares, or `None`
    /// when `list` is no such list.
    pub fn declared_options(list: &Expr) -> Option<Vec<CallableOption>> {
        let list @ Expr::Collection(ExprCollection::List(_)) = list.unparenthesized()? else {
            return None;
        };
        list.collection_elements()?
            .into_iter()
            .map(|entry| match entry? {
                Expr::Option(option) => Some(CallableOption {
                    name: ObjectName::new(option.key.symbol_name()?),
                    possible_values: Vec::new(),
                    accepted_types: Vec::new(),
                }),
                _ => None,
            })
            .collect()
    }

    /// The list of option declarations `expression` makes: the `{…}` of
    /// `{…} >> o -> …`, or of `method(Options => {…})`.
    pub fn declared_list(expression: &Expr) -> Option<(Self, &List)> {
        fn list(value: &Expr) -> Option<&List> {
            match value {
                Expr::Collection(ExprCollection::List(list)) => Some(list),
                _ => None,
            }
        }
        if let Expr::Op(ExprOp::Bin(ExprOpBin::Regular(declaration))) = expression {
            InstalledFunction::option_declaring_handler(declaration)?;
            return Some((Self::Function, list(&declaration.left_operand)?));
        }
        let options =
            MethodDeclaration::of(expression)?.option(MethodDeclarationOption::Options)?;
        Some((Self::Method, list(options)?))
    }
}

/// The method head, domain, and head token an installation target spells.
pub(crate) struct InstallationShape<'ast> {
    /// Callable or operator receiving the method.
    pub head: MethodHead,
    /// Expressions naming the domain types, in order.
    pub domain: Vec<&'ast Expr>,
    /// Where the head is written: the method function's name or the operator.
    head_span: Option<Span>,
}

/// An installation read from its syntax, before its head is resolved.
struct ClassifiedInstallation<'ast> {
    /// Method installation characterized by this item.
    installation: MethodInstallation,
    /// Domain and codomain spans with their semantic roles.
    method_type_spans: Vec<(TextRange, SourceSemanticRole)>,
    /// Recognized shape of the installed function, when available.
    installed: Option<InstalledFunction<'ast>>,
}

/// Source expressions naming an installed method's argument domains: the
/// elements of `(T, S)` or `{T, S}`, or a single symbol.
fn installation_domain(signature: &Expr) -> Option<Vec<&Expr>> {
    let signature = signature.unparenthesized()?;
    match signature {
        Expr::Collection(ExprCollection::Sequence(_) | ExprCollection::List(_)) => {
            let domain = signature
                .collection_elements()?
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            (!domain.is_empty()).then_some(domain)
        }
        _ => signature.symbol_name().map(|_| vec![signature]),
    }
}

impl Operator {
    /// The operator an operation applies, with its form.
    pub fn of_operation(operation: &ExprOp) -> Option<Self> {
        let (token, form) = match operation {
            ExprOp::Bin(ExprOpBin::Adj(_)) => (
                Macaulay2Conventions::APPLICATION_OPERATOR,
                OperatorForm::Binary,
            ),
            ExprOp::Bin(binary) => (binary.infix_operands()?.operator, OperatorForm::Binary),
            ExprOp::Prefix(prefix) => (prefix.operator.spelling(), OperatorForm::Prefix),
            ExprOp::Postfix(postfix) => (postfix.operator.spelling(), OperatorForm::Postfix),
        };
        Some(Self {
            token: ObjectName::new(token),
            form,
        })
    }
}

impl Analysis {
    /// The domain of the installation whose arguments `lambda` receives.
    pub(super) fn argument_lambda_domain(
        &self,
        lambda: &ExprLambda,
        source: &(impl SourceNavigation + ?Sized),
    ) -> Option<&Vec<ObjectName>> {
        Some(
            &self
                .installation_receiving_arguments(lambda, source)?
                .method
                .domain,
        )
    }

    /// The installation whose method arguments `lambda` receives.
    pub fn installation_receiving_arguments(
        &self,
        lambda: &ExprLambda,
        source: &(impl SourceNavigation + ?Sized),
    ) -> Option<&MethodInstallation> {
        let id = self
            .registry
            .installations_by_argument_lambda
            .get(&source_range_key(source.range_for_syntax(lambda)?))?;
        Some(&self.registry.installations[id.0])
    }

    /// Method installation an assignment or `installMethod` call makes.
    pub fn installation_for(
        &self,
        installing: &(impl Spanned + ?Sized),
        source: &(impl SourceNavigation + ?Sized),
    ) -> Option<&MethodInstallation> {
        let span = source.range_for_syntax(installing)?;
        self.registry
            .installations
            .iter()
            .find(|installation| installation.span == span)
    }

    /// Callable head and domain expressions an installation target spells, or
    /// `None` when it is not an installation target. Whether the assignment
    /// operator installs is decided by the caller.
    pub(crate) fn installation_shape<'ast>(
        &self,
        target: AssignmentTarget<'ast>,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<InstallationShape<'ast>> {
        match target {
            AssignmentTarget::Method { method, signature } => {
                let name = method.symbol_name()?;
                if self.operand_is_type(name, position, knowledge) {
                    signature.symbol_name()?;
                    Some(InstallationShape {
                        head: MethodHead::Operator(Operator {
                            token: ObjectName::new(Macaulay2Conventions::APPLICATION_OPERATOR),
                            form: OperatorForm::Binary,
                        }),
                        domain: vec![method, signature],
                        head_span: None,
                    })
                } else {
                    Some(InstallationShape {
                        head: MethodHead::Function(ObjectName::new(name)),
                        domain: installation_domain(signature)?,
                        head_span: method.try_span(),
                    })
                }
            }
            AssignmentTarget::Operator(operation) => {
                self.operation_shape(operation, position, knowledge)
            }
            AssignmentTarget::New(new) => Self::new_shape(new),
            _ => None,
        }
    }

    /// The installation shape of an operation written as a target: `f T`,
    /// `T S`, `T op S`, `op T`, or `T op`.
    fn operation_shape<'ast>(
        &self,
        operation: &'ast ExprOp,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<InstallationShape<'ast>> {
        let operator = Operator::of_operation(operation)?;
        match operation {
            ExprOp::Bin(ExprOpBin::Adj(call)) => {
                // `A B` (juxtaposition = the SPACE operator): a method on the
                // named function `A` when `A` is a function, or a SPACE
                // operator method on the type pair when `A` is a type.
                let left_name = call.left_operand.symbol_name()?;
                if self.operand_is_type(left_name, position, knowledge) {
                    call.right_operand.symbol_name()?;
                    Some(InstallationShape {
                        head: MethodHead::Operator(operator),
                        domain: vec![&call.left_operand, &call.right_operand],
                        head_span: call.operator.try_span(),
                    })
                } else {
                    Some(InstallationShape {
                        head: MethodHead::Function(ObjectName::new(left_name)),
                        domain: installation_domain(&call.right_operand)?,
                        head_span: call.left_operand.try_span(),
                    })
                }
            }
            ExprOp::Bin(binary) => {
                // `X op Y`: an explicit binary-operator method.
                if operator.is_assignment() || operator.is_option_assignment() {
                    return None;
                }
                let operands = binary.infix_operands()?;
                operands.left.symbol_name()?;
                operands.right.symbol_name()?;
                Some(InstallationShape {
                    head: MethodHead::Operator(operator),
                    domain: vec![operands.left, operands.right],
                    head_span: binary_operator_span(binary),
                })
            }
            ExprOp::Prefix(prefix) => {
                prefix.operand.symbol_name()?;
                Some(InstallationShape {
                    head: MethodHead::Operator(operator),
                    domain: vec![&prefix.operand],
                    head_span: prefix.operator.try_span(),
                })
            }
            ExprOp::Postfix(postfix) => {
                postfix.operand.symbol_name()?;
                Some(InstallationShape {
                    head: MethodHead::Operator(operator),
                    domain: vec![&postfix.operand],
                    head_span: postfix.operator.try_span(),
                })
            }
        }
    }

    /// The installation shape of `new T of P from I`: a `new` method on the
    /// class, parent, and instance types.
    fn new_shape(new: &ExprNew) -> Option<InstallationShape<'_>> {
        new.class.symbol_name()?;
        let mut domain = vec![new.class.as_ref()];
        if let Some(parent) = &new.parent_class {
            domain.extend(installation_domain(parent.clause_value())?);
        }
        if let Some(instance) = &new.instance {
            domain.extend(installation_domain(instance.clause_value())?);
        }
        Some(InstallationShape {
            head: MethodHead::Operator(Operator {
                token: ObjectName::new(<Token![new]>::SPELLING),
                form: OperatorForm::Prefix,
            }),
            domain,
            head_span: None,
        })
    }

    /// Characterize an `=`/`:=` assignment as a method installation, or `None`
    /// when it is an ordinary assignment/call. The single source of truth for
    /// the install-vs-call distinction.
    ///
    /// - `:=` installs whenever the left side matches an installation shape (the
    ///   six forms: `f Type`, `f (T..)`, `T1 op T2`, `T1 T2` adjacency,
    ///   `prefix T`, `T postfix`).
    /// - `=` installs ONLY the assignment-operator form `T1 op T2 = f`, and only
    ///   when both operands are types — otherwise the identical syntax assigns
    ///   `f` to the lvalue `T1 op T2`, which is a call.
    fn classify_installation<'ast>(
        &self,
        id: MethodInstallationId,
        assignment: &'ast ExprAssign,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<ClassifiedInstallation<'ast>> {
        let parts = assignment.parts();
        let position = source.position_for_byte(parts.target.bytes(source)?.start);
        let shape = self.installation_shape(parts.target, position, knowledge)?;
        let domain = shape
            .domain
            .iter()
            .map(|operand| {
                ObjectName::new(
                    operand
                        .symbol_name()
                        .or_else(|| source.text_for_syntax(*operand))
                        .unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>();
        let operand_arity = domain.len();
        let span = source.range_for_syntax(assignment)?;
        let codomain_annotation = codomain_annotation(&parts);
        // `new` resets the initializer's result class to its requested class.
        let codomain = match parts.target {
            AssignmentTarget::New(new) => new.class.symbol_name().map(ObjectName::new),
            _ => codomain_annotation
                .and_then(ExpressionSyntax::symbol_name)
                .map(ObjectName::new),
        };
        // Domain types, the written codomain, and the method function named
        // on the left or the operator written between its operands.
        let method_type_spans = shape
            .domain
            .iter()
            .filter_map(|operand| source.range_for_syntax(*operand))
            .map(|range| (range, SourceSemanticRole::MethodTypeParameter))
            .chain(
                codomain_annotation
                    .and_then(|annotation| source.range_for_syntax(annotation))
                    .map(|range| (range, SourceSemanticRole::CodomainAnnotation)),
            )
            .chain(
                shape
                    .head_span
                    .as_ref()
                    .and_then(|head| source.range_for_syntax(head))
                    .map(|range| (range, SourceSemanticRole::InstallationHead)),
            )
            .collect::<Vec<_>>();
        let expected_argument_arity = match parts.operator {
            // `:=` installs by shape alone — no type check on the operands.
            AssignmentOperator::Local => operand_arity,
            // `=` installs only the assignment form of a BINARY operator (incl.
            // SPACE), and only when every operand is a type; otherwise the same
            // syntax assigns to the lvalue `X op Y`, which is a call.
            AssignmentOperator::Global => match &shape.head {
                MethodHead::Operator(operator)
                    if operator.form == OperatorForm::Binary
                        && domain.iter().all(|operand| {
                            self.operand_is_type(operand.name(), position, knowledge)
                        }) =>
                {
                    operand_arity + 1
                }
                _ => return None,
            },
            _ => return None,
        };
        Some(ClassifiedInstallation {
            installation: MethodInstallation {
                id,
                codomain_origin: codomain.as_ref().map(|_| {
                    if matches!(parts.target, AssignmentTarget::New(_)) {
                        CodomainOrigin::Deduction
                    } else {
                        CodomainOrigin::Annotation
                    }
                }),
                method: Method::new(shape.head, domain, codomain),
                span,
                expected_argument_arity,
                options: MethodOptions::Absent,
                installed: InstalledValue::Unknown,
                installed_lambda_dispatch: None,
                argument_lambda_dispatch: None,
                syntax: MethodInstallationSyntax::Classical,
                effect: MethodInstallationEffect::Rejected,
            },
            method_type_spans,
            installed: InstalledFunction::of(parts.value),
        })
    }

    /// Characterize an `installMethod(head, T…, f)` call as an installation.
    fn classify_install_method<'ast>(
        &self,
        id: MethodInstallationId,
        call: &'ast ExprOpBinAdj,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<ClassifiedInstallation<'ast>> {
        if call.left_operand.symbol_name() != Some(INSTALL_METHOD_FUNCTION) {
            return None;
        }
        let mut arguments = call
            .right_operand
            .collection_elements()?
            .into_iter()
            .collect::<Option<Vec<_>>>()?;
        if arguments.len() < 2 {
            return None;
        }
        let value = arguments.pop()?;
        let head = arguments.remove(0);
        let domain_operands = arguments;
        let domain = domain_operands
            .iter()
            .map(|operand| operand.symbol_name().map(ObjectName::new))
            .collect::<Option<Vec<_>>>()?;
        let head = self.install_method_head(head, domain.len(), knowledge)?;
        let expected_argument_arity = match &head {
            // `<-` methods are installed like `(op, =)` assignment methods: the
            // assigned value is one extra parameter after the domain.
            MethodHead::Operator(operator) if operator.token.name() == <Token![<-]>::SPELLING => {
                domain.len() + 1
            }
            MethodHead::Operator(operator) if operator.form == OperatorForm::Binary => 2,
            MethodHead::Function(_) | MethodHead::Operator(_) => domain.len(),
        };
        let method_type_spans = domain_operands
            .iter()
            .filter_map(|operand| source.range_for_syntax(*operand))
            .map(|range| (range, SourceSemanticRole::MethodTypeParameter))
            .collect();
        Some(ClassifiedInstallation {
            installation: MethodInstallation {
                id,
                method: Method::new(head, domain, None),
                codomain_origin: None,
                span: source.range_for_syntax(call)?,
                expected_argument_arity,
                options: MethodOptions::Absent,
                installed: InstalledValue::Unknown,
                installed_lambda_dispatch: None,
                argument_lambda_dispatch: None,
                syntax: MethodInstallationSyntax::InstallMethod,
                effect: MethodInstallationEffect::Rejected,
            },
            method_type_spans,
            installed: InstalledFunction::of(value),
        })
    }

    /// The head `installMethod` installs on: a method function's name, or a
    /// quoted operator in a form the domain arity selects.
    fn install_method_head(
        &self,
        head: &Expr,
        domain_arity: usize,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<MethodHead> {
        if let Some(name) = head.symbol_name() {
            return Some(MethodHead::Function(ObjectName::new(name)));
        }
        let Expr::Quote(quote) = head else {
            return None;
        };
        let token = ObjectName::new(quote.quoted_name().text());
        let operator = knowledge.get_record(&token)?.operator_info()?;
        let supports = |form| operator.forms.iter().any(|candidate| candidate == form);
        let form = match domain_arity {
            2.. if supports("Binary") => OperatorForm::Binary,
            1 if supports("Prefix") => OperatorForm::Prefix,
            1 if supports("Postfix") => OperatorForm::Postfix,
            1 if supports("Binary") => OperatorForm::Binary,
            _ => return None,
        };
        Some(MethodHead::Operator(Operator { token, form }))
    }

    /// Characterize an assignment once, retain its source fact, and attach that
    /// fact to the local callable registry when the installation takes effect.
    pub(super) fn record_method_installation(
        &mut self,
        assignment: &ExprAssign,
        source: &(impl SyntaxSource + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<MethodInstallationId> {
        let id = MethodInstallationId(self.registry.installations.len());
        let classified = self.classify_installation(id, assignment, source, knowledge)?;
        // Preserve M2's distinct assignment-method form: only `:=` contributes
        // a callable signature here. `=` installations are retained for
        // diagnostics/document symbols but are not ordinary call methods.
        let attach = assignment.parts().operator == AssignmentOperator::Local;
        self.retain_method_installation(classified, attach, source, knowledge)
    }

    /// Records an `installMethod` call for later semantic or editor queries.
    pub(super) fn record_install_method_call(
        &mut self,
        call: &ExprOpBinAdj,
        source: &(impl SyntaxSource + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<MethodInstallationId> {
        let id = MethodInstallationId(self.registry.installations.len());
        let classified = self.classify_install_method(id, call, source, knowledge)?;
        self.retain_method_installation(classified, true, source, knowledge)
    }

    /// Stores an installation and associates its source facts with a stable identity.
    fn retain_method_installation(
        &mut self,
        classified: ClassifiedInstallation<'_>,
        attach: bool,
        source: &(impl SyntaxSource + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<MethodInstallationId> {
        let ClassifiedInstallation {
            mut installation,
            method_type_spans,
            installed,
        } = classified;
        if attach {
            installation.effect = self.attach_method_installation(&mut installation, knowledge);
        }
        installation.installed = installed.map_or(InstalledValue::Unknown, |installed| {
            self.installed_value(installed, installation.span.start, knowledge)
        });
        // Only a lambda carries a checkable arity.
        installation.installed_lambda_dispatch = match installed {
            Some(InstalledFunction::Lambda(lambda)) => Some(lambda.dispatch()),
            _ => None,
        };
        let argument_lambda =
            installed.and_then(|installed| installation.options.argument_lambda(installed));
        installation.argument_lambda_dispatch = argument_lambda.map(LambdaSyntax::dispatch);
        if let Some(range) = argument_lambda.and_then(|lambda| source.range_for_syntax(lambda)) {
            self.registry
                .installations_by_argument_lambda
                .insert(source_range_key(range), installation.id);
        }

        for (range, role) in method_type_spans {
            self.set_source_semantic_role(range, role);
        }
        let id = installation.id;
        debug_assert_eq!(installation.id.0, self.registry.installations.len());
        let position = installation.span.start;
        self.registry.installations.push(installation);
        if let Some(lambda) = argument_lambda {
            self.deduce_installation_codomain(id, lambda, position, source, knowledge);
        }
        Some(id)
    }

    /// Take the type the lambda `argument_lambda` of the installation `id`,
    /// at `position`, returns as its codomain when nothing states one: calls
    /// of the method then return it, as they would were it annotated.
    fn deduce_installation_codomain(
        &mut self,
        id: MethodInstallationId,
        argument_lambda: &ExprLambda,
        position: Position,
        source: &(impl SyntaxSource + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) {
        if self.registry.installations[id.0].method.codomain.is_some() {
            return;
        }
        let Some(codomain) = self.deducible_codomain(argument_lambda, position, source, knowledge)
        else {
            return;
        };
        let installation = &mut self.registry.installations[id.0];
        installation.method.codomain = Some(codomain);
        installation.codomain_origin = Some(CodomainOrigin::Deduction);
    }

    /// The type the lambda `argument_lambda`, installed at `position`,
    /// always returns, when inference knows it and an annotation at the
    /// installation could name it: a type named by a value of the lambda's
    /// own scope, such as `T` in `(T, x) -> new T from x`, exists only when
    /// the method runs.
    fn deducible_codomain(
        &self,
        argument_lambda: &ExprLambda,
        position: Position,
        source: &(impl SyntaxSource + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<ObjectName> {
        let body = argument_lambda.body();
        let codomain = self.infer_expression_static_type(body, source, knowledge)?;
        if codomain == TypeRole::Thing.object_name() {
            return None;
        }
        (self.binding_id_at(codomain.name(), position)
            == self.binding_id_at(codomain.name(), source.position_for_syntax(body)?))
        .then_some(codomain)
    }

    /// Function value contributed by an assignment or installMethod expression.
    fn installed_value(
        &self,
        installed: InstalledFunction<'_>,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> InstalledValue {
        match installed {
            InstalledFunction::Lambda(_) => InstalledValue::Lambda,
            InstalledFunction::OptionDeclaring { .. } => InstalledValue::OptionDeclaring,
            InstalledFunction::Other(value) => {
                let library_record = value
                    .symbol_name()
                    .filter(|name| self.get_binding_at(name, position).is_none())
                    .and_then(|name| knowledge.get_record(&ObjectName::new(name)))
                    .and_then(|record| Some((record, record.callable()?)));
                match library_record {
                    Some((record, callable)) => InstalledValue::LibraryFunction {
                        declares_options: !callable.options.is_empty()
                            || MethodOptions::of_runtime_class(&record.class, knowledge)
                                .is_some_and(|options| options != MethodOptions::Absent),
                    },
                    None => InstalledValue::Unknown,
                }
            }
        }
    }

    /// Finds an explicit installation annotation that repeats its method's
    /// declared typical value. The returned range includes the annotation arrow
    /// and following whitespace, leaving the installed expression intact.
    /// Annotations containing comments are retained to avoid deleting commentary.
    pub fn redundant_method_codomain(
        &self,
        assignment: &ExprAssign,
        source: &(impl SyntaxSource + ?Sized),
    ) -> Option<TextRange> {
        let installation = self.installation_for(assignment, source)?;
        let MethodHead::Function(name) = &installation.method.head else {
            return None;
        };
        let function = self.function_at(name.name(), installation.span.start)?;
        if !function.is_method_function() {
            return None;
        }
        let parts = assignment.parts();
        let annotation = codomain_annotation(&parts)?;
        let annotated = ObjectName::new(annotation.symbol_name()?);
        if function.typical_value.as_ref() != Some(&annotated) {
            return None;
        }
        let value = match parts.value {
            Expr::Option(option) => option.value.as_ref(),
            value => value,
        };
        let start = source.syntax_bytes(annotation)?.start;
        let end = source.syntax_bytes(value)?.start;
        if source.source().text().get(start..end)?.contains("--") {
            return None;
        }
        Some(source.range_for_bytes(start..end))
    }

    /// Deduces an installed method's result class from its implementation and declared annotation.
    pub fn method_codomain_deduction(
        &self,
        assignment: &ExprAssign,
        source: &(impl SyntaxSource + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<MethodCodomainDeduction> {
        let installation = self.installation_for(assignment, source)?;
        let parts = assignment.parts();
        if matches!(parts.target, AssignmentTarget::New(_)) {
            return None;
        }
        // The annotation precedes the installed lambda, while the codomain is
        // the value of the lambda receiving the arguments.
        let installed = InstalledFunction::of(parts.value)?;
        let codomain = self.deducible_codomain(
            installation.options.argument_lambda(installed)?,
            source.position_for_syntax(assignment)?,
            source,
            knowledge,
        )?;

        let annotation = codomain_annotation(&parts);
        // Without a local annotation the installation inherits the method's
        // `TypicalValue`; annotating that same type again adds nothing. A
        // codomain only deduced is still worth writing down.
        if annotation.is_none()
            && installation.codomain_origin == Some(CodomainOrigin::TypicalValue)
            && installation.method.codomain.as_ref() == Some(&codomain)
        {
            return None;
        }
        let annotated_codomain = annotation
            .and_then(|annotation| source.text_for_syntax(annotation))
            .map(ObjectName::new);
        if let Some(annotation) = annotation {
            match TypeChecker::new(self, knowledge).subtype_evidence(
                &codomain,
                annotated_codomain.as_ref()?,
                source.position_for_syntax(annotation)?,
                knowledge,
            ) {
                SubtypeEvidence::Proven | SubtypeEvidence::Unknown => return None,
                SubtypeEvidence::Disproven => {}
            }
        }

        let edit = match annotation {
            Some(_) => MethodCodomainEdit::Replace,
            None => {
                let start = source.position_for_syntax(installed.syntax())?;
                MethodCodomainEdit::Add(TextRange::new(start, start))
            }
        };
        let diagnostic_range = match annotation {
            Some(annotation) => source.range_for_syntax(annotation)?,
            None => source.range_for_bytes(parts.target.bytes(source)?),
        };
        Some(MethodCodomainDeduction {
            codomain,
            annotated_codomain,
            diagnostic_range,
            edit,
        })
    }
}

/// Where the operator of an explicit binary operation is written.
fn binary_operator_span(binary: &ExprOpBin) -> Option<Span> {
    match binary {
        ExprOpBin::Adj(call) => call.operator.try_span(),
        ExprOpBin::Regular(regular) => regular.operator.try_span(),
        ExprOpBin::Index(index) => index.operator.try_span(),
        ExprOpBin::Member(m2_syn::nodes::ExprOpBinMember::Dot(member)) => {
            member.operator.try_span()
        }
        ExprOpBin::Member(m2_syn::nodes::ExprOpBinMember::Optional(member)) => {
            member.operator.try_span()
        }
    }
}

/// The codomain written for an installation: the annotation of `f T := C =>
/// …`, or the key of an annotated value `C => …` of the other forms.
fn codomain_annotation<'ast>(parts: &crate::syntax::AssignmentParts<'ast>) -> Option<&'ast Expr> {
    parts.codomain.or(match parts.value {
        Expr::Option(annotated) => Some(&annotated.key),
        _ => None,
    })
}
