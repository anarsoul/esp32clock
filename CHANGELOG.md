# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Releases are annotated `v*` tags; each one has its images on the
[GitHub release](https://github.com/anarsoul/esp32clock/releases) and
in the [web installer](https://anarsoul.github.io/esp32clock/).
A running clock takes the `esp32clock-ota-<tag>.bin` of a release
from `http://<ip>/update`; its settings survive the update.

## [Unreleased]

## [0.4] - 2026-11-10

The display scan runs in hardware. Nothing new is configured; updating
keeps every setting.

### Changed

- The panel is scanned by one SPI transfer the hardware repeats by
  itself, a DMA descriptor ring in the ESP32-C3's segmented-transfer
  mode, with no CPU in the loop. The dark gap between two laps is a
  fixed 0.6 µs. Before, the firmware restarted every transfer, which
  took 10 to 19 µs each time and up to 1 ms whenever Wi-Fi, the heap
  or the log held interrupts off. The minute heartbeat's `scan` field
  reports images published, stops and the descriptor the DMA is at,
  in place of the gap statistics.
- The thermometer offset defaults to −2.5 °C instead of 0, the typical
  difference between the sensor in the case and the room. It applies
  to a clock that has no settings yet (a fresh flash, or the web
  installer with the erase chosen) and to one updated from a firmware
  older than the offset setting; a clock that already has settings
  keeps its offset, 0 included.

### Fixed

- A bare `curl -X POST` to `/reload`, `/ap`, `/pin` or `/scan` works
  again. v0.3 answered `411 Length Required` to every `POST` without
  `Content-Length`, and these four read no body, so
  `scripts/reload.sh` and `scripts/pin.sh` were refused. Only the
  routes that read a body (`/`, `/api`, `/play`, `/token`, `/ota`)
  ask for the length now.

## [0.3] - 2026-11-07

Fixes from a review of the v0.2 code. Nothing new is configured;
updating keeps every setting.

### Changed

- A `POST` without `Content-Length` is answered `411 Length Required`,
  and one with any `Transfer-Encoding` is answered `400 Bad Request`,
  before a route is looked at. Before, a bodiless or chunked post to
  the settings page was taken as an empty form, which cleared the
  checkboxes (alarm armed, 12 hour, skip) and saved them.
- The page's buttons sit at the same distance under a field or a
  fieldset as any field does; the shared stylesheet had stacked a
  button's own top margin on the block above it.
- The settings record that cannot be decoded is logged with its
  version byte and length only. The whole record used to be printed,
  and it holds the Wi-Fi password, the API token and the MQTT password
  in the clear.
- The README says the project was written with the assistance of an AI
  agent, from the author's design, measurements and review.

### Fixed

- A timezone rule with a standard offset past 14 hours (`<+15>-15`)
  was accepted, saved, and thrown away at the next boot with the whole
  record: network, token, broker and alarm. The rule is kept now, and
  an image older than this one reads it as a fixed rule clamped to
  its own range.
- A timezone transition that falls in the neighbouring calendar year
  (a rule like `J1/-1`, whose first transition is 23:00 on December
  31) is now evaluated in the right year; the evaluator takes the
  transitions of the year before and after as well.
- The **Use this browser's zone** button no longer calls a zone fixed
  because its January and July offsets match, and no longer returns a
  rule that fails to predict the six following years. It finds every
  offset change of the year; a zone with other than none or two, or
  one no candidate rule predicts (Casablanca's Ramadan hour), shows
  `no rule for <zone>` instead of a confident wrong rule.
- A broker host or hostname changed while the MQTT session was
  starting up was dropped unread, leaving the clock connected to the
  old broker for good or Home Assistant with stale discovery. The
  change is compared after the start now.
- The MQTT session checks the broker's SUBACKs: a subscription refused
  (reason code 0x80 or more) or unanswered within 10 s fails the
  session with the reason on the page, where before the page said
  `connected` and the controls did nothing.
- A settings save that failed was not tried again, so the unsaved
  settings lived in RAM until something else changed; on the access
  point a setup form could answer "saved, joining" and the clock stay
  where it was. A failed save is retried after 2 s, doubling to 60 s.
- Two Rescan requests at once on the access point (a double click, or
  a phone's sign-in sheet and a browser tab) each took the other's
  wake-up, and one waited out the timeout. Every waiter is now
  released by the next scan to finish.
- A firmware upload failing during the pause of a deliberate reset
  (self-heal, reload, OTA reboot) restarted the display scan, so the
  reset landed with a transfer in flight and the last row stayed lit
  at full current until the firmware was back. The stop is latched
  against the upload's resume.

### Security

- The settings page refuses to move the MQTT broker to another host
  unless the password is typed again, with the notice "a new broker
  needs its password typed again". Before, an unauthenticated post to
  the page could move the clock to a broker of the poster's choosing,
  and the clock handed it the stored user and password. The same host
  keeps its password on an empty field, an empty host clears the
  credentials, and the token and the setup form are exempt.

## [0.2] - 2026-11-07

### Added

- The timezone setting is a POSIX TZ rule (`PST8PDT,M3.2.0,M11.1.0`,
  `CET-1CEST,M3.5.0,M10.5.0/3`, `<-07>7`), evaluated on the clock from
  the NTP time, so daylight saving needs nothing from the network.
- The settings page and the setup form have a **Use this browser's
  zone** button that fills the rule in from the browser's own time
  zone and names the zone it used. The rule is derived from next
  year's transitions and checked against the six years after, so
  zones that change at midnight (Chile) or on a negative hour
  (Greenland) come out right, and a zone that is dropping daylight
  time this year comes out fixed.
- The status block shows the local time the rule gives, with the
  zone's name and the offset in force, for checking a rule.
- `/api` gains the read-only `utc_offset` (minutes east of UTC in
  force) and `dst` (whether daylight time is in force), both `null`
  before the first time sync.
- Build: `TZ_RULE` seeds the rule and a bad one fails the build.
- The README points at the installer first and walks through an OTA
  update.

### Changed

- The `tz` key of `/api` takes a rule or, as before, minutes east of
  UTC, which become a fixed rule; the JSON answers `tz` as a string
  now. `TZ_OFFSET_MINUTES` still works at build time as a fixed rule.
  The prebuilt images stay on Vancouver time without daylight saving,
  now written `<-07>7`.
- Updating from v0.1 keeps every setting: the stored offset becomes
  the fixed rule with the same minutes.

### Fixed

- The alarm handles a transition: an alarm minute that the spring
  jump skips rings when the clock lands past it, and the hour that
  the autumn change repeats rings once.
- The installer flashes four parts (bootloader, partition table,
  erased `otadata`, the application) and leaves the `nvs` partition
  alone, so a reflash keeps the saved settings unless the erase is
  chosen. The v0.1 installer wrote one merged image, which cleared
  them.

## [0.1] - 2026-11-07

The first release, for the HU-058 LED clock kit on an ESP32-C3.

### Added

- Time over NTP, 12 or 24 hour, with a fixed UTC offset.
- Alarm with a choice of melodies and a random pick, per-weekday
  arming, a one-time skip, set from the buttons or the page.
- Nine colours and three brightness steps, with a build-time current
  ceiling for the supply.
- Room temperature and light level from the kit's NTC and LDR
  (optional), the temperature shown on the panel once a minute, with
  an offset setting for the case's warm air.
- Settings page at `http://<ip>/` and a form-encoded JSON API at
  `/api`; a DHCP hostname.
- Wi-Fi provisioning from the clock's own access point with a captive
  setup form, and a 10 s button hold to get back there.
- Home Assistant integration over MQTT with discovery: the settings as
  entities, the readings as sensors, buttons for play, silence and
  reload.
- Firmware updates over the LAN from `/update` with a PIN shown on the
  panel, or from a script with a token; two application slots with
  rollback of an image that does not come up.
- Self-healing network: the clock resets itself when the gateway
  stops answering or the join keeps failing, and carries the time and
  the alarm state across the reset.
- A web installer on GitHub Pages and a release workflow.

[Unreleased]: https://github.com/anarsoul/esp32clock/compare/v0.4...HEAD
[0.4]: https://github.com/anarsoul/esp32clock/compare/v0.3...v0.4
[0.3]: https://github.com/anarsoul/esp32clock/compare/v0.2...v0.3
[0.2]: https://github.com/anarsoul/esp32clock/compare/v0.1...v0.2
[0.1]: https://github.com/anarsoul/esp32clock/releases/tag/v0.1
