//! Source semantic tokens and the roles that refine them.
//!
//! Collection runs after enrichment in three steps. A role pass records
//! roles that syntax around a token decides, such as option keys and quoted
//! symbols. A symbol pass emits each symbol with the context it is written
//! in, and merges the parts of single objects spelled with `_`, such as the
//! ring generator `z_0`, into one token. A lexical pass classifies the
//! remaining tokens of each cell — keywords, operators, literals, and
//! comments — from the token stream, so it needs no syntax around them.

use std::collections::HashSet;

use m2_syn::nodes::{
    Expr, ExprAssign, ExprAssignEvaluated, ExprIf, ExprOpBinAdj, ExprOpBinIndex, ExprOpBinMember,
    ExprOpBinRegular, ExprQuote, ExprWhile, Symbol,
};
use m2_syn::visit::{self, Visit};
use m2_syn::{Lex, Spanned, Token, TokenStream, TokenTree, Trivia};

use super::*;
use crate::macaulay2::Macaulay2Conventions;
use crate::object_registry::ObjectKnowledge;
use crate::semantic_token::{lexical_semantic_token_type, M2SemanticTokenType};
use crate::source::ByteRange;
use crate::source::SyntaxSource;
use crate::syntax::{
    AssignmentSyntax, AssignmentTarget, BoundSymbols, ExpressionSyntax, LambdaSyntax,
};

impl Analysis {
    /// Collects the source semantic tokens of every cell.
    pub(super) fn collect_semantics(
        &mut self,
        source: &(impl SyntaxSource + ?Sized),
        knowledge_provider: &(impl PositionedTypeKnowledge + ?Sized),
    ) {
        source.syntax().visit(&mut SemanticRoles {
            analysis: self,
            source,
            knowledge_provider,
            option_tables: Vec::new(),
        });
        let mut symbols = SymbolTokens {
            analysis: self,
            source,
            knowledge_provider,
            context: SymbolContext::default(),
            tokens: Vec::new(),
            symbol_starts: HashSet::new(),
            merged: Vec::new(),
        };
        source.syntax().visit(&mut symbols);
        let SymbolTokens {
            mut tokens,
            symbol_starts,
            merged,
            ..
        } = symbols;
        let mut lexical = LexicalTokens {
            analysis: self,
            source,
            symbol_starts: &symbol_starts,
            merged: &merged,
            tokens: Vec::new(),
        };
        for cell in source.syntax().cells() {
            lexical.comments(cell.trivia());
            if let Some(syntax) = cell.syntax() {
                lexical.stream(&syntax.to_token_stream());
            }
        }
        tokens.extend(lexical.tokens);
        tokens.sort_by_key(|token| token.span.bytes().start);
        self.registry.source_semantic_tokens = tokens;
        self.registry.pending_source_semantic_roles.clear();
    }

    /// Registers source semantic role in the current semantic index.
    fn register_source_semantic_role(&mut self, range: TextRange, role: SourceSemanticRole) {
        if self
            .registry
            .pending_source_semantic_roles
            .iter()
            .all(|(registered, _)| *registered != range)
        {
            self.registry
                .pending_source_semantic_roles
                .push((range, role));
        }
    }

    /// Replaces the semantic role at this source extent, or registers it when absent.
    pub(super) fn set_source_semantic_role(&mut self, range: TextRange, role: SourceSemanticRole) {
        if let Some((_, registered)) = self
            .registry
            .pending_source_semantic_roles
            .iter_mut()
            .find(|(registered, _)| *registered == range)
        {
            *registered = role;
        } else {
            self.registry
                .pending_source_semantic_roles
                .push((range, role));
        }
    }

    /// The role of a ring generator written at `range`: declared there when
    /// it lies in a constructor's variables, otherwise referred to.
    pub(super) fn ring_generator_role(&self, range: TextRange) -> SourceSemanticRole {
        if self.declares_ring_generators(range) {
            SourceSemanticRole::RingGeneratorDeclaration
        } else {
            SourceSemanticRole::RingGenerator
        }
    }

