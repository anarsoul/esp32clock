# Behaviour reference

What the firmware does today, as seen from outside. Read the section
for the area you are about to change; `AGENTS.md` has the rules,
`HISTORY.md` the history. Keep this file current in the same change as
the behaviour.

## Building and flashing

```
cargo build --release
cargo clippy --release
cargo host-test                          # the core crate's tests on the host (alias in .cargo/config.toml)
cargo clippy -p onebitdawn-core --target host-tuple --tests
node scripts/tz-check.mjs                # src/tz.js against zones with known rules
WIFI_SSID='…' WIFI_PASSWORD='…' TZ_RULE='<-07>7' API_TOKEN='…' cargo run --release
TZ_OFFSET_MINUTES=-420 cargo build --release    # the older variable: minutes east of UTC, stored as the fixed rule <-07>7
MAX_CURRENT_STEP=8 cargo build --release        # raise the drive-current ceiling (default 5)
WIFI_TX_POWER_QDBM=44 cargo build --release     # the transmit power setting's default, quarter dBm, one of its levels (default 68)
MQTT_HOST=10.1.1.5 MQTT_USER=clock MQTT_PASSWORD='…' cargo build --release   # the broker (MQTT_PORT defaults to 1883)
AP_PASSWORD='…' cargo build --release           # the access point's password (default onebitdawn)
API_TOKEN='…' scripts/ota.sh <ip>               # later updates, over the LAN
scripts/ota.sh                                  # just the image file, for http://<ip>/system
API_TOKEN='…' scripts/reload.sh <ip>            # software reset with the carry record, waits for the clock to come back
scripts/pin.sh <ip> <serial-log>                # POST /pin, then the four digits read back from the monitor's log
scripts/verify-csrf.sh <ip>                     # a foreign Origin or Host is refused, the clock's own pages and a script are not
```

- `cargo run` flashes with espflash and opens the monitor;
  `.cargo/config.toml` sets the runner and `ESP_LOG=info`. The runner
  passes `partitions.csv` (two app slots, `ota_0` and `ota_1`, plus
  `otadata`) and erases `otadata`, so a USB flash always boots
  `ota_0`, where espflash put the image.
- `scripts/ota.sh` builds, makes the application image with `espflash
  save-image` and posts it to `/ota` with the token; the clock writes
  the other slot, selects it and reboots.
- The build-time variables are read by `core/src/config.rs`;
  `core/build.rs` reruns that crate when they change. The root
  `build.rs` exports `git describe --always --dirty` as
  `FIRMWARE_VERSION`, which goes into the app descriptor, the boot
  line, the page and `/api`.

### Seeds

The network, the token and the broker from the build are seeds
(`settings::Settings::seed`): whatever the record lacks is filled from
the build at boot, the next save carries it, and from then on the
record wins over a later build's constants.

- `WIFI_SSID` may be empty.
- The token is `API_TOKEN` at build time and a setting after that
  (`auth`). A build without one has no script path (a `Bearer` request
  is `403`) until one is set from `/system` with the PIN or by `POST
  /token`. The boot line says `api token: stored`, `built in`,
  `cleared` or `none`.
- A token cleared with `POST /token` stays cleared: the record's flags
  byte (`token_cleared`, version 7) tells a cleared token from one
  never set, and only the latter is filled from the build.
- The broker (`MQTT_HOST`, `MQTT_PORT`, `MQTT_USER`, `MQTT_PASSWORD`)
  the same way, with bit 1 of the flags (`mqtt_cleared`) for a host
  cleared on the page.

### Logs

- Every log line carries the uptime in ms.
- For a network debugging run set
  `ESP_LOG='info,embassy_net=debug,esp_radio=debug'` on the command
  line (it overrides the config, and esp-println compiles the filter
  in); the extra lines are DHCP state and driver events, not packets.
- `scripts/watch-clock.sh <ip>` sends a 498-byte ping (the largest the
  clock answers, see the MTU in `agents/sizes.md`) and fetches `/api`
  every 30 s, for the host's side of the story.
- The minute heartbeat logs the filtered millivolts of both sensor
  dividers, for calibration.

## Settings

Saved in the `nvs` partition; editable from the buttons, the pages and
`/api`.

