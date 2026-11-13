use crate::prelude::*;
use dolos_core::StateStore;
use pallas::codec::minicbor::{self, data::IanaTag};
use pallas::codec::utils::{AnyCbor, AnyUInt, KeyValuePairs};
use pallas::crypto::hash::Hash;
use pallas::ledger::addresses::Address;
use pallas::ledger::primitives::conway::DatumOption;
use pallas::ledger::traverse::{Era, MultiEraOutput, OriginalHash};
use pallas::network::miniprotocols::localstate::queries_v16 as q16;
use std::collections::HashSet;
use tracing::debug;

pub fn build_utxo_by_address_response<D: Domain>(
    domain: &D,
    addrs: &q16::Addrs,
) -> Result<AnyCbor, Error> {
    let mut utxo_pairs: Vec<(q16::UTxO, ReplyOutput)> = Vec::new();
    let mut all_refs = HashSet::new();

    for addr in addrs.iter() {
        let addr_bytes: &[u8] = addr.as_ref();
        debug!(addr_len = addr_bytes.len(), addr_hex = %hex::encode(addr_bytes), "looking up utxos for address");

        let mut refs = domain
            .state()
            .utxos_by_tag("address", addr_bytes)
            .map_err(|e| Error::server(format!("failed to get utxos by address: {}", e)))?;

        debug!(num_refs = refs.len(), "found utxo refs by full address");

        if refs.is_empty() {
            if let Ok(Address::Shelley(shelley_addr)) = Address::from_bytes(addr_bytes) {
                let payment_bytes = shelley_addr.payment().to_vec();
                debug!(payment_hex = %hex::encode(&payment_bytes), "trying payment credential lookup");
                refs = domain
                    .state()
                    .utxos_by_tag("payment", &payment_bytes)
                    .map_err(|e| Error::server(format!("failed to get utxos by payment: {}", e)))?;
                debug!(
                    num_refs = refs.len(),
                    "found utxo refs by payment credential"
                );
            }
        }

        all_refs.extend(refs);
    }

    debug!(
        total_refs = all_refs.len(),
        "total unique utxo refs to fetch"
    );

    let refs_vec: Vec<_> = all_refs.into_iter().collect();
    let utxos = domain
        .state()
        .get_utxos(refs_vec.clone())
        .map_err(|e| Error::server(format!("failed to get utxos: {}", e)))?;

    debug!(fetched_utxos = utxos.len(), "fetched utxo data");

    for utxo_ref in refs_vec {
        if let Some(era_cbor) = utxos.get(&utxo_ref) {
            let output = MultiEraOutput::try_from(era_cbor.as_ref())
                .map_err(|e| Error::server(format!("failed to decode utxo: {}", e)))?;
            let q16_utxo = q16::UTxO {
                transaction_id: utxo_ref.0,
                index: AnyUInt::U32(utxo_ref.1),
            };

            let q16_output = convert_output_to_q16(&output)?;
            utxo_pairs.push((q16_utxo, q16_output));
        }
    }

    debug!(num_utxos = utxo_pairs.len(), "returning utxos");

    let response: KeyValuePairs<q16::UTxO, ReplyOutput> = KeyValuePairs::Def(utxo_pairs);

    Ok(AnyCbor::from_encode((response,)))
}

fn convert_output_to_q16(output: &MultiEraOutput) -> Result<ReplyOutput, Error> {
    use pallas::codec::utils::NonEmptyKeyValuePairs;

    let address = output.address().map_err(Error::server)?.to_vec();
    let value_data = output.value();
    let lovelace = AnyUInt::U64(value_data.coin());

    let assets = value_data.assets();
    let has_assets = !assets.is_empty();

    let value = if has_assets {
        let mut policy_map: Vec<(
            pallas::crypto::hash::Hash<28>,
            NonEmptyKeyValuePairs<pallas::codec::utils::Bytes, AnyUInt>,
        )> = vec![];

        for policy_assets in assets {
            let policy_id = *policy_assets.policy();
            let mut asset_entries: Vec<(pallas::codec::utils::Bytes, AnyUInt)> = vec![];

            for asset in policy_assets.assets() {
                let name = asset.name();
                let amount = asset.output_coin().unwrap_or(0);
                asset_entries.push((name.to_vec().into(), AnyUInt::U64(amount)));
            }

            if !asset_entries.is_empty() {
                policy_map.push((policy_id, NonEmptyKeyValuePairs::Def(asset_entries)));
            }
        }

        if policy_map.is_empty() {
            q16::Value::Coin(lovelace)
        } else {
            q16::Value::Multiasset(lovelace, NonEmptyKeyValuePairs::Def(policy_map))
        }
    } else {
        q16::Value::Coin(lovelace)
    };

    if output.era() >= Era::Alonzo {
        let datum = output.datum().map(|d| match d {
            DatumOption::Hash(h) => ReplyDatum::Hash(h),
            DatumOption::Data(data) => ReplyDatum::Data(data.0.raw_cbor().to_vec()),
        });
        Ok(ReplyOutput::Current {
            address,
            amount: value,
            datum,
        })
    } else {
        let datum_hash = output.datum().map(|d| match d {
            DatumOption::Hash(h) => h,
            DatumOption::Data(data) => data.original_hash(),
        });
        Ok(ReplyOutput::Legacy(q16::LegacyTransactionOutput {
            address: address.into(),
            amount: value,
            datum_hash,
        }))
    }
}