    /// The role a token written at `range` takes from its position: a role
    /// registered there, or a ring generator's role when it names one.
    fn source_role_at(&self, range: TextRange, ring_generator: bool) -> Option<SourceSemanticRole> {
        let generator_role = ring_generator.then(|| self.ring_generator_role(range));
        generator_role
            .clone()
            .filter(|role| *role == SourceSemanticRole::RingGeneratorDeclaration)
            .or_else(|| {
                self.registry
                    .pending_source_semantic_roles
                    .iter()
                    .find(|(registered, _)| *registered == range)
                    .map(|(_, role)| role.clone())
            })
            .or(generator_role)
    }
}

/// AST visitor assigning semantic roles using positioned object knowledge.
struct SemanticRoles<'analysis, 'source, 'knowledge, Source: ?Sized, Knowledge: ?Sized> {
    /// Semantic facts for the current document snapshot.
    analysis: &'analysis mut Analysis,
    /// Source data supplying text and coordinate conversions.
    source: &'source Source,
    /// Object and type knowledge queried at each source position.
    knowledge_provider: &'knowledge Knowledge,
    /// The option-table parameters of `{…} >> o -> body` functions, with
    /// the bytes of the body they are visible in.
    option_tables: Vec<(String, ByteRange)>,
}

impl<'source, Source, Knowledge> SemanticRoles<'_, 'source, '_, Source, Knowledge>
where
    Source: SyntaxSource + ?Sized,
    Knowledge: PositionedTypeKnowledge + ?Sized,
{
    /// Records a role using the generated syntax's own source span.
    fn register_role(&mut self, syntax: &(impl Spanned + ?Sized), role: SourceSemanticRole) {
        if let Some(range) = self.source.range_for_syntax(syntax) {
            self.analysis.register_source_semantic_role(range, role);
        }
    }

    /// Records property keys without reconstructing navigation nodes. A key
    /// of an option table, `o.depth`, is an optional parameter.
    fn register_property(&mut self, node: &impl PropertyAccess, table: &Expr) {
        if let Some(property) = node.property_key() {
            let role = if self.is_option_table(table, property) {
                SourceSemanticRole::OptionKey
            } else {
                SourceSemanticRole::PropertyKey
            };
            self.register_role(property, role);
        }
    }

    /// Records `key` as an option key when it is a name denoting a `Symbol`:
    /// a protected symbol such as `Strategy`, or a name nothing binds, which
    /// evaluates to its own symbol. A name bound to anything else is a value
    /// there, not a key, and keeps its own token.
    fn register_symbol_option_key(&mut self, key: &Expr) {
        let (Some(name), Some(range)) = (key.symbol_name(), self.source.range_for_syntax(key))
        else {
            return;
        };
        let knowledge = self.knowledge_provider.at_position(range.start);
        let denotes_symbol = self.analysis.get_binding_at(name, range.start).is_none()
            && knowledge
                .get_record(&ObjectName::new(name))
                .is_none_or(|record| record.class == TypeRole::Symbol.object_name());
        if denotes_symbol {
            self.register_role(key, SourceSemanticRole::OptionKey);
        }
    }

    /// Records `key` as an option key when `table` is an option table, in
    /// `o#depth`, where the key is evaluated and so must denote a symbol.
    fn register_option_table_key(&mut self, table: &Expr, key: &Expr) {
        if self.is_option_table(table, key) {
            self.register_symbol_option_key(key);
        }
    }

    /// Records the first parameter of `function`, when it is a lambda, as an
    /// option table visible in its body.
    fn register_option_table(&mut self, function: &Expr) {
        if let Expr::Lambda(lambda) = function {
            if let (Some(parameter), Some(body)) = (
                lambda.bound_symbols().first(),
                self.source.syntax_bytes(lambda.body()),
            ) {
                self.option_tables
                    .push((parameter.text().to_string(), body));
            }
        }
    }

    /// Whether `table` is the option-table parameter of a function whose
    /// body contains `key`.
    fn is_option_table(&self, table: &Expr, key: &Expr) -> bool {
        let (Some(name), Some(position)) = (table.symbol_name(), self.source.syntax_bytes(key))
        else {
            return false;
        };
        self.option_tables
            .iter()
            .any(|(parameter, body)| parameter == name && body.contains(&position.start))
    }

    /// Records an accessor's lexical token, without its attached trivia.
    fn register_access_operator(&mut self, operator: &impl Lex) {
        if let Some(token) = operator.to_token_stream().into_iter().next() {
            self.register_role(&token, SourceSemanticRole::AccessOperator);
        }
    }

    /// Records a literal selector as a ring generator or property key.
    fn register_selector<'ast>(
        &mut self,
        owner: &impl Spanned,
        left: &'ast Expr,
        right: &Expr,
        subscript: bool,
    ) where
        'source: 'ast,
    {
        if !matches!(right, Expr::Literal(m2_syn::nodes::ExprLiteral::Integer(_))) {
            return;
        }
        let Some(range) = self.source.range_for_syntax(owner) else {
            return;
        };
        let knowledge = self.knowledge_provider.at_position(range.start);
        let ring = if subscript {
            let checker = TypeChecker::new(self.analysis, &knowledge);
            let session = InferenceSession::new(&checker, self.source, self.source.syntax());
            let scope = self.analysis.find_scope_at(range.start).unwrap_or(0);
            session
                .infer(left, scope)
                .single()
                .is_some_and(|base| knowledge.has_type_role(base, TypeRole::Ring))
        } else {
            false
        };
        let role = if ring {
            self.analysis.ring_generator_role(range)
        } else {
            SourceSemanticRole::PropertyKey
        };
        self.register_role(right, role);
    }
}

