//! The classes where pallas's reading differs from the ledger's, each with
//! what holds the difference.

use ledger_oracle_judge::comparison::{Allowance, Allowed, Answer, Disagreement, Layer, Owner};

use super::{Decoded, Refusal};

type Entry = (&'static str, Allowance<Decoded, Refusal>);

fn expected_map(answer: &Answer<Decoded, Refusal>) -> bool {
    matches!(answer, Answer::Refuses(r) if r.error.contains("expected map"))
}

pub const LIST: &[Entry] = &[(
    "eb",
    Allowance {
        allowed: Allowed::new(
            "endorser block references as the node sends them",
            Disagreement::Refuses,
            Owner::Check {
                layer: Layer::Networking,
                test: "pallas-primitives dijkstra::tests::endorser_block_isomorphic_decoding_encoding",
            },
        ),
        shown: expected_map,
    },
)];
