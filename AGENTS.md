# esp32clock — guide for agents

Firmware for the HU-058 / HU-058D LED clock panel driven by an ESP32-C3,
written in Rust with esp-hal and Embassy on esp-rtos. The display's two
AiP33628 drivers are fed by SPI2 in quad mode used as a four-wire pattern
generator, on one DMA segmented transfer the hardware loops for ever;
see `PLAN.md` for the full design and its history.
`ARCHITECTURE.md` is the brief overview (crates, tasks, data paths,
resets).
The repo is a two-member workspace: the firmware is the root package,
and `core/` (`esp32clock-core`) is the hardware-independent half, a
`no_std` library the firmware re-exports and the host runs the tests
of. `CLAUDE.md` is a symlink to this file; edit this one.

## Workflow

- Plan before implementing. `PLAN.md` is the design record: each milestone
  has its design, what was measured, and what was learned. Add to it before
  starting work and mark milestones verified when the user confirms on the
  panel. It ends with a review backlog of known, undone findings; read it
  before proposing work and strike items you finish.
- Update `ARCHITECTURE.md` in the same change as any major change to
  the architecture: a task or executor added, removed or moved, a new
  peripheral or DMA path, a new way tasks talk to each other, a new
  network service or page, a new persistent store or reset path, a
  module moved between `core/` and `src/`. It stays brief and names
  constants rather than their values; tuning, measurements and
  history go in `PLAN.md`, not there.
- Nothing here can be verified without the hardware. Build, run clippy
  and `cargo host-test`, and put new pure logic under test in `core/`;
  then hand the user a flash command and say exactly what to look for.
  Do not claim hardware results.
- Commit only when asked. Commit messages end with
  `Co-Authored-By: Claude Fable 5.2 <noreply@anthropic.com>` (the user asked
  for this trailer in this project).
- `3rdparty/` is the reverse-engineering reference (public domain), ignored
  by git. Read it; do not modify it.
- `clips/` holds the clips of the default clip pack
  (`scripts/clips.sh`, the CI and the release make it): Trantor's title
  music and two of Orion's modules, whose sources are in `mods/`. They
  are other people's music, not under the project's licence;
  `clips/README.md` credits each and must go on doing so. A clip
  nobody may redistribute stays in the ignored `wav/`, `tap/`, `sid/` and `pt3/`,
  and a pack made of such in the ignored `packs/`.
- The project is licensed MIT OR Apache-2.0 (`LICENSE-MIT`,
  `LICENSE-APACHE`, the `license` field of both manifests). Every `.rs`
  and `.sh` file starts with an `SPDX-License-Identifier` line and the
  copyright line; give a new file both. The reference in `3rdparty/` is CC0 and needs nothing.

## Build and flash

```
cargo build --release
cargo clippy --release
cargo host-test                          # the core crate's tests on the host (alias in .cargo/config.toml)
cargo clippy -p esp32clock-core --target host-tuple --tests
WIFI_SSID='…' WIFI_PASSWORD='…' TZ_RULE='<-07>7' API_TOKEN='…' cargo run --release
TZ_OFFSET_MINUTES=-420 cargo build --release   # the older variable: minutes east of UTC, stored as the fixed rule <-07>7
node scripts/tz-check.mjs                 # the page's browser-zone script (src/tz.js) against zones with known rules
MAX_CURRENT_STEP=8 cargo build --release   # optional: raise the drive-current ceiling (default 5)
WIFI_TX_POWER_QDBM=44 cargo build --release   # optional: the default of the transmit power setting, quarter dBm, one of its levels (default 68)
MQTT_HOST=10.1.1.5 MQTT_USER=clock MQTT_PASSWORD='…' cargo build --release   # optional: the Home Assistant broker (MQTT_PORT defaults to 1883)
API_TOKEN='…' scripts/ota.sh <ip>      # later updates, over the LAN
scripts/ota.sh                          # just the image file, for http://<ip>/system
API_TOKEN='…' scripts/reload.sh <ip>   # software reset with the carry record, waits for the clock to come back
scripts/pin.sh <ip> <serial-log>        # POST /pin, then the four digits read back from the monitor's log
scripts/verify-csrf.sh <ip>             # a foreign Origin or Host is refused, the clock's own pages and a script are not
AP_PASSWORD='…' cargo build --release   # optional: the clock's own access point's password (default esp32clock)
scripts/wav2pcm.sh in.wav wav/tune.pcm [seconds] [skip] [rate]   # a sound file as the clip's raw PCM (sox): mono, 8 bits unsigned, 16 kHz behind a PCM1 header, or bare at 8 kHz for half the flash, or 31250 (see --rate 31250 below) for twice it
CLIP_HIGHPASS=100 CLIP_DRIVE=10 scripts/wav2pcm.sh in.wav wav/tune.pcm   # the same for a speaker: the high-pass at that corner once and not at 900 Hz twice, 10 dB into the limiter and not 20
~/.local/pipx/venvs/skoolkit/bin/python scripts/zxbeeper.py game.tap tap/tune.bpr --seconds 60 [--stretch 4] [--settle 20 --start 64000]   # what a ZX Spectrum program plays on its beeper, as a clip (needs SkoolKit; --start for a loader that waits for a key)
scripts/xm2clip.py tune.xm mods/tune.xmc [--highpass 900 --passes 2] [--raise 1:2] [--drive 160 --release 5] [--knee 100] [--rate 16000] [--bits 8]   # a FastTracker 2 module for the clock's own player; lists what it uses that the player lacks; the bass cut, an instrument's octave and the limiter are the clip's (--knee is the level the limiter starts at, in 1024s of the mix where one voice is 32; the default is 20 dB of drive, for the buzzer, and a knee above the tune's peak is the limiter off, for a speaker; the clip is at `clip::FAST_HZ`, 31 250 samples a second, here and in the two below, which the chip's converter at 256 bits a sample sends at 8 Mbit/s exactly: the default since 2026-11-13, heard on the breadboard stage and on white-clock; 32 kHz is not among the converter's whole dividers, and at CLIP_PDM_BITS=384 such a clip is refused; --rate 16000 is the rate before it, half a module's share of the processor, and 8000 a module's third)
scripts/sid2clip.py tune.sid sid/tune.sidc [--song 2] [--seconds 90] [--mute 2] [--model 8580] [--steps 4] [--rate 16000] [--highpass 300 --passes 1]   # a Commodore 64 tune (PSID) for the clock's own SID: runs the tune's 6502 code on the host and keeps each frame's register writes, to the point where the tune loops; --mute keeps the voices' gates closed for the first frames, for a routine that clicks as it starts; says what it does that the clock will not play; --flat for a speaker: no limiter, --drive then a plain gain with the three voices at their top as full scale (flags bit 1; a firmware before 2026-11-15 plays such a clip limited)
scripts/pt32clip.py tune.pt3 pt3/tune.pt3c [--machine pentagon|spectrum] [--clock 1773400] [--chip ym] [--restart] [--highpass 100 --passes 1] [--drive 180] [--rate 16000] [--limiter --highpass 300 --noise 11] [--noise-lowpass 3000]   # a ZX Spectrum tune (Pro Tracker 3 or Vortex Tracker II module) for the clock's own player and AY-3-8910: the module as it is behind a header with the machine's clock and frame rate (by the module's tone table unless told), the length and where it goes round; --restart for a module whose loop is only its ending; the clip is the chip as it is but for a 100 Hz high-pass, at a plain gain 3 dB over three channels at full scale, the user's choice of 2026-11-13 on headphones and on white-clock over the limiter, which --limiter brings back as the first PT3 clips had it
scripts/speaker-clips.sh   # the default pack for a clock with a speaker in the buzzer's place (speaker-mod/): packs/clips-default-speaker.bin, the two modules made again with a 100 Hz high-pass and the limiter's release at 128 ms (`--release 10`), under the default pack's names; heard and kept by the user 2026-11-15, after a pack with the limiter off was too quiet
scripts/mkpack.py packs/mine.bin clips/trantor.bpr wav/tune.pcm:loop=My\ Tune   # a clip pack for the `clips` partition: up to 16 clips, named and looped as ALARM_CLIP's are, `=Label` for another label
CLIP_PACK_FILE=packs/mine.bin cargo host-test the_pack_file_parses -- --nocapture   # the pack through the parser the clock reads it with
API_TOKEN='…' scripts/clips.sh <ip> [pack]   # POST /clips: write the pack (the default one, made of clips/, if none is named) and restart; with no arguments just make packs/clips-default.bin
espflash write-bin 0x2e0000 packs/mine.bin && espflash reset   # the pack over USB
ALARM_CLIP=clips/trantor.bpr,wav/tune.pcm cargo build --release   # the older way, clips built into the firmware, for a clock whose partition table has no `clips` partition; a pack takes their place where there is one (PCM, beeper, tracker module, SID tune or PT3 module), as the last melodies, named after their files; ALARM_CLIP_LABEL='A,B' for other labels; a ringing alarm loops a beeper clip, a module, a SID tune or a PT3 module with no gap and repeats PCM after a pause, unless the file is given as file:once or file:loop
CLIP_MODULATOR=sd CLIP_KBPS=256 cargo build --release   # a PCM clip or module through the software sigma-delta and not the chip's PDM converter (the default, `pdm`, 256 bits a sample): for a buzzer circuit that the converter's rate is too fast for; CLIP_KBPS is 128, 256 or 512 (default; a 16 kHz clip runs at twice the figure)
CLIP_MODULATOR=pwm cargo build --release   # the other software modulator: a pulse a carrier period (CLIP_CARRIER_KHZ, default 16)
CLIP_PDM_BITS=128 cargo build --release   # the PDM converter at 128 bits a sample, 2.048 Mbit/s for a 16 kHz clip, as the clocks' buzzers have heard it (256, 4.096 Mbit/s, is the default; 384 is the third): more bits are less of the converter's own noise and shorter pulses for the transistor
```

