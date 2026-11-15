//! The outputs a UTxO predicate admits, built from the outputs each of its
//! match patterns admits.
use dolos_core::UtxoSet;
use tonic::Status;

/// The outputs a predicate admits, held as a set of references or as every
/// reference outside one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Selection {
    /// The outputs of the set.
    Only(UtxoSet),
    /// Every output outside the set.
    AllExcept(UtxoSet),
}

impl Selection {
    /// Every output.
    pub(super) fn all() -> Self {
        Self::AllExcept(UtxoSet::new())
    }

    /// No output.
    pub(super) fn none() -> Self {
        Self::Only(UtxoSet::new())
    }

    /// The outputs both selections admit.
    pub(super) fn intersection(self, other: Self) -> Self {
        match (self, other) {
            (Self::Only(a), Self::Only(b)) => Self::Only(&a & &b),
            (Self::Only(a), Self::AllExcept(b)) | (Self::AllExcept(b), Self::Only(a)) => {
                Self::Only(&a - &b)
            }
            (Self::AllExcept(a), Self::AllExcept(b)) => Self::AllExcept(&a | &b),
        }
    }

    /// The outputs either selection admits.
    pub(super) fn union(self, other: Self) -> Self {
        match (self, other) {
            (Self::Only(a), Self::Only(b)) => Self::Only(&a | &b),
            (Self::Only(a), Self::AllExcept(b)) | (Self::AllExcept(b), Self::Only(a)) => {
                Self::AllExcept(&b - &a)
            }
            (Self::AllExcept(a), Self::AllExcept(b)) => Self::AllExcept(&a & &b),
        }
    }

    /// The outputs this selection does not admit.
    pub(super) fn complement(self) -> Self {
        match self {
            Self::Only(x) => Self::AllExcept(x),
            Self::AllExcept(x) => Self::Only(x),
        }
    }

    /// The references of the outputs the selection admits, refused when it
    /// admits every output outside a set.
    pub(super) fn into_bounded(self) -> Result<UtxoSet, Status> {
        match self {
            Self::Only(x) => Ok(x),
            Self::AllExcept(_) => Err(Status::invalid_argument(
                "criteria too broad, narrow it down",
            )),
        }
    }
}

/// The clauses of a UTxO predicate.
pub(super) struct Clauses<P: UtxoPredicate> {
    pub(super) matching: Option<P::Pattern>,
    pub(super) not: Vec<P>,
    pub(super) all_of: Vec<P>,
    pub(super) any_of: Vec<P>,
}

/// A UTxO predicate of one RPC version.
pub(super) trait UtxoPredicate: Sized {
    /// The pattern of the match clause.
    type Pattern;

    /// The clauses of the predicate.
    fn into_clauses(self) -> Clauses<Self>;
}

/// The outputs `predicate` admits, with `matched` giving the outputs a match
/// pattern admits.
pub(super) fn select<P: UtxoPredicate>(
    predicate: P,
    matched: &mut impl FnMut(P::Pattern) -> Result<Selection, Status>,
) -> Result<Selection, Status> {
    let Clauses {
        matching,
        not,
        all_of,
        any_of,
    } = predicate.into_clauses();

    let mut selection = match matching {
        Some(pattern) => matched(pattern)?,
        None => Selection::all(),
    };
    for x in not {
        selection = selection.intersection(select(x, matched)?.complement());
    }
    for x in all_of {
        selection = selection.intersection(select(x, matched)?);
    }
    if !any_of.is_empty() {
        let mut either = Selection::none();
        for x in any_of {
            either = either.union(select(x, matched)?);
        }
        selection = selection.intersection(either);
    }

    Ok(selection)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dolos_core::TxoRef;
    use pallas::crypto::hash::Hash;

    /// The set of the references numbered `held`.
    fn set(held: &[u8]) -> UtxoSet {
        held.iter()
            .map(|i| TxoRef(Hash::new([*i; 32]), 0))
            .collect()
    }

    /// The selection of exactly the references numbered `held`.
    fn only(held: &[u8]) -> Selection {
        Selection::Only(set(held))
    }

    /// The selection of every reference except those numbered `held`.
    fn all_except(held: &[u8]) -> Selection {
        Selection::AllExcept(set(held))
    }

    #[test]
    fn each_operation_admits_what_the_set_operation_on_its_operands_admits() {
        for (got, expected) in [
            (only(&[1, 2]).intersection(only(&[2, 3])), only(&[2])),
            (only(&[1, 2]).intersection(all_except(&[2, 3])), only(&[1])),
            (all_except(&[2, 3]).intersection(only(&[1, 2])), only(&[1])),
            (
                all_except(&[1, 2]).intersection(all_except(&[2, 3])),
                all_except(&[1, 2, 3]),
            ),
            (only(&[1, 2]).union(only(&[2, 3])), only(&[1, 2, 3])),
            (only(&[1, 2]).union(all_except(&[2, 3])), all_except(&[3])),
            (all_except(&[2, 3]).union(only(&[1, 2])), all_except(&[3])),
            (
                all_except(&[1, 2]).union(all_except(&[2, 3])),
                all_except(&[2]),
            ),
            (only(&[1]).complement(), all_except(&[1])),
            (all_except(&[1]).complement(), only(&[1])),
            (Selection::all().complement(), Selection::none()),
        ] {
            assert_eq!(got, expected);
        }
    }

    #[test]
    fn only_a_selection_of_named_outputs_is_bounded() {
        assert_eq!(
            all_except(&[1]).into_bounded().unwrap_err().code(),
            tonic::Code::InvalidArgument
        );
        assert_eq!(
            Selection::all().into_bounded().unwrap_err().code(),
            tonic::Code::InvalidArgument
        );
        assert_eq!(only(&[1, 2]).into_bounded().unwrap(), set(&[1, 2]));
        assert_eq!(Selection::none().into_bounded().unwrap(), set(&[]));
    }
}
