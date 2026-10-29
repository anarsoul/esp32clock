fn main() {
    // `config.rs` reads these with `option_env!`, so this crate is what must rebuild when they change.
    for var in ["WIFI_SSID", "WIFI_PASSWORD", "TZ_OFFSET_MINUTES", "API_TOKEN", "MAX_CURRENT_STEP", "AP_PASSWORD"] {
        println!("cargo:rerun-if-env-changed={var}");
    }
}
