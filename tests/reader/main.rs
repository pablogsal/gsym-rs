//! Reader suite: parsing, address lookup, malformed input and memory mapping.
//!
//! Every case reads a hand-written fixture rather than something `GsymBuilder`
//! produced, so a bug the writer and the reader share cannot survive by round
//! tripping through both. See `tests/writer` for the other direction.

#[path = "../common/bytes.rs"]
mod bytes;
#[path = "../common/fixture.rs"]
mod fixture;
#[path = "../common/leb.rs"]
mod leb;
#[path = "../common/minimal.rs"]
mod minimal;

mod lookup;
mod malformed;
mod parse;

#[cfg(feature = "mmap")]
mod mmap;

/// The innermost frame name the reader resolves at `address`.
///
/// Panics when nothing covers `address`; use `lookup` directly to assert that a
/// gap really is a gap.
fn name_at<D: AsRef<[u8]>>(gsym: &gsym::Gsym<D>, address: u64) -> &[u8] {
    gsym.lookup(address)
        .unwrap()
        .expect("address is covered by a function")
        .frames[0]
        .name
}
