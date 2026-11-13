//! The fields of a served output against the values decoded from the ledger's
//! output bytes.

use pallas::{
    codec::minicbor,
    crypto::hash::Hasher,
    ledger::primitives::{
        alonzo, babbage::GenTransactionOutput, conway, dijkstra, BigInt, PlutusData,
        StakeCredential,
    },
};

use std::fmt;

/// A part of a served output compared against the ledger's output bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OutputField {
    Address,
    Coin,
    Assets,
    Datum,
    Script,
    ParsedState,
}

impl fmt::Display for OutputField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            OutputField::Address => "address",
            OutputField::Coin => "coin",
            OutputField::Assets => "assets",
            OutputField::Datum => "datum",
            OutputField::Script => "script",
            OutputField::ParsedState => "parsed_state",
        })
    }
}

/// An integer as CBOR states it, its sign and the big endian magnitude, without
/// leading zeros, of the value when nonnegative or of minus one minus the value
/// when negative.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Integer {
    negative: bool,
    magnitude: Vec<u8>,
}

impl Integer {
    fn of(value: i128) -> Self {
        let (negative, magnitude) = match value < 0 {
            true => (true, (-1 - value) as u128),
            false => (false, value as u128),
        };
        Integer::from_bytes(negative, &magnitude.to_be_bytes())
    }

    fn from_bytes(negative: bool, bytes: &[u8]) -> Self {
        let start = bytes.iter().position(|b| *b != 0).unwrap_or(bytes.len());
        Integer {
            negative,
            magnitude: bytes[start..].to_vec(),
        }
    }
}

/// A Plutus datum in the terms both the ledger's bytes and a served datum
/// state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Data {
    Constr {
        tag: u64,
        any_constructor: u64,
        fields: Vec<Data>,
    },
    Map(Vec<(Data, Data)>),
    Integer(Integer),
    Bytes(Vec<u8>),
    Array(Vec<Data>),
}

impl Data {
    fn ledger(data: &PlutusData) -> Self {
        match data {
            PlutusData::Constr(c) => Data::Constr {
                tag: c.tag,
                any_constructor: c.any_constructor.unwrap_or_default(),
                fields: c.fields.iter().map(Data::ledger).collect(),
            },
            PlutusData::Map(pairs) => {
                let pairs = pairs.iter();
                Data::Map(
                    pairs
                        .map(|(k, v)| (Data::ledger(k), Data::ledger(v)))
                        .collect(),
                )
            }
            PlutusData::Array(items) => Data::Array(items.iter().map(Data::ledger).collect()),
            PlutusData::BigInt(BigInt::Int(i)) => Data::Integer(Integer::of((*i).into())),
            PlutusData::BigInt(BigInt::BigUInt(b)) => Data::Integer(Integer::from_bytes(false, b)),
            PlutusData::BigInt(BigInt::BigNInt(b)) => Data::Integer(Integer::from_bytes(true, b)),
            PlutusData::BoundedBytes(b) => Data::Bytes(b.to_vec()),
        }
    }
}

/// A credential in the terms both the ledger's bytes and a served credential
/// state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Credential {
    Key(Vec<u8>),
    Script(Vec<u8>),
}

/// A native script in the terms both the ledger's bytes and a served native
/// script state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Native {
    Pubkey(Vec<u8>),
    All(Vec<Native>),
    Any(Vec<Native>),
    NOfK(i64, Vec<Native>),
    InvalidBefore(u64),
    InvalidHereafter(u64),
    RequireGuard(Credential),
    /// A clause a version without a guard clause serves for a guard clause.
    Unset,
}

