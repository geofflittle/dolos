use axum::{
    body::Bytes,
    extract::State,
    http::{header, HeaderMap, StatusCode},
};
use dolos_core::{ChainError, Domain, DomainError, MempoolError, SubmitExt};

use crate::{error::Error, Facade};

fn is_valid_cbor_content_type(headers: &HeaderMap) -> bool {
    let Some(content_type) = headers.get(header::CONTENT_TYPE) else {
        return false;
    };

    let Ok(content_type) = content_type.to_str() else {
        return false;
    };

    content_type == "application/cbor"
}

pub async fn route<D: Domain + SubmitExt>(
    State(domain): State<Facade<D>>,
    headers: HeaderMap,
    cbor: Bytes,
) -> Result<String, Error> {
    if !is_valid_cbor_content_type(&headers) {
        return Err(StatusCode::BAD_REQUEST.into());
    }

    let chain = domain.read_chain();
    let hash = domain
        .inner
        .receive_tx("minibf", &chain, &cbor)
        .map_err(submit_error)?;

    Ok(hex::encode(hash))
}

/// The error a submit answers when receiving its transaction fails.
fn submit_error(error: DomainError) -> Error {
    let status = match error {
        DomainError::ChainError(x) => match x {
            ChainError::BrokenInvariant(_) => StatusCode::BAD_REQUEST,
            ChainError::DecodingError(_) => StatusCode::BAD_REQUEST,
            ChainError::CborDecodingError(_) => StatusCode::BAD_REQUEST,
            ChainError::AddressDecoding(_) => StatusCode::BAD_REQUEST,
            ChainError::Phase1ValidationRejected(_) => StatusCode::BAD_REQUEST,
            ChainError::Phase2ValidationRejected(_) => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        },
        DomainError::MempoolError(x) => match x {
            MempoolError::TraverseError(_) => StatusCode::BAD_REQUEST,
            MempoolError::InvalidTx(_) => StatusCode::BAD_REQUEST,
            MempoolError::DecodeError(_) => StatusCode::BAD_REQUEST,
            MempoolError::PlutusNotSupported => StatusCode::BAD_REQUEST,
            MempoolError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            MempoolError::StateError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            MempoolError::PParamsNotAvailable => StatusCode::INTERNAL_SERVER_ERROR,
            MempoolError::DuplicateTx => StatusCode::CONFLICT,
            MempoolError::Full(_) => return Error::MempoolFull,
        },
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    status.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestApp, TestFault};
    use axum::response::IntoResponse;
    use dolos_core::PassedBound;

    #[test]
    fn a_duplicate_transaction_answers_conflict() {
        let response = submit_error(MempoolError::DuplicateTx.into()).into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn a_full_mempool_answers_too_early() {
        let full = MempoolError::Full(PassedBound::Txs {
            bound: 2,
            would_hold: 3,
        });
        let response = submit_error(full.into()).into_response();
        assert_eq!(response.status(), StatusCode::TOO_EARLY);
        assert_eq!(StatusCode::TOO_EARLY.as_u16(), 425);
    }

    #[tokio::test]
    async fn a_full_mempool_answers_the_blockfrost_body() {
        use http_body_util::BodyExt;

        let full = MempoolError::Full(PassedBound::Bytes {
            bound: 4_000_000,
            would_hold: 4_000_100,
        });
        let response = submit_error(full.into()).into_response();
        assert_eq!(response.status(), StatusCode::TOO_EARLY);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).ok(),
            Some(serde_json::json!({
                "status_code": 425,
                "error": "Mempool Full",
                "message": "Mempool is full, please try resubmitting again later.",
            }))
        );
    }

    async fn assert_status(app: &TestApp, content_type: &str, body: Vec<u8>, expected: StatusCode) {
        let (status, _body) = app.post_bytes("/tx/submit", content_type, body).await;
        assert_eq!(status, expected);
    }

    #[tokio::test]
    async fn tx_submit_happy_path() {
        use dolos_cardano::{model::PParamValue, SingletonEntity};
        use dolos_core::{StateStore, StateWriter};
        use dolos_testing::{synthetic::SyntheticBlockConfig, toy_domain::ToyDomain};

        // This fixture constructs a Conway transaction. The shared preview
        // harness starts at protocol 6, so give submission its matching active
        // Conway context rather than relying on shape-based era inference.
        let app = TestApp::new_with_cfg_and_setup(
            SyntheticBlockConfig {
                block_count: 5,
                txs_per_block: 3,
                ..Default::default()
            },
            |domain, _| {
                let mut epoch = dolos_cardano::load_epoch::<ToyDomain>(domain.state()).unwrap();
                epoch
                    .pparams
                    .unwrap_live_mut()
                    .set(PParamValue::ProtocolVersion((9, 0)));
                let writer = domain.state().start_writer().unwrap();
                writer
                    .write_entity_typed(&dolos_cardano::model::EpochState::singleton_key(), &epoch)
                    .unwrap();
                writer.commit().unwrap();
            },
        );
        let (status, body) = app
            .post_bytes(
                "/tx/submit",
                "application/cbor",
                app.vectors().tx_cbor.clone(),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let hash = String::from_utf8(body).expect("hash must be utf-8");
        assert_eq!(hash.len(), 64);
        assert!(hex::decode(hash).is_ok());
    }

    #[tokio::test]
    async fn tx_submit_rejects_transaction_from_later_era() {
        // Valid Conway bytes must not select Conway on the protocol-6 harness.
        let app = TestApp::new();
        assert_status(
            &app,
            "application/cbor",
            app.vectors().tx_cbor.clone(),
            StatusCode::BAD_REQUEST,
        )
        .await;
    }

    #[tokio::test]
    async fn tx_submit_bad_request_content_type() {
        let app = TestApp::new();
        assert_status(
            &app,
            "application/json",
            app.vectors().tx_cbor.clone(),
            StatusCode::BAD_REQUEST,
        )
        .await;
    }

    #[tokio::test]
    async fn tx_submit_bad_request_invalid_cbor() {
        let app = TestApp::new();
        assert_status(
            &app,
            "application/cbor",
            vec![0xde, 0xad, 0xbe, 0xef],
            StatusCode::BAD_REQUEST,
        )
        .await;
    }

    #[tokio::test]
    #[ignore]
    async fn tx_submit_not_found() {
        let app = TestApp::new();
        assert_status(
            &app,
            "application/cbor",
            app.vectors().tx_cbor.clone(),
            StatusCode::NOT_FOUND,
        )
        .await;
    }

    #[tokio::test]
    async fn tx_submit_internal_error() {
        let app = TestApp::new_with_fault(Some(TestFault::StateStoreError));
        assert_status(
            &app,
            "application/cbor",
            app.vectors().tx_cbor.clone(),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
        .await;
    }
}
