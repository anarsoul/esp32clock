// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

#![no_std]

// The hardware-independent modules live in `esp32clock-core`, tested on
// the host; re-exported so the firmware says `crate::settings` as before.
pub use esp32clock_core::{aip33628, auth, config, form, melody, panel, settings, ui};

pub mod buttons;
pub mod buzzer;
pub mod carry;
pub mod clock;
pub mod dhcpd;
pub mod dnsd;
pub mod display;
pub mod heal;
pub mod http;
pub mod mqtt;
pub mod net;
pub mod ota;
pub mod scan;
pub mod sensors;
pub mod storage;
