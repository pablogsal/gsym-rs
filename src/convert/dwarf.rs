use std::path::Path;

use gimli::{
    AbbreviationsCacheStrategy, AttributeValue, DebuggingInformationEntry, Dwarf,
    DwarfPackageSections, DwarfSections, EndianSlice, Reader, RelocateReader, RunTimeEndian, Unit,
};
use object::Object;

mod dies;
mod lines;
mod references;
mod sections;

use dies::{DetailOptions, extract_subprogram_details};
#[cfg(test)]
use lines::{LineSequenceRange, SequencedLine, scan_line_sequence_offsets};
use lines::{UnitLines, collect_lines, statement_sequence_offset};
#[cfg(test)]
use object::{RelocationEncoding, RelocationKind};
use references::{
    DwarfResolver, absolute_entry_offset, attribute_bytes, resolve_declaration_line, resolve_name,
};
use sections::{
    DwarfRelocations, SectionData, load_dwo_section, load_dwo_section_owned, load_section,
};
#[cfg(test)]
use sections::{RelocationEntry, calculate_relocation_value, debug_relocation_encoding_supported};

use super::ConversionWarning;
use super::elf::{AddressLayout, ConversionStats};
#[cfg(test)]
use crate::model::LineEntry;
use crate::model::{AddressRange, Function};
use crate::{ElfInputKind, Error, GsymBuilder, Result};

const DW_AT_LLVM_STMT_SEQUENCE: gimli::DwAt = gimli::DwAt(0x3e0c);

type SectionReader<'data, 'relocations> =
    RelocateReader<EndianSlice<'data, RunTimeEndian>, &'relocations DwarfRelocations>;

fn runtime_endian(file: &object::File<'_>) -> RunTimeEndian {
    if file.is_little_endian() {
        RunTimeEndian::Little
    } else {
        RunTimeEndian::Big
    }
}

fn borrow_dwarf<'a>(
    sections: &'a DwarfSections<SectionData<'_>>,
    endian: RunTimeEndian,
) -> Dwarf<SectionReader<'a, 'a>> {
    let mut dwarf = sections.borrow(|section| {
        RelocateReader::new(
            EndianSlice::new(section.data.as_ref(), endian),
            &section.relocations,
        )
    });
    // References can revisit even a unit with its own abbreviation table.
    // Gimli's cache lookups do not populate the cache on a miss.
    dwarf.populate_abbreviations_cache(AbbreviationsCacheStrategy::All);
    dwarf
}

fn unsigned_attribute<R: Reader<Offset = usize>>(
    entry: &DebuggingInformationEntry<R>,
    name: gimli::DwAt,
) -> Option<u64> {
    entry.attr_value(name).and_then(|value| value.udata_value())
}

fn file_index_attribute<R: Reader<Offset = usize>>(
    entry: &DebuggingInformationEntry<R>,
    name: gimli::DwAt,
) -> Option<u64> {
    match entry.attr_value(name) {
        Some(AttributeValue::FileIndex(index)) => Some(index),
        Some(value) => value.udata_value(),
        None => None,
    }
}

