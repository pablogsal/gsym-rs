use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use gimli::{AttributeValue, DebuggingInformationEntry, Reader, Unit};

use super::lines::intern_header_files;
use super::{file_index_attribute, gimli_error, unsigned_attribute};
use crate::convert::ConversionWarning;
use crate::model::{FileIndex, LineEntry};
use crate::{Error, GsymBuilder, Result};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum DebugSource {
    Main,
    Supplementary,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct DieKey {
    source: DebugSource,
    offset: usize,
}

pub(super) fn resolve_declaration_line<R: Reader<Offset = usize>>(
    dwarf: &gimli::Dwarf<R>,
    unit: &Unit<R>,
    entry: &DebuggingInformationEntry<R>,
    files: &HashMap<u64, FileIndex>,
    builder: &mut GsymBuilder,
    warnings: &mut Vec<ConversionWarning>,
) -> Result<Option<LineEntry>> {
    DeclarationResolver::new(builder, warnings).resolve(
        dwarf,
        unit,
        entry,
        files,
        DebugSource::Main,
        0,
    )
}

struct DeclarationResolver<'a> {
    builder: &'a mut GsymBuilder,
    warnings: &'a mut Vec<ConversionWarning>,
    visited: HashSet<DieKey>,
}

impl<'a> DeclarationResolver<'a> {
    fn new(builder: &'a mut GsymBuilder, warnings: &'a mut Vec<ConversionWarning>) -> Self {
        Self {
            builder,
            warnings,
            visited: HashSet::new(),
        }
    }

    fn resolve<R: Reader<Offset = usize>>(
        &mut self,
        dwarf: &gimli::Dwarf<R>,
        unit: &Unit<R>,
        entry: &DebuggingInformationEntry<R>,
        files: &HashMap<u64, FileIndex>,
        source: DebugSource,
        depth: u8,
    ) -> Result<Option<LineEntry>> {
        if depth >= 64 {
            return Ok(None);
        }
        if let (Some(file_index), Some(line_number)) = (
            file_index_attribute(entry, gimli::constants::DW_AT_decl_file),
            unsigned_attribute(entry, gimli::constants::DW_AT_decl_line),
        ) {
            match (files.get(&file_index).copied(), u32::try_from(line_number)) {
                (Some(file), Ok(line)) => {
                    return Ok(Some(LineEntry {
                        address: 0,
                        file,
                        line,
                    }));
                }
                (None, _) => self
                    .warnings
                    .push(ConversionWarning::InvalidDeclarationFile {
                        die_offset: absolute_entry_offset(unit, entry.offset())? as u64,
                        index: file_index,
                    }),
                (_, Err(_)) => self
                    .warnings
                    .push(ConversionWarning::InvalidDeclarationLine {
                        die_offset: absolute_entry_offset(unit, entry.offset())? as u64,
                        line: line_number,
                    }),
            }
        }
        for attribute in [
            gimli::constants::DW_AT_abstract_origin,
            gimli::constants::DW_AT_specification,
        ] {
            match entry.attr_value(attribute) {
                Some(AttributeValue::UnitRef(offset)) => {
                    let key = DieKey {
                        source,
                        offset: absolute_entry_offset(unit, offset)?,
                    };
                    if self.visited.insert(key) {
                        let referenced = unit.entry(offset).map_err(gimli_error)?;
                        if let Some(line) = self.resolve(
                            dwarf,
                            unit,
                            &referenced,
                            files,
                            source,
                            depth.saturating_add(1),
                        )? {
                            return Ok(Some(line));
                        }
                    }
                }
                Some(AttributeValue::DebugInfoRef(offset)) => {
                    if let Some(line) = self.resolve_absolute(
                        dwarf,
                        offset.0,
                        DebugSource::Main,
                        depth.saturating_add(1),
                    )? {
                        return Ok(Some(line));
                    }
                }
                Some(AttributeValue::DebugInfoRefSup(offset)) => {
                    if let Some(sup) = dwarf.sup()
                        && let Some(line) = self.resolve_absolute(
                            sup,
                            offset.0,
                            DebugSource::Supplementary,
                            depth.saturating_add(1),
                        )?
                    {
                        return Ok(Some(line));
                    }
                }
                _ => {}
            }
        }
        Ok(None)
    }

    fn resolve_absolute<R: Reader<Offset = usize>>(
        &mut self,
        dwarf: &gimli::Dwarf<R>,
        target: usize,
        source: DebugSource,
        depth: u8,
    ) -> Result<Option<LineEntry>> {
        if !self.visited.insert(DieKey {
            source,
            offset: target,
        }) {
            return Ok(None);
        }
        let Some((unit, offset)) = unit_containing_offset(dwarf, target)? else {
            return Ok(None);
        };
        let referenced = unit.entry(offset).map_err(gimli_error)?;
        let files = unit.line_program.as_ref().map_or_else(
            || Ok(HashMap::new()),
            |program| intern_header_files(dwarf, &unit, program.header(), self.builder),
        )?;
        self.resolve(dwarf, &unit, &referenced, &files, source, depth)
    }
}

pub(super) fn resolve_name<R: Reader<Offset = usize>>(
    dwarf: &gimli::Dwarf<R>,
    unit: &Unit<R>,
    entry: &DebuggingInformationEntry<R>,
    depth: u8,
) -> Result<Option<Vec<u8>>> {
    let mut visited = HashSet::new();
    resolve_name_inner(dwarf, unit, entry, DebugSource::Main, depth, &mut visited)
}

