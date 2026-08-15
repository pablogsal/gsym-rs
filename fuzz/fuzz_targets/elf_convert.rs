#![no_main]

use gsym::Gsym;
use gsym::convert::{
    ConversionOptions, DiscoveryPolicy, DwarfImportOptions, ElfConverter, ElfInputs,
};
use libfuzzer_sys::fuzz_target;

const MAX_EXPANDED_DEBUG_DATA: u64 = 4 * 1024 * 1024;
const ENVELOPE_MAGIC: &[u8; 8] = b"GSYMELF\0";

fuzz_target!(|data: &[u8]| {
    let (inputs, control) = decode_inputs(data);
    let mut options = ConversionOptions {
        discovery: DiscoveryPolicy::Disabled,
        debug_directories: Vec::new(),
        debuginfod_urls: Vec::new(),
        debuginfod_max_download_size: MAX_EXPANDED_DEBUG_DATA,
        gnu_debugdata_max_decompressed_size: MAX_EXPANDED_DEBUG_DATA,
        ..ConversionOptions::default()
    };
    match control % 3 {
        0 => {}
        1 => options.dwarf = None,
        _ => {
            options.include_symbols = false;
            options.dwarf = Some(DwarfImportOptions {
                inline_info: true,
                call_sites: true,
            });
        }
    }

    let converter = ElfConverter::new(options);
    if let Ok(report) = converter.convert(inputs) {
        for warning in &report.warnings {
            drop(warning.to_string());
        }
        if let Ok(bytes) = report.builder.to_bytes() {
            Gsym::parse(bytes)
                .and_then(|gsym| gsym.verify())
                .expect("converted GSYM must verify");
        }
    }
});

fn decode_inputs(data: &[u8]) -> (ElfInputs<'_>, u8) {
    let Some(header) = data.strip_prefix(ENVELOPE_MAGIC) else {
        return (ElfInputs::new(data), data.last().copied().unwrap_or(0));
    };
    let Some((&control, header)) = header.split_first() else {
        return (ElfInputs::new(data), 0);
    };
    let Some(length_bytes) = header.get(..20) else {
        return (ElfInputs::new(data), control);
    };
    let mut lengths = [0_usize; 5];
    for (length, bytes) in lengths.iter_mut().zip(length_bytes.chunks_exact(4)) {
        *length = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    }
    let mut payload = &header[20..];
    let Some(image) = take(&mut payload, lengths[0]) else {
        return (ElfInputs::new(data), control);
    };
    let mut inputs = ElfInputs::new(image);
    let mut parts = [None; 4];
    for (part, length) in parts.iter_mut().zip(lengths[1..].iter().copied()) {
        if length != 0 {
            *part = take(&mut payload, length);
            if part.is_none() {
                return (ElfInputs::new(data), control);
            }
        }
    }
    if let Some(debug) = parts[0] {
        inputs = inputs.with_debug(debug);
    }
    if let Some(symbols) = parts[1] {
        inputs = inputs.with_symbols(symbols);
    }
    if let Some(supplementary) = parts[2] {
        inputs = inputs.with_supplementary(supplementary);
    }
    if let Some(dwp) = parts[3] {
        inputs = inputs.with_dwp(dwp);
    }
    (inputs, control)
}

const fn take<'data>(input: &mut &'data [u8], length: usize) -> Option<&'data [u8]> {
    if length > input.len() {
        return None;
    }
    let (value, remaining) = input.split_at(length);
    *input = remaining;
    Some(value)
}
