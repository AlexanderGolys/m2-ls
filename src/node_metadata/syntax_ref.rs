//! Borrowed typed syntax references used to build document navigation indexes.

use m2_syn::{
    nodes::*,
    visit::{self, Visit},
    DelimiterKind, DelimiterValue, Lex, Span, Spanned, Trivia,
};
use std::any::TypeId;

#[cfg(test)]
thread_local! {
    static CHILD_ENUMERATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn child_enumerations() -> usize {
    CHILD_ENUMERATIONS.get()
}

/// Selects a concrete typed value without comparing its source span.
pub trait SyntaxType: Spanned {
    fn syntax_ref(&self) -> SyntaxRef<'_>;
    fn type_tag() -> TypeId;
    fn cast<'a>(node: &SyntaxRef<'a>) -> Option<&'a Self>;
}

impl<T: SyntaxType> SyntaxType for Box<T> {
    fn syntax_ref(&self) -> SyntaxRef<'_> {
        self.as_ref().syntax_ref()
    }
    fn type_tag() -> TypeId {
        T::type_tag()
    }
    fn cast<'a>(_: &SyntaxRef<'a>) -> Option<&'a Self> {
        None
    }
}

/// Allocation identity scoped to the retained AST that owns the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SyntaxIdentity(TypeId, usize);

macro_rules! syntax_references {
    ($($name:ident => $visit:ident, $category:literal, $token:literal;)*) => {
        #[derive(Debug, Clone)]
        #[allow(clippy::upper_case_acronyms)]
        pub enum SyntaxRef<'a> {
            $($name(&'a $name),)*
            Trivia(Trivia),
            Boundary(bool, Span),
        }
        $(impl SyntaxType for $name {
            fn syntax_ref(&self) -> SyntaxRef<'_> { SyntaxRef::$name(self) }
            fn type_tag() -> TypeId { TypeId::of::<Self>() }
            fn cast<'a>(node: &SyntaxRef<'a>) -> Option<&'a Self> {
                if let SyntaxRef::$name(value) = node { Some(value) } else { None }
            }
        })*
        impl<'a> SyntaxRef<'a> {
            pub fn cast<T: SyntaxType>(&self) -> Option<&'a T> { T::cast(self) }
            pub fn type_tag(&self) -> TypeId { match self {
                $(Self::$name(_) => <$name as SyntaxType>::type_tag(),)*
                Self::Trivia(_) => TypeId::of::<Trivia>(),
                Self::Boundary(..) => TypeId::of::<DelimiterKind>(),
            }}
            pub fn identity(&self) -> Option<SyntaxIdentity> { match self {
                $(Self::$name(value) => Some(SyntaxIdentity(self.type_tag(), std::ptr::from_ref(*value) as usize)),)*
                _ => None,
            }}
            pub fn is_category(&self) -> bool { match self { $(Self::$name(_) => $category,)* _ => false } }
            pub fn is_token(&self) -> bool { match self { $(Self::$name(_) => $token,)* _ => false } }
            pub fn label(&self) -> &'static str { match self {
                $(Self::$name(_) => stringify!($name),)* Self::Trivia(_) => "comment", Self::Boundary(..) => "delimiter",
            }}
            pub fn span(&self) -> Option<Span> { match self {
                $(Self::$name(value) => {
                    if $token { token_span(*value) }
                    else { value.try_span() }
                },)*
                Self::Trivia(value) => value.try_span(), Self::Boundary(_,span) => Some(span.clone()),
            }}
            pub fn children(&self) -> Vec<SyntaxRef<'a>> {
                crate::telemetry::syntax_children();
                #[cfg(test)]
                CHILD_ENUMERATIONS.set(CHILD_ENUMERATIONS.get() + 1);
                struct Children<'a>(Vec<SyntaxRef<'a>>);
                impl<'a> Visit<'a> for Children<'a> {
                    $(fn $visit(&mut self, value: &'a $name) { self.0.push(SyntaxRef::$name(value)); })*
                }
                let mut children = Children(Vec::new());
                match self { $(Self::$name(value) => visit::$visit(&mut children, value),)* _ => {} }
                self.add_punctuation(&mut children.0);
                children.0.sort_by_key(|child|child.span().and_then(|span|span.start()).unwrap_or(0));
                children.0
            }
        }
    }
}