impl<'ast, 'source: 'ast, Source, Knowledge> Visit<'ast>
    for SemanticRoles<'_, 'source, '_, Source, Knowledge>
where
    Source: SyntaxSource + ?Sized,
    Knowledge: PositionedTypeKnowledge + ?Sized,
{
    fn visit_expr_option(&mut self, node: &'ast OptionExpression) {
        self.register_symbol_option_key(node.key.as_ref());
        visit::visit_expr_option(self, node);
    }

    fn visit_expr_op_bin_regular(&mut self, node: &'ast BinaryExpr) {
        let operator = node.operator.spelling();
        if operator == <Token![>>]>::SPELLING {
            self.register_option_table(&node.right_operand);
        }
        self.register_property(node, &node.left_operand);
        let subscript = operator == <Token![_]>::SPELLING;
        if subscript || operator == <Token![#?]>::SPELLING {
            self.register_access_operator(&node.operator);
            self.register_selector(node, &node.left_operand, &node.right_operand, subscript);
        }
        visit::visit_expr_op_bin_regular(self, node);
    }

    fn visit_expr_assign(&mut self, assignment: &'ast ExprAssign) {
        let installs_with_table = self
            .source
            .range_for_syntax(assignment)
            .is_some_and(|range| {
                self.analysis.installations().iter().any(|installation| {
                    installation.span == range && installation.options == MethodOptions::Table
                })
            });
        if installs_with_table {
            self.register_option_table(assignment.parts().value);
        }
        visit::visit_expr_assign(self, assignment);
    }

    fn visit_expr_op_bin_member(&mut self, node: &'ast ExprOpBinMember) {
        let table = match node {
            ExprOpBinMember::Dot(access) => access.receiver.as_ref(),
            ExprOpBinMember::Optional(access) => access.receiver.as_ref(),
        };
        self.register_property(node, table);
        match node {
            ExprOpBinMember::Dot(access) => self.register_access_operator(&access.operator),
            ExprOpBinMember::Optional(access) => self.register_access_operator(&access.operator),
        }
        visit::visit_expr_op_bin_member(self, node);
    }

    fn visit_expr_op_bin_index(&mut self, node: &'ast ExprOpBinIndex) {
        self.register_option_table_key(&node.receiver, &node.index);
        self.register_property(node, &node.receiver);
        self.register_access_operator(&node.operator);
        self.register_selector(node, &node.receiver, &node.index, false);
        visit::visit_expr_op_bin_index(self, node);
    }

    fn visit_expr_quote(&mut self, node: &'ast QuoteExpression) {
        self.register_role(node.quoted_name(), SourceSemanticRole::QuotedSymbol);
        visit::visit_expr_quote(self, node);
    }

    fn visit_expr_op_bin_adj(&mut self, call: &'ast ExprOpBinAdj) {
        if let Some(head) = call.left_operand.symbol_name() {
            let arguments = call
                .right_operand
                .arguments()
                .into_iter()
                .filter(|argument| !matches!(argument, Some(Expr::Option(_))))
                .collect::<Vec<_>>();
            if let Some(argument) = arguments.first().copied().flatten() {
                if argument.literal_source(self.source).is_some() {
                    if let Some(range) = self.source.range_for_syntax(argument) {
                        let knowledge = self.knowledge_provider.at_position(range.start);
                        let role = if Macaulay2Conventions::PACKAGE_NAME_FUNCTIONS.contains(&head)
                            && knowledge.get_record(&ObjectName::new(head)).is_some()
                            && self.analysis.get_binding_at(head, range.start).is_none()
                        {
                            Some(SourceSemanticRole::NamespaceArgument)
                        } else if Macaulay2Conventions::REGEXP_FUNCTIONS.contains(&head)
                            && knowledge.get_record(&ObjectName::new(head)).is_some()
                            && self.analysis.get_binding_at(head, range.start).is_none()
                            && (head != "separate" || arguments.len() > 1)
                        {
                            Some(SourceSemanticRole::RegularExpression)
                        } else if FileLoad::is_load_function(head) {
                            Some(SourceSemanticRole::LoadedFilePath)
                        } else {
                            None
                        };
                        if let Some(role) = role {
                            self.register_role(argument, role);
                        }
                    }
                }
            }
            for option in PackageLoadingOption::ALL {
                for package in option.typed_package_names(call, self.source) {
                    self.register_role(package, SourceSemanticRole::NamespaceArgument);
                }
            }
        }
        visit::visit_expr_op_bin_adj(self, call);
    }
}

