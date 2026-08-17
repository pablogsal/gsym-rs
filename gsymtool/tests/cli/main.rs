//! Command-line suite: the `gsymtool` binary and interoperability with LLVM.
//!
//! Both modules drive real processes on a Linux host. These tests use the same
//! package features as the `gsymtool` binary they execute.

#![cfg(target_os = "linux")]

#[path = "../../../tests/common/tools.rs"]
mod tools;

mod gsymtool;
mod interop;
