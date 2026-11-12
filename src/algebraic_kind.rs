//! The kinds of algebraic and geometric objects a program works with.
//!
//! Macaulay2 has hundreds of classes for them, most defined by packages, but
//! they fall into a few families that read alike: a ring and a quotient of one
//! are rings, a sheaf of modules is a module, a matrix and a ring map are
//! morphisms. The kind of an object is the family of the most specific
//! *root class* it descends from, so a class a package adds later is
//! recognised as soon as its parent is, and the kinds stay few on purpose.
//! Coherent sheaves count as modules and sheaves of algebras as rings.
//!
//! The tables list roots only; a class that is not a root, but descends from
//! one, has the root's kind. Where two roots match, the more specific wins:
//! `ChainComplex` descends from `GradedModule`, and is a complex.

use std::fmt::{self, Display, Formatter};

use strum::EnumIter;

use crate::object_registry::ObjectName;
use crate::typesystem::TypeKnowledge;

/// A family of algebraic or geometric objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter)]
#[non_exhaustive]
pub enum AlgebraicKind {
    /// A ring: a polynomial ring, a quotient, a field, a sheaf of rings, an
    /// algebra of any flavour.
    Ring,
    /// A module over a ring: an ideal, a free module, a coherent sheaf, a
    /// vector space, a vector bundle.
    Module,
    /// A map between algebraic objects: a matrix, a ring map, a map of
    /// complexes, of sheaves, of varieties.
    Morphism,
    /// A complex: a chain or cochain complex, a resolution, a filtered
    /// complex, a spectral sequence, a differential module.
    Complex,
    /// A functor, such as `Ext`, `Tor` or `HH`.
    Functor,
    /// Any other algebraic structure: a monoid, a group, a poset, a matroid,
    /// a Lie algebra, a formal group law.
    Structure,
    /// A geometric space: a variety, a scheme, a polyhedron, a fan, a
    /// simplicial or cell complex, a tropical cycle.
    Space,
}

impl AlgebraicKind {
    /// The classes that begin this kind: it contains them and everything
    /// below them.
    const fn roots(self) -> &'static [&'static str] {
        match self {
            Self::Ring => &[
                "Ring",
                "RingFamily",
                "AbstractGCRing",
                "DGAlgebra",
                "MonomialAlgebra",
                "Subring",
                "PolynomialOIAlgebra",
                "RingOfInvariants",
                "WittPolynomialRing",
                "WittQuotientRing",
            ],
            Self::Module => &[
                "Module",
                "Ideal",
                "CoherentSheaf",
                "AbstractSheaf",
                "GradedModule",
                "DGModule",
                "DGIdeal",
                "LabeledModule",
                "FreeOIModule",
                "ModuleInWidth",
                "VectorSpace",
                "LieAlgebraModule",
                "ToricVectorBundle",
                "VectorBundleOnE",
                "NCIdeal",
                "NCLeftIdeal",
                "NCRightIdeal",
                "WittIdeal",
            ],
            Self::Morphism => &[
                "Matrix",
                "RingMap",
                "NCRingMap",
                "GradedModuleMap",
                "ComplexMap",
                "SheafMap",
                "DGAlgebraMap",
                "DGModuleMap",
                "LabeledModuleMap",
                "FreeOIModuleMap",
                "SimplicialMap",
                "SimplicialModuleMap",
                "ToricMap",
                "LieAlgebraMap",
                "PosetMap",
                "PageMap",
                "EquivariantMap",
                "RationalMap",
                "MultirationalMap",
                "RationalMapping",
                "AbstractVarietyMap",
                "WittRingMap",
                "MackeyFunctorHomomorphism",
                "ZZdFactorizationMap",
            ],
            Self::Complex => &[
                "ChainComplex",
                "Complex",
                "Resolution",
                "NCChainComplex",
                "FilteredComplex",
                "OIResolution",
                "SimplicialModule",
                "ZZdFactorization",
                "SpectralSequence",
            ],
            Self::Functor => &["ScriptedFunctor", "CpMackeyFunctor"],
            Self::Structure => &[
                "Monoid",
                "GroupAction",
                "FinitelyGeneratedAbelianGroup",
                "Poset",
                "Matroid",
                "LieAlgebra",
                "FormalGroupLaw",
            ],
            Self::Space => &[
                "Variety",
                "AbstractVariety",
                "MultiprojectiveVariety",
                "GKMVariety",
                "NumericalVariety",
                "PolyhedralObject",
                "RationalCone",
                "SimplicialComplex",
                "AbstractSimplicialComplex",
                "CellComplex",
                "MomentAngleComplex",
                "QuasiToricManifold",
                "TropicalCycle",
                "Curve",
            ],
        }
    }

    /// The kind of the objects of `class`, if it is one of the families.
    pub fn of(knowledge: &(impl TypeKnowledge + ?Sized), class: &ObjectName) -> Option<Self> {
        let mut roots = <Self as strum::IntoEnumIterator>::iter()
            .flat_map(|kind| {
                kind.roots()
                    .iter()
                    .map(move |root| (kind, ObjectName::new(*root)))
            })
            .filter(|(_, root)| knowledge.is_subtype(class, root));
        let (mut kind, mut closest) = roots.next()?;
        for (candidate, root) in roots {
            if knowledge.is_subtype(&root, &closest) {
                (kind, closest) = (candidate, root);
            }
        }
        Some(kind)
    }
}

impl Display for AlgebraicKind {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Ring => "ring",
            Self::Module => "module",
            Self::Morphism => "morphism",
            Self::Complex => "complex",
            Self::Functor => "functor",
            Self::Structure => "algebraic structure",
            Self::Space => "geometric space",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::AlgebraicKind;
    use crate::object_registry::{ObjectName, ObjectRegistry};

    /// The kind of the class `name` in the catalog.
    fn kind(registry: &ObjectRegistry, name: &str) -> Option<AlgebraicKind> {
        AlgebraicKind::of(registry, &ObjectName::new(name))
    }

    /// Library classes land in the family their ancestry names, and the most
    /// specific root decides where families overlap.
    #[test]
    fn classes_have_the_kind_of_their_closest_root() {
        use AlgebraicKind::*;
        let registry = ObjectRegistry::load(crate::catalog::test_corpus());
        for (class, expected) in [
            ("PolynomialRing", Some(Ring)),
            ("QuotientRing", Some(Ring)),
            ("GaloisField", Some(Ring)),
            ("SheafOfRings", Some(Ring)),
            ("Module", Some(Module)),
            ("Ideal", Some(Module)),
            ("MonomialIdeal", Some(Module)),
            ("CoherentSheaf", Some(Module)),
            ("Matrix", Some(Morphism)),
            ("RingMap", Some(Morphism)),
            ("Complex", Some(Complex)),
            ("ScriptedFunctor", Some(Functor)),
            ("Monoid", Some(Structure)),
            ("ProjectiveVariety", Some(Space)),
            ("Polyhedron", Some(Space)),
            ("List", None),
        ] {
            assert_eq!(kind(&registry, class), expected, "{class}");
        }
    }
}