/// What the syntax around a symbol says about how it is used.
#[derive(Debug, Clone, Copy, Default)]
struct SymbolContext {
    /// Within the condition of the nearest enclosing `if` or `while`.
    condition: bool,
    /// Within the target of the nearest enclosing assignment.
    assignment_target: bool,
    /// The symbol a quote returns without evaluating it.
    quoted: bool,
    /// The function applied by an application.
    application_head: bool,
}

/// Emits symbol tokens with their context, and merged tokens for single
/// objects spelled with `_`.
struct SymbolTokens<'analysis, 'source, 'knowledge, Source: ?Sized, Knowledge: ?Sized> {
    /// Analysis supplying bindings and registered roles.
    analysis: &'analysis Analysis,
    /// Source navigation over the visited revision.
    source: &'source Source,
    /// Object and type knowledge queried at each source position.
    knowledge_provider: &'knowledge Knowledge,
    /// Context of the syntax being visited.
    context: SymbolContext,
    /// Tokens emitted so far.
    tokens: Vec<SourceSemanticToken>,
    /// Where each visited symbol starts, so the lexical pass skips it.
    symbol_starts: HashSet<usize>,
    /// Bytes of merged tokens, whose parts the lexical pass skips.
    merged: Vec<ByteRange>,
}

impl<Source, Knowledge> SymbolTokens<'_, '_, '_, Source, Knowledge>
where
    Source: SyntaxSource + ?Sized,
    Knowledge: PositionedTypeKnowledge + ?Sized,
{
    /// Visits `visit` with the context changed by `change`.
    fn within(&mut self, change: impl FnOnce(&mut SymbolContext), visit: impl FnOnce(&mut Self)) {
        let inherited = self.context;
        change(&mut self.context);
        visit(self);
        self.context = inherited;
    }

    /// Emits one token spanning several syntax parts.
    fn merged_token(&mut self, bytes: ByteRange, role: SourceSemanticRole) {
        self.merged.push(bytes.clone());
        self.tokens.push(SourceSemanticToken {
            span: self.source.span_for_bytes(bytes),
            syntax_token_type: None,
            source_role: Some(role),
            is_symbol: false,
            is_unquoted_symbol: false,
            is_expression_symbol: false,
            is_condition_value: false,
        });
    }
}