impl Native {
    /// The native script of the ledger as a version serves it, a guard clause
    /// unset where `guards` is false.
    fn ledger(script: &dijkstra::NativeScript, guards: bool) -> Self {
        use dijkstra::NativeScript as N;
        let all = |items: &[N]| items.iter().map(|s| Native::ledger(s, guards)).collect();
        match script {
            N::ScriptPubkey(h) => Native::Pubkey(h.to_vec()),
            N::ScriptAll(items) => Native::All(all(items)),
            N::ScriptAny(items) => Native::Any(all(items)),
            N::ScriptNOfK(k, items) => Native::NOfK(*k, all(items)),
            N::InvalidBefore(slot) => Native::InvalidBefore(*slot),
            N::InvalidHereafter(slot) => Native::InvalidHereafter(*slot),
            N::ScriptRequireGuard(_) if !guards => Native::Unset,
            N::ScriptRequireGuard(StakeCredential::AddrKeyhash(h)) => {
                Native::RequireGuard(Credential::Key(h.to_vec()))
            }
            N::ScriptRequireGuard(StakeCredential::ScriptHash(h)) => {
                Native::RequireGuard(Credential::Script(h.to_vec()))
            }
        }
    }
}

/// A reference script in the terms both the ledger's bytes and a served script
/// state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Script {
    Native(Native),
    Plutus(u8, Vec<u8>),
}

impl Script {
    fn ledger(script: &dijkstra::ScriptRef<'_>, guards: bool) -> Self {
        use dijkstra::ScriptRef as R;
        match script {
            R::NativeScript(s) => Script::Native(Native::ledger(s, guards)),
            R::PlutusV1Script(s) => Script::Plutus(1, s.0.to_vec()),
            R::PlutusV2Script(s) => Script::Plutus(2, s.0.to_vec()),
            R::PlutusV3Script(s) => Script::Plutus(3, s.0.to_vec()),
            R::PlutusV4Script(s) => Script::Plutus(4, s.0.to_vec()),
        }
    }
}

/// The datum a served output states, each field none where the message states
/// none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ServedDatum {
    hash: Vec<u8>,
    payload: Option<Option<Data>>,
    original_cbor: Option<Vec<u8>>,
}

/// What a served output states of its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ServedBytes {
    /// The version's output message has no bytes field.
    NoField,
    /// The bytes the message states, none when it states none.
    Stated(Option<Vec<u8>>),
}

/// The fields of a served output in terms every version of the spec states
/// them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ServedOutput {
    /// Whether the version states a native guard clause.
    guards: bool,
    pub(super) bytes: ServedBytes,
    address: Vec<u8>,
    coin: Option<Integer>,
    assets: Option<Vec<Asset>>,
    datum: Option<ServedDatum>,
    script: Option<Option<Script>>,
}

/// The datum of a ledger output.
#[derive(Debug)]
enum LedgerDatum {
    None,
    Hash(Vec<u8>),
    Inline {
        hash: Vec<u8>,
        cbor: Vec<u8>,
        data: Data,
    },
}

impl LedgerDatum {
    /// Whether a served datum states this datum, its hash for a datum hash and
    /// its hash, bytes and value for an inline datum.
    fn agrees(&self, served: Option<&ServedDatum>) -> bool {
        match (self, served) {
            (LedgerDatum::None, None) => true,
            (LedgerDatum::None, Some(d)) => {
                d.hash.is_empty() && d.payload.is_none() && d.original_cbor.is_none()
            }
            (LedgerDatum::Hash(_) | LedgerDatum::Inline { .. }, None) => false,
            (LedgerDatum::Hash(hash), Some(d)) => {
                let bytes_hash = d.original_cbor.as_ref().map(|c| Hasher::<256>::hash(c));
                d.hash == *hash && bytes_hash.is_none_or(|h| h.as_slice() == hash.as_slice())
            }
            (LedgerDatum::Inline { hash, cbor, data }, Some(d)) => {
                d.hash == *hash
                    && d.original_cbor.as_deref() == Some(cbor.as_slice())
                    && d.payload.as_ref().and_then(Option::as_ref) == Some(data)
            }
        }
    }
}

/// The policy, name and quantity of one asset of an output.
type Asset = (Vec<u8>, Vec<u8>, Integer);

