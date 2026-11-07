# Changelog

Releases are annotated `v*` tags; each one has its images on the
[GitHub release](https://github.com/anarsoul/esp32clock/releases) and
in the [web installer](https://anarsoul.github.io/esp32clock/).
A running clock takes the `esp32clock-ota-<tag>.bin` of a release
from `http://<ip>/update`; its settings survive the update.

## v0.2 — 2026-11-07

### Timezone as a POSIX TZ rule, with daylight saving

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
- The `tz` key of `/api` takes a rule or, as before, minutes east of
  UTC, which become a fixed rule; the JSON answers `tz` as a string
  now, and gains the read-only `utc_offset` (minutes east of UTC in
  force) and `dst` (whether daylight time is in force), both `null`
  before the first time sync.
- The alarm handles a transition: an alarm minute that the spring
  jump skips rings when the clock lands past it, and the hour that
  the autumn change repeats rings once.
- Build: `TZ_RULE` seeds the rule and a bad one fails the build;
  `TZ_OFFSET_MINUTES` still works as a fixed rule. The prebuilt images
  stay on Vancouver time without daylight saving, now written `<-07>7`.
- Updating from v0.1 keeps every setting: the stored offset becomes
  the fixed rule with the same minutes.

### Web installer

- The installer flashes four parts (bootloader, partition table,
  erased `otadata`, the application) and leaves the `nvs` partition
  alone, so a reflash keeps the saved settings unless the erase is
  chosen. The v0.1 installer wrote one merged image, which cleared
  them.
- The README points at the installer first and walks through an OTA
  update.

## v0.1 — 2026-11-07

The first release, for the HU-058 LED clock kit on an ESP32-C3:

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
