use any_chain_eval::Chain;
use dolos_core::SubmitExt;
use futures_core::Stream;
use futures_util::{StreamExt as _, TryStreamExt as _};
use pallas::crypto::hash::Hash;
use pallas::interop::utxorpc::v1::spec as u5c;
use pallas::interop::utxorpc::v1::spec::cardano::ExUnits;
use pallas::interop::utxorpc::v1::spec::submit::{WaitForTxResponse, *};
use std::collections::HashSet;
use std::pin::Pin;
use tonic::{Request, Response, Status};
use tracing::{debug, info};

use crate::prelude::*;

pub struct SubmitServiceImpl<D>
where
    D: Domain,
{
    domain: D,
}

impl<D> SubmitServiceImpl<D>
where
    D: Domain,
{
    pub fn new(domain: D) -> Self {
        Self { domain }
    }
}

fn tx_stage_to_u5c(stage: MempoolTxStage) -> i32 {
    match stage {
        MempoolTxStage::Pending => Stage::Mempool as i32,
        MempoolTxStage::Propagated => Stage::Network as i32,
        MempoolTxStage::Acknowledged => Stage::Acknowledged as i32,
        MempoolTxStage::Confirmed => Stage::Confirmed as i32,
        _ => Stage::Unspecified as i32,
    }
}

fn event_to_watch_mempool_response(event: MempoolEvent) -> WatchMempoolResponse {
    WatchMempoolResponse {
        tx: TxInMempool {
            r#ref: event.tx.hash().to_vec().into(),
            native_bytes: event.tx.payload().cbor().to_vec().into(),
            stage: tx_stage_to_u5c(event.tx.stage.clone()),
            parsed_state: None, // TODO
        }
        .into(),
    }
}

fn event_to_wait_for_tx_response(event: MempoolEvent) -> WaitForTxResponse {
    WaitForTxResponse {
        stage: tx_stage_to_u5c(event.tx.stage.clone()),
        r#ref: event.tx.hash().to_vec().into(),
    }
}

fn tx_eval_to_u5c(
    eval: Result<dolos_core::mempool::MultiEraEvalReport, DomainError>,
) -> u5c::cardano::TxEval {
    let report = match eval {
        Ok(report) => report,
        Err(e) => {
            return u5c::cardano::TxEval {
                errors: vec![u5c::cardano::EvalReport {
                    msg: format!("{e:#?}"),
                    ..Default::default()
                }],
                ..Default::default()
            }
        }
    };
    let mut result = u5c::cardano::TxEval {
        ex_units: report.iter().try_fold(ExUnits::default(), |acc, eval| {
            Some(ExUnits {
                steps: acc.steps.checked_add(eval.units.steps)?,
                memory: acc.memory.checked_add(eval.units.mem)?,
            })
        }),
        ..Default::default()
    };
    if result.ex_units.is_none() {
        result.errors.push(u5c::cardano::EvalReport {
            msg: "total execution units overflow".into(),
            ..Default::default()
        });
    }
    for entry in report {
        use pallas::ledger::traverse::MultiEraRedeemerTag;
        use u5c::cardano::RedeemerPurpose;
        let unrepresentable = |redeemer: String| u5c::cardano::TxEval {
            errors: vec![u5c::cardano::EvalReport {
                msg: format!("RPC v1 cannot represent redeemer {redeemer}"),
                ..Default::default()
            }],
            ..Default::default()
        };
        if let Some(sub) = entry.sub_transaction {
            return unrepresentable(format!(
                "{:?}[{}] of sub transaction {sub}",
                entry.tag, entry.index
            ));
        }
        let purpose = match entry.tag {
            MultiEraRedeemerTag::Spend => RedeemerPurpose::Spend,
            MultiEraRedeemerTag::Mint => RedeemerPurpose::Mint,
            MultiEraRedeemerTag::Cert => RedeemerPurpose::Cert,
            MultiEraRedeemerTag::Reward => RedeemerPurpose::Reward,
            MultiEraRedeemerTag::Vote => RedeemerPurpose::Vote,
            MultiEraRedeemerTag::Propose => RedeemerPurpose::Propose,
            MultiEraRedeemerTag::Guarding => RedeemerPurpose::Guarding,
            other => return unrepresentable(format!("{other:?}[{}]", entry.index)),
        };
        let named = |msg| u5c::cardano::EvalReport {
            msg,
            purpose: Some(purpose as i32),
            index: Some(entry.index),
        };
        if !entry.success {
            result.errors.push(named(format!(
                "{:?}[{}]: {}",
                entry.tag,
                entry.index,
                entry
                    .failure_message
                    .as_deref()
                    .unwrap_or("script evaluation failed")
            )));
        }
        result.traces.extend(entry.logs.into_iter().map(named));
        result.redeemers.push(u5c::cardano::Redeemer {
            purpose: purpose as i32,
            index: entry.index,
            ex_units: Some(ExUnits {
                steps: entry.units.steps,
                memory: entry.units.mem,
            }),
            ..Default::default()
        });
    }
    result
}

