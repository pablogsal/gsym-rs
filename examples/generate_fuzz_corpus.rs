//! Writes a seed corpus for the `fuzz/` targets.

use std::path::{Path, PathBuf};

use gsym::{
    AddressRange, CallSite, CallSiteFlags, Endian, FileEntry, Function, GsymBuilder, GsymVersion,
    InlineNode, LineEntry,
};

fn main() -> gsym::Result<()> {
    let root = std::env::args_os()
        .nth(1)
        .map_or_else(|| PathBuf::from("fuzz/seeds"), PathBuf::from);
    for target in ["parse", "lookup"] {
        std::fs::create_dir_all(root.join(target)).map_err(gsym::Error::from)?;
    }
    for version in [GsymVersion::V1, GsymVersion::V2] {
        for endian in [Endian::Little, Endian::Big] {
            let bytes = fixture(version, endian)?;
            let name = format!(
                "{}-{}.gsym",
                match version {
                    GsymVersion::V1 => "v1",
                    GsymVersion::V2 => "v2",
                },
                match endian {
                    Endian::Little => "little",
                    Endian::Big => "big",
                }
            );
            for target in ["parse", "lookup"] {
                write(&root.join(target).join(&name), &bytes)?;
            }
        }
    }
    Ok(())
}

fn fixture(version: GsymVersion, endian: Endian) -> gsym::Result<Vec<u8>> {
    let mut builder = GsymBuilder::new()
        .version(version)
        .endian(endian)
        .base_address(0x4000)
        .build_id([0x12, 0x34, 0x56, 0x78]);
    let source = builder.add_file(FileEntry::new(b"src", b"fixture.rs"))?;
    let range = AddressRange::new(0x4010, 0x4090);
    builder.add_function(Function {
        lines: vec![
            LineEntry::new(0x4010, source, 10),
            LineEntry::new(0x4040, source, 20),
        ],
        inline: Some(InlineNode {
            ranges: vec![AddressRange::new(0x4020, 0x4080)],
            name: b"inline_outer".to_vec(),
            call_file: source,
            call_line: 9,
            children: vec![InlineNode {
                ranges: vec![AddressRange::new(0x4030, 0x4050)],
                name: b"inline_inner".to_vec(),
                call_file: source,
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
    })?;
    builder.add_function(Function::new(AddressRange::new(0x4100, 0x4120), b"second"))?;
    builder.to_bytes()
}

fn write(path: &Path, bytes: &[u8]) -> gsym::Result<()> {
    std::fs::write(path, bytes).map_err(|source| gsym::Error::IoAtPath {
        operation: "write fuzz corpus seed",
        path: path.to_path_buf(),
        source,
    })
}