Releases: `CHANGELOG.md` gets the version's section first (the release body points at it), then an annotated tag `v*` (`git tag -a`; `build.rs` runs `git
describe` without `--tags`, so a lightweight tag would not reach the
version) runs `.github/workflows/release.yml`, which builds with the
CI's prebuilt settings, tests, and attaches `esp32clock-ota-<tag>.bin`
(the `/update` image) and `esp32clock-<tag>.bin` (`save-image
--merge`: the whole flash from 0, contiguous, so it covers `nvs` with
erased bytes and clears the settings; `write-bin` then needs an
`espflash reset`) to the GitHub release, then deploys `docs/` to the
`gh-pages` branch with the release under `<tag>/` (a manifest for ESP
Web Tools with four parts, the bootloader and partition table cut
from the merged image, an erased `otadata` and the OTA image at
0x20000, so `nvs` is left alone and the settings survive unless the
erase is chosen) and `latest/`, keeping earlier tags. `docs/index.html` is the
web installer at `https://anarsoul.github.io/esp32clock/`: it lists
the releases from the GitHub API, newest first, and points the install
button at the chosen tag's manifest; without the API it uses `latest/`.
Nothing in `docs/` but the page is in the repository.

`cargo run` flashes with espflash and opens the monitor; `.cargo/config.toml`
sets the runner and `ESP_LOG=info`. The runner passes `partitions.csv`
(two app slots, `ota_0` and `ota_1`, plus `otadata`) and erases
`otadata` so a USB flash always boots `ota_0`, where espflash put the
image. `scripts/ota.sh` builds, makes the application image with
`espflash save-image` and posts it to `/ota` with the token; the clock
writes the other slot, selects it and reboots. The token is
`API_TOKEN` at build time and a setting after that (`src/auth.rs`): a
build without one has no script path (a `Bearer` request is `403`)
until one is set from `/system` with the PIN or by `POST /token`, and
the boot line says `api token: stored`, `built in`, `cleared` or
`none`. The Wi-Fi credentials are seeds the same way, and `WIFI_SSID`
may be empty: whatever the record lacks is filled from the build at
boot, the next save carries it, and from then on the record wins over
a later build's constants (`settings::Settings::seed`). A token
cleared with `POST /token` stays cleared: the record's flags byte
(`token_cleared`, version 7) tells a cleared token from one never set,
and only the latter is filled from the build. The MQTT broker
(`MQTT_HOST`, `MQTT_PORT`, `MQTT_USER`, `MQTT_PASSWORD`) is a seed the
same way, with bit 1 of the flags (`mqtt_cleared`) for a host cleared
on the page. For a network debugging run set
`ESP_LOG='info,embassy_net=debug,esp_radio=debug'` on the command line
(it overrides the config, and esp-println compiles the filter in); the
extra lines are DHCP state and driver events, not packets. Every log
line carries the uptime in ms. `scripts/watch-clock.sh <ip>` sends a
498-byte ping (the largest the clock answers, see the MTU below) and
fetches `/api` from the host every 30 s for the other side of the story.

