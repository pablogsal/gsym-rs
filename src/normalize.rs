use crate::model::{Function, LineEntry};

pub(crate) fn compact_line_rows(lines: &mut Vec<LineEntry>) {
    lines.dedup_by(|current, previous| {
        current.file == previous.file && current.line == previous.line
    });
}

pub(crate) fn compact_function_lines(functions: &mut [Function]) {
    for function in functions {
        compact_line_rows(&mut function.lines);
        compact_function_lines(&mut function.merged);
    }
}
