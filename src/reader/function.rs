use std::fmt;
use std::ops::Range;

use zerocopy::byteorder::{BigEndian, LittleEndian, U32, U64};
use zerocopy::{FromBytes, Immutable, KnownLayout};

use crate::endian::{Cursor, Endian};
use crate::error::{Error, Result};
use crate::format::function::{self, EncodedFunction, InfoRecord};
use crate::model::{AddressRange, FileIndex, Function};

use super::{Gsym, ParsedLayout};

#[derive(Clone, Copy, Debug)]
pub(super) struct RawFunction<'data> {
    pub(super) range: AddressRange,
    pub(super) name: u64,
    pub(super) data: &'data [u8],
    pub(super) records: &'data [u8],
}

impl<'data> RawFunction<'data> {
    pub(super) const fn records(self, endian: Endian) -> InfoRecords<'data> {
        InfoRecords {
            cursor: Cursor::new(self.records, endian),
            done: false,
        }
    }
}

pub(super) struct InfoRecords<'data> {
    cursor: Cursor<'data>,
    done: bool,
}

impl<'data> Iterator for InfoRecords<'data> {
    type Item = Result<InfoRecord<'data>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let result = function::next_record(&mut self.cursor, &mut self.done);
        match result {
            Ok(Some(record)) => Some(Ok(record)),
            Ok(None) => None,
            Err(error) => {
                self.done = true;
                Some(Err(error))
            }
        }
    }
}

/// Borrowed view of one indexed `FunctionInfo` record.
///
/// [`Self::index`], [`Self::range`], and [`Self::name`] are cheap, so listing
/// or filtering symbols does not need to decode anything. [`Self::decode`]
/// returns the whole record as an owned [`Function`].
///
/// Obtained from [`Gsym::function`](crate::Gsym::function),
/// [`Gsym::get_function`](crate::Gsym::get_function), or
/// [`Gsym::functions`](crate::Gsym::functions).
pub struct FunctionRef<'data> {
    pub(super) index: usize,
    pub(super) name: &'data [u8],
    pub(super) all_data: &'data [u8],
    pub(super) raw: RawFunction<'data>,
    pub(super) layout: &'data ParsedLayout,
}

impl<'data> FunctionRef<'data> {
    /// Returns the address-table index.
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    /// Returns the function's half-open address range.
    #[must_use]
    pub const fn range(&self) -> AddressRange {
        self.raw.range
    }

    /// Returns raw function-name bytes borrowed from the string table.
    #[must_use]
    pub const fn name(&self) -> &'data [u8] {
        self.name
    }

    /// Returns the function start address.
    #[must_use]
    pub const fn start(&self) -> u64 {
        self.raw.range.start
    }

    /// Decodes the record into an owned semantic model.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed or semantically invalid line, inline,
    /// merged-function, or call-site data, an invalid string reference, or a
    /// record type this crate cannot preserve.
    pub fn decode(&self) -> Result<Function> {
        let encoded = self.decode_encoded()?;
        super::owned::decode(self, encoded)
    }

    pub(super) fn decode_encoded(&self) -> Result<EncodedFunction> {
        function::decode(
            self.raw.data,
            self.layout.endian,
            self.layout.string_offset_size,
            self.raw.range.start,
        )
    }

    pub(super) fn string(&self, offset: u64) -> Result<&'data [u8]> {
        string_at(self.all_data, &self.layout.string_table, offset)
    }

    pub(super) fn file(&self, index: FileIndex) -> Result<(&'data [u8], &'data [u8])> {
        file_at(
            self.all_data,
            self.layout.endian,
            self.layout.string_offset_size,
            &self.layout.file_table,
            self.layout.file_count,
            &self.layout.string_table,
            index,
        )
    }
}

impl fmt::Debug for FunctionRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FunctionRef")
            .field("index", &self.index)
            .field("range", &self.raw.range)
            .field("name", &String::from_utf8_lossy(self.name))
            .finish()
    }
}

pub(super) fn string_at<'data>(
    data: &'data [u8],
    table: &Range<usize>,
    offset: u64,
) -> Result<&'data [u8]> {
    let offset = usize::try_from(offset).map_err(|_| Error::Overflow("string offset"))?;
    let start = table
        .start
        .checked_add(offset)
        .ok_or(Error::Overflow("string address"))?;
    if start >= table.end {
        return Err(Error::InvalidOffset {
            offset: start as u64,
            input_len: data.len(),
        });
    }
    let tail = data
        .get(start..table.end)
        .ok_or(Error::InvalidFormat("string table is outside the file"))?;
    let end = tail
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(Error::InvalidFormat("unterminated string"))?;
    tail.get(..end)
        .ok_or(Error::InvalidFormat("unterminated string"))
}

