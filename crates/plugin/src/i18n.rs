//! Language of messages shown to the player (diagnostics, status,
//! config.ini comments): English by default, or Italian.

use std::sync::atomic::{AtomicBool, Ordering};

static ITALIAN: AtomicBool = AtomicBool::new(false);

pub fn set(language: &str) {
    ITALIAN.store(language == "it", Ordering::Relaxed);
}

pub fn italian() -> bool {
    ITALIAN.load(Ordering::Relaxed)
}

/// `tr!("italiano {x}", "english {x}")`: formats the current language's text.
#[macro_export]
macro_rules! tr {
    ($it:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::italian() { format!($it $(, $arg)*) } else { format!($en $(, $arg)*) }
    };
}
