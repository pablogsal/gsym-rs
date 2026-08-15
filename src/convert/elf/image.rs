use std::collections::HashMap;

use object::{
    Object, ObjectKind, ObjectSection, ObjectSymbol, SectionIndex, SectionKind, SymbolKind,
};

use super::super::is_debug_section;
use crate::model::{AddressRange, Function};
use crate::{CompanionMismatch, ElfInputKind, Error, Result};

pub(super) fn parse_elf(bytes: &[u8], input: ElfInputKind) -> Result<object::File<'_>> {
    let file = object::File::parse(bytes).map_err(|source| Error::ElfParse {
        input,
        source: crate::ParserError::object(source),
    })?;
    if file.format() != object::BinaryFormat::Elf {
        return Err(Error::NotElf { input });
    }
    Ok(file)
}

pub(super) fn require_supported_kind(file: &object::File<'_>) -> Result<()> {
    if matches!(
        file.kind(),
        ObjectKind::Executable | ObjectKind::Dynamic | ObjectKind::Relocatable
    ) {
        Ok(())
    } else {
        Err(Error::InvalidModel(
            "ELF input is not ET_EXEC, ET_DYN, or ET_REL",
        ))
    }
}

#[derive(Clone, Debug)]
pub(in crate::convert) struct AddressLayout {
    pub(in crate::convert) ranges: Vec<AddressRange>,
    section_bases: HashMap<SectionIndex, u64>,
    relocatable: bool,
}

impl AddressLayout {
    pub(in crate::convert) fn new(file: &object::File<'_>) -> Result<Self> {
        let relocatable = file.kind() == ObjectKind::Relocatable;
        let mut ranges = Vec::new();
        let mut section_bases = HashMap::new();
        let mut next_base = 0_u64;
        for section in file.sections() {
            if section.kind() != SectionKind::Text || section.size() == 0 {
                continue;
            }
            let base = if relocatable {
                next_base = align_section(next_base)?;
                next_base
            } else {
                section.address()
            };
            let end = base
                .checked_add(section.size())
                .ok_or(Error::Overflow("executable section end"))?;
            if relocatable {
                section_bases.insert(section.index(), base);
                next_base = end;
            }
            ranges.push(AddressRange::new(base, end));
        }
        if relocatable {
            for section in file.sections() {
                if section.size() == 0
                    || section_bases.contains_key(&section.index())
                    || is_debug_section(&section)
                {
                    continue;
                }
                next_base = align_section(next_base)?;
                section_bases.insert(section.index(), next_base);
                next_base = next_base
                    .checked_add(section.size())
                    .ok_or(Error::Overflow("relocatable section end"))?;
            }
        }
        ranges.sort_unstable();
        Ok(Self {
            ranges,
            section_bases,
            relocatable,
        })
    }

    pub(in crate::convert) fn section_base(
        &self,
        section: &object::Section<'_, '_>,
    ) -> Result<u64> {
        if !self.relocatable || is_debug_section(section) {
            return Ok(section.address());
        }
        self.section_bases
            .get(&section.index())
            .copied()
            .ok_or(Error::InvalidModel("relocation targets an unknown section"))
    }
}

fn align_section(value: u64) -> Result<u64> {
    value
        .checked_add(15)
        .map(|value| value & !15)
        .ok_or(Error::Overflow("relocatable section alignment"))
}

pub(super) fn cross_check_elf_identity(
    image: &object::File<'_>,
    companion: &object::File<'_>,
    input: ElfInputKind,
) -> Result<()> {
    cross_check_elf_architecture(image, companion, input)?;
    let image_id = image
        .build_id()
        .map_err(|error| malformed("image build ID", error))?;
    let companion_id = companion
        .build_id()
        .map_err(|error| malformed("companion build ID", error))?;
    if let (Some(image_id), Some(companion_id)) = (image_id, companion_id)
        && image_id != companion_id
    {
        return Err(Error::CompanionMismatch {
            input,
            mismatch: CompanionMismatch::BuildId,
        });
    }
    Ok(())
}

pub(super) fn cross_check_elf_architecture(
    image: &object::File<'_>,
    companion: &object::File<'_>,
    input: ElfInputKind,
) -> Result<()> {
    if image.architecture() != companion.architecture()
        || image.is_64() != companion.is_64()
        || image.is_little_endian() != companion.is_little_endian()
    {
        return Err(Error::CompanionMismatch {
            input,
            mismatch: CompanionMismatch::Architecture,
        });
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SymbolDisposition {
    Import,
    Reject,
}

pub(super) fn visit_symbols(
    file: &object::File<'_>,
    layout: &AddressLayout,
    mut visitor: impl FnMut(Function, SymbolDisposition) -> Result<()>,
) -> Result<()> {
    for symbol in file.symbols().chain(file.dynamic_symbols()) {
        if symbol.kind() != SymbolKind::Text || !symbol.is_definition() {
            continue;
        }
        let address = if layout.relocatable {
            let Some(section_index) = symbol.section_index() else {
                continue;
            };
            let section = file
                .section_by_index(section_index)
                .map_err(|error| malformed("ELF symbol section", error))?;
            layout
                .section_base(&section)?
                .checked_add(symbol.address())
                .ok_or(Error::Overflow("relocatable symbol address"))?
        } else {
            symbol.address()
        };
        let name = match symbol.name_bytes() {
            Ok(name) if !name.is_empty() => name.to_vec(),
            _ => continue,
        };
        let end = address
            .checked_add(symbol.size())
            .ok_or(Error::Overflow("ELF symbol end"))?;
        let symbol_range = AddressRange::new(address, end);
        if symbol.size() > u64::from(u32::MAX) {
            visitor(
                Function {
                    range: symbol_range,
                    name,
                    ..Function::default()
                },
                SymbolDisposition::Reject,
            )?;
            continue;
        }
        if !layout
            .ranges
            .iter()
            .any(|range| range.contains_range(symbol_range))
        {
            visitor(
                Function {
                    range: symbol_range,
                    name,
                    ..Function::default()
                },
                SymbolDisposition::Reject,
            )?;
            continue;
        }
        visitor(
            Function {
                range: symbol_range,
                name,
                ..Function::default()
            },
            SymbolDisposition::Import,
        )?;
    }
    Ok(())
}

pub(super) fn malformed(context: &'static str, error: impl std::fmt::Display) -> Error {
    Error::malformed(context, error.to_string())
}