| setting | notes |
| --- | --- |
| timezone (`tz`) | a POSIX TZ rule such as `PST8PDT,M3.2.0,M11.1.0`, parsed by `core/src/tz.rs` and evaluated on the chip from the NTP time, so daylight saving needs nothing from the network. The key also takes minutes east of UTC, stored as the fixed rule `<-07>7`. Default: `TZ_RULE` from the build, checked by a `const` assert so a bad rule fails the build, else `TZ_OFFSET_MINUTES` as a fixed rule (the CI's `<-07>7` is Vancouver without daylight saving). The page's `Use this browser's zone` button fills it in through `/tz.js` (`src/tz.js`). |
| 12/24 hour, colour, brightness | |
| alarm, alarm days, alarm melody | the melody is an index into the table, see "Clip packs" |
| `temp_offset` | tenths of a degree Celsius added to the NTC's reading, ±10.0, for the warm air at the sensor's spot in the case. Default −2.5 (`config::DEFAULT_TEMP_OFFSET_DC`, what the user's clocks typically need) for a clock with no record or one older than version 8; a stored value, 0 included, stays. |
| `temp_show` | `off`, `c` or `f`, default `c`. With a reading in hand the UI shows ` 23C` with the degree mark for seconds 30 to 34 of every minute in clock mode (`ui::TEMP_SHOW_AT`, `TEMP_SHOW_SECS`). |
| hostname | the DHCP hostname |
| `tx_power` | dBm: `2`, `5`, `7`, `8.5`, `11`, `13`, `14`, `15` or `16`; see "Wi-Fi power" in `agents/hardware.md` |
| Wi-Fi network, API token | `ssid`, `password`; the token through `POST /token` |
| `mqtt_host`, `mqtt_port`, `mqtt_user`, `mqtt_password` | see "Home Assistant" |
| `skip_next` (page checkbox, key `skip`) | the one setting the record does not store: it skips one alarm and is cleared by it or by a power cut. A software reset (self-heal, panic) carries it over in RTC RAM, `src/carry.rs`. |

## The pages

Five (`form::Page`), each starting with a line of links to the others:

| page | holds |
| --- | --- |
| `/` | the alarm with Play and Stop, the display, the status lines |
| `/settings` | timezone, 12 hour, the temperature on the panel and its offset, hostname, the transmit power with its warning, the local-time line |
| `/ha` | the MQTT broker and its status line |
| `/clips` | where the clips come from, their labels, the pack upload |
| `/system` | firmware upload, API token, Reboot, Access point |

Common to all:

- They follow the browser's light or dark (`color-scheme`); a link is
  one colour whether visited or not (`a{color:LinkText}`). The heading
  is `onebitdawn: <hostname>`, the title `<hostname>: <page> -
  onebitdawn`.
- They say nothing of `/api`: that is `MANUAL.md`'s (the user,
  2026-11-15: "it should be in the documentation, not in the
  interface").
- Each form is posted to its own page and is a partial update, as
  `/api` is: only the keys sent change. A checkbox is sent either way,
  a hidden `0` under the same name before it (`form::Check`), so
  saving the alarm leaves 12 hour alone.
- A saved form comes back to its page with `?saved=1` and the page
  says `Saved.`; a refused one with `?error=value&key=tz`, and the
  page names the field from `form::FIELDS` (the key's label, never the
  query's text). The script takes the query off the address.
- With script a form is posted with `fetch`, and only the fields
  that differ from what the page was served with: a name goes with
  every pair the form has under it (a checkbox with its hidden `0`,
  the days all that are ticked), so a tab opened a while ago puts
  back nothing it did not change. A refusal's notice is
  taken out of the answer and put above the form, which stays as
  typed, and a saved form goes on to the page the redirect named.
  Without script the form is posted and reloaded.

The clock page, `/`:

- The alarm and the display are one form with Save under both, and
  Save posts both: colour and brightness change on the panel when the
  form is saved, not as they are chosen.
- The script makes Play and Stop plain buttons: as the form's first
  submit buttons, Enter in the alarm time would play the tune.
- Play or Stop that fails says so above Save.
- The alarm's days are seven checkboxes all named `days`.
  `form::apply` reads a `days` key repeated in one body as a union:
  the first replaces, the rest add; none ticked sends no key and
  changes nothing. `/api` takes `days=mon,wed` and `all`, `weekdays`,
  `weekend`.
- Above the boxes is a select of the presets (Every day, Weekdays,
  Weekend, Custom; the user's, 2026-11-15). It has no name and is not
  posted; the script shows it, ticks the boxes from it and sets it
  from them.
- Under the skip box the page says `Next alarm: Mon 07:00.`, or `Tue
  07:00 (Mon skipped)`, from `form::next_alarm` and the status's local
  time; nothing while disarmed or unsynced.
- `POST /play` previews the melody chosen in the form without saving
  (it applies the form with `Allow::ALL`, since nothing is saved); the
  page posts it by script so the form is not reloaded. `POST /stop`,
  the Stop button, the same way: no token, its body ignored, it
  silences a preview and ends a ringing alarm as a press would.
- The status lines, under the firmware line: the network and signal
  (`Wi-Fi home, -62 dBm.`, or `not connected`, or `no network
  stored`), the broker (`MQTT connected 10.1.1.5:1883.`, `MQTT
  connecting …, last: not authorized.` or `MQTT off.`), the room
  (`Room 23.4 °C (74.1 °F), light 62 %`) and the local time the rule
  gives (`Local time Sat 15:20 PST, UTC-08:00.`, with `, daylight
  time` appended when it is in force, for checking a rule).

`/ha`'s script asks `/api` every 5 s for two minutes and loads the
page again when `mqtt` is no longer what it was served with, unless
the form has unsaved changes.

## `/api`

Form-encoded keys in, JSON back; a partial update.

Guarded keys (`form::Allow`: `PAGE`, `ALL`, `NONE`):

- `ssid` and `password` are taken only with the `Bearer` token,
  refused as `403` on the key alone (never on whether the value would
  change, which would make a password oracle), and never on the
  settings form. On the access point `/api` takes them without the
  token.
- The `mqtt_*` keys need the token on `/api` (a broker of the
  attacker's choosing would be handed the stored password). The
  settings form takes them under one rule, checked in `form::apply`
  after the pairs: a form that moves the broker to another host
  without typing the password again is refused
  (`FormError::BrokerPassword`, with its own notice), so the stored
  password goes only to the host it was typed for. The same host keeps
  it on an empty field, an empty host clears it, and the token is not
  held to the rule.
- The password and the token are never read back: the JSON has `ssid`,
  `password_set`, `token_set` and `mqtt_password_set`.

A saved change to the network resets the clock into it
(`heal::switch_mode`, with the carry record started over), so a wrong
one costs the LAN until the password is right again.

Read-only keys; posting one is refused as an unknown key:

| key | value |
| --- | --- |
| `version` | the firmware version |
| `slot` | the app slot the firmware runs from |
| `mode` | `sta` or `ap` |
| `uptime` | seconds |
| `heals`, `last_heal` | self-heal resets since power-up; the last one's Unix seconds or `null` |
| `temp_c`, `temp_f` | the room in tenths; `null` with no thermometer fitted or before the first reading |
| `light` | 0 dark to 100; `null` before the first reading |
| `rssi` | the station's signal in dBm from its last beacon, read every 10 s by `net::wifi_task` into an atomic; `null` while not connected and on the access point |
| `utc_offset`, `dst` | minutes east of UTC and whether daylight time is in force right now under the rule; `null` before the first time sync |
| `mqtt` | `off`, `connecting` or `connected` |
| `ringing` | whether the alarm rings right now |
| `clips` | `pack` or `builtin` |

## Clip packs

- `GET /clips` says where the clips come from: the pack in the `clips`
  partition with its size, the firmware, or that this clock's table
  has no such partition, in which case the page has no upload and asks
  for no PIN. It lists the labels.
- `POST /clips` uploads a pack as the raw body, with `Bearer` or
  `PIN`, as `/ota`.
- A bad header (`not a clip pack`, a length that is not the body's) is
  refused before anything is erased and the pack in use stays. A hash
  mismatch after the write leaves the clock without a pack until the
  next upload. A good one restarts the clock (`heal::reboot`).
- The melody setting is an index: after another pack it names whatever
  is at that place, and a place past the table's end is read as
  `beeps` (`Settings::from_record`), which is what a clock updated
  over the air onto the old table gets for a clip.

## The system page and the PIN

`/system` has four actions: the firmware upload, a new API token,
Reboot and Access point. `GET /update` and `GET /token` redirect to
it.

- Pressing an action's button makes the page's script `POST /pin`.
  The clock draws four random digits, shows them on the panel for a
  minute and logs them, and the field for them appears under that
  button. The `GET` issues none, since any site can make a browser
  send one. The uploader must be able to see the clock or the serial
  port.
- On the panel any press ends the PIN; ringing and alarm-set take
  precedence over it.
- A PIN is good for five minutes and one attempt, and one PIN is one
  action. The attempt, right or wrong, takes the digits off the panel
  (`auth::pin_to_show` yields `None`, `ui::hide_pin`). A wrong one
  locks new PINs out for ten seconds.
- After an answer the buttons work again and the next press draws a
  new PIN. The token's rule and a missing file are caught before one
  is drawn.
- `POST /pin` without the page, for a bench script (`scripts/pin.sh`):
  the PIN is on the panel and in the serial log, never in the
  response.

The routes behind the actions, each with `Bearer <token>` or `PIN
<digits>` (`auth::token_or_pin`):

- `POST /ota`: see "Firmware updates".
- `POST /token` with `token=<new>` (8 to 32 printable characters
  without spaces, empty to clear). The page says whether a token is
  set, and an empty one asks the browser to confirm before it clears.
  The body is parsed before the PIN is spent, so a bad token costs a
  `400` and not the PIN.
- `POST /reload` answers once and then resets the chip the deliberate
  way: panel blanked, time and alarm state carried, not counted as a
  self-heal and not held for a ringing alarm. It is the way to
  exercise the carry record from a script: `curl -X POST -H
  "Authorization: Bearer $API_TOKEN" http://<ip>/reload`.
- `POST /ap` resets a running clock into the access point, for a bench
  without hands on the buttons.

## Provisioning: the access point

A clock with no network in its record (and none built in), or one
whose buttons were held 10 s, boots as an access point instead of a
station for that whole boot, since the driver does not do both at
once.

- The panel shows `AP`. The network is `clock-xxxx` (the last two
  bytes of the MAC), WPA2 with `AP_PASSWORD` from the build (default
  `onebitdawn`), channel 1. The clock is at `http://192.168.4.1/` with
  its own DHCP server (`src/dhcpd.rs`, eight leases, replies
  broadcast). There is no time, so no alarm.
- A captive DNS (`src/dnsd.rs`) answers every name with the clock's
  address, and any path that is not a route is a `302` to
  `http://192.168.4.1/`, so a phone's connectivity probe opens the
  setup form in its sign-in sheet by itself.
- `GET /` is the setup form: network, password, timezone, hostname,
  the station's transmit power.
- `/system`, `/ota`, `/api` and `/token` work there too. `/settings`
  and `/ha` do not: the captive redirect takes them, and the system
  page there links to the setup form and leaves out its access-point
  button.
- A station that cannot join never turns into an access point by
  itself: a wrong password is a blank panel, `wrong password?` in the
  log every attempt, and the hold.

The scan:

- The access point runs as AP+STA with a station half that never
  connects, because the driver refuses a scan in AP-only mode. It
  stays up through a scan.
- It scans once when it comes up and again on `POST /scan`, the page's
  Rescan button, which comes back to `/` with the list. `ap_task`
  counts the scans it has done and each waiter polls the count, so two
  clicks from two tabs are each answered rather than one waking the
  other's `Signal`.
- The form lists what the scan saw, strongest first, with an "Other"
  entry and a text field for a hidden network.

The form's rules:

- A `POST` of `ssid=` (empty) leaves the network unchanged, since the
  form always sends both the list's pick and the typed name. A
  non-empty `ssid_pick` wins over `ssid` whatever the order
  (`form::apply`); the page also disables the typed field while a
  listed network is picked, since a hidden input is still posted.
- The password field is write-only and always posted
  (`form::settle_password`): an empty password with the stored network
  keeps the stored password unless the scan lists that network as
  open, and an empty password for another network the scan lists as
  secured is refused with the notice. `/api` takes what it is given.
- A form that changes nothing resets at once (`Then::Join`). One that
  changes anything is committed, the persist task saves it and then
  resets. Every save on the access point resets into the station once
  a network is stored, whatever changed, so a timezone or hostname
  posted with the form reaches flash before the clock leaves.

Both switches between station and access point (`heal::switch_mode`)
start the carry record over, keeping only the OTA boot count: no time,
heals or alarm state crosses a mode change. A panic in AP mode keeps
the flag, so the clock comes back as an access point.

## Presses, self-heal and the carried time

- Any press ends ringing or the IP display, and presses in the second
  after it are ignored (`ui::PRESS_QUIET`), so a two-handed slap does
  not step a setting with its second event.
- Set mode times out after 15 s, ringing after 5 minutes.

Self-heal, each ending in a reset with the panel blanked first:

- Once the gateway has answered a ping, ten failed probes in a row
  (five minutes) with the link still up
  (`config::PROBE_RESET_FAILURES`). The probe is a ping of the largest
  frame the clock sends (`config::PROBE_PAYLOAD_BYTES`, 498 bytes at
  the 540-byte MTU), because a marginal uplink passes short frames
  while full ones fail.
- Ten failed connects in a row or three minutes of them
  (`config::CONNECT_RESET_ATTEMPTS`, `CONNECT_RESET_SECS`), because
  the driver avoids the access point that last dropped it and only a
  reset clears that.
- The boot line and the heartbeat carry the count since power-up.
- A self-heal due while the alarm rings waits for the ring to end, by
  a press or its timeout.

The time across a software reset:

- The display tick writes the Unix time to RTC RAM every second; a
  deliberate reset writes it once more and pauses
  `clock::RESET_PAUSE`, and the boot adds that pause back. The panel
  shows the time again before Wi-Fi is up, within a second.
- The RTC timer is not used: esp-hal recalibrates it at every boot and
  an offset from it drifted with the uptime since power-up.

The alarm across a reset:

- An alarm missed by up to 10 minutes across a power cut rings on the
  first valid time after boot.
- After a software reset the alarm last rung-and-ended or skipped
  within that grace is not rung again, while one cut off mid-ring
  comes back.

## Home Assistant

The clock announces itself over MQTT through MQTT discovery: one
device named after the hostname, the settings as entities and the
readings as sensors; nothing is typed into Home Assistant. The pure
half is `core/src/mqtt.rs`, the task `src/mqtt.rs`, spawned on the
station only.

- The broker is the `mqtt_*` settings. An empty host is MQTT off, and
  clearing it on the page clears the user and password with it.
- The client is `rust-mqtt` 0.6, MQTT 5 only, so the broker must speak
  MQTT 5 (Mosquitto 1.6 and later, including Home Assistant's add-on,
  EMQX, NanoMQ). No TLS, QoS 0, clean session, 60 s keepalive.
- The id is `clock-` and the last three bytes of the MAC in hex
  (`mqtt::DeviceId`), stable across hostname changes.

Topics under `onebitdawn/<id>/`:

| topic | payload |
| --- | --- |
| `status` | `online` or `offline`, retained; `offline` is also the will |
| `state` | the `/api` JSON byte for byte, retained: after connect, on every settings change debounced 300 ms, on the ringing edges and every 60 s |
| `set/<key>` | the value as `/api` takes it; one subscription, `set/+` |
| `cmd` | `play`, `silence`, `reload`; its own subscription |

- `set/` takes the keys of `mqtt::SET_KEYS`: colour, brightness,
  alarm, armed, days, skip, melody, twelve_hour, temp_show,
  temp_offset. Anything else (`hostname`, `tz`, the network, the
  token, the broker) is logged and dropped, so the bus is no way to
  move the clock elsewhere.
- The alarm parser takes `HH:MM:SS`, as Home Assistant's `time` entity
  sends it.

Discovery:

- The payload goes to `homeassistant/device/<id>/config`, not
  retained: at every connect, on a hostname change, and when Home
  Assistant publishes `online` on `homeassistant/status` (after a
  random delay under 2 s, as its docs ask).
- Every entity reads a key of the state JSON through `val_tpl`. The
  host test parses the payload and checks each key against the JSON,
  each `set/` key against the allow list and each select option
  against the parser.
- Nineteen entities: selects for colour, brightness, melody and the
  panel temperature; a `time` for the alarm; switches for armed, skip
  and 12 hour; a text for the days; a number for the thermometer
  offset; sensors for temperature, light, signal, uptime and
  self-heals; a binary sensor for ringing; buttons for play, silence
  and reload.

The session:

- The task resolves the host (an IPv4 literal or DNS) and connects,
  with a 5 s timeout each. On any failure it aborts, logs the reason
  once and backs off from 5 s doubling to 60 s.
- A change to the broker settings disconnects cleanly and starts over;
  a cleared host publishes `offline` and waits. A broker or hostname
  change that lands during the start is compared after it, not dropped
  with its notification.
- `poll_header` is the one cancel-safe client future the loop selects
  on; a header is followed by `poll_body` to completion, and publishes
  happen between polls.
- The three subscriptions are confirmed by SUBACKs the loop matches by
  packet identifier (`subscribe` only sends): a reason code of 0x80 or
  more, or no answer within 10 s, fails the session with the reason on
  the page.
- Received packets land in a 1 KB `BumpBuffer`, reset after each event
  (the one `unsafe` in the module).
- Before a deliberate reset (`heal::reboot`, `heal::reset`,
  `heal::switch_mode`, the OTA reboot) `mqtt::goodbye` lets the task
  publish `offline` and send DISCONNECT, within 500 ms, so Home
  Assistant shows the clock unavailable at once rather than after the
  broker's 90 s keepalive timeout. A panic cannot, and the will covers
  it late. `cmd` `reload` says goodbye on its own connection and then
  reboots.
- The heartbeat carries an `mqtt:` line: state, connects, publishes,
  commands.

Verify with `mosquitto_sub -v -t 'onebitdawn/#' -t 'homeassistant/#'`
on the host.

## Cross-site requests

Every request is held to `core::http::admit` before a route is looked
at.

- `Host` must be absent (no browser), the clock's own IPv4 address,
  or, on the station, the hostname bare or with `.lan` or `.local`
  (`HOST_SUFFIXES`; a port and a trailing dot dropped, any case). On
  the access point the address only, since the captive DNS answers
  every name.
- Another `Host` is a `302` to `http://<ip>/` for a `GET` and `403`
  for a `POST`. That is DNS rebinding, a name made to resolve to the
  clock so a browser calls the attacker's page the clock's origin; it
  is also a LAN with another domain, which gets there by address.
- A `POST` whose `Origin` is present and not `http://` plus one of
  those names (`null` included) is `403`; absent is `curl` and the
  scripts.
- `Sec-Fetch-Site` is not used, since a browser sends it only to HTTPS
  and localhost.
- No `GET` may have a side effect (an `<img>` on any site sends one),
  which is why `GET /system` issues no PIN. A new route that changes
  anything is a `POST`, and is covered without doing anything.

## Firmware updates

`POST /ota` with the application image as the raw body (no multipart)
and an `Authorization` header: `Bearer <API_TOKEN>` from
`scripts/ota.sh`, or `PIN <digits>` from the system page, which posts
the chosen file the same way and polls `/api` until the clock is back.

In order:

1. The credentials are checked before the body is read. A refused
   upload reads and discards the rest of the body before answering, or
   the browser sees a reset instead of the `403`.
2. The first sector is checked for the image and app-descriptor magics
   and the hash-appended flag.
3. The scan is stopped; the panel is dark for the whole write, about
   11 s for 680 KB.
4. The other slot is erased, written and read back a sector at a time.
5. The SHA-256 espflash appends is checked against the streamed body.
6. `otadata` is switched and the clock reboots with the time carried
   over.

Any failure, a truncated or corrupt file included, brings the panel
back on the running image with `otadata` untouched.

Rollback: the bootloader only falls back on a bad checksum, so the
carry record counts the boots of the new image. A software reset
before a gateway probe has been answered or an HTTP request served
bumps the count, and the third such boot (`config::OTA_BOOT_LIMIT`)
switches back. A power cut clears the count.

## Releases

1. `CHANGELOG.md` gets the version's section first; the release body
   points at it.
2. An annotated tag `v*` (`git tag -a`; `build.rs` runs `git describe`
   without `--tags`, so a lightweight tag would not reach the version)
   runs `.github/workflows/release.yml`.
3. The workflow builds with the CI's prebuilt settings, tests, and
   attaches two files to the GitHub release:
   - `onebitdawn-ota-<tag>.bin`, the `/update` image;
   - `onebitdawn-<tag>.bin` (`save-image --merge`): the whole flash
     from 0, contiguous, so it covers `nvs` with erased bytes and
     clears the settings; `write-bin` then needs an `espflash reset`.
4. It deploys `docs/` to the `gh-pages` branch with the release under
   `<tag>/` and `latest/`, keeping earlier tags. The release's
   manifest for ESP Web Tools has five parts: the bootloader and the
   partition table cut from the merged image, an erased `otadata`, the
   OTA image at 0x20000 and the default clip pack at 0x2E0000. The
   pack replaces one the clock had; `nvs` is left alone, so the
   settings survive unless the erase is chosen.

`docs/index.html` is the web installer at
`https://anarsoul.github.io/onebitdawn/`: it lists the releases from
the GitHub API, newest first, and points the install button at the
chosen tag's manifest; without the API it uses `latest/`. Nothing in
`docs/` but the page is in the repository.
