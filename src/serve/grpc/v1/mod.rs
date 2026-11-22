pub mod query;
pub mod submit;
pub mod sync;
pub mod watch;

#[cfg(test)]
mod ledger_oracle_tests;

pub(super) use pallas::interop::utxorpc::v1::spec;

/// The value an optional pattern field constrains a match to, none when it is
/// unset or empty.
fn criterion(field: &Option<bytes::Bytes>) -> Option<&bytes::Bytes> {
    field.as_ref().filter(|x| !x.is_empty())
}
