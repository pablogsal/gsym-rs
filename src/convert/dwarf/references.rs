use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::rc::Rc;

use gimli::{AttributeValue, DebuggingInformationEntry, Reader, Unit};
use smallvec::SmallVec;

use super::lines::{FileIndices, intern_header_files};
use super::{file_index_attribute, gimli_error, unsigned_attribute};
use crate::convert::ConversionWarning;
use crate::model::LineEntry;
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

// Reference chains usually contain only one or two entries. Keep those on
// the stack while retaining hashed lookup for large, branching graphs.
pub(super) enum VisitedDies {
    Small(SmallVec<[DieKey; 4]>),
    Large(HashSet<DieKey>),
}

impl Default for VisitedDies {
    fn default() -> Self {
        Self::Small(SmallVec::new())
    }
}

impl VisitedDies {
    fn insert(&mut self, key: DieKey) -> bool {
        match self {
            Self::Small(keys) => {
                if keys.contains(&key) {
                    return false;
                }
                if keys.len() < keys.inline_size() {
                    keys.push(key);
                } else {
                    *self = Self::Large(keys.iter().copied().chain([key]).collect());
                }
                true
            }
            Self::Large(keys) => keys.insert(key),
        }
    }
}

type ReferencedUnit<R> = (Rc<Unit<R>>, gimli::UnitOffset<usize>);

/// Reuses a bounded number of referenced units while importing a unit's DIEs.
pub(super) struct DwarfResolver<'a, R: Reader<Offset = usize>> {
    pub(super) dwarf: &'a gimli::Dwarf<R>,
    units: RefCell<VecDeque<Rc<Unit<R>>>>,
    supplementary: Option<Box<Self>>,
}

impl<'a, R: Reader<Offset = usize>> DwarfResolver<'a, R> {
    pub(super) fn new(dwarf: &'a gimli::Dwarf<R>) -> Self {
        Self {
            dwarf,
            units: RefCell::new(VecDeque::new()),
            supplementary: dwarf.sup().map(|sup| Box::new(Self::new(sup))),
        }
    }

    fn sup(&self) -> Option<&Self> {
        self.supplementary.as_deref()
    }

    fn unit_containing_offset(&self, target: usize) -> Result<Option<ReferencedUnit<R>>> {
        const CAPACITY: usize = 8;
        let mut cached = self.units.borrow_mut();
        let target = gimli::DebugInfoOffset(target);
        if let Some((index, offset)) = cached.iter().enumerate().find_map(|(index, unit)| {
            target
                .to_unit_offset(&unit.header)
                .map(|offset| (index, offset))
        }) && let Some(unit) = cached.remove(index)
        {
            cached.push_front(Rc::clone(&unit));
            return Ok(Some((unit, offset)));
        }
        let Some((unit, offset)) = unit_containing_offset(self.dwarf, target.0)? else {
            return Ok(None);
        };
        let unit = Rc::new(unit);
        if cached.len() == CAPACITY {
            cached.pop_back();
        }
        cached.push_front(Rc::clone(&unit));
        Ok(Some((unit, offset)))
    }
}

pub(super) fn resolve_declaration_line<R: Reader<Offset = usize>>(
    dwarf: &DwarfResolver<'_, R>,
    unit: &Unit<R>,
    entry: &DebuggingInformationEntry<R>,
    files: &FileIndices,
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
    visited: VisitedDies,
}

impl<'a> DeclarationResolver<'a> {
    fn new(builder: &'a mut GsymBuilder, warnings: &'a mut Vec<ConversionWarning>) -> Self {
        Self {
            builder,
            warnings,
            visited: VisitedDies::default(),
        }
    }