impl<'ast, Source, Knowledge> Visit<'ast> for SymbolTokens<'_, '_, '_, Source, Knowledge>
where
    Source: SyntaxSource + ?Sized,
    Knowledge: PositionedTypeKnowledge + ?Sized,
{
    fn visit_symbol(&mut self, symbol: &'ast Symbol) {
        let Some(bytes) = self.source.syntax_bytes(symbol) else {
            return;
        };
        self.symbol_starts.insert(bytes.start);
        let span = self.source.span_for_bytes(bytes);
        let position = span.range().start;
        let knowledge = self.knowledge_provider.at_position(position);
        let name = symbol.text();
        let binding = self
            .analysis
            .visible_source_binding_at(name, position, &knowledge);
        if OutputReference::parse(name).is_some() && binding.is_none() {
            return;
        }
        let ring_generator = binding.is_some_and(|binding| binding.state.is_ring_generator);
        let context = self.context;
        self.tokens.push(SourceSemanticToken {
            source_role: self.analysis.source_role_at(span.range(), ring_generator),
            span,
            syntax_token_type: None,
            is_symbol: true,
            is_unquoted_symbol: true,
            is_expression_symbol: !matches!(name, "true" | "false")
                && !context.quoted
                && !context.assignment_target,
            is_condition_value: context.condition && !context.application_head,
        });
    }

    fn visit_expr_op_bin_regular(&mut self, access: &'ast ExprOpBinRegular) {
        if let Some(position) = self.source.position_for_syntax(access) {
            let knowledge = self.knowledge_provider.at_position(position);
            if self
                .analysis
                .indexed_ring_generator(access, self.source)
                .is_some()
            {
                if let Some(bytes) = Analysis::indexed_ring_generator_bytes(access, self.source) {
                    let role = self
                        .analysis
                        .ring_generator_role(self.source.range_for_bytes(bytes.clone()));
                    self.merged_token(bytes, role);
                    return;
                }
            }
            if let Some(role) =
                self.analysis
                    .constant_subscript_role(access, self.source, &knowledge)
            {
                if let Some(bytes) = self.source.syntax_bytes(access) {
                    self.merged_token(bytes, role);
                    return;
                }
            }
        }
        visit::visit_expr_op_bin_regular(self, access);
    }

    fn visit_expr_if(&mut self, node: &'ast ExprIf) {
        self.within(
            |context| context.condition = true,
            |this| this.visit_expr(&node.condition),
        );
        self.within(
            |context| context.condition = false,
            |this| {
                this.visit_then_clause(&node.then_clause);
                if let Some(else_clause) = &node.else_clause {
                    this.visit_else_clause(else_clause);
                }
            },
        );
    }

    fn visit_expr_while(&mut self, node: &'ast ExprWhile) {
        self.within(
            |context| context.condition = true,
            |this| this.visit_expr(&node.condition),
        );
        self.within(
            |context| context.condition = false,
            |this| this.visit_loop_body(&node.body),
        );
    }

    fn visit_expr_op_bin_adj(&mut self, call: &'ast ExprOpBinAdj) {
        self.within(
            |context| {
                context.application_head = matches!(call.left_operand.as_ref(), Expr::Symbol(_))
            },
            |this| this.visit_expr(&call.left_operand),
        );
        self.within(
            |context| context.application_head = false,
            |this| this.visit_expr(&call.right_operand),
        );
    }

    fn visit_expr_quote(&mut self, quote: &'ast ExprQuote) {
        self.within(
            |context| context.quoted = true,
            |this| this.visit_symbol(quote.quoted_name()),
        );
    }

    fn visit_expr_assign_evaluated(&mut self, node: &'ast ExprAssignEvaluated) {
        self.within(
            |context| context.assignment_target = true,
            |this| this.visit_expr(&node.evaluated_domain),
        );
        self.within(
            |context| context.assignment_target = false,
            |this| this.visit_expr(&node.value),
        );
    }

    fn visit_expr_assign(&mut self, assignment: &'ast ExprAssign) {
        if let ExprAssign::Evaluated(node) = assignment {
            return self.visit_expr_assign_evaluated(node);
        }
        let parts = assignment.parts();
        self.within(
            |context| context.assignment_target = true,
            |this| match parts.target {
                AssignmentTarget::Symbol(symbol) => this.visit_symbol(symbol),
                AssignmentTarget::List(list) => this.visit_assignment_list(list),
                AssignmentTarget::Evaluated(target) | AssignmentTarget::Augmented(target) => {
                    this.visit_expr(target)
                }
                AssignmentTarget::Method { method, .. } => this.visit_expr(method),
                AssignmentTarget::Operator(operation) => this.visit_expr_op(operation),
                AssignmentTarget::New(new) => this.visit_expr_new(new),
            },
        );
        self.within(
            |context| context.assignment_target = false,
            |this| {
                if let AssignmentTarget::Method { signature, .. } = parts.target {
                    this.visit_expr(signature);
                }
                if let Some(codomain) = parts.codomain {
                    this.visit_expr(codomain);
                }
                this.visit_expr(parts.value);
            },
        );
    }
}