pub(super) fn file_at<'data>(
    data: &'data [u8],
    endian: Endian,
    string_offset_size: u8,
    file_table: &Range<usize>,
    file_count: u32,
    string_table: &Range<usize>,
    index: FileIndex,
) -> Result<(&'data [u8], &'data [u8])> {
    if index.get() >= file_count {
        return Err(Error::OutOfRange {
            field: "file index",
            value: u64::from(index),
            max: u64::from(file_count.saturating_sub(1)),
        });
    }
    let entry_size = usize::from(string_offset_size)
        .checked_mul(2)
        .ok_or(Error::Overflow("file entry size"))?;
    let entry_offset = usize::try_from(index.get())
        .map_err(|_| Error::Overflow("file index conversion"))?
        .checked_mul(entry_size)
        .ok_or(Error::Overflow("file entry offset"))?;
    let offset = file_table
        .start
        .checked_add(4)
        .and_then(|start| start.checked_add(entry_offset))
        .ok_or(Error::Overflow("file entry offset"))?;
    let end = offset
        .checked_add(entry_size)
        .ok_or(Error::Overflow("file entry end"))?;
    let bytes = data.get(offset..end).ok_or_else(|| Error::UnexpectedEof {
        offset,
        needed: entry_size,
        remaining: data.len().saturating_sub(offset),
    })?;
    let (directory, basename) = match (string_offset_size, endian) {
        (4, Endian::Little) => {
            decode_pair::<U32<LittleEndian>>(bytes, |value| u64::from(value.get()))?
        }
        (4, Endian::Big) => decode_pair::<U32<BigEndian>>(bytes, |value| u64::from(value.get()))?,
        (8, Endian::Little) => decode_pair::<U64<LittleEndian>>(bytes, |value| value.get())?,
        (8, Endian::Big) => decode_pair::<U64<BigEndian>>(bytes, |value| value.get())?,
        _ => {
            return Err(Error::OutOfRange {
                field: "string offset size",
                value: u64::from(string_offset_size),
                max: 8,
            });
        }
    };
    Ok((
        string_at(data, string_table, directory)?,
        string_at(data, string_table, basename)?,
    ))
}

fn decode_pair<T>(bytes: &[u8], decode: impl Fn(&T) -> u64) -> Result<(u64, u64)>
where
    [T; 2]: FromBytes + KnownLayout + Immutable,
{
    let pair = <[T; 2]>::ref_from_bytes(bytes)
        .map_err(|_| Error::InvalidFormat("file entry has an invalid typed layout"))?;
    let [directory, basename] = pair;
    Ok((decode(directory), decode(basename)))
}

/// Iterator over indexed functions in address-table order.
///
/// Yields `Result<FunctionRef>`, since a malformed record is only detected when
/// it is reached. Iteration is by ascending address, and functions that share
/// an address appear consecutively.
///
/// ```
/// use gsym::{AddressRange, Function, Gsym, GsymBuilder};
///
/// let mut builder = GsymBuilder::new();
/// builder.add_function(Function::new(AddressRange::new(0x2000, 0x2010), b"b"))?;
/// builder.add_function(Function::new(AddressRange::new(0x1000, 0x1010), b"a"))?;
/// let bytes = builder.to_bytes()?;
/// let gsym = Gsym::parse(&bytes)?;
///
/// let names = gsym
///     .functions()
///     .map(|function| Ok(function?.name().to_vec()))
///     .collect::<gsym::Result<Vec<_>>>()?;
/// assert_eq!(names, [b"a".to_vec(), b"b".to_vec()]);
/// # Ok::<(), gsym::Error>(())
/// ```
pub struct Functions<'gsym, D> {
    pub(super) gsym: &'gsym Gsym<D>,
    pub(super) next: usize,
}

impl<D> fmt::Debug for Functions<'_, D> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Functions")
            .field("next", &self.next)
            .finish_non_exhaustive()
    }
}

impl<'gsym, D: AsRef<[u8]>> Iterator for Functions<'gsym, D> {
    type Item = Result<FunctionRef<'gsym>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.gsym.layout.address_count as usize {
            return None;
        }
        let index = self.next;
        self.next = self.next.saturating_add(1);
        Some(self.gsym.function(index))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = (self.gsym.layout.address_count as usize).saturating_sub(self.next);
        (remaining, Some(remaining))
    }
}

impl<D: AsRef<[u8]>> ExactSizeIterator for Functions<'_, D> {}

impl<D: AsRef<[u8]>> std::iter::FusedIterator for Functions<'_, D> {}

#[cfg(test)]
mod tests {
    use crate::model::{AddressRange, FileEntry, Function, LineEntry};
    use crate::{Gsym, GsymBuilder};

    /// Builds an image whose only fault is semantic rather than structural.
    ///
    /// Every record decodes, but the encoded function size is shrunk after the
    /// fact so the second line row sits past the end of the function it
    /// belongs to. Structural decoding cannot notice, because line rows are
    /// encoded relative to the start address and never consult the size.
    fn image_with_a_line_outside_its_function() -> Vec<u8> {
        let mut builder = GsymBuilder::new();
        let file = builder.add_file(FileEntry::new("/src", "main.c")).unwrap();
        let mut function = Function::new(AddressRange::new(0x1000, 0x1020), b"main");
        function.lines = vec![
            LineEntry {
                address: 0x1000,
                file,
                line: 1,
            },
            LineEntry {
                address: 0x1010,
                file,
                line: 2,
            },
        ];
        builder.add_function(function).unwrap();
        let mut bytes = builder.to_bytes().unwrap();

        let offset = Gsym::parse(bytes.as_slice())
            .unwrap()
            .function_offset(0)
            .unwrap();
        bytes
            .split_at_mut(offset)
            .1
            .split_at_mut(4)
            .0
            .copy_from_slice(&8_u32.to_le_bytes());
        bytes
    }

    #[test]
    fn decode_rejects_every_record_verification_rejects() {
        let bytes = image_with_a_line_outside_its_function();
        let gsym = Gsym::parse(bytes.as_slice()).unwrap();

        assert!(gsym.verify().is_err());
        assert!(gsym.function(0).unwrap().decode_encoded().is_ok());
        assert!(gsym.function(0).unwrap().decode().is_err());
    }
}