syntax_references! {
    Adj => visit_adj, false, false;
    AngleBarList => visit_angle_bar_list, false, false;
    ArgumentListContentPack => visit_argument_list_content_pack, false, false;
    Array => visit_array, false, false;
    AssignmentList => visit_assignment_list, false, false;
    AssignmentListContentPack => visit_assignment_list_content_pack, false, false;
    BOF => visit_bof, false, false;
    Cell => visit_cell, false, false;
    DoStep => visit_do_step, false, false;
    EOC => visit_eoc, false, false;
    EOF => visit_eof, false, false;
    ElseClause => visit_else_clause, false, false;
    Empty => visit_empty, false, false;
    EscapeSequence => visit_escape_sequence, false, false;
    ExprAssignAugmented => visit_expr_assign_augmented, false, false;
    ExprAssignEvaluated => visit_expr_assign_evaluated, false, false;
    ExprAssignInstallMethod => visit_expr_assign_install_method, false, false;
    ExprAssignInstallNew => visit_expr_assign_install_new, false, false;
    ExprAssignInstallOp => visit_expr_assign_install_op, false, false;
    ExprAssignOp => visit_expr_assign_op, false, false;
    ExprAssignParallelGlobal => visit_expr_assign_parallel_global, false, false;
    ExprAssignParallelLocal => visit_expr_assign_parallel_local, false, false;
    ExprAssignSimpleGlobal => visit_expr_assign_simple_global, false, false;
    ExprAssignSimpleLocal => visit_expr_assign_simple_local, false, false;
    ExprCatch => visit_expr_catch, false, false;
    ExprControlBreak => visit_expr_control_break, false, false;
    ExprControlContinue => visit_expr_control_continue, false, false;
    ExprControlReturn => visit_expr_control_return, false, false;
    ExprDebugBreakpoint => visit_expr_debug_breakpoint, false, false;
    ExprDebugFinish => visit_expr_debug_finish, false, false;
    ExprDebugShield => visit_expr_debug_shield, false, false;
    ExprDebugStep => visit_expr_debug_step, false, false;
    ExprDebugTest => visit_expr_debug_test, false, false;
    ExprForBare => visit_expr_for_bare, false, false;
    ExprForIn => visit_expr_for_in, false, false;
    ExprForRange => visit_expr_for_range, false, false;
    ExprIf => visit_expr_if, false, false;
    ExprLambdaList => visit_expr_lambda_list, false, false;
    ExprLambdaVariadic => visit_expr_lambda_variadic, false, false;
    ExprNew => visit_expr_new, false, false;
    ExprOpBinAdj => visit_expr_op_bin_adj, false, false;
    ExprOpBinIndex => visit_expr_op_bin_index, false, false;
    ExprOpBinMemberDot => visit_expr_op_bin_member_dot, false, false;
    ExprOpBinMemberOptional => visit_expr_op_bin_member_optional, false, false;
    ExprOpBinRegular => visit_expr_op_bin_regular, false, false;
    ExprOpPostfix => visit_expr_op_postfix, false, false;
    ExprOpPrefix => visit_expr_op_prefix, false, false;
    ExprOption => visit_expr_option, false, false;
    ExprPack => visit_expr_pack, false, false;
    ExprProfileElapsedTime => visit_expr_profile_elapsed_time, false, false;
    ExprProfileElapsedTiming => visit_expr_profile_elapsed_timing, false, false;
    ExprProfileProfile => visit_expr_profile_profile, false, false;
    ExprProfileTime => visit_expr_profile_time, false, false;
    ExprProfileTiming => visit_expr_profile_timing, false, false;
    ExprQuoteGlobal => visit_expr_quote_global, false, false;
    ExprQuoteLocal => visit_expr_quote_local, false, false;
    ExprQuoteSymbol => visit_expr_quote_symbol, false, false;
    ExprQuoteThreadLocal => visit_expr_quote_thread_local, false, false;
    ExprQuoteThreadVariable => visit_expr_quote_thread_variable, false, false;
    ExprThrow => visit_expr_throw, false, false;
    ExprTrap => visit_expr_trap, false, false;
    ExprTry => visit_expr_try, false, false;
    ExprWhile => visit_expr_while, false, false;
    FloatLiteral => visit_float_literal, false, false;
    FromInstance => visit_from_instance, false, false;
    Holder => visit_holder, false, false;
    IntegerLiteral => visit_integer_literal, false, false;
    List => visit_list, false, false;
    ListStep => visit_list_step, false, false;
    LoopBody => visit_loop_body, false, false;
    OfClass => visit_of_class, false, false;
    RangeBoundFrom => visit_range_bound_from, false, false;
    RangeBoundTo => visit_range_bound_to, false, false;
    RawStringContent => visit_raw_string_content, false, false;
    RawStringLiteral => visit_raw_string_literal, false, false;
    Sequence => visit_sequence, false, false;
    SourceFile => visit_source_file, false, false;
    Statement => visit_statement, false, false;
    StringContent => visit_string_content, false, false;
    StringLiteral => visit_string_literal, false, false;
    Symbol => visit_symbol, false, false;
    ThenClause => visit_then_clause, false, false;
    TryFallbackExcept => visit_try_fallback_except, false, false;
    TypeAnnotation => visit_type_annotation, false, false;
    WhenCondition => visit_when_condition, false, false;
    _Add => visit___add, false, true;
    _AddAdd => visit___add_add, false, true;
    _AddAddEql => visit___add_add_eql, false, true;
    _AddEql => visit___add_eql, false, true;
    _Amp => visit___amp, false, true;
    _AmpEql => visit___amp_eql, false, true;
    _And => visit___and, false, true;
    _Att => visit___att, false, true;
    _AttAtt => visit___att_att, false, true;
    _AttAttEql => visit___att_att_eql, false, true;
    _AttAttQsm => visit___att_att_qsm, false, true;
    _AttAttQsmEql => visit___att_att_qsm_eql, false, true;
    _AttEql => visit___att_eql, false, true;
    _Bng => visit___bng, false, true;
    _BngEql => visit___bng_eql, false, true;
    _Box => visit___box, false, true;
    _BoxEql => visit___box_eql, false, true;
    _Break => visit___break, false, true;
    _Breakpoint => visit___breakpoint, false, true;
    _Bsl => visit___bsl, false, true;
    _BslBsl => visit___bsl_bsl, false, true;
    _BslBslEql => visit___bsl_bsl_eql, false, true;
    _BslEql => visit___bsl_eql, false, true;
    _Catch => visit___catch, false, true;
    _Cdt => visit___cdt, false, true;
    _CdtEql => visit___cdt_eql, false, true;
    _Cma => visit___cma, false, true;
    _Col => visit___col, false, true;
    _ColEql => visit___col_eql, false, true;
    _Continue => visit___continue, false, true;
    _Crt => visit___crt, false, true;
    _CrtBng => visit___crt_bng, false, true;
    _CrtCrt => visit___crt_crt, false, true;
    _CrtCrtEql => visit___crt_crt_eql, false, true;
    _CrtEql => visit___crt_eql, false, true;
    _CrtGst => visit___crt_gst, false, true;
    _CrtGstEql => visit___crt_gst_eql, false, true;
    _CrtLst => visit___crt_lst, false, true;
    _CrtLstEql => visit___crt_lst_eql, false, true;
    _CrtMul => visit___crt_mul, false, true;
    _CrtMulMul => visit___crt_mul_mul, false, true;
    _CrtMulMulEql => visit___crt_mul_mul_eql, false, true;
    _CrtTld => visit___crt_tld, false, true;
    _Do => visit___do, false, true;
    _Dot => visit___dot, false, true;
    _DotDot => visit___dot_dot, false, true;
    _DotDotEql => visit___dot_dot_eql, false, true;
    _DotDotLst => visit___dot_dot_lst, false, true;
    _DotDotLstEql => visit___dot_dot_lst_eql, false, true;
    _DotQsm => visit___dot_qsm, false, true;
    _ElapsedTime => visit___elapsed_time, false, true;
    _ElapsedTiming => visit___elapsed_timing, false, true;
    _Else => visit___else, false, true;
    _Eql => visit___eql, false, true;
    _EqlBngEql => visit___eql_bng_eql, false, true;
    _EqlEql => visit___eql_eql, false, true;
    _EqlEqlEql => visit___eql_eql_eql, false, true;
    _EqlEqlEqlGst => visit___eql_eql_eql_gst, false, true;
    _EqlEqlEqlGstEql => visit___eql_eql_eql_gst_eql, false, true;
    _EqlEqlGst => visit___eql_eql_gst, false, true;
    _EqlEqlGstEql => visit___eql_eql_gst_eql, false, true;
    _EqlGst => visit___eql_gst, false, true;
    _Except => visit___except, false, true;
    _Finish => visit___finish, false, true;
    _For => visit___for, false, true;
    _From => visit___from, false, true;
    _Global => visit___global, false, true;
    _Graded => visit___graded, false, true;
    _Gst => visit___gst, false, true;
    _GstEql => visit___gst_eql, false, true;
    _GstGst => visit___gst_gst, false, true;
    _GstGstEql => visit___gst_gst_eql, false, true;
    _Hsh => visit___hsh, false, true;
    _HshQsm => visit___hsh_qsm, false, true;
    _If => visit___if, false, true;
    _In => visit___in, false, true;
    _List => visit___list, false, true;
    _Local => visit___local, false, true;
    _Lst => visit___lst, false, true;
    _LstEql => visit___lst_eql, false, true;
    _LstEqlEql => visit___lst_eql_eql, false, true;
    _LstEqlEqlEql => visit___lst_eql_eql_eql, false, true;
    _LstEqlEqlGst => visit___lst_eql_eql_gst, false, true;
    _LstEqlEqlGstEql => visit___lst_eql_eql_gst_eql, false, true;
    _LstLst => visit___lst_lst, false, true;
    _LstLstEql => visit___lst_lst_eql, false, true;
    _LstSub => visit___lst_sub, false, true;
    _Mod => visit___mod, false, true;
    _ModEql => visit___mod_eql, false, true;
    _Mul => visit___mul, false, true;
    _MulEql => visit___mul_eql, false, true;
    _MulMul => visit___mul_mul, false, true;
    _MulMulEql => visit___mul_mul_eql, false, true;
    _New => visit___new, false, true;
    _Not => visit___not, false, true;
    _Of => visit___of, false, true;
    _Or => visit___or, false, true;
    _Pip => visit___pip, false, true;
    _PipEql => visit___pip_eql, false, true;
    _PipPip => visit___pip_pip, false, true;
    _PipPipEql => visit___pip_pip_eql, false, true;
    _PipSub => visit___pip_sub, false, true;
    _PipSubEql => visit___pip_sub_eql, false, true;
    _PipUbs => visit___pip_ubs, false, true;
    _PipUbsEql => visit___pip_ubs_eql, false, true;
    _Profile => visit___profile, false, true;
    _Qsm => visit___qsm, false, true;
    _QsmQsm => visit___qsm_qsm, false, true;
    _QsmQsmEql => visit___qsm_qsm_eql, false, true;
    _Return => visit___return, false, true;
    _Scl => visit___scl, false, true;
    _Sfp => visit___sfp, false, true;
    _SfpEql => visit___sfp_eql, false, true;
    _Shield => visit___shield, false, true;
    _Slh => visit___slh, false, true;
    _SlhEql => visit___slh_eql, false, true;
    _SlhSlh => visit___slh_slh, false, true;
    _SlhSlhEql => visit___slh_slh_eql, false, true;
    _Step => visit___step, false, true;
    _Sub => visit___sub, false, true;
    _SubEql => visit___sub_eql, false, true;
    _SubGst => visit___sub_gst, false, true;
    _Symbol => visit___symbol, false, true;
    _Test => visit___test, false, true;
    _Then => visit___then, false, true;
    _ThreadLocal => visit___thread_local, false, true;
    _ThreadVariable => visit___thread_variable, false, true;
    _Throw => visit___throw, false, true;
    _Time => visit___time, false, true;
    _Timing => visit___timing, false, true;
    _Tld => visit___tld, false, true;
    _TldEql => visit___tld_eql, false, true;
    _To => visit___to, false, true;
    _Trap => visit___trap, false, true;
    _Try => visit___try, false, true;
    _Ubs => visit___ubs, false, true;
    _UbsBng => visit___ubs_bng, false, true;
    _UbsEql => visit___ubs_eql, false, true;
    _UbsGst => visit___ubs_gst, false, true;
    _UbsGstEql => visit___ubs_gst_eql, false, true;
    _UbsLst => visit___ubs_lst, false, true;
    _UbsLstEql => visit___ubs_lst_eql, false, true;
    _UbsMul => visit___ubs_mul, false, true;
    _UbsTld => visit___ubs_tld, false, true;
    _When => visit___when, false, true;
    _While => visit___while, false, true;
    _Xor => visit___xor, false, true;
    ArgumentListContent => visit_argument_list_content, true, false;
    AssignmentArgument => visit_assignment_argument, true, false;
    AssignmentListContent => visit_assignment_list_content, true, false;
    AugmentedOperator => visit_augmented_operator, true, false;
    BinaryOperator => visit_binary_operator, true, false;
    Component => visit_component, true, false;
    Expr => visit_expr, true, false;
    ExprAssign => visit_expr_assign, true, false;
    ExprAssignInstall => visit_expr_assign_install, true, false;
    ExprAssignParallel => visit_expr_assign_parallel, true, false;
    ExprAssignSimple => visit_expr_assign_simple, true, false;
    ExprCollection => visit_expr_collection, true, false;
    ExprControl => visit_expr_control, true, false;
    ExprDebug => visit_expr_debug, true, false;
    ExprFor => visit_expr_for, true, false;
    ExprLambda => visit_expr_lambda, true, false;
    ExprLiteral => visit_expr_literal, true, false;
    ExprOp => visit_expr_op, true, false;
    ExprOpBin => visit_expr_op_bin, true, false;
    ExprOpBinMember => visit_expr_op_bin_member, true, false;
    ExprProfile => visit_expr_profile, true, false;
    ExprQuote => visit_expr_quote, true, false;
    PackedContent => visit_packed_content, true, false;
    PostfixOperator => visit_postfix_operator, true, false;
    PrefixOperator => visit_prefix_operator, true, false;
    RawStringElement => visit_raw_string_element, true, false;
    StringElement => visit_string_element, true, false;
    TryFallback => visit_try_fallback, true, false;
}