/// The datum of a post Alonzo reply output, an inline datum held as the bytes
/// its transaction carried.
#[derive(Debug, Clone)]
enum ReplyDatum {
    Hash(Hash<32>),
    Data(Vec<u8>),
}

impl<C> minicbor::Encode<C> for ReplyDatum {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        _: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        match self {
            ReplyDatum::Hash(hash) => e.array(2)?.u8(0)?.bytes(hash.as_ref())?,
            ReplyDatum::Data(raw) => e.array(2)?.u8(1)?.tag(IanaTag::Cbor)?.bytes(raw)?,
        };
        Ok(())
    }
}

/// One output of a UTxO query reply.
#[derive(Debug, Clone)]
enum ReplyOutput {
    Current {
        address: Vec<u8>,
        amount: q16::Value,
        datum: Option<ReplyDatum>,
    },
    Legacy(q16::LegacyTransactionOutput),
}

impl<C> minicbor::Encode<C> for ReplyOutput {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        match self {
            ReplyOutput::Current {
                address,
                amount,
                datum,
            } => {
                e.map(2 + u64::from(datum.is_some()))?;
                e.u8(0)?.bytes(address)?;
                e.u8(1)?.encode_with(amount, ctx)?;
                if let Some(datum) = datum {
                    e.u8(2)?.encode_with(datum, ctx)?;
                }
            }
            ReplyOutput::Legacy(output) => {
                e.encode_with(output, ctx)?;
            }
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use pallas::codec::minicbor;
    use pallas::ledger::traverse::{Era, MultiEraOutput};

    use super::convert_output_to_q16;

    /// The inline datum field of an output, its key and its option holding the
    /// datum bytes.
    fn datum_field(datum: &[u8]) -> Vec<u8> {
        let mut field = vec![0x02, 0x82, 0x01, 0xd8, 0x18];
        field.extend(minicbor::to_vec(<&minicbor::bytes::ByteSlice>::from(datum)).unwrap());
        field
    }

    /// The reply's CBOR for an output to a Shelley testnet key address of one
    /// ada with the inline datum.
    fn reply(datum: &[u8]) -> Vec<u8> {
        let mut output = vec![0xa3, 0x00, 0x58, 0x1d, 0x60];
        output.extend([0x11; 28]);
        output.extend([0x01, 0x1a, 0x00, 0x0f, 0x42, 0x40]);
        output.extend(datum_field(datum));
        let output = MultiEraOutput::decode(Era::Conway, &output).unwrap();
        minicbor::to_vec(convert_output_to_q16(&output).unwrap()).unwrap()
    }

    #[test]
    fn a_datum_in_the_ledger_encoding_answers_its_bytes() {
        let datum = [0xd8, 0x79, 0x9f, 0x01, 0xff];
        assert!(reply(&datum).ends_with(&datum_field(&datum)));
    }

    #[test]
    fn a_datum_in_another_encoding_answers_the_bytes_it_was_sent_as() {
        for datum in [
            vec![0xd8, 0x79, 0x81, 0x01],
            vec![0xbf, 0x01, 0x02, 0xff],
            vec![0x9f, 0x40, 0xff],
            vec![0x5f, 0x41, 0x00, 0xff],
        ] {
            let reply = reply(&datum);
            assert!(
                reply.ends_with(&datum_field(&datum)),
                "datum {} answered {}",
                hex::encode(&datum),
                hex::encode(&reply)
            );
        }
    }
}
