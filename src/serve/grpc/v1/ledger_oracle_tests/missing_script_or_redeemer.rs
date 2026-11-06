//! Refusals of corpus transactions that lack a script or redeemer one of their credentials needs.
use super::guarding::{fixture, reports, rpc};

async fn assert_refused_naming(number: u32, reason: &str) {
    let f = fixture(number);
    let (evaluated, estimated) = reports(&f, &f.tx);
    let calls = f.group.estimate(&f.tx);
    let wire = rpc(&f, &f.tx).await;
    let errors = [
        format!("{:?}", evaluated.expect_err("evaluation")),
        format!("{:?}", estimated.expect_err("estimation")),
        format!("{:?}", calls.expect_err("estimation with calls")),
        wire.errors.first().map(|e| e.msg.clone()).unwrap_or_default(),
    ];
    for error in errors {
        assert!(error.contains(reason), "ic#{number}: {error}");
    }
}

#[tokio::test]
async fn a_script_spend_with_no_redeemer_is_refused() {
    assert_refused_naming(2176, "redeemer").await;
}

#[tokio::test]
async fn a_sub_transaction_script_spend_with_no_redeemer_is_refused() {
    assert_refused_naming(1353, "redeemer").await;
}

#[tokio::test]
async fn a_script_credential_with_no_script_is_refused() {
    assert_refused_naming(1638, "missing required script").await;
}