/// The values of a ledger output in the fields a served output states.
#[derive(Debug)]
struct LedgerOutput {
    address: Vec<u8>,
    coin: Integer,
    assets: Vec<Asset>,
    datum: LedgerDatum,
    script: Option<Script>,
}

fn alonzo_assets(value: &alonzo::Value) -> (u64, Vec<Asset>) {
    match value {
        alonzo::Value::Coin(coin) => (*coin, Vec::new()),
        alonzo::Value::Multiasset(coin, policies) => {
            let assets = policies.iter().flat_map(|(policy, assets)| {
                let amount = |q: &u64| Integer::of((*q).into());
                assets
                    .iter()
                    .map(move |(name, q)| (policy.to_vec(), name.to_vec(), amount(q)))
            });
            (*coin, assets.collect())
        }
    }
}

fn conway_assets(value: &conway::Value) -> (u64, Vec<Asset>) {
    match value {
        conway::Value::Coin(coin) => (*coin, Vec::new()),
        conway::Value::Multiasset(coin, policies) => {
            let assets = policies.iter().flat_map(|(policy, assets)| {
                let amount = |q| Integer::of(u64::from(q).into());
                assets
                    .iter()
                    .map(move |(name, q)| (policy.to_vec(), name.to_vec(), amount(q)))
            });
            (*coin, assets.collect())
        }
    }
}

impl LedgerOutput {
    fn decode(bytes: &[u8], guards: bool) -> Result<Self, minicbor::decode::Error> {
        let output: dijkstra::TransactionOutput = minicbor::decode(bytes)?;
        let (address, (coin, mut assets), datum, script) = match &output {
            GenTransactionOutput::Legacy(o) => {
                let datum = match &o.datum_hash {
                    Some(h) => LedgerDatum::Hash(h.to_vec()),
                    None => LedgerDatum::None,
                };
                (o.address.to_vec(), alonzo_assets(&o.amount), datum, None)
            }
            GenTransactionOutput::PostAlonzo(o) => {
                let datum = match o.datum_option.as_deref() {
                    None => LedgerDatum::None,
                    Some(conway::DatumOption::Hash(h)) => LedgerDatum::Hash(h.to_vec()),
                    Some(conway::DatumOption::Data(d)) => LedgerDatum::Inline {
                        hash: Hasher::<256>::hash(d.raw_cbor()).to_vec(),
                        cbor: d.raw_cbor().to_vec(),
                        data: Data::ledger(d),
                    },
                };
                let script = o.script_ref.as_ref().map(|s| Script::ledger(s, guards));
                (o.address.to_vec(), conway_assets(&o.value), datum, script)
            }
        };
        assets.sort();
        Ok(LedgerOutput {
            address,
            coin: Integer::of(coin.into()),
            assets,
            datum,
            script,
        })
    }
}

impl ServedOutput {
    /// The datum bytes the output states, none when it states no datum or no
    /// bytes.
    pub(super) fn datum_cbor(&self) -> Option<&[u8]> {
        self.datum.as_ref()?.original_cbor.as_deref()
    }
}

/// The fields of a served output that state other values than the ledger's
/// output bytes hold.
pub(super) fn differing(
    ledger: &[u8],
    served: &ServedOutput,
) -> Result<Vec<OutputField>, minicbor::decode::Error> {
    let ledger = LedgerOutput::decode(ledger, served.guards)?;
    let script = match (&ledger.script, &served.script) {
        (None, None) => true,
        (Some(ledger), Some(served)) => served.as_ref() == Some(ledger),
        (None, Some(_)) | (Some(_), None) => false,
    };
    let fields = [
        (OutputField::Address, served.address == ledger.address),
        (
            OutputField::Coin,
            served.coin.as_ref() == Some(&ledger.coin),
        ),
        (
            OutputField::Assets,
            served.assets.as_ref() == Some(&ledger.assets),
        ),
        (
            OutputField::Datum,
            ledger.datum.agrees(served.datum.as_ref()),
        ),
        (OutputField::Script, script),
    ];
    let differing = fields.into_iter().filter(|(_, same)| !same);
    Ok(differing.map(|(name, _)| name).collect())
}

