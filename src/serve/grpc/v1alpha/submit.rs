use any_chain_eval::Chain;
use dolos_core::SubmitExt;
use futures_core::Stream;
use futures_util::{StreamExt as _, TryStreamExt as _};
use pallas::crypto::hash::Hash;
use pallas::interop::utxorpc::v1alpha::spec::cardano::ExUnits;
use pallas::interop::utxorpc::v1alpha::spec::submit::{WaitForTxResponse, *};
use pallas::interop::utxorpc::v1alpha::{self as interop, spec as u5c};
use std::collections::HashSet;
use std::pin::Pin;
use tonic::{Request, Response, Status};
use tracing::{debug, info};

use crate::prelude::*;
use crate::serve::grpc::block_context::{BlockContext, NoInputs, WithUnconfirmed};
use crate::serve::grpc::eval::{evaluate_tx, TxEvaluation};
use crate::serve::grpc::masking::MempoolTxMask;
use crate::serve::grpc::run_blocking;
use crate::serve::ChainReads;

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

/// The watch predicate that states the same rule as a mempool predicate.
fn watch_predicate(predicate: TxPredicate) -> u5c::watch::TxPredicate {
    let TxPredicate {
        r#match,
        not,
        all_of,
        any_of,
    } = predicate;
    let each = |list: Vec<TxPredicate>| -> Vec<u5c::watch::TxPredicate> {
        list.into_iter().map(watch_predicate).collect()
    };

    u5c::watch::TxPredicate {
        r#match: r#match.map(
            |AnyChainTxPattern { chain }| u5c::watch::AnyChainTxPattern {
                chain: chain.map(|any_chain_tx_pattern::Chain::Cardano(pattern)| {
                    u5c::watch::any_chain_tx_pattern::Chain::Cardano(pattern)
                }),
            },
        ),
        not: each(not),
        all_of: each(all_of),
        any_of: each(any_of),
    }
}

/// The answer to a mempool change the predicate admits, holding the parts the
/// mask selects.
fn event_to_watch_mempool_response<C: ChainReads>(
    event: MempoolEvent,
    context: &C,
    predicate: Option<&u5c::watch::TxPredicate>,
    mask: MempoolTxMask,
) -> Result<Option<WatchMempoolResponse>, Status> {
    let parsed = if predicate.is_some() || mask.parsed {
        let tx = event
            .tx
            .with_tx(|tx| {
                tx.map(|tx| {
                    interop::Mapper::new(BlockContext::for_lone_tx(context.clone(), tx)).map_tx(tx)
                })
            })
            .map_err(|e| Status::internal(e.to_string()))?;
        if predicate.is_some_and(|p| !super::watch::apply_predicate(p, &tx)) {
            return Ok(None);
        }
        mask.parsed.then_some(tx)
    } else {
        None
    };

    Ok(Some(WatchMempoolResponse {
        tx: Some(TxInMempool {
            r#ref: if mask.r#ref {
                event.tx.hash().to_vec().into()
            } else {
                Default::default()
            },
            native_bytes: if mask.native_bytes {
                event.tx.payload().cbor().to_vec().into()
            } else {
                Default::default()
            },
            stage: if mask.stage {
                tx_stage_to_u5c(event.tx.stage.clone())
            } else {
                Default::default()
            },
            parsed_state: parsed.map(tx_in_mempool::ParsedState::Cardano),
        }),
    }))
}

