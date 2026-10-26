//! Navigation metadata over the retained native typed AST.

use super::cells::{CellId, ParsedCell, SyntaxSnapshot};
use super::syntax_ref::{SyntaxIdentity, SyntaxRef, SyntaxType};
use m2_syn::nodes::*;
use std::any::TypeId;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug)]
pub(super) struct Entry {
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub aliases: HashMap<TypeId, usize>,
    pub start: usize,
    pub end: usize,
    pub visible: bool,
    pub collection: bool,
    pub error: bool,
    pub canonical: usize,
    pub syntax_type: TypeId,
    pub label: &'static str,
    pub token: bool,
    pub comment: bool,
    pub boundary: Option<bool>,
}

self_cell::self_cell! {
    struct RetainedCellIndex {
        owner: ParsedCell,
        #[covariant]
        dependent: CellIndexData,
    }
    impl {Debug}
}

/// Navigation facts and direct typed references share one retained cell owner.
#[derive(Debug)]
pub(super) struct CellIndex {
    retained: RetainedCellIndex,
}

impl CellIndex {
    pub fn new(cell: &ParsedCell) -> Self {
        Self {
            retained: RetainedCellIndex::new(cell.clone(), |cell| CellIndexData::new(cell)),
        }
    }
    pub fn id(&self) -> CellId {
        self.retained.borrow_dependent().id
    }
    pub fn entries(&self) -> &[Entry] {
        &self.retained.borrow_dependent().entries
    }
    pub fn identities(&self) -> &HashMap<SyntaxIdentity, usize> {
        &self.retained.borrow_dependent().identities
    }
    pub fn resolve(&self, id: usize) -> Option<SyntaxRef<'_>> {
        crate::telemetry::syntax_lookup();
        self.retained.borrow_dependent().references.get(id)?.clone()
    }
    pub fn visible_parent(&self, id: usize) -> Option<usize> {
        self.retained.borrow_dependent().visible_parent(id)
    }
    pub fn children(&self, id: usize) -> Vec<usize> {
        self.retained.borrow_dependent().children(id)
    }
}

#[derive(Debug)]
struct CellIndexData<'a> {
    id: CellId,
    entries: Vec<Entry>,
    identities: HashMap<SyntaxIdentity, usize>,
    references: Vec<Option<SyntaxRef<'a>>>,
}

