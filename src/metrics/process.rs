//! Process metrics: panics.

use std::panic;

use ::metrics::counter;

/// Count panics in the counter `name`, then run the previous panic hook, so
/// panics still print (or reach your error reporter) as before.
///
/// ```
/// use tower_telemetry::metrics::install_panic_counter;
///
/// // After installing your exporter, and after any other panic hook (such
/// // as an error reporter's), since this one wraps whatever is set.
/// install_panic_counter("process_panics_total");
/// ```
///
/// The counter is set to 0 at once, so it exists before the first panic and
/// `increase()` alerts work from the start. A panic inside a Tokio task
/// counts too, even though Tokio catches it and the process keeps running:
/// that's what makes this worth alerting on.
///
/// Call it once; each call wraps the hook again and counts every panic once
/// more.
pub fn install_panic_counter(name: &'static str) {
    counter!(name).absolute(0);
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        counter!(name).increment(1);
        previous(info);
    }));
}
