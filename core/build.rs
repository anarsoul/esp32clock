// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

fn main() {
    // `config.rs` reads these with `option_env!`, so this crate is what must rebuild when they change.
    for var in ["WIFI_SSID", "WIFI_PASSWORD", "TZ_OFFSET_MINUTES", "TZ_RULE", "API_TOKEN", "MAX_CURRENT_STEP", "AP_PASSWORD", "ESP_RADIO_CONFIG_WIFI_MTU", "MQTT_HOST", "MQTT_PORT", "MQTT_USER", "MQTT_PASSWORD"] {
        println!("cargo:rerun-if-env-changed={var}");
    }
}
