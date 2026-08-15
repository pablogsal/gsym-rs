# GSYM compatibility coverage

This audit uses LLVM commit `dd19d5210352f004758d0a877b396ea9f04cfd70`.
It enumerates every `TEST` in `GSYMTest.cpp`, `GSYMV2Test.cpp`, and
`GsymDataExtractorTest.cpp` (134 cases), every `.test` and `.yaml` input under
`llvm/test/tools/llvm-gsymutil` (18 files), and LLVM's GSYM-only symbolizer test
(21 command variants).

The status terms have narrow meanings:

- Ported: a direct behavioral case is in the `compat_*` integration suite.
- Equivalent: an existing Rust test covers the same wire-format or
  conversion contract without copying LLVM's C++ harness.
- Excluded: the case tests LLVM-only machinery or a non-Linux input format.
- Blocked: the behavior is in scope, but the public Rust implementation or
  a practical fixture is not yet sufficient for a faithful test.

LLVM's creators emit canonical address-offset widths 1, 2, 4, and 8. The port
uses LLVM's exact forcing deltas for target byte sizes 1 through 8 and verifies
the same rounding for v1 and v2 in both byte orders. The v2 decoder separately
accepts every header width from 1 through 8, matching current LLVM. Tests of
multi-gigabyte table offsets use hand-built semantic records rather than
allocating more than 4 GiB in a normal test job.

| Suite | Ported | Equivalent | Excluded | Blocked |
|---|---:|---:|---:|---:|
| Upstream GSYM units (134) | 43 | 90 | 1 | 0 |
| `llvm-gsymutil` inputs (18) | 6 | 3 | 9 | 0 |
| GSYM-only symbolizer semantics | 0 | 1 | 0 | 0 |

## `GsymDataExtractorTest.cpp` (5)

| Upstream case | Status | Rust coverage or reason |
|---|---|---|
| `GsymDataExtractorTest.DefaultStringOffsetSize` | Equivalent | `src/format/tests/cursor_leb.rs::data_extractor_reads_every_integer_width_and_endian` covers the default cursor width. |
| `GsymDataExtractorTest.ExplicitStringOffsetSize` | Equivalent | The same test covers explicit 1-through-8-byte reads in both byte orders. |
| `GsymDataExtractorTest.SubrangeConstructor` | Equivalent | Independent sliced-input parsing is exercised by `tests/reader/lookup.rs` and every-truncation tests in `tests/reader/malformed.rs`. |
| `GsymDataExtractorTest.GetStringOffset` | Equivalent | `data_extractor_reads_every_integer_width_and_endian` covers width-aware integer/string offsets. |
| `GsymDataExtractorTest.GetStringOffsetCursor` | Equivalent | The same test covers cursor advancement and error state on truncated reads. |

## `GSYMV2Test.cpp` (23)