/// The answers to a WatchMempool request over the mempool changes `updates`
/// yields.
fn watch_mempool_answers<D: Domain + ChainReads>(
    domain: D,
    updates: impl Stream<Item = Result<MempoolEvent, MempoolError>> + Send + 'static,
    request: WatchMempoolRequest,
) -> impl Stream<Item = Result<WatchMempoolResponse, Status>> + Send + 'static {
    let mask = MempoolTxMask::from_paths(
        request
            .field_mask
            .as_ref()
            .map(|m| m.paths.as_slice())
            .unwrap_or_default(),
    );
    let predicate = std::sync::Arc::new(request.predicate.map(watch_predicate));
    let reads_stores = predicate.is_some() || mask.parsed;

    updates
        .map_err(|e| Status::internal(e.to_string()))
        .and_then(move |event| {
            let domain = domain.clone();
            let predicate = predicate.clone();
            async move {
                if !reads_stores {
                    return event_to_watch_mempool_response(event, &domain, None, mask);
                }
                run_blocking(&domain, move |domain| {
                    event_to_watch_mempool_response(
                        event,
                        &WithUnconfirmed(domain),
                        predicate.as_ref().as_ref(),
                        mask,
                    )
                })
                .await
            }
        })
        .try_filter_map(|answer| async move { Ok(answer) })
}

fn event_to_wait_for_tx_response(event: MempoolEvent) -> WaitForTxResponse {
    WaitForTxResponse {
        stage: tx_stage_to_u5c(event.tx.stage.clone()),
        r#ref: event.tx.hash().to_vec().into(),
    }
}

fn tx_eval_to_u5c<R>(
    eval: Result<TxEvaluation<R>, DomainError>,
    map: impl Fn(&R) -> u5c::cardano::Redeemer,
) -> u5c::cardano::TxEval {
    let evaluation = match eval {
        Ok(evaluation) => evaluation,
        Err(e) => {
            return u5c::cardano::TxEval {
                errors: vec![u5c::cardano::EvalError {
                    msg: format!("{e:#?}"),
                }],
                ..Default::default()
            }
        }
    };
    let report = &evaluation.report;
    let mut result = u5c::cardano::TxEval {
        ex_units: report.iter().try_fold(ExUnits::default(), |acc, eval| {
            Some(ExUnits {
                steps: acc.steps.checked_add(eval.units.steps)?,
                memory: acc.memory.checked_add(eval.units.mem)?,
            })
        }),
        fee: evaluation.min_fee.and_then(super::query::u64_to_bigint),
        ..Default::default()
    };
    if result.ex_units.is_none() {
        result.errors.push(u5c::cardano::EvalError {
            msg: "total execution units overflow".into(),
        });
    }
    for entry in report {
        use pallas::ledger::traverse::MultiEraRedeemerTag as RedeemerTag;
        use u5c::cardano::RedeemerPurpose;
        let unrepresentable = |redeemer: String| u5c::cardano::TxEval {
            errors: vec![u5c::cardano::EvalError {
                msg: format!("RPC v1alpha cannot represent redeemer {redeemer}"),
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
            RedeemerTag::Spend => RedeemerPurpose::Spend,
            RedeemerTag::Mint => RedeemerPurpose::Mint,
            RedeemerTag::Cert => RedeemerPurpose::Cert,
            RedeemerTag::Reward => RedeemerPurpose::Reward,
            RedeemerTag::Vote => RedeemerPurpose::Vote,
            RedeemerTag::Propose => RedeemerPurpose::Propose,
            other => return unrepresentable(format!("{other:?}[{}]", entry.index)),
        };
        if !entry.success {
            result.errors.push(u5c::cardano::EvalError {
                msg: format!(
                    "{:?}[{}]: {}",
                    entry.tag,
                    entry.index,
                    entry
                        .failure_message
                        .as_deref()
                        .unwrap_or("script evaluation failed")
                ),
            });
        }
        result.traces.extend(
            entry
                .logs
                .iter()
                .cloned()
                .map(|msg| u5c::cardano::EvalTrace { msg }),
        );
        let held = evaluation.redeemer(entry.tag, entry.index).map(&map);
        let (payload, original_cbor) = held
            .map(|r| (r.payload, r.original_cbor))
            .unwrap_or_default();
        result.redeemers.push(u5c::cardano::Redeemer {
            purpose: purpose as i32,
            payload,
            index: entry.index,
            ex_units: Some(ExUnits {
                steps: entry.units.steps,
                memory: entry.units.mem,
            }),
            original_cbor,
        });
    }
    result
}

#[async_trait::async_trait]
impl<D> submit_service_server::SubmitService for SubmitServiceImpl<D>
where
    D: Domain + ChainReads,
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

        let tx = message
            .tx
            .ok_or_else(|| Status::invalid_argument("missing tx"))?;
        let tx_bytes = match tx.r#type {
            Some(any_chain_tx::Type::Raw(bytes)) => bytes,
            _ => return Err(Status::invalid_argument("missing or unsupported tx type")),
        };

        let hash = run_blocking(&self.domain, move |domain| {
            let chain = domain.read_chain();

            domain
                .receive_tx("grpc", &chain, tx_bytes.as_ref())
                .map_err(|error| crate::serve::grpc::convert::submit_error_status(error.into()))
        })
        .await?;

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

        let (subjects, initial_stages, updates) = run_blocking(&self.domain, move |domain| {
            let initial_stages: Vec<_> = subjects
                .iter()
                .map(|x| {
                    Result::<_, Status>::Ok(WaitForTxResponse {
                        stage: tx_stage_to_u5c(domain.mempool().check_status(x).stage),
                        r#ref: x.to_vec().into(),
                    })
                })
                .collect();

            let updates = domain.mempool().subscribe();

            Ok((subjects, initial_stages, updates))
        })
        .await?;

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
        request: tonic::Request<WatchMempoolRequest>,
    ) -> Result<tonic::Response<Self::WatchMempoolStream>, tonic::Status> {
        let updates = self.domain.mempool().subscribe();

        Ok(Response::new(
            watch_mempool_answers(self.domain.clone(), updates, request.into_inner()).boxed(),
        ))
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

        let result = run_blocking(&self.domain, move |domain| {
            let chain = domain.read_chain();

            let mapper = interop::Mapper::new(NoInputs);
            Ok(evaluate_tx(&domain, &chain, &tx_raw, |evaluation| {
                tx_eval_to_u5c(evaluation, |r| mapper.map_redeemer(r))
            }))
        })
        .await?;

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

    fn tx_hash(tx: &u5c::cardano::Tx) -> Vec<u8> {
        tx.hash.to_vec()
    }
}