struct ImportContext<'a> {
    executable_ranges: &'a [AddressRange],
    builder: &'a mut GsymBuilder,
    include_inlines: bool,
    include_call_sites: bool,
    stats: &'a mut ConversionStats,
    warnings: &'a mut Vec<ConversionWarning>,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum DwoResolver<'path> {
    Disabled,
    Filesystem { base: &'path Path },
}

enum LooseDwoImport {
    Imported,
    Unavailable,
    Failed(Box<str>),
}

pub(super) struct DwarfImport<'files, 'data> {
    pub(super) file: &'files object::File<'data>,
    pub(super) supplementary: Option<&'files object::File<'data>>,
    pub(super) dwp: Option<&'files object::File<'data>>,
    pub(super) dwo_resolver: DwoResolver<'files>,
    pub(super) layout: &'files AddressLayout,
    pub(super) executable_ranges: &'files [AddressRange],
    pub(super) builder: &'files mut GsymBuilder,
    pub(super) include_inlines: bool,
    pub(super) include_call_sites: bool,
    pub(super) stats: &'files mut ConversionStats,
    pub(super) warnings: &'files mut Vec<ConversionWarning>,
}

pub(super) fn import_dwarf(request: DwarfImport<'_, '_>) -> Result<()> {
    let DwarfImport {
        file,
        supplementary,
        dwp,
        dwo_resolver,
        layout,
        executable_ranges,
        builder,
        include_inlines,
        include_call_sites,
        stats,
        warnings,
    } = request;
    let endian = runtime_endian(file);
    let sections = DwarfSections::load(|id| load_section(file, id, layout))?;
    let supplementary_sections = supplementary
        .map(|file| DwarfSections::load(|id| load_section(file, id, layout)))
        .transpose()?;
    let mut dwarf = borrow_dwarf(&sections, endian);
    if let Some(sections) = &supplementary_sections {
        dwarf.set_sup(borrow_dwarf(sections, endian));
    }
    let package_endian = dwp.map(runtime_endian);
    let package_sections = dwp
        .map(|file| DwarfPackageSections::load(|id| load_dwo_section(file, id, layout)))
        .transpose()?;
    let empty_relocations = DwarfRelocations::default();
    let dwp_package = package_sections
        .as_ref()
        .zip(package_endian)
        .map(|(sections, endian)| {
            sections.borrow(
                |section| {
                    RelocateReader::new(
                        EndianSlice::new(section.data.as_ref(), endian),
                        &section.relocations,
                    )
                },
                RelocateReader::new(EndianSlice::new(&[], endian), &empty_relocations),
            )
        })
        .transpose()
        .map_err(gimli_error)?;

    let mut context = ImportContext {
        executable_ranges,
        builder,
        include_inlines,
        include_call_sites,
        stats,
        warnings,
    };
    let mut headers = dwarf.units();
    while let Some(header) = headers.next().map_err(gimli_error)? {
        let unit = dwarf.unit(header).map_err(gimli_error)?;
        if let Some(dwo_id) = unit.dwo_id {
            let skeleton_lines = collect_lines(&dwarf, &unit, context.builder, context.warnings)?;
            let mut failures = Vec::new();
            if let DwoResolver::Filesystem { base } = dwo_resolver {
                match try_import_loose_dwo(
                    &dwarf,
                    &unit,
                    dwo_id,
                    base,
                    layout,
                    &skeleton_lines,
                    &mut context,
                )? {
                    LooseDwoImport::Imported => continue,
                    LooseDwoImport::Unavailable => {}
                    LooseDwoImport::Failed(reason) => failures.push(reason),
                }
            }
            if let Some(package) = &dwp_package {
                match package.find_cu(dwo_id, &dwarf).map_err(gimli_error) {
                    Ok(Some(mut dwo)) => {
                        dwo.populate_abbreviations_cache(AbbreviationsCacheStrategy::All);
                        let split_unit = find_split_unit(&dwo, dwo_id)?;
                        import_split_unit_for_skeleton(
                            &dwo,
                            split_unit,
                            &unit,
                            &skeleton_lines,
                            &mut context,
                        )?;
                        continue;
                    }
                    Ok(None) => failures.push("unit is absent from the DWP index".into()),
                    Err(error) => failures.push(format!("DWP: {error}").into_boxed_str()),
                }
            }
            context
                .warnings
                .push(ConversionWarning::SplitDwarfUnavailable {
                    dwo_id: dwo_id.0,
                    reasons: failures.into_boxed_slice(),
                });
            import_unit_details(
                &dwarf,
                &unit,
                &skeleton_lines,
                &skeleton_lines.files,
                &mut context,
            )?;
            continue;
        }
        import_unit(&dwarf, &unit, &mut context)?;
    }

    if context.include_inlines && context.stats.inline_nodes == 0 {
        context.warnings.push(ConversionWarning::NoInlineRecords);
    }
    Ok(())
}

fn try_import_loose_dwo<'data, 'relocations>(
    parent: &Dwarf<SectionReader<'data, 'relocations>>,
    skeleton_unit: &Unit<SectionReader<'data, 'relocations>>,
    dwo_id: gimli::DwoId,
    base: &Path,
    layout: &AddressLayout,
    skeleton_lines: &UnitLines,
    context: &mut ImportContext<'_>,
) -> Result<LooseDwoImport> {
    let (sections, endian) = match load_dwo(parent, skeleton_unit, dwo_id, base, layout) {
        Ok(Some(dwo)) => dwo,
        Ok(None) => return Ok(LooseDwoImport::Unavailable),
        Err(error) => return Ok(LooseDwoImport::Failed(format!("DWO: {error}").into())),
    };
    let mut dwo = borrow_dwarf(&sections, endian);
    dwo.make_dwo(parent);
    let split_unit = match find_split_unit(&dwo, dwo_id) {
        Ok(unit) => unit,
        Err(error) => return Ok(LooseDwoImport::Failed(format!("DWO: {error}").into())),
    };
    import_split_unit_for_skeleton(&dwo, split_unit, skeleton_unit, skeleton_lines, context)?;
    Ok(LooseDwoImport::Imported)
}

fn find_split_unit<R: Reader<Offset = usize>>(
    dwarf: &Dwarf<R>,
    expected_id: gimli::DwoId,
) -> Result<Unit<R>> {
    let mut matched_unit = None;
    let mut headers = dwarf.units();
    while let Some(header) = headers.next().map_err(gimli_error)? {
        let unit = dwarf.unit(header).map_err(gimli_error)?;
        if unit.dwo_id != Some(expected_id) {
            continue;
        }
        if matched_unit.is_some() {
            return Err(Error::malformed(
                "split DWARF unit",
                format!("duplicate ID {:#x}", expected_id.0),
            ));
        }
        matched_unit = Some(unit);
    }
    matched_unit.ok_or_else(|| {
        Error::malformed(
            "split DWARF unit",
            format!("ID mismatch: expected {:#x}", expected_id.0),
        )
    })
}

fn import_split_unit_for_skeleton<R: Reader<Offset = usize>>(
    dwarf: &Dwarf<R>,
    mut split_unit: Unit<R>,
    skeleton_unit: &Unit<R>,
    skeleton_lines: &UnitLines,
    context: &mut ImportContext<'_>,
) -> Result<()> {
    split_unit.copy_relocated_attributes(skeleton_unit);
    import_split_unit(dwarf, &split_unit, skeleton_lines, context)?;
    context.stats.split_dwarf_units = context.stats.split_dwarf_units.saturating_add(1);
    Ok(())
}

fn import_unit<R: Reader<Offset = usize>>(
    dwarf: &Dwarf<R>,
    unit: &Unit<R>,
    context: &mut ImportContext<'_>,
) -> Result<()> {
    let unit_lines = collect_lines(dwarf, unit, context.builder, context.warnings)?;
    import_unit_details(dwarf, unit, &unit_lines, &unit_lines.files, context)
}

fn import_split_unit<R: Reader<Offset = usize>>(
    dwarf: &Dwarf<R>,
    unit: &Unit<R>,
    skeleton_lines: &UnitLines,
    context: &mut ImportContext<'_>,
) -> Result<()> {
    let unit_lines = collect_lines(dwarf, unit, context.builder, context.warnings)?;
    let executable_lines =
        if skeleton_lines.entries.is_empty() && skeleton_lines.sequences.is_empty() {
            &unit_lines
        } else {
            skeleton_lines
        };
    let file_indices = if unit_lines.files.is_empty() {
        &executable_lines.files
    } else {
        &unit_lines.files
    };
    import_unit_details(dwarf, unit, executable_lines, file_indices, context)
}

fn import_unit_details<R: Reader<Offset = usize>>(
    dwarf: &Dwarf<R>,
    unit: &Unit<R>,
    executable_lines: &UnitLines,
    file_indices: &lines::FileIndices,
    context: &mut ImportContext<'_>,
) -> Result<()> {
    let resolver = DwarfResolver::new(dwarf);
    let mut entries = unit.entries_raw(None).map_err(gimli_error)?;
    let mut entry = DebuggingInformationEntry::null();
    while let Some(entry) = next_subprogram(&mut entries, &mut entry).map_err(gimli_error)? {
        let Some(name) = resolve_name(&resolver, unit, entry, 0)? else {
            continue;
        };
        let mut ranges = match dwarf.die_ranges(unit, entry) {
            Ok(ranges) => ranges,
            Err(error) => {
                context.stats.rejected_ranges = context.stats.rejected_ranges.saturating_add(1);
                context.warnings.push(ConversionWarning::MalformedRanges {
                    stopped: false,
                    reason: error.to_string().into_boxed_str(),
                });
                continue;
            }
        };
        loop {
            let range = match ranges.next() {
                Ok(Some(range)) => range,
                Ok(None) => break,
                Err(error) => {
                    context.stats.rejected_ranges = context.stats.rejected_ranges.saturating_add(1);
                    context.warnings.push(ConversionWarning::MalformedRanges {
                        stopped: true,
                        reason: error.to_string().into_boxed_str(),
                    });
                    break;
                }
            };
            let candidate = AddressRange::new(range.begin, range.end);
            let possible_tombstone =
                candidate.start == 0 || unit.is_tombstone_address(candidate.start);
            if possible_tombstone
                && !context
                    .executable_ranges
                    .iter()
                    .any(|text| text.contains(candidate.start))
            {
                context.stats.rejected_ranges = context.stats.rejected_ranges.saturating_add(1);
                continue;
            }
            if !is_live_range(candidate, context.executable_ranges) {
                context.stats.rejected_ranges = context.stats.rejected_ranges.saturating_add(1);
                context
                    .warnings
                    .push(ConversionWarning::RejectedRange { range: candidate });
                continue;
            }
            let statement_sequence = statement_sequence_offset(unit, entry);
            let (mut function_lines, invalid_statement_sequence) =
                executable_lines.for_range(candidate, statement_sequence);
            if invalid_statement_sequence {
                let Some(sequence_offset) = statement_sequence else {
                    return Err(Error::InvalidModel(
                        "line lookup rejected a statement sequence the subprogram does not request",
                    ));
                };
                context
                    .warnings
                    .push(ConversionWarning::InvalidStatementSequence {
                        die_offset: absolute_entry_offset(unit, entry.offset())? as u64,
                        sequence_offset,
                    });
            }
            if function_lines.is_empty()
                && let Some(mut declaration) = resolve_declaration_line(
                    &resolver,
                    unit,
                    entry,
                    file_indices,
                    context.builder,
                    context.warnings,
                )?
            {
                declaration.address = candidate.start;
                function_lines.push(declaration);
            }
            context.stats.line_rows = context.stats.line_rows.saturating_add(function_lines.len());
            let (inline, call_sites, inline_count) = extract_subprogram_details(
                &resolver,
                unit,
                entry.offset(),
                AddressRange::new(range.begin, range.end),
                &name,
                file_indices,
                &mut DetailOptions {
                    include_inlines: context.include_inlines,
                    include_call_sites: context.include_call_sites,
                    warnings: context.warnings,
                },
            )?;
            context.stats.inline_nodes = context.stats.inline_nodes.saturating_add(inline_count);
            context.builder.add_function(Function {
                range: AddressRange::new(range.begin, range.end),
                name: name.clone(),
                lines: function_lines,
                inline,
                call_sites,
                ..Function::default()
            })?;
            context.stats.dwarf_functions = context.stats.dwarf_functions.saturating_add(1);
        }
    }
    Ok(())
}

fn next_subprogram<'entry, R: Reader>(
    entries: &mut gimli::EntriesRaw<'_, R>,
    entry: &'entry mut DebuggingInformationEntry<R>,
) -> gimli::Result<Option<&'entry DebuggingInformationEntry<R>>> {
    while !entries.is_empty() {
        let mut probe = entries.clone();
        if let Some(abbreviation) = probe.read_abbreviation()? {
            if abbreviation.tag() == gimli::DW_TAG_subprogram {
                entries.read_entry(entry)?;
                return Ok(Some(entry));
            }
            // Parse unused attributes to preserve errors, but avoid building
            // an attribute vector for entries the importer does not consume.
            for specification in abbreviation.attributes() {
                drop(probe.read_attribute_inline(*specification)?);
            }
        }
        *entries = probe;
    }
    Ok(None)
}

