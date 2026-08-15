//! Converter suite: turning Linux ELF/DWARF images into GSYM.
//!
//! These cases build their inputs with the real toolchain -- `cc`, `objcopy`,
//! `clang`, `yaml2obj`, `llvm-dwp` -- rather than hand-written bytes, so they
//! need a Linux host and the `convert` feature; the whole suite compiles away
//! without them. The `debuginfod` feature also enables cases in
//! `debuginfo` that serve debug files over a local HTTP socket.

#![cfg(all(target_os = "linux", feature = "convert"))]

#[path = "../common/elf.rs"]
mod elf;
#[path = "../common/tools.rs"]
mod tools;

mod debuginfo;
mod dwarf;
mod inputs;
mod split_dwarf;
