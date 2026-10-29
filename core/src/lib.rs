//! The hardware-independent half of the clock firmware: the settings and
//! their flash record, the form and pages, the UI state machine, the
//! panel map and the AiP33628 encoder, the melodies, the HTTP head
//! parser, the OTA image checks and the carry record's codec.
//!
//! Nothing here touches a peripheral, so the crate builds for the host
//! and `cargo test -p esp32clock-core --target host-tuple` (aliased
//! `cargo host-test`) runs the tests at the end of each module. The
//! firmware depends on this crate and re-exports its modules, so its
//! own code says `crate::settings` as before.

#![cfg_attr(not(test), no_std)]

pub mod aip33628;
pub mod auth;
pub mod buttons;
pub mod carry;
pub mod clock;
pub mod config;
pub mod form;
pub mod http;
pub mod melody;
pub mod ota;
pub mod panel;
pub mod settings;
pub mod ui;
