#![no_main]

use gsym::{Endian, Gsym, GsymVersion, TranscodeOptions};
use libfuzzer_sys::fuzz_target;

/// Upper bound on the number of table entries a single case walks.
const MAX_ENTRIES: usize = 64;

/// Number of file entries probed when the file table cannot be verified.
const UNVERIFIED_FILES: usize = 8;

fuzz_target!(|data: &[u8]| {
    if let Ok(gsym) = Gsym::parse(data) {
        let header = gsym.header();
        let _build_id: &[u8] = gsym.build_id();
        drop(gsym.string(0));
        for index in 0..usize::min(header.address_count as usize, MAX_ENTRIES) {
            if let Ok(function) = gsym.function(index) {
                let _name = function.name();
                let _range = function.range();
                drop(function.decode());
            }
        }
        let verified = gsym.verify();
        let files = verified
            .as_ref()
            .map_or(UNVERIFIED_FILES, |report| report.files);
        for index in 0..files.saturating_add(1).min(MAX_ENTRIES) {
            drop(gsym.file(u32::try_from(index).unwrap_or(u32::MAX)));
        }
        drop(gsym.file(u32::MAX));
        if verified.is_ok() {
            drop(gsym.decode_all());
            drop(gsym.transcode(TranscodeOptions {
                version: Some(if header.version == GsymVersion::V1 {
                    GsymVersion::V2
                } else {
                    GsymVersion::V1
                }),
                endian: Some(match header.endian {
                    Endian::Little => Endian::Big,
                    Endian::Big => Endian::Little,
                }),
            }));
        }
    }
});
