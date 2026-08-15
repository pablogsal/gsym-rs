//! Command-line suite: the `gsymtool` binary and interoperability with LLVM.
//!
//! Both modules drive real processes on a Linux host. The `gsymtool` binary
//! pins its own feature set, so these tests only need `convert` for the
//! in-process conversion `interop` performs before handing bytes to LLVM.

#![cfg(target_os = "linux")]

#[path = "../../../tests/common/tools.rs"]
mod tools;

mod gsymtool;
mod interop;
