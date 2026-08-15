#![no_main]

use gsym::{FrameLookupOptions, Gsym, LookupOptions, LookupScratch};
use libfuzzer_sys::fuzz_target;

const MAX_FUNCTIONS: usize = 64;

fuzz_target!(|data: &[u8]| {
    let Ok(gsym) = Gsym::parse(data) else {
        return;
    };
    let mut addresses = Vec::new();
    for function in gsym.functions().take(MAX_FUNCTIONS) {
        let Ok(function) = function else {
            continue;
        };
        let range = function.range();
        addresses.extend([
            range.start.saturating_sub(1),
            range.start,
            range.start + range.size() / 2,
            range.end.saturating_sub(1),
            range.end,
        ]);
        drop(function.decode());
    }
    addresses.sort_unstable();
    addresses.dedup();

    let mut scratch = LookupScratch::with_capacity(16);
    for address in addresses {
        for mask in 0_u8..8 {
            drop(gsym.lookup_with_options(
                address,
                LookupOptions {
                    line_information: mask & 1 != 0,
                    inline_frames: mask & 2 != 0,
                    call_sites: mask & 4 != 0,
                },
                &mut scratch,
            ));
        }

        let options = FrameLookupOptions::default();
        let expected = gsym.lookup_with_options(address, options.into(), &mut scratch);
        let mut visited = Vec::new();
        let actual = gsym.for_each_frame(address, options, &mut scratch, |frame| {
            visited.push(frame);
        });
        match (expected, actual) {
            (Ok(expected), Ok(found)) => {
                assert_eq!(found, expected.is_some());
                if let Some(expected) = expected {
                    assert_eq!(visited.as_slice(), expected.frames.as_ref());
                }
            }
            (Err(_), Err(_)) => {}
            (expected, actual) => {
                panic!("lookup and visitor disagree: lookup={expected:?}, visitor={actual:?}")
            }
        }
    }
});