impl<'a> CellIndexData<'a> {
    fn new(cell: &'a ParsedCell) -> Self {
        let mut index = Self {
            id: cell.id(),
            entries: Vec::new(),
            identities: HashMap::new(),
            references: Vec::new(),
        };
        if let Some(syntax) = cell.syntax() {
            index.push(syntax.syntax_ref(), None);
        } else {
            index.entries.push(Entry {
                parent: None,
                children: Vec::new(),
                aliases: HashMap::new(),
                start: 0,
                end: cell.source().len(),
                visible: true,
                collection: false,
                error: cell.parse_error().is_some() || !cell.lexer_diagnostics().is_empty(),
                canonical: 0,
                syntax_type: TypeId::of::<()>(),
                label: if cell.parse_error().is_some() || !cell.lexer_diagnostics().is_empty() {
                    "invalid cell"
                } else {
                    "Cell"
                },
                token: false,
                comment: false,
                boundary: None,
            });
            index.references.push(None);
        }
        for trivia in cell.trivia() {
            let range = m2_syn::Spanned::try_span(trivia)
                .and_then(|span| span.range())
                .unwrap_or(0..0);
            let parent = index
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| {
                    entry.visible
                        && !entry.token
                        && !entry.comment
                        && entry.boundary.is_none()
                        && entry.start <= range.start
                        && range.end <= entry.end
                })
                .min_by_key(|(_, entry)| entry.end - entry.start)
                .map_or(0, |(id, _)| id);
            let child = index.push(SyntaxRef::Trivia(trivia.clone()), Some(parent));
            index.entries[parent].children.push(child);
        }
        for i in 0..index.entries.len() {
            let mut children = std::mem::take(&mut index.entries[i].children);
            children.sort_by_key(|child| index.entries[*child].start);
            index.entries[i].children = children;
        }
        for i in (0..index.entries.len()).rev() {
            let canonical = if index.entries[i].visible {
                i
            } else if index.entries[i].children.len() == 1 {
                index.entries[index.entries[i].children[0]].canonical
            } else {
                index.visible_parent(i).unwrap_or(0)
            };
            index.entries[i].canonical = canonical;
        }
        // Hidden category wrappers alias their concrete typed node, while their
        // structural identity remains separately available in the index.
        let tags = index
            .references
            .iter()
            .enumerate()
            .filter_map(|(i, syntax)| syntax.as_ref().map(|syntax| (i, syntax.type_tag())))
            .collect::<Vec<_>>();
        for (i, tag) in tags {
            let canonical = index.entries[i].canonical;
            index.entries[canonical].aliases.insert(tag, i);
        }
        index
    }

    fn push(&mut self, syntax: SyntaxRef<'a>, parent: Option<usize>) -> usize {
        let id = self.entries.len();
        self.references.push(Some(syntax.clone()));
        let range = syntax.span().and_then(|span| span.range()).unwrap_or(0..0);
        let standalone_assignment = matches!(syntax, SyntaxRef::ExprAssignEvaluated(_))
            && parent
                .is_none_or(|parent| self.entries[parent].syntax_type != ExprAssign::type_tag());
        let visible = (standalone_assignment || visible(&syntax))
            && !(matches!(syntax, SyntaxRef::ExprPack(_))
                && parent
                    .and_then(|parent| {
                        if self.entries[parent].visible {
                            Some(parent)
                        } else {
                            self.visible_parent(parent)
                        }
                    })
                    .is_some_and(|parent| self.entries[parent].collection));
        let mut range = range;
        if matches!(syntax, SyntaxRef::Trivia(_)) {
            if let Some(text) = syntax
                .span()
                .and_then(|span| span.text().map(str::to_owned))
            {
                range.start += text.len() - text.trim_start_matches([' ', '\t']).len();
                range.end -= text.len() - text.trim_end_matches(['\r', '\n']).len();
            }
        }
        self.entries.push(Entry {
            parent,
            children: Vec::new(),
            aliases: HashMap::new(),
            start: range.start,
            end: range.end,
            visible,
            collection: matches!(
                syntax,
                SyntaxRef::Sequence(_)
                    | SyntaxRef::List(_)
                    | SyntaxRef::Array(_)
                    | SyntaxRef::AngleBarList(_)
            ),
            error: false,
            canonical: id,
            syntax_type: syntax.type_tag(),
            label: syntax.label(),
            token: syntax.is_token(),
            comment: matches!(syntax, SyntaxRef::Trivia(_)),
            boundary: match syntax {
                SyntaxRef::Boundary(opening, _) => Some(opening),
                _ => None,
            },
        });
        if let Some(identity) = syntax.identity() {
            self.identities.insert(identity, id);
        }
        for child in syntax.children() {
            let child_id = self.push(child, Some(id));
            self.entries[id].children.push(child_id);
        }
        id
    }

    pub fn visible_parent(&self, mut id: usize) -> Option<usize> {
        while let Some(parent) = self.entries[id].parent {
            if self.entries[parent].visible {
                return Some(parent);
            }
            id = parent;
        }
        None
    }

    pub fn children(&self, id: usize) -> Vec<usize> {
        let mut result = Vec::new();
        self.collect_children(id, &mut result);
        result
    }

    fn collect_children(&self, id: usize, result: &mut Vec<usize>) {
        for &child in &self.entries[id].children {
            if self.entries[child].visible {
                result.push(child);
            } else {
                self.collect_children(child, result);
            }
        }
    }
}

