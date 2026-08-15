#![no_main]

use gsym::{Endian, Gsym, GsymVersion, TranscodeOptions};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(gsym) = Gsym::parse(data) {
        let header = gsym.header();
        let _build_id: &[u8] = gsym.build_id();
        drop(gsym.string(0));
        for index in 0..usize::min(header.address_count as usize, 64) {
            if let Ok(function) = gsym.function(index) {
                let _name = function.name();
                let _range = function.range();
                drop(function.decode());
            }
        }
        for index in 0..usize::min(header.address_count as usize, 64) {
            drop(gsym.file(u32::try_from(index).unwrap_or(u32::MAX)));
        }
        if gsym.verify().is_ok() {
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