#[cfg(test)]
mod mapping_tests {
    include!("../submit_mapping_tests.rs");
    #[test]
    fn guarding_results_are_explicitly_rejected() {
        for success in [false, true] {
            for position in 0..3 {
                let mut guard = entry(RedeemerTag::Guarding, 9, success);
                guard.logs = vec!["guard log".into()];
                guard.failure_message = Some("guard rejected".into());
                let mut entries = vec![
                    entry(RedeemerTag::Spend, 4, true),
                    entry(RedeemerTag::Cert, 2, false),
                ];
                entries.insert(position, guard);
                let report = tx_eval_to_u5c(Ok(evaluation(entries)));
                assert_eq!(
                    report.errors,
                    vec![u5c::cardano::EvalError {
                        msg: "RPC v1alpha cannot represent redeemer Guarding[9]".into(),
                    }]
                );
                assert!(report.redeemers.is_empty());
                assert!(report.traces.is_empty());
                assert!(report.ex_units.is_none());
            }
        }
    }

    #[test]
    fn a_redeemer_this_version_cannot_represent_is_never_mapped() {
        let mapped = std::cell::RefCell::new(vec![]);
        let mut held = evaluation(vec![
            entry(RedeemerTag::Spend, 4, true),
            entry(RedeemerTag::Guarding, 9, true),
        ]);
        let redeemer = |index| u5c::cardano::Redeemer {
            index,
            ..Default::default()
        };
        held.redeemers = vec![
            (RedeemerTag::Spend, 4, redeemer(4)),
            (RedeemerTag::Guarding, 9, redeemer(9)),
        ];

        let report = super::tx_eval_to_u5c(Ok(held), |r| {
            mapped.borrow_mut().push(r.index);
            r.clone()
        });

        assert_eq!(mapped.into_inner(), [4]);
        assert_eq!(
            report.errors,
            vec![u5c::cardano::EvalError {
                msg: "RPC v1alpha cannot represent redeemer Guarding[9]".into(),
            }]
        );
    }
}
