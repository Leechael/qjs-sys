use alloc::vec::Vec;
use qjs::{AsBytes, JsUint8Array};
pub use sha3::{Digest, Sha3_256, Sha3_512};

#[qjs::host_call]
pub fn sha3_256(data: JsUint8Array) -> AsBytes<Vec<u8>> {
    let mut hasher = Sha3_256::new();
    hasher.update(data.as_bytes());
    AsBytes(hasher.finalize().to_vec())
}

#[qjs::host_call]
pub fn sha3_512(data: JsUint8Array) -> AsBytes<Vec<u8>> {
    let mut hasher = Sha3_512::new();
    hasher.update(data.as_bytes());
    AsBytes(hasher.finalize().to_vec())
}