impl<'a> SyntaxRef<'a> {
    fn add_punctuation(&self, children: &mut Vec<Self>) {
        let delimiter = match self {
            Self::Array(n) => Some(n.delimiter.delimiter()),
            Self::List(n) => Some(n.delimiter.delimiter()),
            Self::Sequence(n) => Some(n.delimiter.delimiter()),
            Self::Holder(n) => Some(n.delimiter.delimiter()),
            Self::AngleBarList(n) => Some(n.delimiter.delimiter()),
            Self::AssignmentList(n) => Some(n.contents.delimiter.delimiter()),
            Self::ExprLambdaList(n) => Some(n.arguments.delimiter.delimiter()),
            _ => None,
        };
        if let Some(delimiter) = delimiter {
            children.push(Self::Boundary(true, delimiter.opening_span()));
            if let Some(span) = delimiter.closing_span() {
                children.push(Self::Boundary(false, span));
            }
        }
        match self {
            Self::ExprPack(n) => {
                children.extend(n.pack.pairs().map(|(punct, _)| punct.syntax_ref()))
            }
            Self::ArgumentListContentPack(n) => {
                children.extend(n.arguments.pairs().map(|(punct, _)| punct.syntax_ref()))
            }
            Self::AssignmentListContentPack(n) => {
                children.extend(n.arguments.pairs().map(|(punct, _)| punct.syntax_ref()))
            }
            _ => {}
        }
    }
}

#[inline(never)]
fn token_span(value: &impl Lex) -> Option<Span> {
    value
        .to_token_stream()
        .into_iter()
        .next()
        .and_then(|token| token.try_span())
}
