use crate::endian::{Cursor, Endian};
use crate::error::{Error, Result};
use crate::format::function::InfoType;
use crate::format::leb::read_uleb;
use crate::format::line;
use crate::model::{FileIndex, LineEntry, Lookup, LookupFrame};
use smallvec::SmallVec;

use super::function::RawFunction;
use super::{FrameLookupOptions, Gsym, LookupOptions, LookupScratch, RawInlineFrame};

#[derive(Clone, Copy, Debug, Default)]
struct ScannedRecords<'data> {
    line: Option<LineEntry>,
    inline: Option<&'data [u8]>,
    call_sites: Option<&'data [u8]>,
}

#[derive(Clone, Copy, Debug)]
struct FrameRequest<'data> {
    address: u64,
    options: LookupOptions,
    function: RawFunction<'data>,
}

#[derive(Clone, Copy, Debug)]
struct PreparedFrames<'data> {
    function_name: &'data [u8],
    directory: &'data [u8],
    basename: &'data [u8],
    line: u32,
    count: usize,
    call_sites: Option<&'data [u8]>,
}

impl<D: AsRef<[u8]>> Gsym<D> {
    /// Resolves an unslid virtual address with the default lookup options.
    ///
    /// `address` must be an address of the image the file describes. An address
    /// from a running PIE executable or shared object needs its load bias
    /// removed first; see
    /// [`docs::symbolication`](crate::docs::symbolication).
    ///
    /// Returns `Ok(None)` when no function covers the address, which is the
    /// normal answer for padding between functions and for addresses belonging
    /// to another module. The result borrows names and paths from this reader.
    ///
    /// Frames come back innermost first. Use
    /// [`Self::lookup_with_options`] to skip record kinds you do not need, or
    /// [`Self::for_each_frame`] to resolve an address without allocating.
    ///
    /// # Errors
    ///
    /// Returns an error if the matched function's data is malformed.
    ///
    /// ```
    /// use gsym::{AddressRange, FileEntry, Function, Gsym, GsymBuilder, LineEntry};
    ///
    /// let mut builder = GsymBuilder::new();
    /// let file = builder.add_file(FileEntry::new(b"/src", b"main.rs"))?;
    /// builder.add_function(Function {
    ///     lines: vec![LineEntry::new(0x1000, file, 7)],
    ///     ..Function::new(AddressRange::new(0x1000, 0x1010), b"main")
    /// })?;
    /// let bytes = builder.to_bytes()?;
    /// let gsym = Gsym::parse(&bytes)?;
    ///
    /// let hit = gsym.lookup(0x1004)?.expect("covered address");
    /// assert_eq!(hit.frames()[0].name, b"main");
    /// assert_eq!(hit.frames()[0].basename, b"main.rs");
    /// assert_eq!(hit.frames()[0].line, 7);
    ///
    /// assert!(gsym.lookup(0x2000)?.is_none());
    /// # Ok::<(), gsym::Error>(())
    /// ```
    pub fn lookup(&self, address: u64) -> Result<Option<Lookup<'_>>> {
        let mut scratch = LookupScratch::default();
        self.lookup_with_options(address, LookupOptions::default(), &mut scratch)
    }

    /// Resolves an address while reusing caller-owned inline scratch storage.
    ///
    /// Same result as [`Self::lookup`], with control over which optional
    /// records are read and with the scratch buffer supplied by the caller.
    /// The returned [`Lookup`] still owns its frames; use
    /// [`Self::for_each_frame`] to avoid that allocation as well.
    ///
    /// # Errors
    ///
    /// Returns an error if the matched function's data is malformed.
    pub fn lookup_with_options<'data>(
        &'data self,
        address: u64,
        options: LookupOptions,
        scratch: &mut LookupScratch,
    ) -> Result<Option<Lookup<'data>>> {
        scratch.clear();
        let Some(function) = self.matching_function(address)? else {
            return Ok(None);
        };
        let request = FrameRequest {
            address,
            options,
            function,
        };
        let prepared = self.prepare_frames(request, scratch)?;
        let mut frames = Vec::with_capacity(prepared.count);
        self.emit_frames(request, prepared, scratch, |frame| {
            frames.push(frame);
        })?;
        self.finish_lookup(address, function, prepared.call_sites, frames)
            .map(Some)
    }

    /// Visits source frames without allocating an output collection.
    ///
    /// Frames are yielded innermost first and borrow their names and paths from
    /// the reader. Sizing the [`LookupScratch`] with
    /// [`LookupScratch::with_capacity`] keeps repeated lookups allocation-free.
    ///
    /// Returns whether a function covered the address. The visitor is not
    /// called when it returns `false`. Call-site patterns are not reported
    /// here; use [`Self::lookup_with_options`] when they are needed.
    ///
    /// ```
    /// use gsym::{
    ///     AddressRange, FrameLookupOptions, Function, Gsym, GsymBuilder,
    ///     LookupScratch,
    /// };
    ///
    /// let mut builder = GsymBuilder::new();
    /// builder.add_function(Function::new(
    ///     AddressRange::new(0x3000, 0x3010),
    ///     b"visited",
    /// ))?;
    /// let bytes = builder.to_bytes()?;
    /// let gsym = Gsym::parse(bytes)?;
    ///
    /// let mut scratch = LookupScratch::with_capacity(8);
    /// let mut names = Vec::new();
    /// let found = gsym.for_each_frame(
    ///     0x3004,
    ///     FrameLookupOptions::default(),
    ///     &mut scratch,
    ///     |frame| names.push(frame.name.to_vec()),
    /// )?;
    /// assert!(found);
    /// assert_eq!(names, [b"visited".to_vec()]);
    /// # Ok::<(), gsym::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if the matched function's data is malformed.
    pub fn for_each_frame<'data>(
        &'data self,
        address: u64,
        options: FrameLookupOptions,
        scratch: &mut LookupScratch,
        visitor: impl FnMut(LookupFrame<'data>),
    ) -> Result<bool> {
        scratch.clear();
        let Some(function) = self.matching_function(address)? else {
            return Ok(false);
        };
        self.visit_function_frames(
            FrameRequest {
                address,
                options: options.into(),
                function,
            },
            scratch,
            visitor,
        )?;
        Ok(true)
    }

    fn matching_function(&self, address: u64) -> Result<Option<RawFunction<'_>>> {
        let Some(mut index) = self.find_address_index(address)? else {
            return Ok(None);
        };
        let first_start = self.address(index)?;
        while index > 0 && self.address(index.saturating_sub(1))? == first_start {
            index = index.saturating_sub(1);
        }
        let count = self.layout.address_count as usize;
        while index < count {
            let raw = self.raw_function_at(index, first_start)?;
            if raw.range.is_empty() || raw.range.contains(address) {
                return Ok(Some(raw));
            }
            index = index.saturating_add(1);
            if index >= count || self.address(index)? != first_start {
                break;
            }
        }
        Ok(None)
    }

    fn visit_function_frames<'data>(
        &'data self,
        request: FrameRequest<'data>,
        scratch: &mut LookupScratch,
        visitor: impl FnMut(LookupFrame<'data>),
    ) -> Result<Option<&'data [u8]>> {
        let prepared = self.prepare_frames(request, scratch)?;
        self.emit_frames(request, prepared, scratch, visitor)?;
        Ok(prepared.call_sites)
    }

    fn prepare_frames<'data>(
        &'data self,
        request: FrameRequest<'data>,
        scratch: &mut LookupScratch,
    ) -> Result<PreparedFrames<'data>> {
        let records = self.scan_records(request)?;
        let function = request.function;
        let function_name = self.string(function.name)?;
        let (directory, basename, line_number) = if let Some(row) = records.line {
            let (directory, basename) = self.file(row.file)?;
            (directory, basename, row.line)
        } else {
            (&[][..], &[][..], 0)
        };

        if let Some(payload) = records.inline {
            let mut cursor = Cursor::new(payload, self.layout.endian);
            let mut scan = InlineScan {
                string_offset_size: self.layout.string_offset_size,
                address: request.address,
                frames: &mut scratch.inline_frames,
            };
            let (present, _) =
                scan_inline_node(&mut cursor, function.range.start, true, 0, &mut scan)?;
            if !present || !cursor.is_empty() {
                return Err(Error::InvalidFormat("malformed inline-info payload"));
            }
        }

        Ok(PreparedFrames {
            function_name,
            directory,
            basename,
            line: line_number,
            count: scratch.inline_frames.len().max(1),
            call_sites: records.call_sites,
        })
    }

    fn emit_frames<'data>(
        &'data self,
        request: FrameRequest<'data>,
        prepared: PreparedFrames<'data>,
        scratch: &LookupScratch,
        mut visitor: impl FnMut(LookupFrame<'data>),
    ) -> Result<()> {
        let PreparedFrames {
            function_name,
            directory,
            basename,
            line: line_number,
            ..
        } = prepared;
        let address = request.address;
        let function = request.function;

        if scratch.inline_frames.is_empty() {
            visitor(LookupFrame {
                name: function_name,
                directory,
                basename,
                line: line_number,
                offset: address.saturating_sub(function.range.start),
                inlined: false,
            });
        } else {
            let nodes = &scratch.inline_frames;
            for (index, node) in nodes.iter().enumerate().rev() {
                let (frame_directory, frame_basename, frame_line) =
                    if let Some(callee) = nodes.get(index.saturating_add(1)) {
                        let (directory, basename) = self.file(callee.call_file)?;
                        (directory, basename, callee.call_line)
                    } else {
                        (directory, basename, line_number)
                    };
                visitor(LookupFrame {
                    name: if node.name == 0 {
                        function_name
                    } else {
                        self.string(node.name)?
                    },
                    directory: frame_directory,
                    basename: frame_basename,
                    line: frame_line,
                    offset: address.saturating_sub(node.start),
                    inlined: index != 0,
                });
            }
        }
        Ok(())
    }

    fn scan_records<'data>(&self, request: FrameRequest<'data>) -> Result<ScannedRecords<'data>> {
        let FrameRequest {
            address,
            options,
            function,
        } = request;
        if !options.line_information && !options.inline_frames && !options.call_sites {
            return Ok(ScannedRecords::default());
        }
        let mut scanned = ScannedRecords::default();
        for record in function.records(self.layout.endian) {
            let record = record?;
            match record.kind {
                InfoType::LineTable => {
                    if options.line_information {
                        scanned.line = line::lookup(
                            record.payload,
                            self.layout.endian,
                            function.range.start,
                            address,
                        )?;
                    }
                }
                InfoType::Inline => {
                    if options.inline_frames {
                        scanned.inline = Some(record.payload);
                    }
                }
                InfoType::Merged | InfoType::Unknown(_) => {}
                InfoType::CallSite => {
                    if options.call_sites {
                        scanned.call_sites = Some(record.payload);
                    }
                }
            }
        }
        Ok(scanned)
    }

    fn finish_lookup<'data>(
        &'data self,
        address: u64,
        function: RawFunction<'data>,
        call_site_payload: Option<&[u8]>,
        frames: Vec<LookupFrame<'data>>,
    ) -> Result<Lookup<'data>> {
        let mut call_site_patterns = Vec::new();
        if let Some(payload) = call_site_payload {
            read_call_site_patterns(
                payload,
                self.layout.endian,
                self.layout.string_offset_size,
                address.saturating_sub(function.range.start),
                self,
                &mut call_site_patterns,
            )?;
        }
        Ok(Lookup::new(
            address,
            function.range,
            frames.into_boxed_slice(),
            call_site_patterns.into_boxed_slice(),
        ))
    }
}