/// Generates the conversion of one version's output message into a served
/// output.
macro_rules! served_output {
    (
        $version:ident,
        guards: $guards:literal,
        pubkey: $pubkey:ident,
        $(guard: $guard:ident,)?
        bytes: |$o:ident| $bytes:expr,
        datum_cbor: |$d:ident| $cbor:expr,
        quantity: |$a:ident| $quantity:expr $(,)?
    ) => {
        pub(super) mod $version {
            use super::{Data, Integer, Native, Script, ServedBytes, ServedDatum, ServedOutput};
            use crate::serve::grpc::$version::spec::cardano;

            /// The integer a served big integer states, none when the message states none.
            fn integer(value: &cardano::BigInt) -> Option<Integer> {
                use cardano::big_int::BigInt as B;
                match value.big_int.as_ref()? {
                    B::Int(i) => Some(Integer::of((*i).into())),
                    B::BigUInt(bytes) => Some(Integer::from_bytes(false, bytes)),
                    B::BigNInt(bytes) => Some(Integer::from_bytes(true, bytes)),
                }
            }

            /// The datum a served Plutus data message states, none when a message in it states none.
            fn data(value: &cardano::PlutusData) -> Option<Data> {
                use cardano::plutus_data::PlutusData as P;
                let all = |items: &[cardano::PlutusData]| -> Option<Vec<Data>> {
                    items.iter().map(data).collect()
                };
                Some(match value.plutus_data.as_ref()? {
                    P::Constr(c) => Data::Constr {
                        tag: c.tag.into(),
                        any_constructor: c.any_constructor,
                        fields: all(&c.fields)?,
                    },
                    P::Map(m) => {
                        let pair = |p: &cardano::PlutusDataPair| {
                            let key = data(p.key.as_ref()?)?;
                            Some((key, data(p.value.as_ref()?)?))
                        };
                        Data::Map(m.pairs.iter().map(pair).collect::<Option<_>>()?)
                    }
                    P::BigInt(i) => Data::Integer(integer(i)?),
                    P::BoundedBytes(b) => Data::Bytes(b.to_vec()),
                    P::Array(a) => Data::Array(all(&a.items)?),
                })
            }

            /// The native script a served message states, none when a message in it states none.
            fn native(script: &cardano::NativeScript) -> Option<Native> {
                use cardano::native_script::NativeScript as N;
                let all = |items: &[cardano::NativeScript]| -> Option<Vec<Native>> {
                    items.iter().map(native).collect()
                };
                let Some(clause) = script.native_script.as_ref() else {
                    return (!$guards).then_some(Native::Unset);
                };
                Some(match clause {
                    N::$pubkey(h) => Native::Pubkey(h.to_vec()),
                    N::ScriptAll(list) => Native::All(all(&list.items)?),
                    N::ScriptAny(list) => Native::Any(all(&list.items)?),
                    N::ScriptNOfK(n) => Native::NOfK(n.k.into(), all(&n.scripts)?),
                    N::InvalidBefore(slot) => Native::InvalidBefore(*slot),
                    N::InvalidHereafter(slot) => Native::InvalidHereafter(*slot),
                    $(N::$guard(c) => {
                        use cardano::stake_credential::StakeCredential as C;
                        use super::Credential;
                        Native::RequireGuard(match c.stake_credential.as_ref()? {
                            C::AddrKeyHash(h) => Credential::Key(h.to_vec()),
                            C::ScriptHash(h) => Credential::Script(h.to_vec()),
                        })
                    })?
                })
            }

            /// The script a served message states, none when a message in it states none.
            fn script(script: &cardano::Script) -> Option<Script> {
                use cardano::script::Script as S;
                Some(match script.script.as_ref()? {
                    S::Native(s) => Script::Native(native(s)?),
                    S::PlutusV1(b) => Script::Plutus(1, b.to_vec()),
                    S::PlutusV2(b) => Script::Plutus(2, b.to_vec()),
                    S::PlutusV3(b) => Script::Plutus(3, b.to_vec()),
                    S::PlutusV4(b) => Script::Plutus(4, b.to_vec()),
                })
            }

            /// The served output this version's output message states.
            pub(in super::super) fn output($o: &cardano::TxOutput) -> ServedOutput {
                let assets = $o.assets.iter().flat_map(|policy| {
                    policy.assets.iter().map(|$a| {
                        let amount = $quantity?;
                        Some((policy.policy_id.to_vec(), $a.name.to_vec(), amount))
                    })
                });
                let mut assets: Option<Vec<_>> = assets.collect();
                if let Some(assets) = &mut assets {
                    assets.sort();
                }
                let datum = $o.datum.as_ref().map(|$d| ServedDatum {
                    hash: $d.hash.to_vec(),
                    payload: $d.payload.as_ref().map(data),
                    original_cbor: $cbor,
                });
                ServedOutput {
                    guards: $guards,
                    bytes: $bytes,
                    address: $o.address.to_vec(),
                    coin: $o.coin.as_ref().and_then(integer),
                    assets,
                    datum,
                    script: $o.script.as_ref().map(script),
                }
            }
        }
    };
}