fn load_dwo<R: Reader<Offset = usize>>(
    parent: &Dwarf<R>,
    unit: &Unit<R>,
    expected_id: gimli::DwoId,
    base: &Path,
    layout: &AddressLayout,
) -> Result<Option<(DwarfSections<SectionData<'static>>, RunTimeEndian)>> {
    let Some(name) = unit.dwo_name().map_err(gimli_error)? else {
        return Ok(None);
    };
    let name = attribute_bytes(parent, unit, name)?;
    if name.is_empty() {
        return Ok(None);
    }
    let name = bytes_path(&name);
    let mut paths = if let Some(comp_dir) = &unit.comp_dir {
        let comp_dir = bytes_path(&comp_dir.to_slice().map_err(gimli_error)?);
        let relative = comp_dir.join(&name);
        if comp_dir.is_relative() {
            vec![base.join(&relative), relative]
        } else {
            vec![relative]
        }
    } else {
        vec![base.join(&name)]
    };
    paths.dedup();
    let mut failure = None;
    for path in paths {
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                failure = Some(Error::IoAtPath {
                    operation: "read DWO file",
                    path,
                    source,
                });
                continue;
            }
        };
        match parse_dwo(&bytes, &path, expected_id, layout) {
            Ok(dwo) => return Ok(Some(dwo)),
            Err(error) => failure = Some(error),
        }
    }
    failure.map_or(Ok(None), Err)
}

