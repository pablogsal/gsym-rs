pub(crate) mod function;
pub(crate) mod leb;
pub(crate) mod line;
pub(crate) mod v1;
pub(crate) mod v2;

#[cfg(test)]
mod tests;

pub(crate) const GSYM_MAGIC: u32 = 0x4753_594d;
pub(crate) const GSYM_CIGAM: u32 = GSYM_MAGIC.swap_bytes();

pub(crate) const INFO_END: u32 = 0;
pub(crate) const INFO_LINE_TABLE: u32 = 1;
pub(crate) const INFO_INLINE: u32 = 2;
pub(crate) const INFO_MERGED: u32 = 3;
pub(crate) const INFO_CALL_SITE: u32 = 4;

pub(crate) fn file_table_size(
    file_count: u32,
    entry_size: usize,
    context: &'static str,
) -> crate::Result<usize> {
    usize::try_from(file_count)
        .map_err(|_| crate::Error::Overflow(context))?
        .checked_mul(entry_size)
        .and_then(|size| size.checked_add(4))
        .ok_or(crate::Error::Overflow(context))
}

#[inline]
pub(crate) const fn align_up(value: usize, alignment: usize) -> Option<usize> {
    value.checked_next_multiple_of(alignment)
}