#[async_trait::async_trait]
impl<D> submit_service_server::SubmitService for SubmitServiceImpl<D>
where
    D: Domain,
{
    type WaitForTxStream =
        Pin<Box<dyn Stream<Item = Result<WaitForTxResponse, tonic::Status>> + Send + 'static>>;

    type WatchMempoolStream =
        Pin<Box<dyn Stream<Item = Result<WatchMempoolResponse, tonic::Status>> + Send + 'static>>;

    async fn submit_tx(
        &self,
        request: Request<SubmitTxRequest>,
    ) -> Result<Response<SubmitTxResponse>, Status> {
        let message = request.into_inner();

        debug!("received new grpc submit tx request");

        let chain = self.domain.read_chain();

        let tx = message
            .tx
            .ok_or_else(|| Status::invalid_argument("missing tx"))?;
        let tx_bytes = match tx.r#type {
            Some(any_chain_tx::Type::Raw(bytes)) => bytes,
            _ => return Err(Status::invalid_argument("missing or unsupported tx type")),
        };

        let hash = self
            .domain
            .receive_tx("grpc", &chain, tx_bytes.as_ref())
            .map_err(|e| Status::invalid_argument(format!("could not process tx: {e}")))?;

        info!(tx.hash = %hash, "accepted grpc transaction into mempool");
        Ok(Response::new(SubmitTxResponse {
            r#ref: hash.to_vec().into(),
        }))
    }

    async fn wait_for_tx(
        &self,
        request: Request<WaitForTxRequest>,
    ) -> Result<Response<Self::WaitForTxStream>, Status> {
        let subjects: HashSet<_> = request
            .into_inner()
            .r#ref
            .into_iter()
            .map(|x| Hash::from(x.as_ref()))
            .collect();

        let initial_stages: Vec<_> = subjects
            .iter()
            .map(|x| {
                Result::<_, Status>::Ok(WaitForTxResponse {
                    stage: tx_stage_to_u5c(self.domain.mempool().check_status(x).stage),
                    r#ref: x.to_vec().into(),
                })
            })
            .collect();

        let updates = self.domain.mempool().subscribe();

        let updates = UpdateFilter::<D::Mempool>::new(updates, subjects)
            .map(|x| Ok(event_to_wait_for_tx_response(x)))
            .boxed();

        let stream = tokio_stream::iter(initial_stages).chain(updates).boxed();

        Ok(Response::new(stream))
    }

    async fn read_mempool(
        &self,
        _request: tonic::Request<ReadMempoolRequest>,
    ) -> Result<tonic::Response<ReadMempoolResponse>, tonic::Status> {
        Err(Status::unimplemented("read_mempool is not yet available"))
    }

    async fn watch_mempool(
        &self,
        _request: tonic::Request<WatchMempoolRequest>,
    ) -> Result<tonic::Response<Self::WatchMempoolStream>, tonic::Status> {
        let updates = self.domain.mempool().subscribe();

        let stream = updates
            .map_ok(event_to_watch_mempool_response)
            .map_err(|e| Status::internal(e.to_string()))
            .boxed();

        Ok(Response::new(stream))
    }

    async fn eval_tx(
        &self,
        request: tonic::Request<EvalTxRequest>,
    ) -> Result<tonic::Response<EvalTxResponse>, tonic::Status> {
        let tx = request
            .into_inner()
            .tx
            .ok_or_else(|| Status::invalid_argument("missing tx"))?;

        let tx_raw = match tx.r#type {
            Some(any_chain_tx::Type::Raw(bytes)) => bytes.to_vec(),
            _ => return Err(Status::invalid_argument("missing or unsupported tx type")),
        };

        let chain = self.domain.read_chain();

        let result = self.domain.estimate_tx(&chain, &tx_raw);
        let result = tx_eval_to_u5c(result);

        let report = AnyChainEval {
            chain: Some(Chain::Cardano(result)),
        };

        Ok(Response::new(EvalTxResponse {
            report: Some(report),
        }))
    }
}

#[cfg(test)]
mod tests {
    include!("../submit_tests.rs");
}

#[cfg(test)]
mod mapping_tests {
    include!("../submit_mapping_tests.rs");

