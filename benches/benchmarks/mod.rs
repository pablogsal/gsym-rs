#[expect(
    dead_code,
    reason = "the shared fixture also supports the separate writer benchmark target"
)]
pub(crate) mod fixture;
pub(crate) mod reader;
