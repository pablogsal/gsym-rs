//! Shared entry points into the ELF/DWARF converter, plus the ELF mutations
//! that companion-validation tests need.

use std::sync::LazyLock;

use gsym::convert::{ConversionOptions, ConversionReport, ElfConverter, ElfInputs};
use object::Object;

static CURRENT_IMAGE: LazyLock<Vec<u8>> =
    LazyLock::new(|| std::fs::read(std::env::current_exe().unwrap()).unwrap());

/// Borrow the running test binary.
pub(crate) fn current_image() -> &'static [u8] {
    &CURRENT_IMAGE
}

/// Convert `inputs` with default options.
pub(crate) fn convert(inputs: ElfInputs<'_>) -> gsym::Result<ConversionReport> {
    ElfConverter::new(ConversionOptions::default()).convert(inputs)
}

/// The converted function named `name`, if the report produced one.
pub(crate) fn find_function<'report>(
    report: &'report ConversionReport,
    name: &[u8],
) -> Option<&'report gsym::Function> {
    report
        .builder
        .functions()
        .iter()
        .find(|function| function.name == name)
}

/// Rewrite `e_machine` to `AArch64` so the image no longer matches its companion.
pub(crate) fn retarget_machine(bytes: &mut [u8]) {
    const AARCH64: u16 = 183;

    assert_eq!(&bytes[..4], b"\x7fELF");
    let little_endian = bytes[5] == 1;
    let machine = if little_endian {
        AARCH64.to_le_bytes()
    } else {
        AARCH64.to_be_bytes()
    };
    bytes[18..20].copy_from_slice(&machine);
}

/// Flip a bit inside the image's build ID so companion validation rejects it.
pub(crate) fn corrupt_build_id(bytes: &mut [u8]) {
    let build_id = object::File::parse(&*bytes)
        .unwrap()
        .build_id()
        .unwrap()
        .expect("test binaries are linked with a build ID")
        .to_vec();
    assert!(!build_id.is_empty());
    let position = bytes
        .windows(build_id.len())
        .position(|window| window == build_id)
        .unwrap();
    bytes[position] ^= 0x80;
}
