//! Process metrics: panics.

use std::panic;

use ::metrics::counter;

/// Count panics in the counter `name` (e.g. `"process_panics_total"`), then
/// run the previous panic hook. The counter starts at zero, so it exists
/// before the first panic and alerts on `increase()` work.
pub fn install_panic_counter(name: &'static str) {
    counter!(name).absolute(0);
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        counter!(name).increment(1);
        previous(info);
    }));
}