fn parse_dwo(
    bytes: &[u8],
    path: &Path,
    expected_id: gimli::DwoId,
    layout: &AddressLayout,
) -> Result<(DwarfSections<SectionData<'static>>, RunTimeEndian)> {
    let file = object::File::parse(bytes).map_err(|source| Error::ElfParse {
        input: ElfInputKind::Dwo,
        source: crate::ParserError::object(source),
    })?;
    let endian = runtime_endian(&file);
    let sections = DwarfSections::load(|id| load_dwo_section_owned(&file, id, layout))?;
    let (saw_unit, found) = {
        let borrowed = sections.borrow(|section| {
            RelocateReader::new(
                EndianSlice::new(section.data.as_ref(), endian),
                &section.relocations,
            )
        });
        let mut found = false;
        let mut saw_unit = false;
        let mut headers = borrowed.units();
        while let Some(header) = headers.next().map_err(gimli_error)? {
            saw_unit = true;
            let dwo_unit = borrowed.unit(header).map_err(gimli_error)?;
            if dwo_unit.dwo_id == Some(expected_id) {
                found = true;
                break;
            }
        }
        (saw_unit, found)
    };
    if !saw_unit {
        return Err(Error::malformed(
            "DWO file",
            format!("no compilation unit in {}", path.display()),
        ));
    }
    if !found {
        return Err(Error::malformed(
            "DWO file",
            format!("ID mismatch for {}", path.display()),
        ));
    }
    Ok((sections, endian))
}