struct InlineScan<'scratch> {
    string_offset_size: u8,
    address: u64,
    frames: &'scratch mut SmallVec<[RawInlineFrame; 4]>,
}

fn scan_inline_node(
    cursor: &mut Cursor<'_>,
    base: u64,
    collect: bool,
    depth: usize,
    scan: &mut InlineScan<'_>,
) -> Result<(bool, bool)> {
    const MAX_DEPTH: usize = 256;
    if depth > MAX_DEPTH {
        return Err(Error::Limit {
            context: "inline tree depth",
            value: depth as u64,
            limit: MAX_DEPTH as u64,
        });
    }
    let count = read_uleb(cursor)?;
    if count == 0 {
        return Ok((false, false));
    }
    if count > cursor.remaining() as u64 / 2 {
        return Err(Error::InvalidFormat(
            "inline range count exceeds remaining payload",
        ));
    }
    let mut contains = false;
    let mut first_start = None;
    for _ in 0..count {
        let start = base
            .checked_add(read_uleb(cursor)?)
            .ok_or(Error::Overflow("inline range start"))?;
        let end = start
            .checked_add(read_uleb(cursor)?)
            .ok_or(Error::Overflow("inline range end"))?;
        first_start.get_or_insert(start);
        if collect {
            contains |= start <= scan.address && scan.address < end;
        }
    }
    let first_start = first_start.ok_or(Error::InvalidFormat(
        "inline node declares no address range",
    ))?;
    let has_children = cursor.read_u8()? != 0;
    let name = cursor.read_uint(scan.string_offset_size)?;
    let call_file = read_uleb(cursor)?;
    let call_line = read_uleb(cursor)?;
    let matched = collect && contains;
    let original_len = scan.frames.len();
    if matched && name != 0 {
        scan.frames.push(RawInlineFrame {
            name,
            call_file: FileIndex::new(u32::try_from(call_file).map_err(|_| Error::OutOfRange {
                field: "inline call-file index",
                value: call_file,
                max: u64::from(u32::MAX),
            })?),
            call_line: u32::try_from(call_line).map_err(|_| Error::OutOfRange {
                field: "inline call-line",
                value: call_line,
                max: u64::from(u32::MAX),
            })?,
            start: first_start,
        });
    }
    if has_children {
        let child_base = first_start;
        let mut found_child = false;
        loop {
            let (present, child_matched) = scan_inline_node(
                cursor,
                child_base,
                matched && !found_child,
                depth.saturating_add(1),
                scan,
            )?;
            if !present {
                break;
            }
            found_child |= child_matched;
        }
    }
    if !matched {
        scan.frames.truncate(original_len);
    }
    Ok((true, matched))
}

fn read_call_site_patterns<'data, D: AsRef<[u8]>>(
    payload: &[u8],
    endian: Endian,
    string_offset_size: u8,
    return_offset: u64,
    gsym: &'data Gsym<D>,
    output: &mut Vec<&'data [u8]>,
) -> Result<()> {
    let mut cursor = Cursor::new(payload, endian);
    let count = cursor.read_u32()?;
    for _ in 0..count {
        let candidate = cursor.read_u64()?;
        let _flags = cursor.read_u8()?;
        let regex_count = cursor.read_u32()?;
        for _ in 0..regex_count {
            let offset = cursor.read_uint(string_offset_size)?;
            if candidate == return_offset {
                output.push(gsym.string(offset)?);
            }
        }
    }
    if !cursor.is_empty() {
        return Err(Error::InvalidFormat("trailing call-site bytes"));
    }
    Ok(())
}