Credentials, the default timezone and the API token are read at build
time by `core/src/config.rs`; `core/build.rs` reruns that crate when
they change, and the root `build.rs` exports `git describe --always
--dirty` as `FIRMWARE_VERSION`, which
goes into the app descriptor, the boot line, the page and `/api`. The timezone (`tz`: a POSIX TZ rule such as `PST8PDT,M3.2.0,M11.1.0`, parsed by `core/src/tz.rs` and evaluated on the chip from the NTP time, so daylight saving needs nothing from the network; the key also takes minutes east of UTC, stored as the fixed rule `<-07>7`; the default is `TZ_RULE` from the build, checked by a `const` assert so a bad rule fails the build, else `TZ_OFFSET_MINUTES` as a fixed rule, the CI's `<-07>7` being Vancouver without daylight saving; the page's `Use this browser's zone` button fills it in through `/tz.js`, see `src/tz.js`), 12/24 hour, colour, brightness, alarm, alarm days,
alarm melody, thermometer offset (`temp_offset`, tenths of a degree
Celsius added to the NTC's reading, ±10.0, for the warm air at the
sensor's spot in the case; the default is −2.5, `config::DEFAULT_TEMP_OFFSET_DC`,
what the user's clocks typically need, for a clock with no record or one older than
version 8, while a stored value, 0 included, stays), the
temperature on the panel (`temp_show`: `off`, `c` or `f`, default
`c`; with a reading in hand the UI shows ` 23C` with the degree mark
for seconds 30 to 34 of every minute in clock mode, `ui::TEMP_SHOW_AT`
and `TEMP_SHOW_SECS`), DHCP
hostname, Wi-Fi transmit power (`tx_power`, dBm: `2`, `5`, `7`, `8.5`, `11`, `13`, `14`, `15` or `16`, see "Wi-Fi power" below), Wi-Fi network, API token and MQTT broker (`mqtt_host`,
`mqtt_port`, `mqtt_user`, `mqtt_password`; see "Home Assistant" below)
are settings:
saved in the `nvs` partition, editable from the buttons and from
the clock's pages or `/api` (form-encoded keys, JSON back). The pages
are five, each starting with a line of links to the others
(`form::Page`): `/` (the alarm with Play and Stop, the display, the
status lines), `/settings` (timezone, 12 hour, the temperature on the
panel and its offset, hostname, the transmit power with its warning, and the local-time line), `/ha` (the
MQTT broker and its status line) and `/system` (below). Each form is
posted to its own page and is a partial update, as `/api` is: only
the keys sent change, and a checkbox is sent either way, a hidden
`0` under the same name before it (`form::Check`), so saving the
alarm leaves 12 hour alone. On `/` the alarm and the display are one
form with Save under both; colour and brightness are posted to
`/api` by script as they are chosen, and the script takes their
names off the selects, so Save posts the alarm only and a tab opened
a while ago cannot put an old colour back (2026-11-15; without script
Save posts both). The script also makes Play and Stop plain buttons:
they are the form's first submit buttons, so Enter in the alarm time
played the tune. A post by script that fails says so above Save, and
a colour that did not reach the clock goes back in its select. A
saved form comes back to its page with `?saved=1` and the page says
`Saved.`; a refused one with `?error=value&key=tz`, and the page
names the field from `form::FIELDS` (the key's label, never the
query's text); the script takes the query off the address. A form's script posts
it with `fetch` (since 2026-11-15): a refusal's notice is taken out
of the answer and put above the form, which stays as typed, and a
saved form goes on to the page the redirect named; without script the
form is posted and reloaded as before. The alarm's days are seven
checkboxes all named `days`, and `form::apply` reads a `days` key
repeated in one body as a union (the first replaces, the rest add;
none ticked sends no key and changes nothing), while `/api` takes
`days=mon,wed` and `all`, `weekdays`, `weekend` as it did. Above the
boxes is a select of the presets (Every day, Weekdays, Weekend,
Custom), the user's of 2026-11-15: it has no name and is not posted,
the script shows it, ticks the boxes from it and sets it from them.
Under the
skip box the page says `Next alarm: Mon 07:00.`, or `Tue 07:00 (Mon
skipped)`, from `form::next_alarm` and the status's local time;
nothing while disarmed or unsynced. `/ha`'s script asks `/api` every
5 s for two minutes and loads the page again when `mqtt` is no longer
what it was served with, unless the form has unsaved changes. The
pages follow the browser's light or dark (`color-scheme`), a link is
one colour whether visited or not (`a{color:LinkText}`), a page's
title is `<hostname>: <page>` under the common heading, and the pages
say nothing of `/api`: that is the README's (the user, 2026-11-15:
"it should be in the documentation, not in the interface"). The
network keys `ssid` and `password` are taken on `/api` only with the
`Bearer` token, refused as `403` on the key alone (never on whether the
value would change, which would make a password oracle), and never on
the settings form; the broker keys `mqtt_*` need the token on `/api`
the same way (a broker of the attacker's choosing would be handed the
stored password) but the settings form takes them (`form::Allow`:
`PAGE`, `ALL`, `NONE`) under one rule, checked in `form::apply` after
the pairs: a form that moves the broker to another host without typing
the password again is refused (`FormError::BrokerPassword`, with its
own notice), so the stored password goes only to the host it was typed
for; the same host keeps it on an empty field, an empty host clears it,
the token is not held to the rule, and `POST /play` applies the form
with `ALL` since nothing is saved; a saved change resets the clock into the new network
(`heal::switch_mode`, with the carry record started over), so a wrong
one costs the LAN until the password is right again. On the access
point every save resets into the station once a network is stored,
whatever changed, so the timezone or hostname posted with the setup
form reaches flash before the clock leaves. The password and
the token are never read back: the JSON has `ssid`, `password_set` and
`token_set`. The page ends with a read-only status
line, and the JSON with the read-only keys `version`, `slot` (the app
slot the firmware runs from), `mode` (`sta` or `ap`), `uptime` (seconds), `heals` (self-heal
resets since power-up), `temp_c` and `temp_f` (the room in tenths, `null`
with no thermometer fitted or before the first reading), `light` (0 dark
to 100, `null` before the first reading), `rssi` (the station's signal
in dBm from its last beacon, read every 10 s by `net::wifi_task` into
an atomic, `null` while not connected and on the access point),
`utc_offset` and `dst` (the minutes east of UTC and whether daylight time is in force right now under the rule, `null` before the first time sync), `mqtt` (`off`, `connecting` or `connected`), `ringing` (whether the alarm rings right now) and `last_heal` (Unix seconds or null); posting them is refused as an unknown key. The settings keys carry
`mqtt_password_set` instead of the password. The page shows the
network and the signal under the firmware line (`Wi-Fi home, -62
dBm.`, or `not connected`, or `no network stored`), the broker on the
next (`MQTT connected 10.1.1.5:1883.`, `MQTT connecting …, last: not
authorized.` or `MQTT off.`), the room on the line after (`Room 23.4 °C (74.1 °F), light 62 %`), the local time the rule gives on the next (`Local time Sat 15:20 PST, UTC-08:00.`, with `, daylight time` appended when it is in force, for checking a rule), and the minute heartbeat logs the filtered millivolts of
both dividers for calibration. `skip_next` (page checkbox, key `skip`) is
the one setting the record does not store: it skips one alarm and is cleared
by it or by a power cut; a software reset (self-heal, panic) carries it over
in RTC RAM, see `src/carry.rs`. `POST /play` previews the melody chosen in the form
without saving; the page posts it by script so the form is not reloaded. `POST /stop` (the page's Stop button, the same way; no token, its body ignored) silences a preview and ends a ringing alarm as a press would. `rust-version` must stay at 1.95 or higher or cargo's
MSRV-aware resolver picks an esp-rom-sys that esp-storage cannot build with.

## Controls

| press | in clock mode |
| --- | --- |
| top short | next colour |
| bottom short | next brightness |
| top long | set alarm hour, then bottom short for minutes, bottom short again to leave |
| bottom long | arm or disarm the alarm (degree mark shows armed) |
| both long | show the IP, one octet at a time, twice |
| both held 10 s | reset into the access point; from the access point, back to the station if a network is stored |

Clip packs (2026-11-14): `http://<ip>/clips` says where the clips
come from (the pack in the `clips` partition with its size, the
firmware, or that this clock's table has no such partition, in which
case the page has no upload and asks for no PIN), lists their labels,
and uploads a pack: the raw body of `POST /clips` with `Bearer` or
`PIN`, as `/ota`. A bad header (`not a clip pack`, a length that is
not the body's) is refused before anything is erased and the pack in
use stays; a hash mismatch after the write leaves the clock without a
pack until the next upload. A good one restarts the clock
(`heal::reboot`). The melody setting is an index: after another pack
it names whatever is at that place, and a place past the table's end
is read as `beeps` (`Settings::from_record`), which is what a clock
updated over the air onto the old table gets for a clip. `/api` has
the read-only key `clips`, `pack` or `builtin`.

`http://<ip>/system` has four actions: the firmware upload, a new
API token, Reboot and Access point. Pressing an action's button shows
a four-digit PIN on the panel for a minute, or until it is used,
right or wrong (the page's script asks for it with `POST /pin`; the
`GET` issues none, since any site can make a browser send one), and
the field for it appears under that button (since 2026-11-15; before,
the page drew a PIN as it loaded and any answer meant a reload, which
lost the chosen file). After an answer the buttons work again and the
next press draws a new PIN; the token's rule and a missing file are
caught before one is drawn. `/update` and `/token`, the pages it replaced on 2026-11-14,
redirect to it.

Provisioning: a clock with no network in its record (and none built
in), or one whose buttons were held 10 s, boots as an access point
instead of a station for that whole boot, since the driver does not do
both at once. The panel shows `AP`; the network is `clock-xxxx` (the
last two bytes of the MAC), WPA2 with `AP_PASSWORD` from the build
(default `esp32clock`), channel 1, the clock at `http://192.168.4.1/`
with its own DHCP server (`src/dhcpd.rs`, eight leases, replies
broadcast) and no time, so no alarm. `GET /` there is the setup form
(network, password, timezone, hostname, the station's transmit power); saving answers a page and the
clock resets into the network once the record is written, or at once
if nothing changed. Both switches (`heal::switch_mode`) start the carry
record over, keeping only the OTA boot count: no time, heals or alarm
state crosses a mode change, and a panic in AP mode keeps the flag so
the clock comes back as an access point. A station that cannot join
never turns into an access point by itself: a wrong password is a
blank panel, `wrong password?` in the log every attempt, and the hold.
`/system`, `/ota`, `/api` and `/token` work on the access point too
(`/settings` and `/ha` do not: the captive redirect takes them, and
the system page there links to the setup form and leaves out its
access-point button);
`/api` takes the network keys there without the token. A captive DNS
(`src/dnsd.rs`) answers every name with the clock's address and any
path that is not a route is a `302` to `http://192.168.4.1/`, so a
phone's connectivity probe opens the setup form in its sign-in sheet
by itself. `POST /ap` with
the `Bearer` token, or the PIN from the system page, resets a running
clock into the access point, for a
bench without hands on the buttons. The access point runs as AP+STA
with a station half that never connects, because the driver refuses a
scan in AP-only mode: it scans once when it comes up and again on
`POST /scan` (the page's Rescan button, which comes back to `/` with
the list; `ap_task` counts the scans it has done and each waiter polls
the count, so two clicks from two tabs are each answered rather than
one waking the other's `Signal`), and the setup form lists what it saw, strongest first, with
an "Other" entry and a text field for a hidden network. The AP stays
up through a scan. A `POST` of `ssid=` (empty) leaves the network
unchanged, since the form always sends both the list's pick and the
typed name; a non-empty `ssid_pick` wins over `ssid` whatever the
order (`form::apply`), and the page also disables the typed field
while a listed network is picked, since a hidden input is still
posted. The form's password field is write-only and always posted:
on the setup form an empty password with the stored network keeps the
stored password unless the scan lists that network as open, and an
empty password for another network the scan lists as secured is
refused with the notice (`form::settle_password`); `/api` takes what
it is given. A setup form that changes nothing resets at once
(`Then::Join`); one that changes anything is committed and the
persist task saves it, then resets.

Any press ends ringing or the IP display, and presses in the second
after it are ignored (`ui::PRESS_QUIET`), so a two-handed slap does not
step a setting with its second event. Set mode times out after 15 s,
ringing after 5 minutes. Self-heal: once the gateway has answered a ping, ten
failed probes in a row (five minutes) with the link still up blank the
panel and reset the chip (`config::PROBE_RESET_FAILURES`). The probe is
a ping of the largest frame the clock sends (`config::PROBE_PAYLOAD_BYTES`,
498 bytes at the 540-byte MTU) because a marginal uplink passes short
frames while full ones fail; so do ten
failed connects in a row or three minutes of them
(`config::CONNECT_RESET_ATTEMPTS`, `CONNECT_RESET_SECS`), because the
driver avoids the access point that last dropped it and only a reset
clears that. The boot line and the heartbeat carry the count since
power-up. The time survives a software reset: the display tick writes
the Unix time to RTC RAM every second, a deliberate reset writes it once
more and pauses `clock::RESET_PAUSE`, and the boot adds that pause back,
so the panel shows the time again before Wi-Fi is up, within a second
(the RTC timer is not used; esp-hal recalibrates it at every boot and an
offset from it drifted with the uptime since power-up). An alarm missed by up to
10 minutes across a power cut rings on the first valid time after boot;
after a software reset the alarm last rung-and-ended or skipped within
that grace is not rung again, while one cut off mid-ring comes back. A
self-heal due while the alarm rings waits for the ring to end, by a press
or its timeout, before blanking and resetting.

`POST /reload` with the `Bearer` token, or the PIN from the system
page's Reboot button (since 2026-11-14; `auth::token_or_pin`),
answers once and
then resets the chip the deliberate way: panel blanked, time and alarm
state carried, not counted as a self-heal and not held for a ringing
alarm. It is the way to exercise the carry record from a script:
`curl -X POST -H "Authorization: Bearer $API_TOKEN" http://<ip>/reload`.

`POST /token` with `token=<new>` (8 to 32 printable characters without
spaces, empty to clear) and either `Bearer <current token>` or `PIN
<digits>` sets the API token. The system page has the field for it and
says whether one is set; an empty token asks the browser to confirm
before it clears. (Until 2026-11-14 the token had a page of its own
that nothing linked to; the user dropped that: "for the open source
project it doesn't make much sense to have it unlinked".) The body is parsed before the PIN is spent, so a bad
token costs a `400` and not the PIN. `POST /pin` issues a PIN without
the page, on the panel and in the serial log and never in the
response, for a bench script (`scripts/pin.sh`). One PIN is one
action: upload, token, reboot or access point.

## Home Assistant

The clock announces itself to Home Assistant over MQTT (M12, built
2026-11-07): one device named after the hostname, with the settings as
entities and the readings as sensors, through MQTT discovery; nothing
is typed into Home Assistant. The broker is the `mqtt_*` settings
above; an empty host is MQTT off, and clearing it on the page clears
the user and password with it. The client is `rust-mqtt` 0.6 (MQTT 5
only, so the broker must speak MQTT 5: Mosquitto 1.6 and later,
including Home Assistant's add-on, EMQX, NanoMQ), no TLS, QoS 0,
clean session, 60 s keepalive. The pure half is `core/src/mqtt.rs`,
the task `src/mqtt.rs`, spawned on the station only.

The id is `clock-` and the last three bytes of the MAC in hex
(`mqtt::DeviceId`), stable across hostname changes. Topics under
`esp32clock/<id>/`: `status` (`online`/`offline`, retained; `offline`
is also the will), `state` (the `/api` JSON byte for byte, retained,
published after connect, on every settings change debounced 300 ms,
on the ringing edges and every 60 s), `set/<key>` (the value as `/api`
takes it; one subscription `set/+`) and `cmd` (`play`, `silence`,
`reload`; its own subscription, which the first bench run found
missing). The keys `set/` takes are `mqtt::SET_KEYS` (colour,
brightness, alarm, armed, days, skip, melody, twelve_hour, temp_show,
temp_offset); anything else (`hostname`, `tz`, the network, the token,
the broker) is logged and dropped, so the bus is no way to move the
clock elsewhere. The alarm parser takes `HH:MM:SS` as Home
Assistant's `time` entity sends it. The discovery payload goes to
`homeassistant/device/<id>/config`, not retained, at every connect, on
a hostname change and when Home Assistant publishes `online` on
`homeassistant/status` (after a random delay under 2 s, as its docs
ask); every entity reads a key of the state JSON through `val_tpl`,
and the host test parses the payload and checks each key against the
JSON, each `set/` key against the allow list and each select option
against the parser. Nineteen entities: selects for colour, brightness,
melody and the panel temperature, a `time` for the alarm, switches for
armed, skip and 12 hour, a text for the days, a number for the
thermometer offset, sensors for temperature, light, signal, uptime and
self-heals, a binary sensor for ringing, buttons for play, silence and
reload.

The task resolves the host (an IPv4 literal or DNS), connects with a
5 s timeout each, and on any failure aborts, logs the reason once and
backs off from 5 s doubling to 60 s; a change to the broker settings
disconnects cleanly and starts over, a cleared host publishes
`offline` and waits. `poll_header` is the one cancel-safe client
future the loop selects on; a header is followed by `poll_body` to
completion, and publishes happen between polls. The three subscriptions are
confirmed by SUBACKs the loop matches by packet identifier: a reason
code of 0x80 or more, or no answer within 10 s, fails the session
with the reason on the page (`subscribe` only sends). A broker or
hostname change that lands during the start is compared after it,
not dropped with its notification. Received packets land
in a 1 KB `BumpBuffer`, reset after each event (the one `unsafe` in
the module). Before a deliberate reset (`heal::reboot`, `heal::reset`,
`heal::switch_mode`, the OTA reboot) `mqtt::goodbye` lets the task
publish `offline` and send DISCONNECT, within 500 ms, so Home
Assistant shows the clock unavailable at once rather than after the
broker's 90 s keepalive timeout; a panic cannot, and the will covers
it late. `cmd` `reload` says goodbye on its own connection and then
reboots. The heartbeat carries an `mqtt:` line (state, connects,
publishes, commands). Verify with `mosquitto_sub -v -t 'esp32clock/#'
-t 'homeassistant/#'` on the host.

Cross-site requests (2026-11-14): every request is held to
`core::http::admit` before a route is looked at. `Host` must be absent
(no browser), the clock's own IPv4 address, or on the station the
hostname bare or with `.lan` or `.local` (`HOST_SUFFIXES`; a port and
a trailing dot dropped, any case); on the access point the address
only, since the captive DNS answers every name. Another `Host` is a
`302` to `http://<ip>/` for a `GET` and `403` for a `POST`: that is
DNS rebinding, a name made to resolve to the clock so a browser calls
the attacker's page the clock's origin, and also a LAN with another
domain, which gets there by address. A `POST` whose `Origin` is
present and not `http://` plus one of those names (`null` included) is
`403`; absent is `curl` and the scripts. `Sec-Fetch-Site` is not
used, since a browser sends it only to HTTPS and localhost. No `GET`
may have a side effect (an `<img>` on any site sends one), which is
why `GET /system` issues no PIN. A new route that changes anything is
a `POST`, and is covered without doing anything.

Firmware updates: `POST /ota` with the application image as the body
and an `Authorization` header: `Bearer <API_TOKEN>` from `scripts/ota.sh`,
or `PIN <digits>` from the system page. Pressing Upload there (its script's
`POST /pin`) makes the
clock draw four random digits, show them on the panel for a minute
(any press ends it; ringing and alarm-set take precedence) and log them,
so the uploader must be able to see the clock or the serial port. A PIN
is good for five minutes and one attempt; a wrong one locks new PINs out
for ten seconds. The attempt, right or wrong, takes the digits off the
panel (`auth::pin_to_show` yields `None`, `ui::hide_pin`). The page posts the chosen file the same way as the
script (raw body plus header, no multipart) and polls `/api` until the
clock is back. The credentials are checked before
the body is read, and a refused upload reads and discards the rest of
the body before answering, or the browser sees a reset instead of the
`403`; then the first sector is checked for the image and
app-descriptor magics and the hash-appended flag, the scan is stopped
(the panel is dark for the whole write, about 11 s for 680 KB), the
other slot is erased, written and read back a sector at a time, the
SHA-256 espflash appends is checked against the streamed body, `otadata`
is switched and the clock reboots with the time carried over. Any
failure, a truncated or corrupt file included, brings the panel back on
the running image with `otadata` untouched. The bootloader only
falls back on a bad checksum, so the carry record counts the boots of
the new image: a software reset before a gateway probe has been
answered or an HTTP request served bumps it, and the third such boot
(`config::OTA_BOOT_LIMIT`) switches back. A power cut clears the count.

## Layout

The pure modules live in `core/src/` and the firmware re-exports them
from `src/lib.rs`, so firmware code says `crate::settings`; inside
`core` the same `crate::` paths point at its own modules. Seven modules
are split, the pure half in `core/src/` under the same name and the
hardware half in `src/`: `buttons` (event types / GPIO task), `clock`
(calendar arithmetic / the offset and the carry), `carry` (record
codec / RTC static and reset gate), `http` (head parser and
`RESPONSE_BUF` / sockets), `ota` (image constants, `Slot`,
`hash_chunk`, `version_from` / flash and partitions), `pack` (the
clip pack's layout and checks / the partition, its mapping and the
upload), `clip` (the
pulse-width `Player` and the ring's `Pacer` / I2S0, its DMA ring and
the pin). Each `core`
module ends with its `#[cfg(test)] mod tests`.

| file | role |
| --- | --- |
| `core/src/lib.rs` | `#![cfg_attr(not(test), no_std)]`; the module list; nothing else |
| `core/src/aip33628.rs` | frame format, bit-reversed current table, quad-nibble waveform encoder; pure `core` |
| `core/src/panel.rs` | COM/SEG map, font, per-position colour as duty `Levels` (no RGB; the palette is written in levels), `Panel::render` → `ScanImage` |
| `src/scan.rs` | cycle/transfer (one lap) builder; the scan task, which builds the SPI2 quad + DMA bus itself (an async esp-hal driver is `!Send`), runs one plain dark transfer so esp-hal programs the registers, starts the ring and from then on only acts on `stop` (the ring ends on its blanking frame within two laps, before a flash write), `resume` after a failed upload and `halt` before a deliberate reset (the stop latched: `resume` is void after it, so an upload failing under the reset's pause cannot restart the scan); `publish` forwards to the ring; `Report` for the heartbeat |
| `src/ring.rs` | the scan as one SPI2 DMA-controlled configurable segmented transfer (TRM 27.5.8.5) on a one-segment descriptor ring: `Ring`, a `DmaTxBuffer` of a conf descriptor (bitmap with magic 0xA and the USER word) and eleven data descriptors of two cycles each, the last pointing at the first, whose `prepare` switches the mode on in the window esp-hal leaves between programming the registers and setting `usr`; `request_stop` clears `usr_conf_nxt` in the conf USER word; `publish` builds into the spare buffer and repoints the data descriptors (`point_at`), holding the buffer it left for two laps before it is written again; `position` for the heartbeat. The one module with register access, through esp-hal's `regs()` |
| `src/display.rs` | UI loop: tick, button or settings change → `ui` → draw → publish; rings the alarm and signals the ringing edges (`ringing_changed`) for the MQTT state; `silence` ends a ring as a press would; shows the IP, or `AP` in place of the sweep on the access point; acts on the hold before the UI sees it; seeds and carries the skip flag and the time the alarm last rang |
| `core/src/ui.rs` | mode state machine, pure logic, one timer for the current mode; edits the settings it is handed; alarm due check and missed-alarm grace, both gated by the carried local second the alarm last rang (local rather than Unix, so the hour a fall-back transition repeats rings once; an alarm minute a spring-forward transition skips rings when the clock lands past it); consumes `skip_next`; `view` takes the room reading and answers the `Temp` face mid-minute in clock mode |
| `core/src/settings.rs` | `Settings`, palette and brightness tables, `Hostname`, `Text`/`Secret` strings, `Credentials`, the token and the MQTT `Broker` with their rules, the build-time `seed`, shared `Watch` with change signal, versioned append-only flash record |
| `src/storage.rs` | the one flash handle (`with_flash`), nvs partition lookup, `sequential-storage` map over it, debounced persist task that compares records, not structs, and resets into station mode after a save that changed the credentials, or after any save on the access point with a network stored |
| `src/heal.rs` | deliberate resets: `reboot` says the MQTT goodbye, blanks the panel through `scan::halt`, carries the time and resets, for `/reload` and the OTA reboot; `reset`, the self-heal, waits out a ringing alarm and counts itself first; `switch_mode`, the reset between station and access point, which starts the carry record over and freezes it |
| `core/src/auth.rs` | the API token (a setting; constant-time compare) and the panel PIN with its TTL and lockout; `token`, `token_or_pin`, `issue_pin`, `pin_to_show`; `Refusal` with its HTTP status; pure rules with the clock and the random number passed in |
| `core/src/form.rs` | key=value parsing, all-or-nothing `apply` on a copy with the `Allow` flags for the guarded keys, JSON, the five pages (`Page`; `write_page`, `write_settings_page`, `write_ha_page`, `write_clips_page`, `write_system_page`) over the shared `write_top` (head, heading, the line of links, the error notice), `Check` (a checkbox with its hidden `0`), `SAVE` and `UNSAVED_JS` (a script compares the form with the defaults it was served with), the read-only `Status` lines, the script of the two pages that need the PIN, `PIN_FORM` (the PIN's field, hidden until asked for) and `PIN_JS` (`ask`, which posts `/pin` at a button's click and moves the form under it, `say`, `go`, `back`, `up` for an upload) before the system page's `SYSTEM_JS` (four actions, a PIN each) or the clips page's `up('/clips')`, `Note` and `FIELDS` (what a page says of the post that led to it, with the bad field's label), `apply_naming`, the setup form, `JsonStr`/`HtmlStr` escaping for the SSID, `parse_token`; pure logic |
| `core/src/http.rs` | `Method`, `Head` (`content_length` is `None` when the header is absent, and `transfer_encoding` says one was sent), `parse_head`; `RESPONSE_BUF` and `PAGE_MAX`, which the page tests assert against |
| `core/src/mqtt.rs` | Home Assistant over MQTT, pure: `DeviceId` and its topics, `Link` (the state for the page), `Route` and `Command`, `SET_KEYS` and `apply_set` over `form::apply_one`, `write_discovery` (the device payload with every entity) and `DISCOVERY_MAX` |
| `src/mqtt.rs` | the MQTT task: resolve, connect with the will, subscribe, discovery and state publishes, keepalive pings, the select loop over `poll_header`, the back-off, `goodbye` for the reset paths, `link()` and `Report` for the page and the heartbeat |
| `src/http.rs` | HTTP/1.0 server, `LISTENERS` sockets each in its own task: every request goes through `http::admit` first and one it refuses is answered by `turn_away` (a `302` to the address for a `GET` under another `Host`, else `403`; not counted as served) and `status_line` sends `X-Frame-Options: DENY`; a `POST` without `Content-Length` to a route that reads a body (`/`, `/settings`, `/ha`, `/api`, `/play`, `/token`, `/ota`; `reads_body`) is answered `411` and any `Transfer-Encoding` `400` before a route is looked at (`NoRequest`), since a bodiless post is not an empty form, while the command posts (`/reload`, `/ap`, `/pin`, `/scan`, `/stop`) take a bare `curl -X POST`; `GET /clips` (the clips page) and `POST /clips` (a pack streamed to `pack::install`, read as `/ota`'s body is; `411` without a length); `GET`/`POST` on `/`, `/settings` and `/ha` (the three forms, on the station; `/` is the setup form on the access point) and `/api`, `POST /play`, `POST /stop`, `GET /system` (issues no PIN; its script posts `/pin`), `GET /update` and `GET /token` (redirects to it), `POST /ota` with the body streamed to `ota::install`, `POST /reload`, `POST /ap` and `POST /token` with the token or the PIN, `POST /pin`, `POST /scan` on the access point, `GET /favicon.ico` and `GET /tz.js` (`src/favicon.ico` and `src/tz.js` embedded, the two cacheable responses; the page loads the script as `/tz.js?v=<version>` so an update is not served the cached one); the pages are headed `Clock: <hostname>` and titled `<hostname>: <page>`; a saved form redirects to its own page with `?saved=1` and a refused one with `?error=…` and the bad field's `key`; per-listener state and counters for the heartbeat |
| `core/src/buttons.rs` | `Button`, `Kind`, `Event` |
| `src/buttons.rs` | debounced short/long press events, the two-button long press, and the 10 s two-button `Hold` |
| `core/src/melody.rs` | alarm tunes as pitch and length data with tempo; `RANDOM` and the pure `pick` that turns it into a tune; `CLIP_COUNT`, `CLIP_NAMES` and `CLIP_DATA` (included from `clips.rs`, which `core/build.rs` writes from `ALARM_CLIP`; none by default); the table at run time, the nine note melodies and then the clips of the pack `install` was given, else the built-in ones: `count`, `get` (a `Melody` by value, the beeps past the end), `all`, `clip_data`, `clip_loops`, `LIMIT` (what a settings index is held under), `RANDOM` (`usize::MAX`); the pack in force sits in a `critical_section` cell, a thread-local in the tests; pure |
| `core/src/pack.rs` | the clip pack: `CLPK`, a version, up to `MAX_CLIPS` (16) entries of offset, length, loop flag, API name (20 bytes) and label (32), the clips' data, a SHA-256 at the end; `declared_len` from a header, `Pack::parse` with every check (`PackError`), `clip(k)`; the test `the_pack_file_parses` reads the file `CLIP_PACK_FILE` names |
| `src/pack.rs` | the `clips` partition: found by its label in the table (absent on a clock flashed before 2026-11-14, which is not an error), its pages written into the flash MMU's entries below the last (`map`; refused if any is in use), the pack parsed through the mapping and installed, all in `init` before the settings load; `install`, the upload of `POST /clips`: header checked, pack out of the table, a clip playing from it stopped (`clip::playing_in`), scan stopped, `ota::write_hashed`, the cache invalidated, the pack parsed in place (its first sector erased if refused), then the caller reboots; `room` for the page |
| `core/src/carry.rs` | `Carried`, `MARK` with its layout version, `encode`/`decode` |
| `src/carry.rs` | RTC-RAM record used only after a software reset (`CoreSw`/`Cpu0Sw`; anything else zeroes it at boot): self-heal count and time, the skip flag, the local second the alarm last rang, the Unix time last written, boots of an unconfirmed OTA image, the access-point flag for the next boot; the mark carries a layout version; `freeze` for a mode switch |
| `src/buzzer.rs` | LEDC PWM on an open-drain pin; plays a melody on command, looped or once, stops within a note; a clip goes to `clip::play`, which hands it to the clip task on the same pin, and for a ringing alarm is asked to loop with no gap if `melody::CLIP_LOOPS` says the clip is made to (`Stream::new(data, looping)`: a beeper clip starts over, a module follows its own jump back, PCM wraps) where the note melodies repeat after `repeat_gap_ms` |
| `core/src/clip.rs` | sampled sound for the 1-bit buzzer line, pure: `Player`, which in the default build (`CONVERTER`, from `config::CLIP_PDM`) writes a PCM clip's or a module's levels as 16-bit samples for the chip's PCM-to-PDM converter (`fill_samples`: the ramps, the clip with full scale at 0.85 of the rail, `CONVERTER_LEVEL`, the loudest that did not overload by ear, `SAMPLE_REST` for the released line) and `converter_clock`, the bit-clock divider and the true ring rate under the converter's whole dividers; in a `CLIP_MODULATOR=sd` or `pwm` build a software modulator, sigma-delta a bit at a time or pulse-width (one pulse per carrier period, centred, its length the level at mid-period with the rounding error fed back as far as the carrier leaves room: `ORDER`; integer, linear interpolation between the 8 kHz samples, a sample of 255 at 0.95 of the rail) that writes a clip's bitstream a buffer at a time, wrapped in 30 ms ramps from and to the released line; `Beeper`, a clip that is the line's level as run lengths (`BPR2`, LEB128 counts of 1/512 000 s, off and on alternately; `scripts/zxbeeper.py`), written out as it is, with the line at its own `BEEPER_BIT_RATE`, 512 kbit/s (`Stream::bit_rate`); `Runs`, which writes a `RunSource` (the beeper clip) out as bytes; `Stream`, either kind by its first bytes (`Stream::new` is never inlined and builds a tune's player in place through `Player::take_*`: inlined, with a slot a kind, it was 6 KB of the clip task's frame, on whatever stack the interrupt found), with `converter`, `sample_rate`, `rest` and `bit_rate` (the rate the ring is read at: the line's bits, or 16 a sample) for the firmware half; `Pacer`, which says which chunks of the ring to write from the time since the start alone (one crystal clocks the line and the timer), counts underruns, and counts as strays the reads of the DMA's descriptor register that are not two slots on from that; `BIT_RATE` and `CARRIER_HZ` from `config::CLIP_KBPS` and `CLIP_CARRIER_KHZ`, `CHUNK` 1024, `SLOTS` 16 |
| `core/src/xm.rs` | a tracker module as a clip, pure: `Tune`, a player for the part of FastTracker 2's XM that chiptunes use (see its header for the effects), reading in place the `XMC2` file `scripts/xm2clip.py` makes of a module, no heap, eight channels; mixes at 32 kHz, averages to 16 kHz (8 if the clip asks; at 62.5 kHz to 31.25 for a clip at `clip::FAST_HZ`), then a high-pass and a hard limiter as `scripts/wav2pcm.sh` gives PCM (`shape.rs`), their settings read from the clip's header (the converter works out the filter's coefficients); its samples go through `shape::Shaper` to `clip::Player` as `i16` in 256ths of PCM's 8-bit step (`xm::STEP`), whole steps only for a clip converted with `--bits 8` |
| `core/src/sid.rs` | a Commodore 64 tune as a clip, pure: `Tune` plays the `SID1` record `scripts/sid2clip.py` makes (the register writes of each frame, then a byte for the frames to the next; a loop offset in the header) through `Chip`, a SID written for the clock: three voices (accumulator, triangle with ring modulation, sawtooth, pulse, noise, combined waveforms as an AND, sync, test bit, the envelope with the chip's rate periods, exponential decay and rate-counter wrap), the filter with a 6581's or an 8580's cutoff curve, routing and volume; stepped 64 000 times a second (32 000 if the clip's header says 2 steps a sample) and averaged to 16 kHz, or to 31 250 for a clip that says so, then `shape::Shaper`, with the limiter's knee at three voices' top for a clip whose flags say `FLAT` (a plain gain, for a speaker); no 6502 on the chip, no samples through the volume register |
| `core/src/ay.rs` | the AY-3-8910 (and YM2149), pure: `Chip` with `write` and `run(ticks)`, which answers the output summed over that many ticks of clock/8 by going from one change of a generator to the next, so a sample is the chip's exact mean and nothing aliases; three tones, the 17-bit noise, the envelope's sixteen shapes over 32 positions, the mixer, the levels as measured on the chips (`LEVELS_AY`, `LEVELS_YM`, Ayumi's tables: not the data sheet's even 3 dB, which made notes 3 to 4 dB too quiet beside the drums), the three channels added up; `set_noise_level`, the clock's own, mixes a channel that has the noise in it at so many sixteenths, for a buzzer that makes more of white noise than of notes (a clip's header byte 38, `scripts/pt32clip.py --noise N`, 11 by default, chosen by ear); `run_parts` answers the output as `Parts`, the noisy channels apart with their tones and the noise's share each by itself, for the player's optional low-pass over the noise alone (header byte 39, `--noise-lowpass HZ`, off by default: the lower level was preferred to it); a generator nothing listens to is moved on in one go; a counter past a shortened period turns at the next tick, not at the write |
| `core/src/pt3.rs` | a ZX Spectrum tune as a clip, pure: `Tune` plays a Pro Tracker 3 / Vortex Tracker II module in place behind the `PT31` header `scripts/pt32clip.py` gives it (clock, cycles a frame, length, flags for a YM2149 and for starting afresh at the end): the pattern codes, the effects (tone slide, portamento, sample and ornament offset, on/off vibrato, envelope slide, tempo), samples and ornaments a line a frame, the tone and volume tables by the module's version; every read bounds-checked; no TurboSound; then, for a clip whose header says `FLAT` (the converter's default), its mean taken out (a 5 Hz tracker) and `shape::Shaper` as the clip's high-pass and a plain gain, the header's drive in 127ths of three channels at their top as full scale; without the flag the shaper limits as it does a module |
| `core/src/shape.rs` | `Shaper`, the high-pass and limiter the players end in, set from a clip's header; `STEP` |
| `src/clip.rs` | `clip_task`, on the scan's interrupt executor so that Wi-Fi work on the thread executor cannot starve the ring (the buzzer task, whose LEDC driver is not `Send`, stays there and asks with `play`, lending the pin for the clip's length; `main.rs` makes the second pin handle, its one `unsafe`); `Sampler`: I2S0's transmitter on GDMA channel 1, in PDM mode with the PCM-to-PDM converter for a PCM clip or a module (mono 16-bit samples in the ring, 256 bits each on the line by default; `tx_pcm_bypass` set again after esp-hal 1.2.2 clears it, or the A-law compander makes white noise of the samples) and in TDM mode for a beeper clip and the software modulators (two 8-bit channels, no clock pins, so the data line is the ring's bytes MSB first at the bit rate), looping over a sixteen-descriptor ring; `play` writes the clip's bits (`clip::Stream`) from flash into the ring ahead of the DMA, connects GPIO5 as a frozen open-drain signal through the GPIO matrix's inverter once the stream runs, and hands it back released before stopping, and logs the clip's chunks, underruns and the time spent filling the ring (the player's share of the processor); reads the DMA's descriptor register through `regs()`, for the pacer's check only, and writes I2S0's `tx_conf` for the bypass bit |
| `core/src/sensors.rs` | the NTC table (10 kΩ, B 3950, under the adapter's 10 kΩ pull-up from 3V3) and `temperature_dc` by interpolation, `fahrenheit_tenths`, `ldr_ohms` and `light_pct` (the logarithm of the LDR's resistance, 1 kΩ is 100 and 50 kΩ is 0), `face` (the panel's four characters for a temperature), `Tenths`; `Hold`, the rule that holds the thermometer for the heat of the buzzer's transistor: told once a second whether a clip is playing through the PDM converter, it holds for `HOLD_PER_SECOND` (4) seconds a second played, `HOLD_MIN_SECS` (120) at least and `HOLD_MAX_SECS` (1200) at most, plays within a hold adding up; pure |
| `src/sensors.rs` | `sensor_task`: the async `Adc<ADC1>` with `AdcCalCurve` (the scheme that reads straight on the C3) on GPIO0 (NTC) and GPIO1 (LDR), 8 samples a second each into a 10 s exponential average, published as millivolts in two atomics; while `clip::converting()` and for the hold after it (`sensors::Hold`) the NTC's cell keeps the reading from before the clip, so the panel, `/api` and MQTT do, while the average goes on underneath for the log, which says `held`; a reset loses the hold; `room()` for `http::status`, `Report` for the heartbeat |
| `core/src/tz.rs` | POSIX TZ rules: `parse` (`const`, so a build-time rule is checked by the compiler), `Tz::period_at(unix)` (the offset, the daylight flag and the name in force), `fixed(minutes)` for the old setting, the calendar arithmetic; `TZ_MAX` 48 |
| `src/tz.js` | the page's `Use this browser's zone` button, served as `/tz.js`: the browser's IANA zone as a POSIX rule from next year's two offset changes (so a zone dropping daylight time this year, as Vancouver did in 2026, comes out fixed), each transition's rule checked against the six years after (a midnight change is Saturday 24:00 in Chile and Sunday 00:00 elsewhere), names from the first locale that has letters; throws, and the page shows the message, for a zone with other than none or two changes a year or none of whose candidate rules predicts the six years after (Casablanca's Ramadan hour); `scripts/tz-check.mjs` runs it under Node |
| `core/src/clock.rs` | `LocalTime` (with the `offset_secs` it was computed with and `local_secs`), `weekday_of_days`, `local_time(unix, offset_secs)` |
| `src/clock.rs` | Unix time = uptime + NTP offset; `period_at` and `local_time` under the settings' rule; `carry_time`/`restore_carried` move the time across a software reset as plain Unix time, written every second by the display tick; `RESET_PAUSE` for every deliberate reset |
| `core/src/ota.rs` | image header and descriptor offsets, `Slot` names, `hash_chunk` for the appended SHA-256, `version_from` |
| `src/ota.rs` | firmware upload: slot lookup and the boot-time rollback, sector writer with read-back and the appended hash (`write_hashed`, shared with `pack::install`, as are the buffers), activate, reboot; `VERSION` from `build.rs` |
| `src/net.rs` | Wi-Fi station on the stored credentials with a scan before every connect, the connect pinned to the BSSID and channel that scan heard loudest (a new choice each time; the driver chooses only when the scan heard nothing), and a transmit-power ceiling, minute RSSI line, connect failures logged with their meaning (`wrong password?`, `no such network in range`) and a budget that ends in `heal::reset`; DHCP switched on 500 ms after each connect and off at disconnect; link and address monitor, gateway ping probe with the self-heal trigger, SNTP, DHCP renew on hostname change; the boot's `Mode`, the access-point task (AP+STA, scans on request into the network list) and its static stack config |
| `src/dhcpd.rs` | DHCP server for the access point: `parse`, `build` and the lease table pure, the socket task around them; every reply broadcast |
| `src/dnsd.rs` | captive DNS for the access point: `answer` pure (every A question is the clock, anything else an empty answer), the socket task around it |
| `core/src/config.rs` | build-time env vars and compile-time defaults |
| `core/build.rs` | `rerun-if-env-changed` for the build-time variables `config.rs` reads; writes `clips.rs` (count, names, labels, `include_bytes!` of the files in `ALARM_CLIP`, and which of them loop) into `OUT_DIR` |
| `src/bin/main.rs` | pin setup, display wires with their pull-downs, executors, task spawning, panic reset hook, log timestamps, reset reason and carried time at boot, OTA rollback check, minute heartbeat log |

## Hardware facts that bite

- **Buzzer (GPIO5)** drives a PNP with its emitter at +5 V. Any driven level
  turns it on, including a 3.3 V high. Only a released open-drain line is
  silent. Never make it push-pull. The sampled clip drives it from
  I2S0 with the GPIO matrix's inverter on, so a 1 bit is the buzzer
  on; a peripheral given the bare pin would make it push-pull, so it
  goes in as an open-drain `Output` turned into a signal, which
  esp-hal freezes. A PCM clip or a module is the chip's PDM converter's
  bits, 8 Mbit/s for a clip at 31 250 samples a second, the converters' default, and 4.096 Mbit/s for a 16 kHz one. At 2.048 Mbit/s
  (16 kHz, `CLIP_PDM_BITS=128`) on white-clock (flyback diode, R5 at
  5.1 k) it was cleaner than the software sigma-delta at 1 Mbit/s
  (2026-11-13); a stock board's transistor has not been heard
  at either, and `CLIP_MODULATOR=sd` is the way back. The default
  of 256 bits a sample is the user's choice of 2026-11-13 and no
  clock's own transistor and buzzer have had it yet: heard only on a
  breadboard copy of the stage with
  headphones for the buzzer (`bench/`, 2026-11-13: a BC557, 5.1 k, a
  100 Ω load), where a silent clip hissed at 128 bits and not at 256,
  and where clips with heavy bass were distorted at either until a
  2.2 kΩ resistor from base to emitter let the saturated transistor
  turn off in time. No clock has that resistor; whether an S8550
  saturates depends on its load. The converter at 64
  bits a sample is white noise. The inverter is part of the pin's connection:
  `clip.rs` takes the pin back as a plain `Output` when it ends,
  which clears it, and a tone's LEDC must never be connected over an
  inverted pin (its released level would be the buzzer on). The
  clip's bits come in long pulses: a per-bit sigma-delta stream at
  256 kbit/s was quieter than one pulse per 16 kHz period
  (2026-11-12). On the scope the pulses at the buzzer are as wide as
  GPIO5's, to the full 5 V, and with no diode on the board the coil
  kicks to −10…−15 V at each turn-off; the clip is still much quieter
  than the beeps, which sit on the buzzer's resonance. The phantom
  presses seen while a clip played were the user's adapter, pins 9
  and 10 of the DIP-16 shorted, not interference. The user's mod, in
  the README as optional: a 1N4148 across the buzzer as a flyback
  diode (cathode to the collector side), which takes the negative
  peak away, and R5 at 5.1 kΩ instead of 10 kΩ, whose doubled base
  current squares the collector's falling edge (a slope with 10 kΩ)
  and makes the buzzer louder. The firmware assumes neither.
- **The GDMA's `out_dscr` is not the descriptor being read.** On the
  C3 it names the one two further on, from the moment a transfer
  starts (traced against the clock, 2026-11-12). The clip's pacer took
  it for the current one and rewrote every chunk as it was played,
  which was heard as skips, crackle and the clip overlapping itself,
  with no underrun counted. Pace a DMA ring by time; use the register
  as a check at most. `ring::position`, the scan heartbeat's figure, is
  off by the same two.
- **Display wires (GPIO6–9)** are SPI2 SIO0–3. The SPI builder resets the
  pull of a bare pin it takes, but leaves a `Flex` alone (it arrives as a
  frozen signal), so `main.rs` sets the idle pull-downs on `Flex` pins and
  the scan task builds the bus from those. No PAC access for the pins;
  `ring.rs`, `clip.rs` and `pack.rs` are the modules that touch registers, SPI2's
  segmented-mode bits, each DMA channel's current descriptor and the
  flash MMU's table, through esp-hal's `regs()`.
- **Buttons:** GPIO4 is the upper button, GPIO3 the lower. No board
  pull-ups; the internal ones are used.
- **Sensors (GPIO0, GPIO1):** the adapter's 10 kΩ pull-ups from 3V3
  with the clock board's NTC (R4) and LDR (R3) to ground; the board's
  own R1 and R2 pull up to 5 V and must stay off. A pin reads high
  when the part is cold, dark or missing, so only the NTC can report
  "no sensor" (the pull-up alone is colder than −20 °C); a missing
  LDR looks dark. The NTC reads a few degrees above the room on the
  bench and creeps up with uptime (2026-11-01): the board warms it;
  the user's thermometer is the reference, not the reading, and the
  `temp_offset` setting carries the difference (−0.7 on blue-clock). Q1,
  the buzzer's transistor, sits beside the NTC and runs at up to 0.2 W
  while a clip plays through the PDM converter (its pulses are shorter
  than the stage switches; cold on beeps and beeper clips): five
  minutes of a module read 9.2 °C high on white-clock and took sixteen
  to come back (2026-11-14), which is what the hold in `sensors` is
  for. A clock
  built on the adapter with the C3 Zero's chip antenna hears the
  access point 10 to 25 dB weaker than the old bench board did at the
  same spot (the antenna sits inside the adapter's ground fill); a
  U.FL receptacle at the feed and a patch antenna put 25 to 30 dB
  back (2026-11-01: −29 dBm on the near access point at the bench;
  installed in the case, both access points read it at −70). The
  SuperMini adapter with a SuperMini Plus and an external antenna on
  its IPEX jack reads
  −21 to −23 there, against −38 to −54 for the old white-clock board in
  the same case and spot (2026-11-12; the access points' side not yet
  read); the v3 Zero adapter, its chip antenna over a notch at the
  board's edge, reads −52 to −60, level with the old board at the near access point,
  and unlike it always lists the far one, at −86 to −92. ADC1 is the converter that works with Wi-Fi on,
  11 dB attenuation is only calibrated to about 2.5 V, and
  `AdcCalCurve` is the scheme to use: the line fit is percent off.
- **Current:** `IS` comes from brightness alone, capped at
  `MAX_CURRENT_STEP`, a build-time variable, default step 5 (the 50 %
  step, about 15 mA per sink); `settings::BRIGHTNESS_STEPS` is cut at
  the cap, so the button, the page and the API offer 10, 25 and 50 % by
  default and a stored higher step lands on the brightest one offered.
  With the transmit ceiling at 16 dBm the
  board browns out on a quick step to 75 % or 100 % at the old cap of
  11; the supply, not the firmware, is the limit (2026-10-22). Never
  derive the current from how much is lit, and never park the panel
  all-white at high current; the drivers get hot.
- **Wi-Fi power:** a setting since 2026-11-15, verified on the bench board that day (`tx_power`, a select
  on `/settings`; `settings::TX_POWER_CHOICES`, the driver's levels
  from 2 to 16 dBm; record version 12), for a board whose antenna
  wants less, as the bench board's ceramic one. `net::apply_tx_power`
  gives it to the driver before every connect and every 10 s, so a
  change needs no reset. Not an MQTT `set/` key. It is the station's
  only: the access point transmits at `config::AP_TX_POWER_QDBM`, 44
  (11 dBm, enough for a phone beside the clock), whatever the setting,
  and never changes it, and the setup form has the select too, which
  is the way back for a clock set too low to reach its network; the
  settings page says so (the user's, 2026-11-15, after a first version
  that reset the setting on the access point). The default is `WIFI_TX_POWER_QDBM` from the build,
  else the ceiling. `WIFI_MAX_TX_POWER_QDBM` is 68, the ceiling, which
  the driver rounds to its 16 dBm level (the level table is in the
  constant's comment; the log prints the setting). The access point hears the
  clock about 20 dB below what the clock hears of it, so the uplink is
  the side that fails, and the clock's own RSSI line looks fine while
  pages and lease renewals are lost. 14 and 16 dBm each gained a full
  step at the access point; 18 and 20 dBm read louder at the near AP
  but quieter at the far one, with slower probes and failed
  associations, which is the SuperMini antenna mismatch. Measure any
  change with the AP's signal figure for the clock, from inside the
  case, not with the clock's RSSI.
- **First frame after connect:** the driver reports the station
  connected before it can pass a frame, so a DHCP discover sent at that
  instant is lost and smoltcp retries only after 10 s. The stack starts
  with no IPv4 config and `net::wifi_task` enables DHCP `LINK_SETTLE`
  after the connect. embassy-net does not wake its runner on a config
  change; `net::poke` (a throwaway UDP bind) does, and must follow any
  `set_config_v4`.
- **Flash writes** run with interrupts off, so the scan stalls: a record
  write is invisible, a sector erase is a brief dark blink. That is why the
  persist task debounces and skips writes that would not change the record,
  and why a firmware upload stops the scan first rather than flicker
  through 200 erases. A save that fails is tried again after 2 s,
  doubling to 60 s, with whatever the settings are by then.
- **One flash handle.** `storage::with_flash` is the only way to the
  flash, one blocking operation at a time, never across an await. A
  second `FlashStorage` does not work: its constructor sizes the flash
  with a chip-ID command over SPI1, and after the first handle has used
  the flash that read comes back 0, so every operation on the second
  handle fails its bounds check (measured 2026-10-22).

## Timing rules in the scan path

- The CPU is not in the scan loop. The transfer is one segment of SPI2's
  configurable segmented transfer on a descriptor ring (`ring.rs`), and
  the hardware runs lap after lap by itself; the gap between laps is the
  CONF state, a fixed 0.6 µs after the blanking frame with the lines idle
  (bench and scope, 2026-11-08). Until then the scan task restarted every
  transfer: 10 to 19 µs each time, and up to 1 ms whenever a critical
  section held the done interrupt off (esp-sync's `RawMutex` masks every
  interrupt on this chip: the Wi-Fi blob's lock, the heap lock on every
  frame buffer, esp-println's lock while a monitor drains the USB FIFO),
  which no priority could fix; the history is in PLAN.md under 2026-11-07
  and 2026-11-08. The scan task keeps its interrupt executor, but nothing
  time-critical runs on it now.
- A frame must fit in one sub-frame unit (`UNIT_BYTES >= FRAME_BYTES`, a
  compile-time assert). Raising `SPI_HZ` or lowering `UNIT_US` changes the
  refresh rate; 20 MHz / 5 µs / three planes gives 7.1 kHz, which is what
  the shimmer tests settled on. Fewer planes shorten the cycle; more planes
  make it visibly shimmer.
- The lap ends with a blanking frame so the CONF state between laps is
  dark. Keep it. A data descriptor holds whole cycles (two, 2800 bytes;
  the last 2847 with the blanking frame; `CHUNK_SIZE` is 4092, asserted),
  so an image swap, which repoints the descriptors one aligned store at
  a time, changes the image at a cycle boundary within one lap.
- A published buffer may still be read for the rest of the lap in flight,
  so `ring::publish` waits `HOLD` (two laps and a bit) after a swap before
  it builds into the buffer it left. One publisher, the display task.
- `stop` takes up to two laps, about 6 ms: the cleared `usr_conf_nxt` in
  the conf buffer is applied at the next CONF state and that lap is the
  last. The ring's data stays the live image, so `resume` shows what was
  showing.
- `Instant - Duration` panics when uptime is shorter than the duration. Use
  `Option<Instant>` for "last time X happened".
- Never reset the chip while a lap is in flight: the row the drivers
  last latched stays lit at full current until the firmware is back. A
  deliberate reset goes through `heal::reset`, which waits for
  `scan::halt` (the ring ends on its blanking frame, and the stop
  is latched against an upload's `resume`) first. The panic
  path in `custom_halt` cannot do this and is the one exception.
- The heartbeat's `scan` field is images published, stops (and stops that
  had to cancel the transfer, which the bench never showed), running or
  halted, and the index of the descriptor the DMA is reading (0 the conf
  descriptor, 1 to 11 the data), which must differ between heartbeats.

## Sizes that bite

- `http::RESPONSE_BUF` (8 KB since 2026-11-15, one per listener task; 6 KB before, raised from 4096
  on 2026-11-02) holds the whole page plus headers, and
  overflow is a 500, not a truncation. The headers are 126 bytes
  for a page (`HEADERS_MAX`; 103 before `X-Frame-Options`, and never
  the 88 this said);
  `core::http::PAGE_MAX` (8032) is what a page may take, and the
  `form` tests build each page at its worst case and assert it
  (2026-11-14, since the split in four, with the three clips of `clips/`: the clock page 4353 with the status lines, a stored SSID of 32 `"` and a 64-byte broker host in them; settings 2433 with a 48-byte timezone rule of `<`, four bytes each through `HtmlStr`; Home Assistant 2253 with a user of 32 `"`; system 3876; setup with twelve 32-character networks about 3200. The one page before it was 5564, and 5865 with nine clips. Since 2026-11-15, with the PIN asked for at the click, the notices, the day checkboxes and the forms' own post: clock 5989, and 7205 with a full pack of sixteen clips at their longest, which leaves 827 bytes; settings 3138 (3812 with the transmit power), Home Assistant 3110, clips 3686, system 4828, setup without a list 3084). `cargo host-test` after adding a field
  or label; the test prints the sizes with `-- --nocapture`. The setup
  page's network list stops at `form::NETWORK_LIST_BUDGET` (1900)
  bytes, strongest first, with a "N more not shown" line, because a
  listed SSID of 32 `"` is 417 bytes of `<option>` through `HtmlStr`
  (twice per network) and five of them would pass the buffer; the rest of the page at its worst is 2324, and `provision_page_budget_is_covered`
  locks the sum under `PAGE_MAX`; the budget was set when the page
  had 3968 and stays, since twelve networks is plenty.
- `http::RX_BUF` (4096) is the TCP receive window; 1 KB made an upload
  one segment per round trip. `REQUEST_BUF` (1536) still bounds the
  head; an upload's body stays on the socket.
- An OTA image must fit a slot (`partitions.csv`, 0x160000 = 1408 KB
  since the `clips` partition took half a megabyte of each, 2026-11-14;
  1920 KB on a clock with the old table); the release image is 877 KB,
  and 1045 KB with the three clips of `clips/` built in. A build with
  Futurama in `ALARM_CLIP` as well no longer fits. A clip pack must
  fit its partition, 0x120000 = 1152 KB to the end of the 4 MB flash (`scripts/mkpack.py` refuses a larger one), and its
  SHA-256 is checked in software at every boot before the settings
  load: 119 ms for the default pack's 172 KB, 482 ms for 700 KB
  (test-clock, 2026-11-14). Static RAM: `ota.rs` holds 11 KB
  of buffers, and what the statics leave of DRAM is the main stack,
  66.9 KB on 2026-11-08 (67.1 on 2026-11-07; the ring's descriptors and
  conf words are 160 bytes, the gap statistics and buffer channels went) (`Settings` is a 386-byte record and sits in several task futures: the 49-byte timezone rule cost 2.2 KB of stack); check `.stack` in `llvm-readelf -S` after adding one, and compare with a build of `HEAD` in a worktree when in doubt: one `Response` per listener loop, since a second one in another arm of the same loop was not overlapped by the compiler and cost 6 KB per listener until it was hoisted (2026-11-07). `src/tz.js` (5.8 KB) is sent from one response buffer and a `const` assert in `src/http.rs` keeps it under `RESPONSE_BUF` with its header; it was trimmed to comments of a line each to fit.
- A PCM clip is 16 KB of flash a second at 16 kHz, 8 KB at 8 kHz (the
  27.6 s test clip is 442 KB of `.rodata`, held once: `melody::CLIP_DATA` is a `static`; a beeper clip is 1 to 3 KB a second, a tracker module some 10 KB whatever its length)
  and must leave the pack inside its partition, or the image inside its slot; the ring is 16 KB of
  static RAM in every build, since any build may meet a pack (2026-11-14: the main
  stack is 49.9 KB, and 43.7 KB since `RESPONSE_BUF` is 8 KB, 2026-11-15). A chunk is 16 ms at
  512 kbit/s (a beeper clip) and the ring holds fourteen ahead of the
  DMA, so the clip task not running for about 220 ms (twice that for
  the 16-bit samples of a 16 kHz clip under the PDM converter) is an
  underrun, logged when the clip ends with the count of DMA positions
  the pacer did not believe; it is on the interrupt
  executor for that reason, and a chunk's modulating, a millisecond
  or two, runs there above the Wi-Fi driver.
- `form::apply` decodes each key into 16 bytes and each value into 64
  (`form::VALUE_MAX`, the password); longer input is reported as a bad
  key or value before parsing. The record is `settings::RECORD_LEN` (386) bytes and the map scratch in `storage.rs` 512, which must hold
  the item (record plus 9 bytes, padded to a word).
- The MQTT task's future holds its socket buffers (1 KB receive, 2 KB
  send), the 1 KB `BumpBuffer`, the state JSON (1 KB) and the
  discovery payload (`mqtt::DISCOVERY_MAX`, 4608; the worst case is
  4174, printed by the `mqtt` test), about 15 KB of static RAM in all
  (2026-11-07: the main stack went from 84.5 KB to 69.6 KB).
- `settings::RECEIVERS` is the number of `Watch` receivers (display,
  persist, hostname, MQTT). Add one when a new task subscribes, or `receiver()`
  panics at boot.
- `StackResources<11>` in `main.rs` covers DHCP, DNS, SNTP, the gateway
  probe, the MQTT connection and the HTTP listeners with headroom (on the access point: the
  DHCP and DNS servers and the listeners); adding sockets means raising it.
- `HOSTNAME_MAX` (24) stays under embassy-net's 32-byte DHCP option.
- `ESP_RADIO_CONFIG_WIFI_MTU` (540, in `.cargo/config.toml`'s `[env]`,
  read back as `config::WIFI_MTU`) bounds every frame the clock sends:
  TCP segments carry 486 bytes, the probe 498, and a page is seven
  segments instead of two. It is a transmit-side limit only (smoltcp
  applies it when emitting; the driver passes received frames whole),
  chosen because the access point hears the clock 20 dB below what the
  clock hears of it and short frames pass where full ones fail
  (2026-11-01, the adapter-built clocks at −52 to −65 dBm). Two
  consequences: a TCP peer is told the 486-byte segment size and sends
  an upload in 486-byte pieces, and a ping larger than 498 bytes from
  the LAN gets no reply, since the reply would not fit;
  `scripts/watch-clock.sh` pings with 498. Raising it means the probe
  grows with it; nothing else depends on the number.
- `config::WIFI_RX_QUEUE_FRAMES` (32) is the driver's receive queue;
  esp-radio's default of 5 dropped bursts of LAN broadcasts, visible as
  `RX QUEUE FULL` with `esp_radio=debug`. The stack answers ping only
  because embassy-net's `auto-icmp-echo-reply` feature is on.

## Verifying without a board

`cargo host-test` runs the tests of `core/` on the host (158 on 2026-11-15, and the players' tests over real clips only with `ALARM_CLIP` naming some, which the CI does in a second run): the AiP33628 encoder against a behavioural model of the
chip (shift on CLK rise, latch on DIN rise while CLK high), the panel
map, the palette and brightness tables, the hostname, password and
token rules, the record round trip and every older version, the form
parser and the page sizes against `PAGE_MAX`, the head parser, the
appended-hash split, the carry codec, the clip pack's layout with every refusal and the melody table with and without a pack, the calendar arithmetic, the POSIX TZ parser with its rejects and the 2026 transitions of both hemispheres, the melodies, the clip's modulator (a sine back out of the bits, one centred pulse a period, the ramps, full-scale stability), the beeper clip's runs bit for bit, the module player (pitch, volumes, each effect's periods tick by tick, the song's order, the envelope, the shaping; `CLIP_PCM_DUMP=file cargo host-test dump_the_tune -- --ignored` writes a built-in module's, SID tune's or PT3 module's samples for comparison with another player's render, `CLIP_INDEX` saying which clip), the SID (pitch, each waveform's shape, ring modulation, sync, the envelope's times and its late note, the filter's modes and its stability at every setting, the frames' timing and the loop; a tune was compared with libsidplayfp's render once, by hand, see PLAN.md), the AY (`run` against a tick-by-tick model of the chip over random registers, pitch, the mixer, noise, each envelope shape, the levels), the PT3 player on modules built in the test (notes by table and version, rows and tempo, sample lines and their sums, ornaments, slides and portamento, the envelope, the positions, the loop and the fresh start, cut and random modules; `CLIP_REG_DUMP=file CLIP_INDEX=n cargo host-test dump_the_registers -- --ignored` writes a built-in module's registers, fourteen a frame, for a reference player), and the ring's pacer, run against a model of the DMA (`CLIP_KBPS` and `CLIP_CARRIER_KHZ` change what is tested; `ALARM_CLIP=… cargo host-test` covers the table with the clip in it), the UI's alarm, grace, skip and daylight-saving logic, the PIN and token
header rules, the NTC and LDR tables against their equations, the thermometer's hold, and the
MQTT topics, routes and discovery payload (parsed with `serde_json`, a
host-only dev-dependency, and checked key by key against the `/api`
JSON and the form parser). `cargo clippy -p esp32clock-core --target host-tuple
--tests` lints the tests. `node scripts/tz-check.mjs` checks the browser half of the timezone field, `src/tz.js`, against zones with known rules under Node, whose `Intl` is a browser's, and against Casablanca, which must throw. `.cargo/config.toml` aims every cargo command at the chip, which is why the alias passes `--target host-tuple`; the `[unstable] build-std` line there is ignored by
stable cargo. Not under test: `scan.rs`'s
`build_cycle` (the M1 lit-time check needs it on the host), `ring.rs`
(descriptors and registers; its layout is `const`-asserted), `src/clip.rs` (the I2S transfer and the pin), `dhcpd.rs`
and `dnsd.rs` (their pure halves
use embassy-net's address types), `storage.rs`'s map over the flash,
`mqtt.rs`'s session loop.
`PLAN.md` records the checks of each milestone; new ones are tests.

## Conventions

- `no_std`, no `alloc` use outside what esp-radio needs; the heap exists
  for the Wi-Fi driver.
- The target is riscv32imc: atomics have load and store only, no
  `fetch_add` or `swap`. Counters shared with the heartbeat have one
  writer and are bumped with a load and a store; anything with more than
  one writer goes behind `critical_section`.
- Keep hardware out of `core/`: it has no esp dependency and must
  build for the host. New pure logic goes there with its tests; a
  module that needs a peripheral and has a pure half is split the way
  `clock`, `carry`, `http` and `ota` are.
- Settings change only through `settings::update`; it is what wakes the
  display and the persist task. The HTTP handlers apply a request to a
  copy and commit it with one `update` only if everything parsed; keep
  parsing out of the watch's closure.
- Adding a setting touches, in order: the `Settings` field and `DEFAULT`,
  `is_valid`, `RECORD_VERSION` with the new field appended (never
  reordered: an older image reads a newer record by its prefix, so a
  rollback keeps the settings) and `from_record` still reading every
  older version, `form::apply_one`, `write_json` and the page it belongs on (`write_page`, `write_settings_page` or `write_ha_page`; a checkbox through `form::Check`),
  the `PLAN.md` entry, and the `settings` and `form` tests (the record
  round trip, `apply_sets_every_key`, `each_page_has_its_own_fields`, the page sizes). A setting Home
  Assistant should see gets a component in `mqtt::write_discovery`
  and, if it may be set from there, its key in `mqtt::SET_KEYS`; the
  `mqtt` test ties the three together. A key only the token may set
  goes behind `form::Allow`. A field that
  must not persist, like `skip_next`, is left out of the record on
  purpose. Read-only status goes in `form::Status`, built in
  `http::status`, so `form.rs` stays clockless.
- Tables copied from the reference docs (`BLOCKS`, `SEGMAP`, `COM_SEQ`,
  `IS_WIRE`) are verbatim; cite `3rdparty/HU-058_ESPHome/docs/` when
  changing anything near them.
