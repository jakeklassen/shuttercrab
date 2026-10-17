//! Shuttercrab's windows, each driven in headless GPUI windows. One test
//! binary rather than one per window: each links all of GPUI, so separate
//! ones build slower and take far more disk.
//!
//!   cargo test -p shuttercrab --test ui
//!   cargo test -p shuttercrab --test ui record_bar::

mod capture_bar;
mod choice_menu;
mod countdown;
mod main_window;
mod overlay;
mod record_bar;
mod settings_window;
mod thumbnail;
