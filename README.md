# esp32clock

Open firmware for the HU-058 family of LED clock kits (HU-058 and
HU-058SE; the HU-058D should work too but is untested), with the kit's
8051 replaced by an ESP32-C3 on a small adapter
board that takes the 8051's place in the DIP-16 footprint. Written in Rust with
[esp-hal](https://github.com/esp-rs/esp-hal) and Embassy.

What the clock does:

- Time over NTP, 12 or 24 hour, with a configurable timezone offset.
- Alarm with a choice of melodies, per-weekday arming and a one-time skip.
- Nine colours and three brightness steps, from the buttons or the web page.
- Room temperature and light level from the kit's NTC and LDR (optional),
  with the temperature shown on the panel once a minute.
- Settings page at `http://<ip>/` and a form-encoded JSON API at `/api`.
- Home Assistant integration over MQTT with discovery: the settings as
  entities, the readings as sensors, no YAML (see
  [Home Assistant](#home-assistant) below).
- Firmware updates over the LAN (`/update` with a PIN shown on the panel,
  or a script with a token), two application slots with rollback.
- Provisioning from the clock's own access point when no network is
  stored, with a captive portal.
- Self-healing: the clock resets itself when the network is lost for
  minutes, carrying the time and the alarm state across the reset.

The display is driven by SPI2 in quad mode used as a four-wire pattern
generator with DMA, so the scan is cycle-exact and immune to Wi-Fi.
`PLAN.md` is the design record; `AGENTS.md` is the maintainer's guide.

## Hardware

### Parts

| part | notes |
| --- | --- |
| HU-058 or HU-058SE kit | the display half comes preassembled; the through-hole side is what you build. Sold on AliExpress as "DIY KIT Electronic Clock LED Microcontroller Soldering Exercise" (links in the [HU-058_ESPHome](https://github.com/misterblack1/HU-058_ESPHome) README). Get this one rather than the pricier HU-058D "ESP8266 IoT Colorful WiFi Clock Kit": the 8051 and the ESP-01S both go unused either way, and the D is untested here |
| ESP32-C3 module | an ESP32-C3 SuperMini **Plus**, the one with an antenna socket, is the one to get, with a small external antenna; a plain ESP32-C3 SuperMini or a Waveshare ESP32-C3-Zero works too, with weaker Wi-Fi. One adapter design for each footprint |
| adapter PCB | KiCad projects and gerbers in `adapter/adapter-esp32c3-zero` and `adapter/adapter-esp32c3-supermini`; two layers, 1.6 mm, any board house. Or order it from [PCBWay](https://www.pcbway.com/project/shareproject/esp32c3_adapter_for_HU_058_clock_e419ee2a.html), which supports this project |
| pin header | the one that comes with the module |
| 2 × 10 kΩ 0603 resistors, ideally 1 % | R1 and R2 on the adapter; only needed for the temperature and light sensors |
| 10 µF 25 V electrolytic capacitor | replaces the kit's C3, optional but recommended |
| 5 V USB power supply | 1 A recommended; 500 mA works |

### Building the clock board

Most of the kit stays in the bag. The ESP32-C3 takes over everything
the 8051 did, and the kit's own timekeeping parts have nothing to talk
to.

**Fit these:**

| reference | part | why |
| --- | --- | --- |
| SW1, SW2 | 6 × 6 mm tactile switches | the two buttons |
| Q1 | S8550 PNP | buzzer driver |
| R5 | 10 kΩ | base resistor of Q1 |
| LS1 | buzzer | the alarm |
| USB1 | micro USB | 5 V in |

**Optional, for the room temperature and light level:**

| reference | part | notes |
| --- | --- | --- |
| R4 | NTC thermistor | room temperature |
| R3 | LDR | light level |

If you fit them, also fit R1 and R2 on the **adapter** (see below).
Do **not** fit the kit's own R1 and R2: they pull the sensors up to 5 V,
which is more than the ESP32-C3's inputs may see. The adapter's
resistors pull up to 3.3 V instead. Without the sensors, leave R3 and
R4 off the clock board and R1 and R2 off the adapter; the clock reports
no reading and shows the time only.

**Replace:** C3, a 0.1 µF ceramic on the 5 V rail next to the MCU
footprint, is best replaced with a 10 µF 25 V electrolytic capacitor.
The drivers draw their current in bursts and the ESP32-C3 adds Wi-Fi
transmit peaks. Fit it last, after the adapter is in place, since it
stands next to the adapter and gets in the way of soldering it. Mind
the polarity: the negative lead goes towards the top of the board.

**Leave out everything else:** the STC8G1K17 MCU (U4), the kit's R1
and R2, and the timekeeping parts of your variant. On the HU-058 and
HU-058SE that is the DS1302 (U3), the 32.768 kHz crystal Y1 with C5
and C6, and the CR1220 holder BT1. On the HU-058D it is the ESP-01S
socket P5, the AMS1117 regulator U3 with C4 and C5, and R6 and R7.
Header P3 is not needed either.

**The DIP-16 socket is optional.** The adapter stands on the pin
header that comes with the module. Whether it can plug into the kit's
socket depends on whether those pins fit the socket's contacts and on
the clearance left under the adapter. If they do, fit the socket and
the adapter is removable; if not, leave the socket out and solder the
adapter's header straight into the U4 footprint.

If you bought the kit assembled, pull the 8051 and (on the HU-058D)
the ESP-01S out of their sockets instead. Both must be out before the
adapter goes in; two devices on one bus fight.

### Building the adapter

The adapter is a two-layer board with the ESP32-C3 module soldered on
top, two 0603 pads (R1, R2) and a DIP-16 footprint underneath for the
module's pin header. Build it in this order, since each step covers
the pads of the next:

1. R1 and R2 first (10 kΩ, 0603, ideally 1 %), if you fitted the NTC
   and LDR on the clock board. Leave them off otherwise.
2. Then the module. The antenna end hangs over a notch in the
   adapter's edge on purpose; do not let it sit over copper or over
   the clock board. A chip antenna buried in a ground fill loses 10 to
   25 dB, which was the first adapter revision's mistake. With a
   SuperMini Plus, plug the external antenna into its socket and check
   that the module's antenna-select pad, where it has one, points at
   the socket.
3. Then the pin header, into the DIP-16 footprint, pins down.
4. Plug the adapter into the kit's socket, or solder its header into
   the U4 footprint if the socket was left out. Pin 1 is the end
   marked on the clock board's silkscreen. Fit C3 on the clock board
   after this.

A word on the antenna. A module with a ceramic chip antenna normally
works fine, and when the link does drop the clock resets itself and
comes back with the time intact. But far from the access point, or
with the clock in its case on a shelf, the uplink gets marginal: pages
load slowly or not at all while the clock's own signal reading still
looks fine, because the access point hears the clock about 20 dB worse
than the clock hears it. An external antenna has been measured 25 to
30 dB better than the chip antenna in the same spot, which is the
difference between a clock that heals itself now and then and one
that is never noticed.

What the adapter connects:

| ESP32-C3 pin | socket pin | net | role |
| --- | --- | --- | --- |
| GPIO9 | 5 | DATA | driver 1 data |
| GPIO8 | 16 | CLK | driver 1 clock |
| GPIO7 | 2 | DATA_1 | driver 2 data |
| GPIO6 | 1 | CLK_1 | driver 2 clock |
| GPIO4 | 10 | S2 | upper button |
| GPIO3 | 9 | S1 | lower button |
| GPIO5 | 11 | R_B | buzzer, open-drain |
| GPIO0 | 3 | R_T | NTC divider, adapter R2 10 kΩ to 3V3 |
| GPIO1 | 4 | R_P | LDR divider, adapter R1 10 kΩ to 3V3 |
| 5V | 6 | +5V | supply |
| GND | 8 | GND | ground |

Note the clock line: the adapter takes driver 1's clock from socket
pin 16, which is where the HU-058 and HU-058SE have it. The HU-058D
routes that signal to pin 14 instead, so on a D board a short wire
from socket pin 14 to pin 16 should be needed on the clock board (the
kit's pin 16 goes only to the unfitted ESP-01S link through R6). That
follows from the schematics; the firmware and the adapter have only
been built on the HU-058, and the HU-058D is untested.

### Power

Feed 5 V through the clock's own micro USB (USB1). A 1 A supply is
recommended; a 500 mA phone charger works. The kit is rated 250 mA but
the display alone passes that easily at the brighter steps, and Wi-Fi
transmit adds short peaks on top. The firmware caps the drive current
at the 50 % step by default (`MAX_CURRENT_STEP`, see below); raising
the cap on a weak supply makes the board brown out and reset when the
brightness is stepped up.

While the module is on USB for flashing, its 5 V pin back-feeds the
clock board, so the panel lights up on the bench. That is expected.
Do not power the clock from USB1 and the module from its own USB at
the same time.

## Firmware

### Prerequisites

- Rust stable 1.95 or newer. `rust-toolchain.toml` pulls the
  `riscv32imc-unknown-none-elf` target and `rust-src` by itself.
- [espflash](https://github.com/esp-rs/espflash) 4 for flashing,
  monitoring and making OTA images: `cargo install espflash`.
- `curl` for the scripts.

### Build and flash

The Wi-Fi credentials, the timezone and the API token are read from
the environment at build time and become the clock's defaults; all of
them can be changed later from the page and are stored in flash from
then on.

```sh
WIFI_SSID='home' WIFI_PASSWORD='…' TZ_OFFSET_MINUTES=-420 API_TOKEN='…' cargo run --release
```

`cargo run` builds, flashes over the module's USB with the two-slot
partition table and opens the serial monitor. The first boot logs the
firmware version, the network and the address the clock got.

| variable | meaning | default |
| --- | --- | --- |
| `WIFI_SSID`, `WIFI_PASSWORD` | network to join; may be left empty, see provisioning | empty |
| `TZ_OFFSET_MINUTES` | local time offset from UTC in minutes | 0 |
| `API_TOKEN` | bearer token for the scripts and the API's protected keys | none |
| `MAX_CURRENT_STEP` | LED current ceiling, 1 to 15; 5 is the 50 % step | 5 |
| `AP_PASSWORD` | the clock's own access point's password | `esp32clock` |

### First boot without a network

A clock built without `WIFI_SSID`, or one whose two buttons are held for
10 s, starts as an access point. The panel shows `AP`, the network is
`clock-xxxx` (last two bytes of the MAC, WPA2, password `AP_PASSWORD`)
and the setup form is at `http://192.168.4.1/`; a phone's sign-in sheet
opens it by itself. Pick the network from the scan or type a hidden
one, set the timezone and hostname, save. The clock stores the record
and reboots into the network. Hold both buttons 10 s again to go back
to the access point if the password was wrong.

### Updates over the LAN

```sh
API_TOKEN='…' scripts/ota.sh <ip>     # build, make the image, upload, reboot
scripts/ota.sh                        # just make the image, for the /update page
```

Or open `http://<ip>/update`: the clock shows a four-digit PIN on its
panel for a minute, and the page takes the image file and the PIN. The
new image goes to the other slot; a failed write leaves the running
one in place, and a new image that cannot reach the network within
three boots is rolled back.

### Prebuilt images

The easiest first flash needs no toolchain: open
<https://anarsoul.github.io/esp32clock/> in Chrome or Edge with the
clock on USB, pick a release and press Install. The page is ESP Web
Tools over Web Serial; it writes the bootloader, the partition table,
an erased `otadata` and the firmware, leaving the settings partition
alone, so a reflash keeps the settings unless you take the erase it
offers first. The [releases page](https://github.com/anarsoul/esp32clock/releases)
has `esp32clock-<tag>.bin`, the whole flash from offset 0 for
`espflash write-bin 0x0 esp32clock-<tag>.bin` (then `espflash reset`;
this one covers the settings partition too and clears it), next to
`esp32clock-ota-<tag>.bin`, the update image for `/update` and
`scripts/ota.sh`. A release is made by pushing an annotated tag `v*`
(`.github/workflows/release.yml`).

Between releases, every push is built on GitHub Actions
(`.github/workflows/ci.yml`) and the OTA image is attached to the run
as an artifact: open the Actions tab, pick the latest run on `master`,
download `esp32clock-ota-<commit>` and unzip it to get
`esp32clock-ota.bin`. GitHub asks for a login to download artifacts
and keeps them for 90 days. The images are built with:

| setting | value in the prebuilt images |
| --- | --- |
| network | none: the clock boots as an access point, see above |
| timezone | `-420`, Vancouver (UTC-7, permanent since BC stopped changing clocks). The offset is fixed; set it on the settings page for your zone |
| API token | `esp32clock` |
| everything else | the defaults: brightness cap 50 %, access point password `esp32clock` |

The OTA image is an update image, for a clock that has been flashed
once over USB (the web installer or `cargo run` above). Install it
from `http://<ip>/update` with the PIN, or from the shell:

```sh
API_TOKEN=esp32clock scripts/ota.sh <ip> esp32clock-ota.bin
```

The token is printed here, so change it once the clock is on your
network: open `http://<ip>/token`, type a new one (8 to 32 printable
characters without spaces) with the PIN the clock shows, or post it
with the old token:

```sh
curl -X POST -H 'Authorization: Bearer esp32clock' -d 'token=<new token>' http://<ip>/token
```

A token stored this way survives later updates with the prebuilt
images: the build's token only seeds a clock that has none. From then
on `scripts/ota.sh` wants the new token in `API_TOKEN`.

## Using the clock

| press | in clock mode |
| --- | --- |
| upper short | next colour |
| lower short | next brightness |
| upper long | set the alarm hour, then lower short for minutes, lower short again to leave |
| lower long | arm or disarm the alarm (the degree mark shows armed) |
| both long | show the IP address, one octet at a time, twice |
| both held 10 s | reset into the access point, or back to the network |

The IP display takes some decoding the first time. The first digit is
the octet's position, 1 to 4, and the other three are its value,
right-aligned, with the colon off. Each octet stays for two seconds
and the four go round twice. For `192.168.0.43` the panel shows:

```
1192    2168    3  0    4 43
```

A clock with no address yet shows `no1P` instead.

Any press ends a ringing alarm or the IP display.

The page at `http://<ip>/` has every setting: timezone, 12/24 hour,
colour, brightness, alarm time, days and melody with a Play button,
skip next alarm, temperature unit and offset, hostname, Wi-Fi network.
The status line under the form shows the firmware version, the slot,
the uptime, the signal and the room reading.

The same keys are available at `/api`: `GET` answers JSON, `POST` with
form-encoded keys applies all of them or none. A posted network, broker
or token needs `Authorization: Bearer <API_TOKEN>`. `scripts/configure-clock.sh`
is an example that sets a whole configuration in one request.

### Home Assistant

Fill in the "Home Assistant" fieldset on the page (or build with
`MQTT_HOST`, `MQTT_USER` and `MQTT_PASSWORD`) with the address of an
MQTT broker Home Assistant is on, and the clock appears under the MQTT
integration as a device named after its hostname, with no YAML: the
colour, brightness, alarm time, armed, skip, days, melody, 12 hour
clock, panel temperature and thermometer offset as entities you can
set, the room temperature, light, signal, uptime and self-heals as
sensors, a binary sensor that is on while the alarm rings, and
buttons to play the melody, silence a ring and reload the clock. The
broker must speak MQTT 5 (Mosquitto 1.6 and later, which includes the
Home Assistant add-on, does); the connection is plain, not TLS, so
give the clock a broker user of its own. Everything is under
`esp32clock/clock-xxxxxx/` (the last three bytes of the MAC):
`state` is the `/api` JSON, `set/<key>` takes a value as `/api` does,
and `cmd` takes `play`, `silence` or `reload`:

```
mosquitto_sub -v -t 'esp32clock/#'
mosquitto_pub -t esp32clock/clock-a1b2c3/set/color -m cyan
```

Only the clock's own settings can be set this way; the network, the
hostname, the token and the broker itself cannot.

## Development

The repository is a two-member workspace: the firmware is the root
package and `core/` is the hardware-independent half, a `no_std`
library with the display encoder, the settings record, the form and
page builders, the UI state machine and the sensor tables. Its tests
run on the host:

```sh
cargo build --release
cargo clippy --release
cargo host-test                                            # the core crate's tests, on the host
cargo clippy -p esp32clock-core --target host-tuple --tests
```

Plain `cargo test` does not work: `.cargo/config.toml` aims every
cargo command at the chip, which is why the alias passes a host
target.

The CI workflow runs the same four commands, with clippy warnings as
errors, and builds the prebuilt image described above; the release
workflow does the same for a tag and publishes the images and the web
installer (`docs/index.html`, served from the `gh-pages` branch).

`AGENTS.md` is the detailed guide to the code: the module layout, the
hardware facts that bite, the timing rules of the scan path and the
sizes that must be kept. `PLAN.md` holds the design of each milestone
and what was measured on the panel.

## License

MIT or Apache-2.0, at your option. See `LICENSE-MIT` and
`LICENSE-APACHE`.

The display protocol and the board documentation come from
[misterblack1/HU-058_ESPHome](https://github.com/misterblack1/HU-058_ESPHome),
the reverse-engineering project for this kit, released as CC0. Its
`docs/` directory has the AiP33628 protocol, the display map and the
schematics of all three board variants.