served_output!(
    v1,
    guards: true,
    pubkey: ScriptPubkeyHash,
    guard: ScriptRequireGuard,
    bytes: |o| ServedBytes::Stated(o.original_cbor.as_ref().map(|b| b.to_vec())),
    datum_cbor: |d| d.original_cbor.as_ref().map(|c| c.to_vec()),
    quantity: |a| integer(a.quantity.as_ref()?),
);

served_output!(
    v1beta,
    guards: false,
    pubkey: ScriptPubkeyHash,
    bytes: |o| ServedBytes::Stated(o.original_cbor.as_ref().map(|b| b.to_vec())),
    datum_cbor: |d| d.original_cbor.as_ref().map(|c| c.to_vec()),
    quantity: |a| integer(a.quantity.as_ref()?),
);

served_output!(
    v1alpha,
    guards: false,
    pubkey: ScriptPubkey,
    bytes: |o| ServedBytes::NoField,
    datum_cbor: |d| (!d.original_cbor.is_empty()).then(|| d.original_cbor.to_vec()),
    quantity: |a| match a.quantity.as_ref()? {
        cardano::asset::Quantity::OutputCoin(q) => integer(q),
        cardano::asset::Quantity::MintCoin(_) => None,
    },
);

/// A ledger context that resolves nothing.
#[cfg(test)]
#[derive(Clone)]
struct Unresolved;

#[cfg(test)]
impl pallas::interop::utxorpc::LedgerContext for Unresolved {
    fn get_utxos(
        &self,
        _: &[pallas::interop::utxorpc::TxoRef],
    ) -> Option<pallas::interop::utxorpc::UtxoMap> {
        None
    }

    fn get_slot_timestamp(&self, _: u64) -> Option<u64> {
        None
    }
}

/// The output each version of Dolos serves for Dijkstra output bytes.
#[cfg(test)]
macro_rules! mapped {
    ($version:ident, $bytes:expr) => {{
        use pallas::ledger::traverse::{Era, MultiEraOutput};
        let output = MultiEraOutput::decode(Era::Dijkstra, $bytes).unwrap();
        pallas::interop::utxorpc::$version::Mapper::new(Unresolved).map_tx_output(&output, None)
    }};
}

/// The output v1 of Dolos serves for Dijkstra output bytes.
#[cfg(test)]
pub(super) fn mapped(bytes: &[u8]) -> super::super::spec::cardano::TxOutput {
    mapped!(v1, bytes)
}

#[cfg(test)]
mod tests {
    use ledger_oracle_judge::{
        corpus,
        record::{CorpusKind, LedgerOutcome},
    };
    use std::collections::BTreeSet;

    use super::super::super::spec::cardano;
    use super::*;