    fn resolve<R: Reader<Offset = usize>>(
        &mut self,
        dwarf: &DwarfResolver<'_, R>,
        unit: &Unit<R>,
        entry: &DebuggingInformationEntry<R>,
        files: &FileIndices,
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
            match (files.get(file_index), u32::try_from(line_number)) {
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
        dwarf: &DwarfResolver<'_, R>,
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
        let Some((unit, offset)) = dwarf.unit_containing_offset(target)? else {
            return Ok(None);
        };
        let referenced = unit.entry(offset).map_err(gimli_error)?;
        let files = unit.line_program.as_ref().map_or_else(
            || Ok(FileIndices::default()),
            |program| intern_header_files(dwarf.dwarf, &unit, program.header(), self.builder),
        )?;
        self.resolve(dwarf, &unit, &referenced, &files, source, depth)
    }
}

pub(super) fn resolve_name<R: Reader<Offset = usize>>(
    dwarf: &DwarfResolver<'_, R>,
    unit: &Unit<R>,
    entry: &DebuggingInformationEntry<R>,
    depth: u8,
) -> Result<Option<Vec<u8>>> {
    let mut visited = VisitedDies::default();
    resolve_name_inner(dwarf, unit, entry, DebugSource::Main, depth, &mut visited)
}

fn resolve_name_inner<R: Reader<Offset = usize>>(
    dwarf: &DwarfResolver<'_, R>,
    unit: &Unit<R>,
    entry: &DebuggingInformationEntry<R>,
    source: DebugSource,
    depth: u8,
    visited: &mut VisitedDies,
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
            let bytes = attribute_bytes(dwarf.dwarf, unit, value)?;
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
    dwarf: &DwarfResolver<'_, R>,
    unit: &Unit<R>,
    reference: &AttributeValue<R>,
    depth: u8,
    visited: &mut VisitedDies,
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
    dwarf: &DwarfResolver<'_, R>,
    target: usize,
    source: DebugSource,
    depth: u8,
    visited: &mut VisitedDies,
) -> Result<Option<Vec<u8>>> {
    if !visited.insert(DieKey {
        source,
        offset: target,
    }) {
        return Ok(None);
    }
    let Some((unit, offset)) = dwarf.unit_containing_offset(target)? else {
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
    let Some(value) = attribute_reader(dwarf, unit, value)? else {
        return Ok(Vec::new());
    };
    value.to_slice().map(Cow::into_owned).map_err(gimli_error)
}

pub(super) fn attribute_reader<R: Reader<Offset = usize>>(
    dwarf: &gimli::Dwarf<R>,
    unit: &Unit<R>,
    value: AttributeValue<R>,
) -> Result<Option<R>> {
    if matches!(value, AttributeValue::DebugStrRefSup(_)) && dwarf.sup().is_none() {
        return Ok(None);
    }
    dwarf
        .attr_string(unit, value)
        .map(Some)
        .map_err(gimli_error)
}

#[cfg(test)]
mod tests {
    use gimli::{DwarfSections, EndianSlice, LittleEndian, SectionId};

    use super::*;

    #[test]
    fn visited_entries_match_a_set_before_and_after_spilling() {
        let mut visited = VisitedDies::default();
        let mut expected = HashSet::new();
        for index in 0..300 {
            let key = DieKey {
                source: if index % 7 == 0 {
                    DebugSource::Supplementary
                } else {
                    DebugSource::Main
                },
                offset: index % 31,
            };
            assert_eq!(visited.insert(key), expected.insert(key));
        }
    }

    fn sections(count: usize, name: &[u8]) -> DwarfSections<Vec<u8>> {
        let mut info = Vec::new();
        for _ in 0..count {
            info.extend_from_slice(
                &u32::try_from(name.len().saturating_add(9))
                    .unwrap()
                    .to_le_bytes(),
            );
            info.extend_from_slice(&[4, 0, 0, 0, 0, 0, 8, 1]);
            info.extend_from_slice(name);
            info.push(0);
        }
        DwarfSections::load(|id| -> gimli::Result<_> {
            Ok(if id == SectionId::DebugInfo {
                info.clone()
            } else if id == SectionId::DebugAbbrev {
                vec![1, 0x11, 0, 3, 8, 0, 0, 0]
            } else {
                Vec::new()
            })
        })
        .unwrap()
    }

    #[test]
    fn referenced_unit_cache_reuses_units_and_evicts_the_least_recently_used() {
        let sections = sections(9, b"unit");
        let dwarf = sections.borrow(|data| EndianSlice::new(data, LittleEndian));
        let resolver = DwarfResolver::new(&dwarf);
        let offset = |index: usize| index.saturating_mul(17).saturating_add(11);
        let (first, _) = resolver.unit_containing_offset(offset(0)).unwrap().unwrap();
        let (second, _) = resolver.unit_containing_offset(offset(1)).unwrap().unwrap();
        for index in 2..8 {
            resolver
                .unit_containing_offset(offset(index))
                .unwrap()
                .unwrap();
        }
        let (reused, _) = resolver.unit_containing_offset(offset(0)).unwrap().unwrap();
        assert!(Rc::ptr_eq(&first, &reused));
        resolver.unit_containing_offset(offset(8)).unwrap().unwrap();
        assert_eq!(resolver.units.borrow().len(), 8);
        let (reused, _) = resolver.unit_containing_offset(offset(0)).unwrap().unwrap();
        assert!(Rc::ptr_eq(&first, &reused));
        let (reloaded, _) = resolver.unit_containing_offset(offset(1)).unwrap().unwrap();
        assert!(!Rc::ptr_eq(&second, &reloaded));
        assert!(resolver.unit_containing_offset(9 * 17).unwrap().is_none());
    }

    #[test]
    fn supplementary_units_have_an_independent_offset_space() {
        let main = sections(1, b"main");
        let sup = sections(1, b"supplementary");
        let dwarf = main.borrow_with_sup(Some(&sup), |data| EndianSlice::new(data, LittleEndian));
        let resolver = DwarfResolver::new(&dwarf);
        let (main, offset) = resolver.unit_containing_offset(11).unwrap().unwrap();
        let (sup, sup_offset) = resolver
            .sup()
            .unwrap()
            .unit_containing_offset(11)
            .unwrap()
            .unwrap();
        assert!(!Rc::ptr_eq(&main, &sup));
        let name = |unit: &Unit<_>, offset| {
            unit.entry(offset)
                .unwrap()
                .attr_value(gimli::DW_AT_name)
                .unwrap()
                .string_value(&dwarf.debug_str)
                .unwrap()
                .to_slice()
                .unwrap()
                .into_owned()
        };
        assert_eq!(name(&main, offset), b"main");
        assert_eq!(name(&sup, sup_offset), b"supplementary");
    }
}
