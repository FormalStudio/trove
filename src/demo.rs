/// Demo mode gives tests and screenshots a fixed starting state.
pub const COUNT: u32 = 42;

pub fn enabled() -> bool {
    std::env::var_os("TROVE_DEMO").is_some()
}
