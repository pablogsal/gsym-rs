//! Writer suite: the path from the public model down to encoded bytes.
//!
//! `model` covers the invariants of the types the builder is fed, `builder` what
//! it accepts and rejects, and `finalize` the repairs applied on the way to
//! bytes. `roundtrip` builds fresh images and reads them back; `transcode`
//! re-encodes images that already exist. Reader-side parsing of images the crate
//! did *not* write lives in `tests/reader`.

#[path = "../common/bytes.rs"]
mod bytes;
#[path = "../common/minimal.rs"]
mod minimal;

mod builder;
mod finalize;
mod model;
mod roundtrip;
mod transcode;
