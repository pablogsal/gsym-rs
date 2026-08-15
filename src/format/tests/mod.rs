mod cursor_leb;
mod function_records;
mod headers_layout;
mod line_program;

use crate::Endian;

const ENDIANS: [Endian; 2] = [Endian::Little, Endian::Big];