    #[test]
    fn guarding_result_keeps_identity_and_budget() {
        let mut success = entry(RedeemerTag::Guarding, 9, true);
        success.logs = vec!["guard completed".into()];
        let report = tx_eval_to_u5c(Ok(vec![success]));
        assert_eq!(
            report.traces,
            vec![u5c::cardano::EvalReport {
                msg: "guard completed".into(),
                purpose: Some(u5c::cardano::RedeemerPurpose::Guarding as i32),
                index: Some(9),
            }]
        );

        assert!(report.errors.is_empty(), "{report:?}");
        assert_eq!(report.redeemers.len(), 1);

        let redeemer = &report.redeemers[0];
        assert_eq!(
            redeemer.purpose,
            u5c::cardano::RedeemerPurpose::Guarding as i32
        );
        assert_eq!(redeemer.index, 9);

        let expected_units = Some(ExUnits {
            memory: 18_485,
            steps: 4_805_428,
        });
        assert_eq!(redeemer.ex_units, expected_units);
        assert_eq!(report.ex_units, expected_units);
    }

    #[test]
    fn guarding_failure_keeps_details() {
        use u5c::cardano::RedeemerPurpose as P;

        let mut failed = entry(RedeemerTag::Guarding, 7, false);
        failed.failure_message = Some("guard rejected".into());
        failed.logs = vec!["before failure".into(), "detail".into()];
        failed.units = PallasUnits { mem: 11, steps: 23 };

        let report = tx_eval_to_u5c(Ok(vec![failed]));

        assert_eq!(report.errors.len(), 1);
        let error = &report.errors[0];
        assert_eq!(error.msg, "Guarding[7]: guard rejected");
        assert_eq!(
            (error.purpose, error.index),
            (Some(P::Guarding as i32), Some(7))
        );

        assert_eq!(report.traces.len(), 2);
        assert_eq!(report.traces[0].msg, "before failure");
        assert_eq!(report.traces[1].msg, "detail");
        for trace in &report.traces {
            assert_eq!(
                (trace.purpose, trace.index),
                (Some(P::Guarding as i32), Some(7))
            );
        }

        assert_eq!(report.redeemers.len(), 1);
        let redeemer = &report.redeemers[0];
        assert_eq!(redeemer.purpose, P::Guarding as i32);
        assert_eq!(redeemer.index, 7);

        let expected_units = Some(ExUnits {
            memory: 11,
            steps: 23,
        });
        assert_eq!(redeemer.ex_units, expected_units);
        assert_eq!(report.ex_units, expected_units);
    }

    #[test]
    fn a_transaction_level_error_names_no_redeemer() {
        let failed = tx_eval_to_u5c(Err(DomainError::Internal("unbalanced".into())));
        let mut huge = entry(RedeemerTag::Spend, 0, true);
        huge.units.mem = u64::MAX;
        let overflow = tx_eval_to_u5c(Ok(vec![huge, entry(RedeemerTag::Mint, 0, true)]));

        for report in [&failed.errors[0], &overflow.errors[0]] {
            assert_eq!((report.purpose, report.index), (None, None), "{report:?}");
        }
    }

    #[test]
    fn redeemer_errors_and_traces_name_their_redeemer() {
        use u5c::cardano::RedeemerPurpose as P;
        let mut failed = entry(RedeemerTag::Guarding, 3, false);
        failed.logs = vec!["before failure".into()];
        let mut success = entry(RedeemerTag::Cert, 2, true);
        success.logs = vec!["completed".into()];

        let report = tx_eval_to_u5c(Ok(vec![failed, success]));
        assert_eq!(report.redeemers.len(), 2);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(
            report.errors[0].msg,
            "Guarding[3]: script evaluation failed"
        );
        assert_eq!(
            report.ex_units,
            Some(ExUnits {
                memory: 2 * 18_485,
                steps: 2 * 4_805_428
            })
        );
        assert_eq!(
            (report.redeemers[0].purpose, report.redeemers[0].index),
            (P::Guarding as i32, 3)
        );
        assert_eq!(
            (report.redeemers[1].purpose, report.redeemers[1].index),
            (P::Cert as i32, 2)
        );
        let named = |x: &u5c::cardano::EvalReport| (x.purpose, x.index);

        assert_eq!(
            report.errors.iter().map(named).collect::<Vec<_>>(),
            [(Some(P::Guarding as i32), Some(3))]
        );
        assert_eq!(
            report.traces.iter().map(named).collect::<Vec<_>>(),
            [
                (Some(P::Guarding as i32), Some(3)),
                (Some(P::Cert as i32), Some(2))
            ]
        );
    }
}
