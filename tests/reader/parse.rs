//! Parsing well-formed images and the storage the reader hands back.

use gsym::{Error, Gsym};

use crate::bytes::ByteOrder;
use crate::minimal::{minimal_v1, minimal_v2};
use crate::name_at;

#[test]
fn reader_preserves_borrowed_or_owned_storage() {
    let bytes = minimal_v1(ByteOrder::Little);
    let borrowed = Gsym::parse(bytes.as_slice()).unwrap();
    assert_eq!(borrowed.as_ref().as_ptr(), bytes.as_ptr());

    let owned = Gsym::parse(bytes.clone()).unwrap();
    assert_eq!(name_at(&owned, 0x1000), b"alpha");
    assert_eq!(owned.into_inner(), bytes);
}

#[test]
fn safely_opens_an_owned_file_snapshot() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), minimal_v1(ByteOrder::Little)).unwrap();

    let gsym = Gsym::open(file.path()).unwrap();
    assert_eq!(name_at(&gsym, 0x1000), b"alpha");
}

#[test]
fn reader_debug_is_bounded_for_owned_storage() {
    let gsym = Gsym::parse(minimal_v1(ByteOrder::Little)).unwrap();
    assert!(format!("{gsym:?}").len() < 256);
}

#[test]
fn reads_independent_v1_fixtures_in_both_byte_orders() {
    for order in [ByteOrder::Little, ByteOrder::Big] {
        let bytes = minimal_v1(order);
        let gsym = Gsym::parse(&bytes).unwrap();

        assert_eq!(gsym.build_id(), &[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(gsym.functions().count(), 2);
        let first = gsym.function(0).unwrap();
        assert_eq!(first.index(), 0);
        assert_eq!(first.name(), b"alpha");
        assert_eq!(first.range(), gsym::AddressRange::new(0x1000, 0x1010));
        assert!(gsym.get_function(2).unwrap().is_none());
        assert!(matches!(
            gsym.function(2),
            Err(Error::FunctionIndexOutOfBounds { index: 2, count: 2 })
        ));
        assert_eq!(name_at(&gsym, 0x1000), b"alpha");
        assert_eq!(name_at(&gsym, 0x100f), b"alpha");
        assert!(gsym.lookup(0x1010).unwrap().is_none());
        assert_eq!(name_at(&gsym, 0x1020), b"beta");
        gsym.verify().unwrap();
    }
}

#[test]
fn reads_independent_v2_fixtures_in_both_byte_orders() {
    for order in [ByteOrder::Little, ByteOrder::Big] {
        let bytes = minimal_v2(order);
        let gsym = Gsym::parse(&bytes).unwrap();

        assert!(gsym.build_id().is_empty());
        assert_eq!(gsym.functions().count(), 2);
        assert_eq!(name_at(&gsym, 0x1000), b"alpha");
        assert_eq!(name_at(&gsym, 0x1020), b"beta");
        gsym.verify().unwrap();
    }
}