#[cfg(unix)]
fn bytes_path(bytes: &[u8]) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStrExt;
    std::path::PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn bytes_path(bytes: &[u8]) -> std::path::PathBuf {
    std::path::PathBuf::from(String::from_utf8_lossy(bytes).as_ref())
}

fn gimli_error(error: gimli::Error) -> Error {
    Error::from(error)
}

fn is_live_range(range: AddressRange, executable_ranges: &[AddressRange]) -> bool {
    !range.is_empty()
        && u32::try_from(range.size()).is_ok()
        && executable_ranges
            .iter()
            .any(|text| text.contains_range(range))
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::sync::Arc;

    use super::*;

    #[test]
    fn subprogram_scan_preserves_entries_and_attribute_errors() {
        for form in [
            0x01, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
            0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16,
        ] {
            let abbrev = [1, 0x34, 1, 3, form, 0, 0, 2, 0x2e, 0, 3, 8, 0, 0, 0];
            let abbrev = gimli::DebugAbbrev::new(&abbrev, gimli::LittleEndian)
                .abbreviations(gimli::DebugAbbrevOffset(0))
                .unwrap();
            for length in 0..=24 {
                for value in [0, 1, 0x80, 0xff] {
                    let mut body = vec![1];
                    body.extend(std::iter::repeat_n(value, length));
                    body.extend_from_slice(&[0, 2, b'f', 0, 0]);
                    let mut data = u32::try_from(body.len() + 7)
                        .unwrap()
                        .to_le_bytes()
                        .to_vec();
                    data.extend_from_slice(&[4, 0, 0, 0, 0, 0, 8]);
                    data.extend(body);
                    let info = gimli::DebugInfo::new(&data, gimli::LittleEndian);
                    let header = info.units().next().unwrap().unwrap();
                    let mut original = header.entries(&abbrev);
                    let mut raw = header.entries_raw(&abbrev, None).unwrap();
                    let mut entry = DebuggingInformationEntry::null();
                    assert_subprogram_scan_matches(&mut original, &mut raw, &mut entry);
                }
            }
        }
    }

    fn assert_subprogram_scan_matches<R: Reader>(
        original: &mut gimli::EntriesCursor<'_, R>,
        raw: &mut gimli::EntriesRaw<'_, R>,
        entry: &mut DebuggingInformationEntry<R>,
    ) {
        loop {
            let expected = loop {
                match original.next_dfs() {
                    Ok(Some(entry)) if entry.tag() != gimli::DW_TAG_subprogram => {}
                    outcome => break outcome.map(|entry| entry.map(|entry| format!("{entry:?}"))),
                }
            };
            let actual =
                next_subprogram(raw, entry).map(|entry| entry.map(|entry| format!("{entry:?}")));
            assert_eq!(actual, expected);
            if !matches!(actual, Ok(Some(_))) {
                break;
            }
        }
    }

    #[test]
    fn abbreviations_are_reused_even_when_only_one_unit_uses_the_table() {
        let sections = DwarfSections::load(|id| -> gimli::Result<_> {
            let data: &[u8] = if id == gimli::SectionId::DebugInfo {
                // DWARF 4 unit header followed by a compile-unit DIE.
                &[8, 0, 0, 0, 4, 0, 0, 0, 0, 0, 8, 1]
            } else if id == gimli::SectionId::DebugAbbrev {
                &[1, 0x11, 0, 0, 0, 0]
            } else {
                &[]
            };
            Ok(SectionData {
                data: Cow::Borrowed(data),
                ..SectionData::default()
            })
        })
        .unwrap();
        let dwarf = borrow_dwarf(&sections, RunTimeEndian::Little);
        let header = dwarf.units().next().unwrap().unwrap();
        let first = dwarf.unit(header.clone()).unwrap();
        let second = dwarf.unit(header).unwrap();
        assert!(Arc::ptr_eq(&first.abbreviations, &second.abbreviations));
    }

    #[test]
    fn dead_and_invalid_ranges_are_rejected_without_losing_live_ranges() {
        let live = [
            AddressRange::new(0x1000, 0x1100),
            AddressRange::new(0x2000, 0x2100),
        ];
        assert!(is_live_range(AddressRange::new(0x1010, 0x1020), &live));
        assert!(!is_live_range(AddressRange::new(0x1010, 0x1010), &live));
        assert!(!is_live_range(AddressRange::new(0x10f0, 0x2010), &live));
        assert!(!is_live_range(AddressRange::new(0x3000, 0x3010), &live));
        assert!(!is_live_range(
            AddressRange::new(0, u64::from(u32::MAX) + 1),
            &[AddressRange::new(0, u64::MAX)]
        ));
    }

    #[test]
    fn debug_relocation_formulas_cover_absolute_relative_and_section_offsets() {
        assert!(debug_relocation_encoding_supported(
            RelocationKind::Absolute,
            RelocationEncoding::X86Signed
        ));
        assert!(debug_relocation_encoding_supported(
            RelocationKind::Relative,
            RelocationEncoding::Generic
        ));
        assert!(!debug_relocation_encoding_supported(
            RelocationKind::Relative,
            RelocationEncoding::X86Branch
        ));
        assert_eq!(
            calculate_relocation_value(RelocationKind::Absolute, 0x1200, 0, 0x40, -4).unwrap(),
            0x11fc
        );
        assert_eq!(
            calculate_relocation_value(RelocationKind::Relative, 0x1200, 0, 0x40, -4).unwrap(),
            0x11bc
        );
        assert_eq!(
            calculate_relocation_value(RelocationKind::SectionOffset, 0x1234, 0x1200, 0x40, 8,)
                .unwrap(),
            0x3c
        );

        let relocations = DwarfRelocations::new(vec![
            RelocationEntry {
                offset: 12,
                implicit_addend: false,
                value: 0x2000,
            },
            RelocationEntry {
                offset: 4,
                implicit_addend: true,
                value: 0x1000,
            },
        ])
        .unwrap();
        assert_eq!(relocations.apply(4, 0x24), 0x1024);
        assert_eq!(relocations.apply(12, 0x24), 0x2000);
        assert_eq!(relocations.apply(8, 0x24), 0x24);
        assert!(
            DwarfRelocations::new(vec![
                RelocationEntry {
                    offset: 4,
                    implicit_addend: false,
                    value: 1,
                },
                RelocationEntry {
                    offset: 4,
                    implicit_addend: false,
                    value: 2,
                },
            ])
            .is_err()
        );
    }

    #[test]
    fn line_rows_clamp_only_inside_their_statement_sequence() {
        let lines = UnitLines {
            entries: vec![
                SequencedLine {
                    entry: LineEntry {
                        address: 0x1000,
                        file: 1.into(),
                        line: 10,
                    },
                    statement_sequence: Some(0x29),
                },
                SequencedLine {
                    entry: LineEntry {
                        address: 0x1018,
                        file: 1.into(),
                        line: 10,
                    },
                    statement_sequence: Some(0x29),
                },
                SequencedLine {
                    entry: LineEntry {
                        address: 0x1020,
                        file: 1.into(),
                        line: 11,
                    },
                    statement_sequence: Some(0x29),
                },
                SequencedLine {
                    entry: LineEntry {
                        address: 0x2000,
                        file: 2.into(),
                        line: 20,
                    },
                    statement_sequence: Some(0x40),
                },
            ],
            files: lines::FileIndices::default(),
            sequences: vec![
                LineSequenceRange {
                    range: AddressRange::new(0x1000, 0x1100),
                    statement_sequence: Some(0x29),
                },
                LineSequenceRange {
                    range: AddressRange::new(0x2000, 0x2100),
                    statement_sequence: Some(0x40),
                },
            ],
        };
        let (clamped, invalid) = lines.for_range(AddressRange::new(0x1010, 0x1030), Some(0x29));
        assert!(!invalid);
        let [first, second, ..] = clamped.as_slice() else {
            panic!("clamped range keeps at least two rows");
        };
        assert_eq!(first.address, 0x1010);
        assert_eq!(first.line, 10);
        assert_eq!(second.address, 0x1020);
        assert!(
            lines
                .for_range(AddressRange::new(0x1800, 0x1810), None)
                .0
                .is_empty()
        );
    }

    #[test]
    fn statement_sequence_filtering_falls_back_only_for_invalid_offsets() {
        let lines = UnitLines {
            entries: vec![
                SequencedLine {
                    entry: LineEntry {
                        address: 0x1000,
                        file: 1.into(),
                        line: 10,
                    },
                    statement_sequence: Some(0x29),
                },
                SequencedLine {
                    entry: LineEntry {
                        address: 0x1000,
                        file: 2.into(),
                        line: 20,
                    },
                    statement_sequence: Some(0x40),
                },
            ],
            files: lines::FileIndices::default(),
            sequences: vec![
                LineSequenceRange {
                    range: AddressRange::new(0x1000, 0x1100),
                    statement_sequence: Some(0x29),
                },
                LineSequenceRange {
                    range: AddressRange::new(0x1000, 0x1100),
                    statement_sequence: Some(0x40),
                },
            ],
        };

        let (selected, invalid) = lines.for_range(AddressRange::new(0x1000, 0x1010), Some(0x40));
        assert!(!invalid);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected.first().unwrap().line, 20);

        let (fallback, invalid) = lines.for_range(AddressRange::new(0x1000, 0x1010), Some(0x41));
        assert!(invalid);
        assert_eq!(fallback.len(), 2);
    }

    #[test]
    fn line_rows_compact_repeated_locations_without_losing_equal_address_order() {
        let row = |address, file: u32, line| SequencedLine {
            entry: LineEntry {
                address,
                file: file.into(),
                line,
            },
            statement_sequence: Some(0x29),
        };
        let lines = UnitLines {
            entries: vec![
                row(0x1000, 1, 10),
                row(0x1004, 1, 10),
                row(0x1004, 1, 11),
                row(0x1004, 1, 10),
                row(0x1008, 1, 10),
                row(0x1008, 2, 10),
                row(0x100c, 2, 10),
            ],
            files: lines::FileIndices::default(),
            sequences: vec![LineSequenceRange {
                range: AddressRange::new(0x1000, 0x1010),
                statement_sequence: Some(0x29),
            }],
        };

        let (selected, invalid) = lines.for_range(AddressRange::new(0x1000, 0x1010), Some(0x29));

        assert!(!invalid);
        assert_eq!(
            selected
                .iter()
                .map(|row| (row.address, row.file.get(), row.line))
                .collect::<Vec<_>>(),
            [
                (0x1000, 1, 10),
                (0x1004, 1, 11),
                (0x1004, 1, 10),
                (0x1008, 2, 10),
            ]
        );
    }

    #[test]
    fn line_instruction_scanner_records_exact_sequence_offsets() {
        let program = [
            0,
            2,
            gimli::constants::DW_LNE_set_address.0,
            0,
            1,
            0,
            1,
            gimli::constants::DW_LNE_end_sequence.0,
            1,
            0,
            1,
            gimli::constants::DW_LNE_end_sequence.0,
        ];
        let offsets = scan_line_sequence_offsets(&program, &[0; 12], 13, 0x29).unwrap();
        assert_eq!(offsets, [0x29, 0x31]);

        let error = scan_line_sequence_offsets(&[0, 2, 1], &[0; 12], 13, 0)
            .expect_err("truncated extended opcode must fail");
        assert!(matches!(
            error,
            Error::Malformed { detail, .. } if detail.contains("truncated")
        ));
    }
}
