//! The memory-mapped reader, which borrows its bytes straight from the file.

#![expect(
    unsafe_code,
    reason = "these tests mutate a mapped file behind the reader to prove the safety contract"
)]

use std::fs::File;
use std::io::Write as _;

use gsym::MappedGsym;

use crate::bytes::ByteOrder;
use crate::minimal::minimal_v1;
use crate::name_at;

#[test]
fn mapped_reader_looks_up_borrowed_data() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&minimal_v1(ByteOrder::Little)).unwrap();

    // SAFETY: this test owns the temporary file and never writes to it again.
    let mapped = unsafe { MappedGsym::map(file.path()) }.unwrap();
    assert_eq!(name_at(&mapped, 0x1000), b"alpha");
    assert_eq!(mapped.functions().count(), 2);

    let handle = File::open(file.path()).unwrap();
    // SAFETY: as above, nothing modifies the file while it stays mapped.
    let mapped = unsafe { MappedGsym::map_file(&handle) }.unwrap();
    assert_eq!(name_at(&mapped, 0x1020), b"beta");
    mapped.verify().unwrap();
}