/// Classifies the tokens of a cell that need no syntax around them.
struct LexicalTokens<'analysis, 'source, 'symbols, Source: ?Sized> {
    /// Analysis supplying registered roles.
    analysis: &'analysis Analysis,
    /// Source navigation over the classified revision.
    source: &'source Source,
    /// Where symbols start, which the symbol pass classifies.
    symbol_starts: &'symbols HashSet<usize>,
    /// Bytes of merged tokens, whose parts are not classified alone.
    merged: &'symbols [ByteRange],
    /// Tokens emitted so far.
    tokens: Vec<SourceSemanticToken>,
}

impl<Source: SourceNavigation + ?Sized> LexicalTokens<'_, '_, '_, Source> {
    /// Classifies the tokens of a stream, descending into delimited groups.
    fn stream(&mut self, stream: &TokenStream) {
        for token in stream {
            if let TokenTree::Group(group) = token {
                self.stream(group.stream());
                continue;
            }
            let Some(kind) = lexical_semantic_token_type(token) else {
                continue;
            };
            let Some(bytes) = self.source.syntax_bytes(token) else {
                continue;
            };
            if self.symbol_starts.contains(&bytes.start) && matches!(token, TokenTree::Ident(_)) {
                continue;
            }
            self.push(bytes, kind);
        }
    }

    /// Classifies the comments of a cell, without surrounding blanks or the
    /// line ending.
    fn comments(&mut self, trivia: &[Trivia]) {
        for comment in trivia {
            let Some(mut bytes) = self.source.syntax_bytes(comment) else {
                continue;
            };
            let text = &self.source.text()[bytes.clone()];
            bytes.start += text.len() - text.trim_start_matches([' ', '\t']).len();
            bytes.end -= text.len() - text.trim_end_matches(['\r', '\n']).len();
            self.push(bytes, M2SemanticTokenType::Comment);
        }
    }

    /// Emits a classified token unless a merged token covers it.
    fn push(&mut self, bytes: ByteRange, kind: M2SemanticTokenType) {
        if self
            .merged
            .iter()
            .any(|merged| merged.start <= bytes.start && bytes.end <= merged.end)
        {
            return;
        }
        let span = self.source.span_for_bytes(bytes);
        self.tokens.push(SourceSemanticToken {
            source_role: self.analysis.source_role_at(span.range(), false),
            span,
            syntax_token_type: Some(kind),
            is_symbol: false,
            is_unquoted_symbol: false,
            is_expression_symbol: false,
            is_condition_value: false,
        });
    }
}
