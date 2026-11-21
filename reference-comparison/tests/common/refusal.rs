//! A refusal by the implementation, judged by its reading of the value the
//! recorded change was made to.

use std::panic::AssertUnwindSafe;

use ledger_oracle_judge::comparison::catch;

/// The implementation's reading of the value a recorded change was made to.
#[derive(Debug, PartialEq)]
pub enum Base {
    Reads,
    Refuses(String),
    Unrecorded,
}

/// A refusal by the implementation, with its reading of the value the change
/// was made to.
#[derive(Debug, PartialEq)]
pub struct Refusal {
    pub error: String,
    pub base: Base,
}

/// The reading `read` gives of the recorded base `bytes`.
pub fn base(bytes: Option<&[u8]>, read: impl Fn(&[u8]) -> Result<(), String>) -> Base {
    match bytes {
        None => Base::Unrecorded,
        Some(bytes) => match catch(AssertUnwindSafe(|| read(bytes))) {
            Ok(Ok(())) => Base::Reads,
            Ok(Err(e)) | Err(e) => Base::Refuses(e),
        },
    }
}

/// Whether the implementation refuses for the reference's reason, which holds
/// when it reads the value the change was made to.
pub fn same_reason(_: &str, refusal: &Refusal) -> bool {
    refusal.base == Base::Reads
}