fn visible(node: &SyntaxRef<'_>) -> bool {
    match node {
        SyntaxRef::ArgumentListContent(_)
        | SyntaxRef::ExprAssign(_)
        | SyntaxRef::ExprLambda(_)
        | SyntaxRef::ExprFor(_)
        | SyntaxRef::ExprQuote(_)
        | SyntaxRef::ExprDebug(_)
        | SyntaxRef::ExprProfile(_)
        | SyntaxRef::ExprOpBinMember(_) => true,
        SyntaxRef::TryFallback(TryFallback::Except { .. }) => true,
        SyntaxRef::ExprAssignSimpleGlobal(_)
        | SyntaxRef::ExprAssignSimpleLocal(_)
        | SyntaxRef::ExprAssignParallelGlobal(_)
        | SyntaxRef::ExprAssignParallelLocal(_)
        | SyntaxRef::ExprAssignEvaluated(_)
        | SyntaxRef::ExprAssignAugmented(_)
        | SyntaxRef::ExprAssignInstallMethod(_)
        | SyntaxRef::ExprAssignInstallOp(_)
        | SyntaxRef::ExprAssignInstallNew(_)
        | SyntaxRef::ExprAssignOp(_)
        | SyntaxRef::ExprLambdaVariadic(_)
        | SyntaxRef::ExprLambdaList(_)
        | SyntaxRef::ExprForBare(_)
        | SyntaxRef::ExprForRange(_)
        | SyntaxRef::ExprForIn(_)
        | SyntaxRef::ExprQuoteSymbol(_)
        | SyntaxRef::ExprQuoteGlobal(_)
        | SyntaxRef::ExprQuoteLocal(_)
        | SyntaxRef::ExprQuoteThreadVariable(_)
        | SyntaxRef::ExprQuoteThreadLocal(_)
        | SyntaxRef::ExprDebugTest(_)
        | SyntaxRef::ExprDebugStep(_)
        | SyntaxRef::ExprDebugFinish(_)
        | SyntaxRef::ExprDebugShield(_)
        | SyntaxRef::ExprDebugBreakpoint(_)
        | SyntaxRef::ExprProfileElapsedTime(_)
        | SyntaxRef::ExprProfileElapsedTiming(_)
        | SyntaxRef::ExprProfileProfile(_)
        | SyntaxRef::ExprProfileTime(_)
        | SyntaxRef::ExprProfileTiming(_)
        | SyntaxRef::ExprOpBinMemberDot(_)
        | SyntaxRef::ExprOpBinMemberOptional(_)
        | SyntaxRef::TryFallbackExcept(_)
        | SyntaxRef::ArgumentListContentPack(_)
        | SyntaxRef::AssignmentListContentPack(_)
        | SyntaxRef::StringContent(_)
        | SyntaxRef::RawStringContent(_)
        | SyntaxRef::EscapeSequence(_)
        | SyntaxRef::ListStep(_)
        | SyntaxRef::DoStep(_)
        | SyntaxRef::BOF(_)
        | SyntaxRef::EOF(_)
        | SyntaxRef::EOC(_) => false,
        _ => !node.is_category(),
    }
}

#[derive(Debug, Clone)]
pub struct TypedSource {
    pub(super) snapshot: SyntaxSnapshot,
}

impl TypedSource {
    pub fn visit<'a>(&'a self, visitor: &mut (impl m2_syn::visit::Visit<'a> + ?Sized)) {
        for cell in self.snapshot.cells() {
            if let Some(syntax) = cell.syntax() {
                visitor.visit_cell(syntax);
            }
        }
    }
}

pub(super) fn indexes(
    snapshot: &SyntaxSnapshot,
    previous: Option<(&SyntaxSnapshot, &[Arc<CellIndex>])>,
) -> Vec<Arc<CellIndex>> {
    let retained = previous
        .into_iter()
        .flat_map(|(old, indexes)| old.cells().iter().zip(indexes))
        .map(|(cell, index)| (cell.id(), index))
        .collect::<HashMap<_, _>>();
    snapshot
        .cells()
        .iter()
        .map(|cell| {
            retained
                .get(&cell.id())
                .map_or_else(|| Arc::new(CellIndex::new(cell)), |index| Arc::clone(index))
        })
        .collect()
}
