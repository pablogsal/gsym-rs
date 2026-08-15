#![no_main]

use gsym::{
    AddressRange, CallSite, CallSiteFlags, Endian, FileEntry, FrameLookupOptions, Function, Gsym,
    GsymBuilder, GsymVersion, InlineNode, LineEntry, LookupOptions, LookupScratch,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let control = data.first().copied().unwrap_or(0);
    let version = if control & 1 == 0 {
        GsymVersion::V1
    } else {
        GsymVersion::V2
    };
    let endian = if control & 2 == 0 {
        Endian::Little
    } else {
        Endian::Big
    };
    let mut bytes = fixture(version, endian);
    mutate(&mut bytes, data.get(1..).unwrap_or_default(), control >> 2);

    let Ok(gsym) = Gsym::parse(bytes) else {
        return;
    };
    drop(gsym.verify());
    drop(gsym.decode_all());
    let mut scratch = LookupScratch::with_capacity(16);
    for function in gsym.functions().take(32) {
        let Ok(function) = function else {
            continue;
        };
        drop(function.decode());
        for address in [
            function.start(),
            function.start() + function.range().size() / 2,
            function.range().end,
        ] {
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
            drop(gsym.for_each_frame(address, FrameLookupOptions::default(), &mut scratch, |_| {}));
        }
    }
});

fn mutate(bytes: &mut Vec<u8>, data: &[u8], mode: u8) {
    match mode % 4 {
        0 => {
            for chunk in data.chunks(3).take(64) {
                let offset = usize::from(chunk.first().copied().unwrap_or(0))
                    | usize::from(chunk.get(1).copied().unwrap_or(0)) << 8;
                if !bytes.is_empty() {
                    let index = offset % bytes.len();
                    bytes[index] ^= chunk.get(2).copied().unwrap_or(0xff);
                }
            }
        }
        1 => {
            if !bytes.is_empty() {
                let requested = data
                    .get(..2)
                    .and_then(|raw| raw.try_into().ok())
                    .map(u16::from_le_bytes)
                    .map_or(0, usize::from);
                bytes.truncate(requested % bytes.len());
            }
        }
        2 => bytes.extend(data.iter().take(4096)),
        _ => {
            for (destination, source) in bytes.iter_mut().zip(data).take(4096) {
                *destination = *source;
            }
        }
    }
}

fn fixture(version: GsymVersion, endian: Endian) -> Vec<u8> {
    let mut builder = GsymBuilder::new()
        .version(version)
        .endian(endian)
        .base_address(0x4000)
        .build_id([0x12, 0x34, 0x56, 0x78]);
    let file = builder
        .add_file(FileEntry::new(b"src", b"fixture.rs"))
        .unwrap();
    let range = AddressRange::new(0x4010, 0x4090);
    builder
        .add_function(Function {
            lines: vec![
                LineEntry::new(0x4010, file, 10),
                LineEntry::new(0x4040, file, 20),
            ],
            inline: Some(InlineNode {
                ranges: vec![AddressRange::new(0x4020, 0x4080)],
                name: b"inline_outer".to_vec(),
                call_file: file,
                call_line: 9,
                children: vec![InlineNode {
                    ranges: vec![AddressRange::new(0x4030, 0x4050)],
                    name: b"inline_inner".to_vec(),
                    call_file: file,
                    call_line: 19,
                    children: Vec::new(),
                }],
            }),
            merged: vec![Function::new(range, b"merged_alias")],
            call_sites: vec![CallSite {
                return_offset: 0x30,
                flags: CallSiteFlags::INTERNAL | CallSiteFlags::EXTERNAL,
                match_regex: vec![b"callee.*".to_vec()],
            }],
            ..Function::new(range, b"fixture")
        })
        .unwrap();
    builder
        .add_function(Function::new(AddressRange::new(0x4100, 0x4120), b"second"))
        .unwrap();
    builder.to_bytes().unwrap()
}