    /// Every output the accepted records of both corpora leave in the ledger
    /// state.
    fn ledger_outputs() -> Vec<Vec<u8>> {
        let mut outputs = BTreeSet::new();
        for kind in [CorpusKind::Ic, CorpusKind::Supp] {
            let corpus = corpus::load(kind).unwrap_or_else(|e| panic!("{e}"));
            for run in corpus.kept() {
                for record in run.records() {
                    if let LedgerOutcome::Accepted { .. } = &record.outcome {
                        for entry in run.post(record) {
                            if !outputs.contains(&entry.output.0) {
                                outputs.insert(entry.output.0.clone());
                            }
                        }
                    }
                }
            }
        }
        outputs.into_iter().collect()
    }

    fn bigint(value: i64) -> Option<cardano::BigInt> {
        let value = cardano::big_int::BigInt::Int(value);
        Some(cardano::BigInt {
            big_int: Some(value),
        })
    }

    /// The CBOR byte string of fewer than 256 bytes.
    fn cbor_bytes(bytes: &[u8]) -> Vec<u8> {
        let head = match bytes.len() {
            n @ 0..24 => vec![0x40 | n as u8],
            n => vec![0x58, u8::try_from(n).unwrap()],
        };
        [head, bytes.to_vec()].concat()
    }

    /// An output with assets, an inline datum and a native reference script
    /// with a guard clause, and one with a datum hash and a Plutus reference
    /// script.
    fn constructed() -> [Vec<u8>; 2] {
        let address = cbor_bytes(&[[0x61].as_slice(), &[0x11; 28]].concat());
        let policy = cbor_bytes(&[0x22; 28]);
        let value = [
            &[0x82, 0x1a, 0x00, 0x0f, 0x42, 0x40, 0xa1],
            policy.as_slice(),
        ]
        .concat();
        let value = [value, vec![0xa1, 0x43, b'a', b'b', b'c', 0x18, 0x2a]].concat();
        let datum = [0xd8, 0x79, 0x9f, 0x05, 0x41, 0xab, 0xa1, 0x01, 0x22, 0xff];
        let inline = [vec![0x82, 0x01, 0xd8, 0x18], cbor_bytes(&datum)].concat();
        let pubkey = [vec![0x82, 0x00], cbor_bytes(&[0x33; 28])].concat();
        let guard = [vec![0x82, 0x06, 0x82, 0x01], cbor_bytes(&[0x44; 28])].concat();
        let n_of_k = [vec![0x83, 0x03, 0x01, 0x81], guard].concat();
        let native = [
            vec![0x82, 0x01, 0x83],
            pubkey,
            vec![0x82, 0x04, 0x05],
            n_of_k,
        ]
        .concat();
        let native = [
            vec![0xd8, 0x18],
            cbor_bytes(&[[0x82, 0x00].as_slice(), &native].concat()),
        ];
        let first = [
            vec![0xa4, 0x00],
            address.clone(),
            vec![0x01],
            value,
            vec![0x02],
            inline,
            vec![0x03],
            native.concat(),
        ];
        let hash = [vec![0x82, 0x00], cbor_bytes(&[0x55; 32])].concat();
        let plutus = [
            vec![0x82, 0x03],
            cbor_bytes(&[0x01, 0x00, 0x00, 0x32, 0x22]),
        ]
        .concat();
        let plutus = [vec![0xd8, 0x18], cbor_bytes(&plutus)].concat();
        let second = [
            vec![0xa4, 0x00],
            address,
            vec![0x01, 0x1a, 0x00, 0x0f, 0x42, 0x40, 0x02],
            hash,
            vec![0x03],
            plutus,
        ];
        [first.concat(), second.concat()]
    }

    #[test]
    fn the_mapped_form_of_every_corpus_output_agrees_with_its_bytes_at_every_version() {
        let outputs = ledger_outputs();
        assert!(!outputs.is_empty());
        for bytes in &outputs {
            let served = [
                v1::output(&mapped!(v1, bytes)),
                v1beta::output(&mapped!(v1beta, bytes)),
                v1alpha::output(&mapped!(v1alpha, bytes)),
            ];
            for served in &served {
                let differing = differing(bytes, served).unwrap();
                assert!(differing.is_empty(), "{} {differing:?}", hex::encode(bytes));
            }
        }
    }

