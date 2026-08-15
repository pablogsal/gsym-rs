//! Semantics of the public model types the builder is fed with.

use gsym::{AddressRange, CallSiteFlags};
use proptest::prelude::*;

#[test]
fn call_site_flags_compose_without_discarding_unknown_bits() {
    let unknown = CallSiteFlags::from_bits_retain(1 << 7);
    let mut flags = CallSiteFlags::INTERNAL | unknown;
    flags |= CallSiteFlags::EXTERNAL;

    assert!(!flags.is_empty());
    assert!(flags.contains(CallSiteFlags::INTERNAL));
    assert!(flags.contains(CallSiteFlags::EXTERNAL));
    assert_eq!(flags.bits(), 0b1000_0011);
    assert!(CallSiteFlags::from_bits_retain(0).is_empty());
}

#[test]
fn reversed_ranges_never_contain_addresses_or_ranges() {
    let reversed = AddressRange::new(20, 10);
    assert!(!reversed.is_valid());
    assert!(!reversed.is_empty());
    assert!(!reversed.contains(15));
    assert!(!reversed.contains_range(AddressRange::new(12, 14)));
}

proptest! {
    #[test]
    fn half_open_ranges_match_integer_ordering(start in any::<u64>(), width in 0_u16..4096) {
        let end = start.saturating_add(u64::from(width));
        let range = AddressRange::new(start, end);

        prop_assert_eq!(range.size(), end - start);
        prop_assert_eq!(range.is_empty(), start == end);
        if start != end {
            prop_assert!(range.contains(start));
            prop_assert!(range.contains(end - 1));
            prop_assert!(!range.contains(end));
        }
    }
}