fn resolve_name_inner<R: Reader<Offset = usize>>(
    dwarf: &gimli::Dwarf<R>,
    unit: &Unit<R>,
    entry: &DebuggingInformationEntry<R>,
    source: DebugSource,
    depth: u8,
    visited: &mut HashSet<DieKey>,
) -> Result<Option<Vec<u8>>> {
    if depth >= 64 {
        return Ok(None);
    }
    for attribute in [
        gimli::constants::DW_AT_linkage_name,
        gimli::constants::DW_AT_MIPS_linkage_name,
        gimli::constants::DW_AT_name,
    ] {
        if let Some(value) = entry.attr_value(attribute) {
            let bytes = attribute_bytes(dwarf, unit, value)?;
            if !bytes.is_empty() {
                return Ok(Some(bytes));
            }
        }
    }
    for attribute in [
        gimli::constants::DW_AT_abstract_origin,
        gimli::constants::DW_AT_specification,
    ] {
        match entry.attr_value(attribute) {
            Some(AttributeValue::UnitRef(offset)) => {
                let key = DieKey {
                    source,
                    offset: absolute_entry_offset(unit, offset)?,
                };
                if visited.insert(key) {
                    let referenced = unit.entry(offset).map_err(gimli_error)?;
                    if let Some(name) = resolve_name_inner(
                        dwarf,
                        unit,
                        &referenced,
                        source,
                        depth.saturating_add(1),
                        visited,
                    )? {
                        return Ok(Some(name));
                    }
                }
            }
            Some(AttributeValue::DebugInfoRef(offset)) => {
                if let Some(name) = resolve_absolute_name(
                    dwarf,
                    offset.0,
                    DebugSource::Main,
                    depth.saturating_add(1),
                    visited,
                )? {
                    return Ok(Some(name));
                }
            }
            Some(AttributeValue::DebugInfoRefSup(offset)) => {
                if let Some(sup) = dwarf.sup()
                    && let Some(name) = resolve_absolute_name(
                        sup,
                        offset.0,
                        DebugSource::Supplementary,
                        depth.saturating_add(1),
                        visited,
                    )?
                {
                    return Ok(Some(name));
                }
            }
            _ => {}
        }
    }
    Ok(None)
}

pub(super) fn resolve_reference_name<R: Reader<Offset = usize>>(
    dwarf: &gimli::Dwarf<R>,
    unit: &Unit<R>,
    reference: &AttributeValue<R>,
    depth: u8,
    visited: &mut HashSet<DieKey>,
) -> Result<Option<Vec<u8>>> {
    let next = depth.saturating_add(1);
    if let AttributeValue::UnitRef(offset) = reference {
        let key = DieKey {
            source: DebugSource::Main,
            offset: absolute_entry_offset(unit, *offset)?,
        };
        if !visited.insert(key) {
            return Ok(None);
        }
        let entry = unit.entry(*offset).map_err(gimli_error)?;
        resolve_name_inner(dwarf, unit, &entry, DebugSource::Main, next, visited)
    } else if let AttributeValue::DebugInfoRef(offset) = reference {
        resolve_absolute_name(dwarf, offset.0, DebugSource::Main, next, visited)
    } else if let AttributeValue::DebugInfoRefSup(offset) = reference {
        dwarf.sup().map_or(Ok(None), |sup| {
            resolve_absolute_name(sup, offset.0, DebugSource::Supplementary, next, visited)
        })
    } else {
        Ok(None)
    }
}

pub(super) fn absolute_entry_offset<R: Reader<Offset = usize>>(
    unit: &Unit<R>,
    offset: gimli::UnitOffset<usize>,
) -> Result<usize> {
    unit.header
        .debug_info_offset()
        .and_then(|base| base.0.checked_add(offset.0))
        .ok_or(Error::Overflow("DWARF entry offset"))
}

fn resolve_absolute_name<R: Reader<Offset = usize>>(
    dwarf: &gimli::Dwarf<R>,
    target: usize,
    source: DebugSource,
    depth: u8,
    visited: &mut HashSet<DieKey>,
) -> Result<Option<Vec<u8>>> {
    if !visited.insert(DieKey {
        source,
        offset: target,
    }) {
        return Ok(None);
    }
    let Some((unit, offset)) = unit_containing_offset(dwarf, target)? else {
        return Ok(None);
    };
    let referenced = unit.entry(offset).map_err(gimli_error)?;
    resolve_name_inner(dwarf, &unit, &referenced, source, depth, visited)
}

fn unit_containing_offset<R: Reader<Offset = usize>>(
    dwarf: &gimli::Dwarf<R>,
    target: usize,
) -> Result<Option<(Unit<R>, gimli::UnitOffset<usize>)>> {
    let mut units = dwarf.units();
    while let Some(header) = units.next().map_err(gimli_error)? {
        let Some(start) = header.debug_info_offset().map(|offset| offset.0) else {
            continue;
        };
        let end = start
            .checked_add(header.length_including_self())
            .ok_or(Error::Overflow("DWARF unit end"))?;
        if start <= target && target < end {
            return Ok(Some((
                dwarf.unit(header).map_err(gimli_error)?,
                gimli::UnitOffset(target.saturating_sub(start)),
            )));
        }
    }
    Ok(None)
}

pub(super) fn attribute_bytes<R: Reader<Offset = usize>>(
    dwarf: &gimli::Dwarf<R>,
    unit: &Unit<R>,
    value: AttributeValue<R>,
) -> Result<Vec<u8>> {
    if matches!(value, AttributeValue::DebugStrRefSup(_)) && dwarf.sup().is_none() {
        return Ok(Vec::new());
    }
    let value = dwarf.attr_string(unit, value).map_err(gimli_error)?;
    value.to_slice().map(Cow::into_owned).map_err(gimli_error)
}