    #[test]
    fn a_served_output_with_one_changed_field_names_that_field() {
        let [first, second] = constructed();
        for bytes in [&first, &second] {
            let differing = differing(bytes, &v1::output(&mapped(bytes))).unwrap();
            assert!(differing.is_empty(), "{} {differing:?}", hex::encode(bytes));
        }
        type Change = fn(&mut cardano::TxOutput);
        use OutputField as F;
        let changes: [(&[u8], OutputField, Change); 11] = [
            (&first, F::Address, |o| o.address = vec![0x61; 29].into()),
            (&first, F::Coin, |o| o.coin = None),
            (&first, F::Coin, |o| o.coin = bigint(1)),
            (&first, F::Assets, |o| o.assets.clear()),
            (&first, F::Assets, |o| {
                o.assets[0].assets[0].quantity = bigint(987_654_321_987)
            }),
            (&first, F::Datum, |o| {
                o.datum.as_mut().unwrap().payload = None
            }),
            (&first, F::Datum, |o| o.datum = None),
            (&first, F::Script, |o| o.script = None),
            (&second, F::Datum, |o| {
                o.datum.as_mut().unwrap().hash = vec![0; 32].into()
            }),
            (&second, F::Datum, |o| {
                o.datum.as_mut().unwrap().original_cbor = Some(vec![0x05].into())
            }),
            (&second, F::Script, |o| o.script = None),
        ];
        for (bytes, field, change) in changes {
            let mut served = mapped(bytes);
            change(&mut served);
            assert_eq!(
                differing(bytes, &v1::output(&served)).unwrap(),
                [field],
                "{served:?}"
            );
        }
    }

    #[test]
    fn a_guard_clause_agrees_unset_only_at_a_version_without_one() {
        let [first, _] = constructed();
        let at_v1beta = v1beta::output(&mapped!(v1beta, &first));
        let at_v1alpha = v1alpha::output(&mapped!(v1alpha, &first));
        assert!(differing(&first, &at_v1beta).unwrap().is_empty());
        assert!(differing(&first, &at_v1alpha).unwrap().is_empty());
        let mut unset = mapped(&first);
        let Some(cardano::script::Script::Native(native)) =
            unset.script.as_mut().and_then(|s| s.script.as_mut())
        else {
            panic!("the first output carries a native script");
        };
        let Some(cardano::native_script::NativeScript::ScriptAll(all)) =
            native.native_script.as_mut()
        else {
            panic!("the native script is a list of all");
        };
        let Some(cardano::native_script::NativeScript::ScriptNOfK(n_of_k)) =
            all.items[2].native_script.as_mut()
        else {
            panic!("the third clause is an n of k");
        };
        n_of_k.scripts[0].native_script = None;
        assert_eq!(
            differing(&first, &v1::output(&unset)).unwrap(),
            [OutputField::Script]
        );
    }

    #[test]
    fn a_mint_quantity_where_an_output_quantity_belongs_differs_at_v1alpha() {
        use pallas::interop::utxorpc::v1alpha::spec::cardano as alpha;
        let [first, _] = constructed();
        let mut served = mapped!(v1alpha, &first);
        assert!(differing(&first, &v1alpha::output(&served))
            .unwrap()
            .is_empty());
        let quantity = served.assets[0].assets[0].quantity.take();
        let Some(alpha::asset::Quantity::OutputCoin(q)) = quantity else {
            panic!("an output asset states an output quantity");
        };
        served.assets[0].assets[0].quantity = Some(alpha::asset::Quantity::MintCoin(q));
        assert_eq!(
            differing(&first, &v1alpha::output(&served)).unwrap(),
            [OutputField::Assets]
        );
    }
}