| Upstream case | Status | Rust coverage or reason |
|---|---|---|
| `GSYMV2Test.TestCreatorV2DoubleFinalize` | Excluded | `GsymBuilder::to_bytes` consumes the builder, so a second finalization is prevented by Rust ownership rather than a runtime error. |
| `GSYMV2Test.TestCreatorV2HeaderAndGlobalDataLittle` | Equivalent | `src/format/tests/headers_layout.rs::v2_directory_layout_matches_reference_with_and_without_uuid` checks the little-endian directory layout. |
| `GSYMV2Test.TestCreatorV2HeaderAndGlobalDataBig` | Equivalent | The same format test plus `tests/writer/roundtrip.rs::all_versions_and_byte_orders_roundtrip` covers big endian. |
| `GSYMV2Test.TestCreatorV2HeaderAndGlobalDataNoUUID` | Equivalent | The format test explicitly encodes the directory without a UUID section. |
| `GSYMV2Test.TestCreatorV2AddrInfoOffsetsPointToFunctionInfo` | Equivalent | `src/format/tests/headers_layout.rs::v2_directory_layout_matches_reference_with_and_without_uuid` validates section offsets; reader round trips dereference each function offset. |
| `GSYMV2Test.TestCreatorV2UUIDSection` | Ported | `v1_rejects_oversized_uuid_while_v2_roundtrips_it` validates the v2 UUID section through the public reader. |
| `GSYMV2Test.TestCreatorV2SectionAlignment` | Equivalent | `v2_directory_layout_matches_reference_with_and_without_uuid` checks LLVM-compatible aligned section placement. |
| `GSYMV2Test.TestReaderV2ParseHandCrafted` | Equivalent | `tests/reader/lookup.rs` builds v2 fixtures independently of the crate writer. |
| `GSYMV2Test.TestReaderV2GetFunctionInfoHandCrafted` | Equivalent | `rich_inline_callsite_and_merged_lookup_v1_v2_both_endians` decodes independent function records. |
| `GSYMV2Test.TestReaderV2LookupHandCrafted` | Equivalent | `boundary_gap_and_zero_size_cases_v1_v2_both_endians` exercises lookups on independent v2 bytes. |
| `GSYMV2Test.TestReaderV2InvalidMagic` | Equivalent | `tests/reader/malformed.rs::rejects_bad_magic_version_and_address_width`. |
| `GSYMV2Test.TestReaderV2TooSmall` | Equivalent | `tests/reader/malformed.rs::rejects_empty_and_short_inputs`. |
| `GSYMV2Test.TestReaderV2TruncatedFileTable` | Equivalent | `tests/reader/malformed.rs::every_truncation_of_valid_files_is_rejected_without_panicking`. |
| `GSYMV2Test.TestRoundTripGetFunctionInfoAtIndex` | Equivalent | `tests/writer/roundtrip.rs::all_versions_and_byte_orders_roundtrip` and the public `function` calls in `tests/writer/finalize.rs`. |
| `GSYMV2Test.TestRoundTripAddressTable` | Equivalent | `all_versions_and_byte_orders_roundtrip` validates address indexing and lookup. |
| `GSYMV2Test.TestRoundTripLargeAddressOffsets` | Ported | `creator_selects_canonical_address_width_at_every_boundary` includes the 4-to-8-byte transition. |
| `GSYMV2Test.TestRoundTripSwappedSingleFunction` | Ported | The width-boundary test writes and verifies single-function big-endian v2 files. |
| `GSYMV2Test.TestRoundTripSwappedMultipleFunctions` | Ported | The width-boundary test writes and verifies multi-function big-endian v2 files. |
| `GSYMV2Test.TestRoundTripSwappedLookup` | Equivalent | `tests/reader/parse.rs::reads_independent_v2_fixtures_in_both_byte_orders`. |
| `GSYMV2Test.TestRoundTripSwappedAddressTable` | Equivalent | Independent v2 fixtures and the all-byte-orders round trip validate the swapped table. |
| `GSYMV2Test.TestVersionRoundTripV1ToV2ToV1` | Ported | `tests/writer/transcode.rs` decodes and re-encodes complete files across both versions and byte orders. |
| `GSYMV2Test.TestVersionRoundTripV2ToV1ToV2` | Ported | Whole-file transcoding is covered, including explicit v1 limit failures. |
| `GSYMV2Test.TestV2SegmentingSize` | Ported | Exact encoded-size partitioning and independent shard verification are covered by `segments_are_independent_size_bounded_and_cover_every_function`. |

## `GSYMTest.cpp` (106)

