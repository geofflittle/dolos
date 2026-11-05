// Synthetic reports exercise the versioned protobuf mappings, including logs
// unavailable from the currently supported native Trace builtin subset.
use super::*;
use pallas::ledger::{
    primitives::ExUnits as PallasUnits, traverse::MultiEraRedeemerTag as RedeemerTag,
    validate::phase2::tx::MultiEraTxEvalResult,
};

fn entry(tag: RedeemerTag, index: u32, success: bool) -> MultiEraTxEvalResult {
    MultiEraTxEvalResult {
        sub_transaction: None,
        tag,
        index,
        success,
        units: PallasUnits {
            mem: 18485,
            steps: 4805428,
        },
        logs: vec![],
        failure_message: None,
    }
}

#[test]
fn estimation_mapping_all_purposes_and_total() {
    use u5c::cardano::RedeemerPurpose as P;
    let tags = [
        (RedeemerTag::Spend, P::Spend),
        (RedeemerTag::Mint, P::Mint),
        (RedeemerTag::Cert, P::Cert),
        (RedeemerTag::Reward, P::Reward),
        (RedeemerTag::Vote, P::Vote),
        (RedeemerTag::Propose, P::Propose),
    ];
    let report = tx_eval_to_u5c(Ok(tags
        .iter()
        .enumerate()
        .map(|(i, (tag, _))| entry(*tag, i as u32 + 2, true))
        .collect()));
    assert!(report.errors.is_empty());
    for (i, (_, purpose)) in tags.iter().enumerate() {
        assert_eq!(report.redeemers[i].purpose, *purpose as i32);
        assert_eq!(report.redeemers[i].index, i as u32 + 2);
    }
    assert_eq!(
        report.ex_units,
        Some(ExUnits {
            memory: 6 * 18485,
            steps: 6 * 4805428
        })
    );
}

#[test]
fn estimation_mapping_failures_traces_and_partial_success() {
    let mut failed = entry(RedeemerTag::Spend, 3, false);
    failed.failure_message = Some("Explicit error".into());
    failed.logs = vec!["before failure".into(), "detail".into()];
    let fallback = entry(RedeemerTag::Mint, 0, false);
    let mut success = entry(RedeemerTag::Cert, 2, true);
    success.logs = vec!["completed".into()];
    let report = tx_eval_to_u5c(Ok(vec![failed, fallback, success]));
    assert_eq!(report.redeemers.len(), 3);
    assert_eq!(report.errors.len(), 2);
    assert_eq!(report.errors[0].msg, "Spend[3]: Explicit error");
    assert_eq!(report.errors[1].msg, "Mint[0]: script evaluation failed");
    assert_eq!(
        report
            .traces
            .iter()
            .map(|x| x.msg.as_str())
            .collect::<Vec<_>>(),
        ["before failure", "detail", "completed"]
    );
    assert_eq!(
        report.ex_units,
        Some(ExUnits {
            memory: 3 * 18485,
            steps: 3 * 4805428
        })
    );
}

#[test]
fn sub_transaction_results_are_explicitly_rejected() {
    for position in 0..3 {
        let mut sub = entry(RedeemerTag::Spend, 4, true);
        sub.sub_transaction = Some(1);
        sub.logs = vec!["sub log".into()];
        let mut entries = vec![
            entry(RedeemerTag::Spend, 4, true),
            entry(RedeemerTag::Cert, 2, false),
        ];
        entries.insert(position, sub);
        let report = tx_eval_to_u5c(Ok(entries));
        let [error] = &report.errors[..] else {
            panic!("{:?}", report.errors)
        };
        let (version, rest) = error.msg.split_once(' ').unwrap();
        assert_eq!(version, "RPC");
        assert!(
            rest.ends_with(" cannot represent redeemer Spend[4] of sub transaction 1"),
            "{error:?}"
        );
        assert!(report.redeemers.is_empty());
        assert!(report.traces.is_empty());
        assert!(report.ex_units.is_none());
    }
}

#[test]
fn estimation_mapping_empty_and_overflow() {
    let empty = tx_eval_to_u5c(Ok(vec![]));
    assert_eq!(empty.ex_units, Some(ExUnits::default()));
    assert!(empty.errors.is_empty());
    for memory in [false, true] {
        let mut huge = entry(RedeemerTag::Spend, 0, true);
        if memory {
            huge.units.mem = u64::MAX;
        } else {
            huge.units.steps = u64::MAX;
        }
        let report = tx_eval_to_u5c(Ok(vec![huge, entry(RedeemerTag::Mint, 0, true)]));
        assert!(report.ex_units.is_none());
        assert_eq!(report.redeemers.len(), 2);
        assert_eq!(report.errors[0].msg, "total execution units overflow");
    }
}