| Upstream case | Status | Rust coverage or reason |
|---|---|---|
| `GSYMTest.TestFileEntry` | Ported | `string_and_file_tables_reserve_zero_and_deduplicate_entries` validates reserved index zero and file round trips. |
| `GSYMTest.TestFunctionInfo` | Equivalent | `src/format/tests/function_records.rs::function_info_round_trips_every_optional_record`. |
| `GSYMTest.TestFunctionInfoDecodeErrors` | Equivalent | `function_info_reports_reference_decode_and_encode_errors` and `function_info_rejects_truncated_payloads_unknown_types_and_bad_end_markers`. |
| `GSYMTest.TestFunctionInfoEncodeErrors` | Equivalent | `function_info_reports_reference_decode_and_encode_errors`. |
| `GSYMTest.TestFunctionInfoEncoding` | Equivalent | `function_info_matches_reference_minimal_bytes` checks LLVM-compatible bytes. |
| `GSYMTest.TestInlineInfo` | Equivalent | `inline_ranges_and_sibling_terminators_match_llvm` and the rich independent reader fixture. |
| `GSYMTest.TestInlineInfoEncodeErrors` | Equivalent | `inline_encoding_rejects_empty_uncontained_unsorted_and_truncated_trees`. |
| `GSYMTest.TestInlineInfoDecodeErrors` | Equivalent | The same test covers truncated and malformed inline trees. |
| `GSYMTest.TestLineEntry` | Equivalent | `src/format/tests/line_program.rs::line_table_handles_mixed_delta_and_same_address_rows`. |
| `GSYMTest.TestStringTable` | Ported | The reservation/deduplication test checks empty offset zero and repeated strings through public APIs. |
| `GSYMTest.TestFileWriter` | Equivalent | `src/format/tests/cursor_leb.rs::file_writer_round_trips_fixed_width_leb_alignment_and_fixups`. |
| `GSYMTest.TestWriteUnsigned` | Equivalent | `src/format/tests/cursor_leb.rs::unsigned_writer_supports_widths_one_through_eight`. |
| `GSYMTest.TestAddressRangeEncodeDecode` | Equivalent | `function_info_round_trips_every_optional_record` covers a single encoded inline range. |
| `GSYMTest.TestAddressRangesEncodeDecode` | Equivalent | `inline_ranges_and_sibling_terminators_match_llvm` covers multiple ranges and children. |
| `GSYMTest.TestLineTable` | Equivalent | `line_table_handles_mixed_delta_and_same_address_rows`. |
| `GSYMTest.TestLineTableDecodeErrors` | Equivalent | `line_table_reports_truncation_and_encode_errors`. |
| `GSYMTest.TestLineTableEncodeErrors` | Equivalent | `line_table_reports_truncation_and_encode_errors`. |
| `GSYMTest.TestHeaderEncodeErrors` | Equivalent | `src/format/tests/headers_layout.rs::v1_headers_match_reference_fields_endianness_and_errors`. |
| `GSYMTest.TestHeaderDecodeErrors` | Equivalent | The same test covers bad v1 magic, version, width, and truncation. |
| `GSYMTest.TestHeaderV2EncodeErrors` | Equivalent | `src/format/tests/headers_layout.rs::v2_headers_accept_supported_widths_and_reject_invalid_fields`. |
| `GSYMTest.TestHeaderV2DecodeErrors` | Equivalent | `v2_directory_rejects_reference_errors_and_structural_corruption`. |
| `GSYMTest.TestHeaderEncodeDecode` | Equivalent | `v1_headers_match_reference_fields_endianness_and_errors`. |
| `GSYMTest.TestHeaderV2EncodeDecode` | Equivalent | `v2_headers_accept_supported_widths_and_reject_invalid_fields`. |
| `GSYMTest.TestGsymCreatorV1EncodeErrors` | Ported | The v1 UUID limit and common function-size wire limit are asserted through the public builder. |
| `GSYMTest.TestGsymCreatorV2EncodeErrors` | Equivalent | Builder validation tests and v2 directory corruption tests cover representability and layout errors. |
| `GSYMTest.TestGsymCreatorV11ByteAddrOffsets` | Ported | The canonical-width boundary test covers v1 width 1 in both byte orders. |
| `GSYMTest.TestGsymCreatorV12ByteAddrOffsets` | Ported | The canonical-width boundary test covers v1 width 2 in both byte orders. |
| `GSYMTest.TestGsymCreatorV13ByteAddrOffsets` | Ported | LLVM expects a three-byte forcing delta to round up to width 4; the canonical-width test uses the same delta. |
| `GSYMTest.TestGsymCreatorV14ByteAddrOffsets` | Ported | The canonical-width boundary test covers v1 width 4 in both byte orders. |
| `GSYMTest.TestGsymCreatorV15ByteAddrOffsets` | Ported | LLVM expects a five-byte forcing delta to round up to width 8; the canonical-width test uses the same delta. |
| `GSYMTest.TestGsymCreatorV16ByteAddrOffsets` | Ported | LLVM expects a six-byte forcing delta to round up to width 8; the canonical-width test uses the same delta. |
| `GSYMTest.TestGsymCreatorV17ByteAddrOffsets` | Ported | LLVM expects a seven-byte forcing delta to round up to width 8; the canonical-width test uses the same delta. |
| `GSYMTest.TestGsymCreatorV18ByteAddrOffsets` | Ported | The canonical-width boundary test covers v1 width 8 in both byte orders. |
| `GSYMTest.TestGsymCreatorV21ByteAddrOffsets` | Ported | The canonical-width boundary test covers v2 width 1 in both byte orders. |
| `GSYMTest.TestGsymCreatorV22ByteAddrOffsets` | Ported | The canonical-width boundary test covers v2 width 2 in both byte orders. |
| `GSYMTest.TestGsymCreatorV23ByteAddrOffsets` | Ported | LLVM expects a three-byte forcing delta to round up to width 4; the canonical-width test uses the same delta. |
| `GSYMTest.TestGsymCreatorV24ByteAddrOffsets` | Ported | The canonical-width boundary test covers v2 width 4 in both byte orders. |
| `GSYMTest.TestGsymCreatorV25ByteAddrOffsets` | Ported | LLVM expects a five-byte forcing delta to round up to width 8; the canonical-width test uses the same delta. |
| `GSYMTest.TestGsymCreatorV26ByteAddrOffsets` | Ported | LLVM expects a six-byte forcing delta to round up to width 8; the canonical-width test uses the same delta. |
| `GSYMTest.TestGsymCreatorV27ByteAddrOffsets` | Ported | LLVM expects a seven-byte forcing delta to round up to width 8; the canonical-width test uses the same delta. |
| `GSYMTest.TestGsymCreatorV28ByteAddrOffsets` | Ported | The canonical-width boundary test covers v2 width 8 in both byte orders. |
| `GSYMTest.TestGsymReaderV1` | Equivalent | `tests/reader/parse.rs::reads_independent_v1_fixtures_in_both_byte_orders`. |
| `GSYMTest.TestGsymReaderV2` | Equivalent | `tests/reader/parse.rs::reads_independent_v2_fixtures_in_both_byte_orders`. |
| `GSYMTest.TestGsymLookups` | Equivalent | `tests/reader/lookup.rs::boundary_gap_and_zero_size_cases_v1_v2_both_endians`. |
| `GSYMTest.TestGsymLookupsV2` | Equivalent | The same independent-fixture test covers v2. |
| `GSYMTest.TestDWARFFunctionWithAddresses` | Equivalent | `tests/convert/dwarf.rs::elf_dwarf_and_gsym_only_equivalent_preserves_lines_and_inlines` converts a real Linux DWARF image with v1 defaults. |
| `GSYMTest.TestDWARFFunctionWithAddressesV2` | Ported | The CLI end-to-end test converts a real Linux DWARF image as v2 and verifies indexed functions. |
| `GSYMTest.TestDWARFFunctionWithAddressAndOffset` | Equivalent | The Linux converter test exercises linked ELF virtual addresses rather than object-relative offsets. |
| `GSYMTest.TestDWARFFunctionWithAddressAndOffsetV2` | Equivalent | The v2 CLI conversion and lookup exercise the same relocated-address result. |
| `GSYMTest.TestDWARFStructMethodNoMangled` | Equivalent | The converter's real Rust/C++-style DWARF path accepts functions without linkage names and preserves their source name. |
| `GSYMTest.TestDWARFStructMethodNoMangledV2` | Equivalent | The same conversion contract is format-independent and the CLI v2 path verifies it. |
| `GSYMTest.TestDWARFTextRanges` | Equivalent | Linux conversion derives executable `PT_LOAD` ranges and filters functions against them. |
| `GSYMTest.TestDWARFTextRangesV2` | Equivalent | The converter finalization is shared by v1 and v2. |
| `GSYMTest.TestEmptySymbolEndAddressOfTextRanges` | Ported | `finalize_repairs_the_last_zero_sized_symbol_to_the_text_end` directly covers the final text-range repair. |
| `GSYMTest.TestEmptySymbolEndAddressOfTextRangesV2` | Ported | The same public finalizer is version-independent and is encoded/verified for v2 by the width matrix. |
| `GSYMTest.TestDWARFInlineInfo` | Equivalent | The real ELF converter test asserts inline-node production and source lookup. |
| `GSYMTest.TestDWARFInlineInfoV2` | Equivalent | The v2 CLI conversion verifies the shared inline encoding path. |
| `GSYMTest.TestDWARFNoLines` | Equivalent | `tests/convert/inputs.rs::converts_the_linked_test_image_from_symbols` covers symbol-only functions without line tables. |
| `GSYMTest.TestDWARFNoLinesV2` | Equivalent | The writer/reader round trips optional line-table absence in both versions. |
| `GSYMTest.TestDWARFDeadStripAddr4` | Equivalent | `dead_and_invalid_ranges_are_rejected_without_losing_live_ranges` verifies full containment and stale-range rejection. |
| `GSYMTest.TestDWARFDeadStripAddr4V2` | Equivalent | Dead-range filtering occurs before version-specific writing. |
| `GSYMTest.TestDWARFDeadStripAddr8` | Equivalent | The same range test covers 64-bit addresses and oversized ranges. |
| `GSYMTest.TestDWARFDeadStripAddr8V2` | Equivalent | Filtering is version-independent. |
| `GSYMTest.TestGsymCreatorV1MultipleSymbolsWithNoSize` | Ported | `finalize_combines_multiple_zero_sized_symbols_at_one_address` checks v1 in both byte orders. |
| `GSYMTest.TestGsymCreatorV2MultipleSymbolsWithNoSize` | Ported | The same test checks v2 in both byte orders. |
| `GSYMTest.TestMangledNameReplacement` | Ported | The writer test checks both insertion orders, rich line retention, and Itanium name replacement. |
| `GSYMTest.TestMangledNameReplacementV2` | Ported | The same test runs for v2. |
| `GSYMTest.TestMangledNameReplacementNegative` | Ported | `mangled_name_replacement_rejects_unrelated_names`. |
| `GSYMTest.TestMangledNameReplacementNegativeV2` | Ported | The finalization rule is version-independent; positive Swift replacement is also directly tested in v2 big endian. |
| `GSYMTest.TestDuplicateRangeKeepsCallSites` | Ported | `duplicate_range_keeps_call_site_information` preserves and looks up the regex collection. |
| `GSYMTest.TestDuplicateRangeKeepsCallSitesV2` | Ported | The direct call-site test uses v2. |
| `GSYMTest.TestGsymSegmenting` | Ported | `DecodedGsym::segments` emits independent size-bounded v1 shards. |
| `GSYMTest.TestGsymSegmentingV2` | Ported | The segmentation test emits and verifies v2 big-endian shards. |
| `GSYMTest.TestGsymSegmentingNoBase` | Ported | Decoding normalizes an implicit source base before deterministic segmentation. |
| `GSYMTest.TestGsymSegmentingNoBaseV2` | Ported | The same semantic path supports v2 output. |
| `GSYMTest.TestDWARFInlineRangeScopes` | Equivalent | Nested inline range containment is covered by format tests and real ELF inline lookup. |
| `GSYMTest.TestDWARFInlineRangeScopesV2` | Equivalent | The inline semantic model and validation are shared by both versions. |
| `GSYMTest.TestDWARFEmptyInline` | Equivalent | Inline encoding treats absence as optional; malformed empty nodes are rejected by `inline_encoding_rejects_empty_uncontained_unsorted_and_truncated_trees`. |
| `GSYMTest.TestDWARFEmptyInlineV2` | Equivalent | The same model validation is shared by v2. |
| `GSYMTest.TestFinalizeForLineTables` | Equivalent | `line_table_handles_mixed_delta_and_same_address_rows` plus writer round trips cover finalized row order and deltas. |
| `GSYMTest.TestFinalizeForLineTablesV2` | Equivalent | Line-table finalization is shared and v2 round trips are covered. |
| `GSYMTest.TestRangeWarnings` | Equivalent | Range parsing skips malformed entries, increments rejected counts, and records address-bearing diagnostics. |
| `GSYMTest.TestRangeWarningsV2` | Equivalent | Diagnostics occur before encoding. |
| `GSYMTest.TestEmptyRangeWarnings` | Equivalent | Empty ranges are rejected by `is_live_range` without affecting adjacent live ranges. |
| `GSYMTest.TestEmptyRangeWarningsV2` | Equivalent | Empty-range handling is version-independent. |
| `GSYMTest.TestEmptyLinkageName` | Equivalent | Empty linkage names fall back to source names in the Linux DWARF converter; empty final function names are rejected. |
| `GSYMTest.TestEmptyLinkageNameV2` | Equivalent | Name selection is performed before version-specific writing. |
| `GSYMTest.TestLineTablesWithEmptyRanges` | Equivalent | `line_rows_clamp_only_inside_their_statement_sequence` verifies empty gaps do not inherit unrelated rows. |
| `GSYMTest.TestLineTablesWithEmptyRangesV2` | Equivalent | Line filtering occurs before encoding. |
| `GSYMTest.TestHandlingOfInvalidFileIndexes` | Equivalent | `tests/reader/malformed.rs::invalid_function_offsets_strings_and_files_fail_verification` rejects invalid file references. |
| `GSYMTest.TestHandlingOfInvalidFileIndexesV2` | Equivalent | The same independent-fixture test covers v2. |
| `GSYMTest.TestLookupsOfOverlappingAndUnequalRanges` | Equivalent | `tests/reader/lookup.rs::equal_start_and_overlapping_unequal_ranges_choose_first_containing`. |
| `GSYMTest.TestLookupsOfOverlappingAndUnequalRangesV2` | Equivalent | The same test runs on v2 fixtures. |
| `GSYMTest.TestUnableToLocateDWO` | Ported | A real `-gsplit-dwarf` fixture verifies missing DWO diagnostics and continued symbol conversion. |
| `GSYMTest.TestUnableToLocateDWOV2` | Ported | Missing-DWO behavior is independent of output version. |
| `GSYMTest.TestDWARFTransformNoErrorForMissingFileDecl` | Equivalent | Missing file declarations remain non-fatal; a separate real DWARF fixture verifies `DW_AT_decl_file`/`DW_AT_decl_line` fallback when a valid declaration is present but the line program has no rows. |
| `GSYMTest.TestDWARFTransformNoErrorForMissingFileDeclV2` | Equivalent | Missing-declaration behavior occurs before encoding. |
| `GSYMTest.TestFunctionInfoLargeNameOffset` | Equivalent | `src/format/tests/function_records.rs::function_and_all_nested_records_preserve_large_v2_offsets`. |
| `GSYMTest.TestInlineInfoLargeNameOffset` | Equivalent | The same format test covers inline names above `u32::MAX` without allocating a giant table. |
| `GSYMTest.TestCallSiteInfoLargeMatchRegex` | Equivalent | The same format test covers 64-bit call-site regex offsets. |
| `GSYMTest.TestCallSiteInfoCollectionLargeMatchRegex` | Equivalent | `merged_and_callsite_collections_round_trip_and_reject_truncation` and the large-offset test cover the collection. |
| `GSYMTest.TestFunctionInfoAllFieldsLargeOffsets` | Equivalent | `function_and_all_nested_records_preserve_large_v2_offsets` exercises every optional field. |
| `GSYMTest.TestMergedFunctionsInfoLargeOffsets` | Equivalent | The large-offset test covers merged functions; the public writer merged-option test covers semantic construction. |
| `GSYMTest.TestDWARFTypedefCycleDoesNotCrash` | Equivalent | The importer never recursively unwraps type DIEs; `recursive_type_graphs_do_not_affect_function_conversion` verifies recursive type graphs do not affect function import. Name-reference traversal has an independent visited-offset cycle guard. |
| `GSYMTest.TestGsymStatisticsV1` | Equivalent | `Gsym::verify` reports functions, files, strings, and function-info bytes; round-trip tests validate v1 counts. |
| `GSYMTest.TestGsymStatisticsV2` | Equivalent | The same `VerifyReport` contract is tested after v2 CLI conversion. |

## `llvm-gsymutil` files (18)

| LLVM input | Status | Rust coverage or reason |
|---|---|---|
| `ARM_AArch64/fat-macho-dwarf.yaml` | Excluded | Fat Mach-O and Apple architectures are outside the Linux ELF converter scope. |
| `ARM_AArch64/fat-macho-symtab-file.yaml` | Excluded | Fat Mach-O symbol-table selection is outside the Linux ELF converter scope. |
| `ARM_AArch64/macho-gsym-callsite-info-dsym.yaml` | Excluded | Mach-O/dSYM container conversion is outside scope; call-site wire data itself is covered. |
| `ARM_AArch64/macho-gsym-callsite-info-exe.yaml` | Excluded | Mach-O executable conversion is outside scope; call-site wire data itself is covered. |
| `ARM_AArch64/macho-gsym-callsite-info-obj.test` | Excluded | Relocatable Mach-O conversion is outside the Linux linked-ELF contract. |
| `ARM_AArch64/macho-gsym-merged-callsites-dsym.yaml` | Excluded | Mach-O/dSYM conversion is outside scope; merged functions and call sites are independently ported. |
| `ARM_AArch64/macho-merged-funcs-dwarf.yaml` | Excluded | Mach-O DWARF conversion is outside scope; `FunctionSetPolicy::MergeEqualRanges` is directly tested. |
| `X86/elf-dwarf.yaml` | Equivalent | `tests/convert/dwarf.rs::elf_dwarf_and_gsym_only_equivalent_preserves_lines_and_inlines` and the CLI v2 test use real Linux ELF/DWARF. |
| `X86/elf-dwo.yaml` | Ported | Real compiler-generated skeleton and `.dwo` files verify discovery, ID matching, and function import. |
| `X86/elf-empty-dir.yaml` | Equivalent | Reserved empty directory/basename strings and file index zero are directly tested through public write/read APIs. |
| `X86/elf-invalid-llvm-stmt-sequence.yaml` | Ported | A synthetic ELF fixture verifies invalid `DW_AT_LLVM_stmt_sequence` recovery, diagnostics, and preservation of matching rows from other sequences. |
| `X86/elf-llvm-stmt-sequence.yaml` | Ported | The same fixture verifies exact line-program sequence offsets and sentinel handling. |
| `X86/elf-mangled-name-replacement.yaml` | Ported | Itanium mangled-name replacement is tested with rich duplicate retention and both insertion orders. |
| `X86/elf-swift-mangled-name-replacement.yaml` | Ported | Swift mangled-name replacement is tested through v2 big-endian writing. |
| `X86/elf-symtab-file.yaml` | Equivalent | `tests/convert/inputs.rs::symtab_file_equivalent_accepts_a_matching_companion` plus the CLI `--symbols` end-to-end path. |
| `X86/mach-dwarf.yaml` | Excluded | Mach-O is outside the Linux ELF converter scope. |
| `X86/macho-invalid-section-offset.yaml` | Excluded | Mach-O section-offset diagnostics are outside the Linux ELF converter scope. |
| `cmdline.test` | Ported | CLI invalid arguments, atomic failed conversion, v2 conversion, lookup, dump, and verify are covered in `tests/cli/gsymtool.rs`. |

## GSYM-only `llvm-symbolizer` test

LLVM's `sym-gsymonly.test` has 21 `llvm-symbolizer` and `llvm-addr2line`
command variants over one stripped ELF plus sibling GSYM fixture. Its semantic
contract is covered by
`elf_dwarf_and_gsym_only_equivalent_preserves_lines_and_inlines`: the Rust
converter reads a separate-debug ELF, writes GSYM, and resolves missing,
concrete, and nested-inline addresses from the stripped linked image. LLVM's
specific command aliases and output presentation are not duplicated because
this crate exposes `gsymtool`, not drop-in `llvm-symbolizer` or
`llvm-addr2line` frontends.

## Resource-bound coverage

No enumerated upstream unit or Linux ELF tool behavior remains blocked. V1's
32-bit function/string offset ceiling is enforced by checked conversions in the
writer; a literal overflow test would require constructing more than 4 GiB of
encoded data, so it is not suitable for the standard suite. V2 large-offset
record fields are instead tested with hand-built semantic records that do not
require that allocation.
