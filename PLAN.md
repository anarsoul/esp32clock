# onebitdawn — plan

What is not done: the list for the first public release, the work
planned and not built, the checks a built change still owes, and the
open findings of the reviews. A new piece of work is designed here
before it is started, with what was measured and learned added as it
goes; when it is built and has had its check, its section moves to the
end of `HISTORY.md` as it stands, and what it still owes stays here as
a line. `HISTORY.md` is the record of every milestone so far, the
original design included; `ARCHITECTURE.md` is the firmware as it is.

## To do before the first public release — a list, 2026-11-17

The user's, and what the readiness check of the same day left open.

- [x] Screenshots of the web interface (the user's): six in
  `screenshots/`, taken from white-clock with headless Chromium at
  420 px and twice the scale, the clock page light and dark and the
  others dark, the network's name and the broker blurred in the page
  before the capture (the user's choice; the version line reads
  `v0.4-67-gfb0e18f`). Three in the README under the feature list,
  all six in the manual beside what they show; the manual said four
  pages and has `/clips` in its list now.
- [x] A picture of the board with the buzzer mod, beside the README's
  description of it (the user's; a photograph, so the user's to take).
  Two, taken by the user 2026-11-17, in `pics/`: the component side
  (RBE beside Q1, R5, the BAT42 under the adapter) and the solder side
  (the flyback diode across the buzzer), under the mod's four changes
  in the README with a caption that says where each part is.
- [ ] `CHANGELOG.md`: `[Unreleased]` made the version's section, the
  compare links under it.
- [x] An install of an image with no token from the web installer or
  the CI's artifact: the access point's setup, then a token set with
  the PIN. Done on test-clock 2026-11-17 with a local build of the
  CI's configuration written by espflash ("An install from nothing",
  below); the web installer itself is the last item's.
- [ ] The default pack heard on a stock buzzer circuit (no diode, R5
  at 10 kΩ), which is what a builder has; the README's wording on the
  mod by what is heard.
- [x] The installer's list no longer offers v0.1 to v0.4 (the old
  partition table, the `esp32clock` token): `docs/index.html` leaves
  them out by tag (`OLD`); their files stay on `gh-pages` and their
  GitHub releases as they are. The page is deployed by the release
  workflow, so this is live with the next tag. Checked under Node
  against GitHub's list as it is today, which leaves nothing, the
  page's fallback to `latest/`.
- [ ] The annotated tag; then the release's three files and one
  install from the live page, since the workflow has not run with the
  pack and the new names.

## Planned, not built

### Dim in the dark (M11b, second half) — planned 2026-11-01, not built

The rest of M11 in `HISTORY.md`: the sensors are read and the
temperature face is on the panel, and the light index is on the page
and in `/api`. The paragraph below is the design as M11 had it; the
record has no `dim_in_dark` field yet.

**Light on the brightness.** One boolean setting, `dim_in_dark`,
labelled "Dim in the dark" on the page (key `dim`). When it is on
and the room is dark, the panel runs at the lowest brightness step;
otherwise it runs at the brightness the user chose. Dark is the
10 s light average below `DARK_BELOW` and it ends when the average
rises above `DARK_ABOVE`, two constants in `config.rs` a comfortable
gap apart (a room light switching on has to clear the gap, so a
candle or a passing car does not flip it), with the first numbers
guessed and tuned on the panel from the heartbeat millivolts. The
user's brightness step is untouched by all this: the bottom short
press still cycles it, the record stores it, and the display task
substitutes the lowest step at draw time while dark is set. A user
who has chosen the lowest step sees no difference. The current step
`IS` still comes from the step actually drawn, cap and all.

### A line output on the adapter for an amplifier — an idea of 2026-11-15, nothing built or measured; to be tried on the bench board when the amplifiers arrive

The user's, of the speaker mod: with a proper amplifier in place of
Q1 as the output stage, the adapter can carry the filter, and the
amplifier's input is soldered to a pad on the adapter. The amplifier
then has the converter's bits from the pin and no saturating
transistor between. Everything below is reasoned, not measured.

**The design, the user's of 2026-11-15: a pin of its own and a
setting that chooses the output.** "That would be cleaner: you
cannot mess up your board by using wrong firmware version." The
line output is a second GPIO, push-pull, and GPIO5 stays what it is,
open-drain to Q1. One firmware and one release image serve both
kinds of clock, and a wrong setting is silence and never a buzzer
held on: `line` on a stock clock drives a pin nothing is soldered
to, `buzzer` on a clock with an amplifier plays into Q1 as it always
did.

- The pin: GPIO20, the user's yes of 2026-11-15. The firmware uses 0, 1 and 3 to
  9. GPIO10, the first thought, has the module's own LED on it on
  both the SuperMini and the Zero (the user, 2026-11-15), which the
  converter's bits would light. GPIO2 is a strapping pin that must
  read high at reset, and the filter's capacitor and divider would
  hold it low. GPIO21 is UART0's TX and carries the ROM's boot text
  into the amplifier at every reset. GPIO18 and 19 are the USB.
  GPIO20 is UART0's RX, which nothing uses (the console is the USB
  one): an input with a pull-up while the ROM runs, so the filter
  sees one step to 3.3 V at a reset and no data. Where it comes
  out on each adapter's layout is for the board's drawing.
- The parts, from the pin to the pad: two RC stages, 1 kΩ / 10 nF and
  then 10 kΩ / 1 nF (the second at ten times the impedance, so it
  does not load the first; −3 dB near 10 kHz, 8.2 nF / 820 pF if that
  is dull); a resistor to ground as a divider, or a trimmer's
  footprint, since a full-scale clip is about 1 V rms off a 3.3 V pin
  and a module of the PAM8302's kind (24 dB) clips near 0.1 V rms
  (about 1.2 kΩ under the 11 kΩ for a tenth; the amplifier's input
  impedance is in parallel with it, so the level is to be checked on
  the module); a series capacitor, about 1 µF (many modules have
  their own).
- The setting: `output`, `buzzer` (the default, and what every older
  record reads as) or `line`; a new record version, on `/settings`
  and in `/api`, not in `mqtt::SET_KEYS`. Read at boot, and a save
  that changes it resets the clock as a change of network does: the
  buzzer task's LEDC and the clip task's pin handle are made once in
  `main.rs`, and swapping them under a playing tune is not worth the
  code.
- On `line` the clip's I2S signal goes to that pin through the GPIO
  matrix without the inverter, and the beeps' LEDC to the same pin,
  both push-pull; GPIO5 is left released for the whole boot. The pin
  rests driven low, so the rest is the ground and the rail's noise
  is not in it, and the clip's ramps start from and return to the
  bottom rail as they do from the released line now. Whether 30 ms
  of ramp is a thump through the capacitor is to be heard.
- The thermometer's hold (`sensors::Hold`) is for Q1's heat and does
  not apply on `line`.
- Still true: 3V3 is the converter's reference while a clip plays,
  so the Wi-Fi's bursts on that rail are in the audio; if they are
  heard, a single-gate buffer from an RC-filtered supply is the fix,
  a footprint at most. The amplifier's shutdown pin on one more GPIO
  is a thought for the idle current, not a plan.

**A first try that needs no firmware: a pull-up on GPIO5.** For the
bench, before the setting exists, and not for the board. A 470 Ω
pull-up from the GPIO5 net to 3V3 (never to 5 V), then 10 kΩ /
680 pF twice (−3 dB near 9 kHz), about 2.2 kΩ to ground for a tenth,
the capacitor. The line stays open-drain, the beeps work as they
are, the released line is the top rail, which the ramps already
start from, and the audio is inverted, which nobody hears. The
filter charges through the pull-up and R1 and discharges through R1
alone: a second harmonic of about (Rp/R1)·a/2, some 1 % at full
scale; 7 mA while the pin is low. A high line turns Q1 on through
R5, so the buzzer must be off the board, as the speaker mod has it;
Q1 may stay.

To try on the bench board, in this order: the pull-up and the filter
on a breadboard with the firmware as it is, the level at the
amplifier's input, the hiss with a silent clip and with Wi-Fi busy;
then the setting and the new pin, and the two side by side.

## An install from nothing on the bench, access point included — run 2026-11-17

The release list's install, and the access point's checks the session
could not make while the host's only Wi-Fi was its LAN: a USB dongle
(`wlp0s20f0u1`) joins the clock's network now, with a NetworkManager
profile that takes no default route and no DNS from it, so the host
stays on the LAN. No code changed. The image was `3eccec4` built as
the CI builds it (no network, no token, `TZ_RULE=<-07>7`), the merged
image, the OTA image and the default pack made with the release
workflow's commands; test-clock's flash was erased whole
(`espflash erase-flash`), the merged image written at 0 and the pack
at 0x2E0000. The pages were driven in headless Chromium, the PINs
read from the serial log.

What was seen, all as designed unless it says otherwise:

- **First boot.** `clips: pack of 4 clips`, the access point
  `clock-f044` on channel 1, the first scan in the log. The dongle
  joined in 2 s and had 192.168.4.2 from `dhcpd`, with the router and
  the DNS server 192.168.4.1.
- **Captive.** The DNS answers any name's A query with 192.168.4.1
  and an AAAA with no records; `/generate_204`,
  `/hotspot-detect.html`, `/settings`, `/ha` and an unknown path are
  a 302 to `http://192.168.4.1/`, a foreign `Host` too; `/`,
  `/system`, `/clips`, `/tz.js` and the favicon answer.
  `scripts/verify-csrf.sh 192.168.4.1` passed whole.
- **The setup form.** A listed network hides and disables the typed
  name; Show the password, the browser-zone button (`PDT7`,
  America/Vancouver) and Rescan work, Rescan keeping the pick and the
  typed password; two `POST /scan` at once were each answered after
  the one scan (2.4 s). A typed password under eight characters is
  stopped by the browser. Refused by the clock with nothing saved: a
  secured network with no password, a short password, a bad hostname,
  a bad timezone, each with its notice naming the field.
- **A wrong password.** Saved for `esptest`: the clock reset into the
  station, logged `wrong password?` at each of ten attempts 14.6 s
  apart, self-healed after 131 s and began again, and never became an
  access point. It was brought back by erasing `nvs` over USB.
- **The right one.** `vasilykh-wrt`, hostname `test-clock`, 11 dBm
  and the browser's zone in one save: the saved page, the reset, the
  address by DHCP 5.3 s after boot, the time synced.
- **The PIN.** A wrong PIN on `/token` is a 403 and a 9 s lockout
  that the next click reports; then the token set from the System
  page with the PIN, the firmware uploaded from the page with a PIN
  (907 KB in 17 s, "Back up … in ota_1" 7 s later, confirmed good at
  its first boot), Reboot with a PIN. With the token:
  `scripts/reload.sh`, `scripts/clips.sh`, a wrong Bearer refused.
- **Back to the access point**, from the page with a PIN and by
  `POST /ap` with the token: the setup form has the stored network
  picked and the stored hostname, zone and power; the System page
  there has no Access point button and says a token is set. Save
  untouched resets into the station at once; the timezone alone with
  an empty password saves, resets and joins with the stored password.

Found (fixed the same day, in the section after this one):

- A request head and body over `REQUEST_BUF` is an empty reply and no
  log line: 1530 bytes were answered, 1590 were not. The open item
  under "Nits", now measured on the air.
- `POST /api password=…` on the unprovisioned access point saved (a
  password set, no network) and did not reset: the open backlog item,
  as it describes.
- A refused setup form comes back as a fresh one: the picked network,
  the hostname and the rest are gone, and on a new clock the pick is
  "Other, typed below" again, so pressing Save a second time is "no
  network was named".
- The System page's line after an upload or a reboot reads
  "…; rebooting Waiting for the clock to come back.", two messages
  joined with no full stop (`back()` in the page's script).
- `agents/behaviour.md` did not list `/clips` among the pages the
  access point serves; it does now.

Not done: the buttons (the 10 s hold, the IP display), the panel
itself (`AP`, the PIN's digits, the blank station), a phone's sign-in
sheet, and the web installer in a browser. test-clock was left as it
was found (lime, alarm 01:00 disarmed, the broker, the local pack and
`futurama`, the session's token), on this image in `ota_1`.

## The install run's findings fixed — built 2026-11-17, run on test-clock

The four things the run above found, no new setting and nothing in
the record.

- **A request too long is answered.** `read_request` gave
  `NoRequest::Close` for a head with no end in `REQUEST_BUF`, a body
  over `BODY_MAX` and a head that does not parse. Now a `431`, a
  `413` and a `400`. An answer written with bytes unread is lost to
  the reset, so the first two read what is coming and throw it away
  first, the request buffer as scratch: the 413 the rest of the
  body by its length (`discard`), the 431, which cannot know a
  length, until the client has been quiet 100 ms or has sent eight
  buffers (`discard_until_quiet`). All of it is inside the
  listener's `HEAD_TIMEOUT`, so a body that takes longer than 2 s
  is still a close.
- **`/api` refuses a password with no network**, a `400`, when the
  request is what made it so (`Credentials::password_alone`, under
  test); a clock already in that state takes its other keys.
- **A refused setup form comes back filled in.** The refusal is a
  redirect, so the page is a new one. Its script keeps the pick, the
  typed name, the timezone, the hostname and the power in
  `sessionStorage` at the submit and puts them back when the query
  has `error=`; never the password. A pick no longer in the list
  falls back to "Other". A test ties the script's list to the
  form's fields. The page is 4761 bytes of 8032 at its worst.
- **The System page's line** has a full stop between the clock's
  answer and "Waiting for the clock to come back."

On test-clock, over the air, then with `nvs` erased for the access
point's half (a build as the CI makes it):

- on the station a 1.7 KB and a 5 KB cookie are a `431`, a 900-byte
  body to `/api` a `413`, `HELLO` a `400`, each with its line in the
  log, and the next request is served; on the access point the setup
  form with a 1.7 KB cookie is a `431`. A 909 KB body to `/api` is
  still an empty reply, the 2 s being over first;
- `POST /api password=…` on the unprovisioned access point is the
  `400` and `password_set` stays false; `color=red` after it is
  taken;
- in headless Chromium the form refused for a secured network with
  no password came back with the pick, `PDT7`, `test-clock` and
  11 dBm; a visit without `error=` shows the clock's own values; a
  typed "Other" name came back typed; the password field is empty
  each time; with the password typed it saved and joined;
- the Reboot line reads "rebooting with the time carried over.
  Waiting for the clock to come back."

One thing not explained: on the access point one `curl` straight
after three others was refused its connection (`Failed to connect`),
once; 60 posts in a row on the station, with and without a `400`
between them, were all answered. test-clock is on this build in
`ota_1` (the CI's configuration), its settings as before.

Not seen: the forms from a phone, whose head is what the 1536 bytes
are tight for.

## Checks still owed

Built, and by its heading in `HISTORY.md` not yet seen, heard or run
where it counts. Each line is that heading as it stood on 2026-11-17,
when the history was split off; strike or delete a line when the check
is made or is no longer wanted, and put the result in the section.

- **Hold the self-heal while the alarm rings, and ring for five minutes (after the brightness ceiling)** — built, not yet verified on the panel.
- **More tunes, and a random pick (after `/reload`)** — random verified on the bench 2026-10-28; the tunes retuned after two listens, a third owed.
- **M11a — read both sensors and show them on the page and in `/api`** — built 2026-11-01; the NTC verified on the bench the same day, the LDR read the rail (see the end).
- **Codex review of 2026-11-07: eleven findings fixed** — built 2026-11-07, released as v0.3 and soaked 11 h on the bench 2026-11-08; the fixes' own checks still owed.
- **ZX Spectrum tunes: an AY-3-8910 and a PT3 player on the chip** — built 2026-11-13, on white-clock to be heard; the register comparison with a reference player owed.
- **A Stop button beside Play** — built 2026-11-13, to be tried on white-clock.
- **Quieter noise for the AY's drums** — built 2026-11-13, an A/B/C on white-clock to be heard.
- **A low-pass for the AY's noise** — built 2026-11-13, an A/B/C on white-clock to be heard.
- **Bass cut for a speaker: an A/B on headphones** — built 2026-11-13, on test-clock to be heard.
- **Where a module's overdrive comes from: the limiter's release** — measured on the host 2026-11-13, the A/B on test-clock's headphones to be heard.
- **PT3 tunes flat, for headphones** — built 2026-11-13, to be heard on test-clock.
- **The station picks its access point: the strongest of each scan** — built 2026-11-13, not on a clock yet.
- **The page split in four: clock, settings, Home Assistant, system** — built 2026-11-14, to be tried on white-clock.
- **The thermometer held while Q1 is hot** — measured on white-clock 2026-11-14, the hold built and run there 2026-11-15; a short preview and the serial log's lines not yet seen.
- **The pages after a UX review: PIN at the click, a saved notice, the bad field named** — built 2026-11-15, run on test-clock in headless Chromium; the setup form the user's, 2026-11-15; the setup form and a real upload from the page run on test-clock 2026-11-17 ("An install from nothing").
- **Synchronization in the default pack** — 2026-11-16, built and tested on the host; the pack written to test-clock and read at boot 2026-11-17, not heard.
- **PCM clips at half the flash: IMA ADPCM, `ADP1`** — the A/B heard on white-clock 2026-11-16 ("not bad", "way better than PCM 8k"); the format built 2026-11-16 and heard by the user on a speaker the same day ("audible, but acceptable"); the serial log's clip line read on test-clock; the buzzer through the clock's own decoder not yet.
- **The PT3 clips' limiter: the buzzer's as the speaker's, the speaker's to be made gentler** — 2026-11-16, the buzzer's clip converted and tested on the host, not heard on a buzzer; a knee for the PT3 limiter built, and `--knee 37 --drive 150` chosen by the user on test-clock's speaker.
- **The project renamed: onebitdawn** — 2026-11-17, built and tested on the host; the pages and Home Assistant not yet seen on a clock.
- **A clip of a version no player reads is refused** — 2026-11-17, built and tested on the host; no upload of such a pack tried on a clock.
- **Forward compatibility before the first public release** — 2026-11-17, built, tested on the host and on test-clock; not committed, not tagged.

Headings that still say planned or not run, though the work is in
the tree or was overtaken; nothing is owed but the heading's wording,
unless the user knows otherwise:

- **M11 — NTC thermometer and light sensor** — the readings built 2026-11-01 (M11a, below), the face and the dimming still planned (the face is "M11b, first half", done; the dimming is above).
- **Web flasher and releases** — planned 2026-11-07 (`docs/index.html`, `release.yml` and four releases are in the tree).
- **Timezone rules: POSIX TZ strings and daylight saving** — planned 2026-11-07 (`core/src/tz.rs` is in the tree).
- **Scan restart gap: measure the whole dark time** — built 2026-11-07, not yet verified on the bench (the restart it measures went with the ring).
- **Scan ring spike: the segmented transfer on the bench** — built 2026-11-08, not yet run (the ring is the scan path since "Hardware-chained scan").
- **Tracker modules (XM)** — first trial as a rendered clip 2026-11-12; a player on the chip only planned (`core/src/xm.rs` is in the tree).

## Review backlog

The findings of the four review passes that were open when the
history was split off, each as it was written, line numbers and all
(they are of the commit the pass read). Each pass's account of what it
read and what checked out, and the items closed, are under the same
headings in `HISTORY.md`. Strike an item when it is done or decided
against, with the date.

Every item was read against the code on 2026-11-17 (commit c08b6cd).
Four were found done and are struck; an item with a "Checked" note
has changed in part; the rest hold as written, in the functions they
name, at other line numbers.

### Review backlog (2026-10-21)

- ~~**Spaced day lists.**~~ Found done 2026-11-17: `form::apply` decodes a
  value into `VALUE_MAX` (64) bytes, so the 33-byte list reaches
  `parse_days`; no test puts the full spaced list through `apply`.
  `parse_days` trims each name, but the 32-byte
  value buffer in `form::apply` is one byte short of
  `mon, tue, wed, thu, fri, sat, sun`, so the API answers "bad value"
  before the trim runs. Raise the buffer to 40 or so.
- **Build-time timezone range.** `TZ_OFFSET_MINUTES` is cast to i16 in
  `config.rs` without a range check, so a wild value wraps silently and
  `Settings::DEFAULT` is never run through `is_valid`. A const assert
  against `TZ_LIMIT_MINUTES` catches it at compile time.

### Review backlog, second pass (2026-10-23)

Bugs, in rough order of value:

- **Self-heal is unreachable once the lease expires on a dead uplink.**
  `probe_task` starts every iteration with `stack.wait_config_up()`
  (`net.rs:328`). When the uplink dies with less lease time left than
  the five-minute budget, smoltcp's DHCP client resets at `expires_at`
  and embassy-net clears the address (`lib.rs:907-909`); from then on
  no probe is sent, the streak is frozen below ten, the link is still
  up so `wifi_task` sees nothing, and the connect budget needs connect
  failures that never come. The clock sits with the carried time and
  no page until power is cut. With a one-hour lease that is about one
  uplink death in twelve. Fix: bound the wait (`with_timeout(PROBE_PERIOD,
  wait_config_up())`) and, once armed, count a period with the link up
  and no address as a failed probe. Left as is, 2026-11-02: a corner case
  that needs a link to die in the last minutes of its lease, while a
  marginal link trips the probe budget well before that. The fix, if
  it is ever wanted, is the bounded wait above with the no-address
  period scored like a lost ping, and the streak logic moved into
  `core` under test.
- **`POST /play` during an alarm silences it.** `buzzer_task`'s `Ring`
  loop breaks on any command and the `Preview` arm plays once and then
  waits for the next command; the display re-issues `ring` only on the
  ringing edge. One preview, from a browser tab left on the page, say,
  leaves the panel flashing in silence for the rest of the 120 s. Fix:
  ignore `Preview` while ringing, or go back into the ring loop after
  it.

Gaps:

- **No watchdog.** `esp_hal::init` disables the SWD, RWDT and both
  timer-group watchdogs and nothing re-enables one. A hang that is not a
  panic (a lockup in the Wi-Fi driver, a critical section waiting on
  something, an infinite loop in a freshly uploaded image) leaves the
  panel dark until someone opens the case, and the OTA rollback counts
  only software resets, so a new image that hangs is never rolled back,
  contrary to `ota.rs`'s header. Fix: enable the RWDT with a few-second
  timeout fed from the heartbeat or the display tick, and keep
  `ota_boots` in `carry::init` on a watchdog reset reason.
- **A PIN issued while ringing or in alarm-set is dropped, not deferred.**
  `ui.show_pin` acts only in the clock, IP and PIN modes, but the
  `Signal` is consumed either way and `ota.rs:218` logs "shown on the
  panel" regardless; the user at the update page has nothing to type
  until they reload. Fix: keep the pending PIN in `Ui` and show it when
  the mode returns to clock within `PIN_TTL_MS`, or log that it was not
  shown.
- **The connect budget is cleared by an association that does not
  survive.** `failures` and `first_failure` reset on any `connect_async`
  success. An access point that associates and drops the station before
  DHCP finishes (MAC filter, client isolation, a flaky AP) gives a loop
  of connect-ok, disconnect, retry in which neither budget fires and the
  probe never arms. Fix: clear the budget only once DHCP has an address
  or the connection has lasted some tens of seconds.

Nits:

- **`scan::stop` is not honoured on the DMA start-failure path.** The
  `Err` branch sleeps and `continue`s past the `STOP` check, so if the
  start kept failing `heal::reset` and `ota::install` would spin for
  ever. Unreachable with esp-hal 1.2.2's constant error conditions;
  a `STOP` check before the retry keeps the invariant.
- ~~**Oversized or unparseable requests get a TCP reset, not a status.**~~
  Fixed 2026-11-17 ("The install run's findings fixed").
  `read_request` returns `None` for a head over `REQUEST_BUF`, a body
  over `BODY_MAX` or a bad request line, and the socket is closed with
  bytes pending. Answer 413/400 after a bounded drain, as `/ota` does.
  Checked 2026-11-17: a `Transfer-Encoding` now gets a 400 and a
  `POST` with no length a 411; the three cases above are still a
  silent close (`NoRequest::Close`).
- **First change on an empty flash writes the default record.** With
  `stored == None` the compare in `persist_task` is skipped, so ticking
  "skip the next alarm" on a fresh board costs a flash write for a field
  the record does not hold. Compare against the defaults when nothing
  was loaded.
- **A persistent `set_config` failure loops outside the connect budget.**
  The error path at `net.rs:162-166` sleeps and retries without touching
  `failures`. Static errors are caught earlier, so unlikely to matter.
- **`FIRMWARE_VERSION` goes into JSON and HTML unescaped.** It is
  `git describe`, so a tag with `"` or `<` breaks `/api` and the page.
  Sanitise in `build.rs`.
- **Silent fallbacks in the panel tables.** `com_index` returns slot 0
  for a value not in `COM_SEQ`, and nothing ties `GAMMA[255]` to
  `MAX_LEVEL`. Two `const` asserts lock in what is verbatim today.
  Checked 2026-11-17: `GAMMA` went with the colours in levels; the
  `com_index` half holds.
- **In the ringing off-phase the degree mark stays lit.** `set_degree`
  runs before the match, so the flash is digits-off, mark-on. Cosmetic.
- ~~**Stale names.**~~ Found done 2026-11-17: `check_rollback` is named
  nowhere, and the image has grown to nearly 900 KB, so the 800 KB in
  `http.rs` is no longer too much. `ota.rs:12` and `carry.rs:49` refer to
  `ota::check_rollback`, which is `ota::init`; `http.rs:29` says an
  upload is 800 KB where the image is about 680 KB; the M10 text said
  two `RECEIVERS` where there are three (fixed in place).

### Review backlog, fourth pass (2026-10-30): the code since the second pass

Bugs, in rough order of value:

- **`POST /scan` can only answer within the socket's 5 s idle
  timeout.** `rescan` waits up to `SCAN_WAIT` (10 s) for the AP task's
  scan, during which the browser sends nothing, and smoltcp's socket
  timeout is an idle timeout: `remote_last_ts` moves on every received
  segment and `dispatch` closes the socket at `remote_last_ts +
  timeout` with no regard to pending transmit data. A scan slower than
  5 s (13 channels at up to 200 ms is about 2.6 s today) answers into a
  closed socket and the phone shows a reset instead of the page. Fix:
  raise the timeout for that request, or cut `SCAN_WAIT` under
  `TIMEOUT` and say so in the comment.

Gaps:

- ~~**A record the map scratch cannot hold wipes every setting on
  rollback.**~~ Found done 2026-11-17 (the forward-compatibility work of
  that day): the scratch is 1024 with `ITEM_MAX` under it, and
  `BufferTooSmall` runs on the defaults without erasing; by design
  the first save after that does erase the map. `Store::load` erases the whole map on any `Err` from
  `fetch_item`, but sequential-storage returns `BufferTooSmall` before
  anything is corrupt when an item does not fit (`Corrupted` is already
  repaired inside `fetch_item`). The scratch is 256 bytes and a
  version-6 item is 176 (8-byte header, 1-byte key, 167 of record; the
  9-byte claim holds); the record grew 99 bytes in the last version, so
  one more set of fields of that order (a second network) takes a
  version-7 item past 256. An image with that record saves it, the
  `OTA_BOOT_LIMIT` rollback boots this one, `load` logs "cannot read the
  map, erasing it", and the clock comes up with hostname `clock`,
  timezone 0 and no network, the exact case the newer-prefix read was
  built to prevent. Fix: erase only on `Corrupted`, treat
  `BufferTooSmall` as "not understood, using defaults" without erasing,
  and size the scratch to a page.
- ~~**A string-rule change in a newer image throws the record away on
  rollback.**~~ Found done 2026-11-17 (the same work): a network or a
  token `from_record` does not accept is that field's default and the
  rest is kept. `from_record` clamps `color`, `melody` and `brightness`
  for exactly this case, but the version-6 strings go through
  `is_valid`, so a password of 1 to 7 bytes or a token outside
  `valid_token`, both legal in a later image that relaxed the rule,
  make this image discard the whole record. Fix: a bad string field
  falls back to its default (`Credentials::NONE`, empty token) and the
  rest is kept.
- ~~**`/api` on the access point saves a password with no network.**~~
  Fixed 2026-11-17 ("The install run's findings fixed").
  Since 2026-10-30 the persist task resets off the access point only
  with a network stored, so this is a pointless save and no reset;
  the guard on `/api` is still missing. The
  `trial.wifi.is_none()` guard is only on the form path; `POST /api
  password=…` on an unprovisioned access point saves, differs from
  `boot_wifi`, and the persist task resets into station mode with an
  empty SSID, which the boot turns back into an access point. A
  harmless loop, but the script sees "saved" and a reset for nothing.
  Fix: the same guard on `/api` in access-point mode.
- **The OTA rollback carries a stale time.** `ota::init` runs before
  `clock::restore_carried` in `main.rs`, so its `carry_time()` is a
  no-op (`unix_micros` is `None`) and the record keeps the time written
  at the end of the previous boot; the restore then adds `RESET_PAUSE`
  to a value older by that boot's span up to `ota::init` plus its own
  pause, about a second, shown until NTP. Fix: `restore_carried` before
  `ota::init`.
- **Without a usable `nvs` partition a provisioning save never leaves
  the access point.** The reset into station mode lives in
  `persist_task`, spawned only when `Store::open` succeeded; with no
  store the setup page still says the clock will reset and nothing
  does, while the unchanged-credentials path does reset. A bad
  partition table only; answer differently or reset from the listener
  when there is no store. The same shape as the open "failed save is
  never retried": on the access point a failed save shows the saved
  page and stays an access point until the next change.
  Checked 2026-11-17: a failed save is retried now (`persist_task`'s
  backoff), so only the no-store case is left.
- **The access point's transmit ceiling is set before the driver is
  restarted.** `ap_task` calls `set_max_tx_power` and then
  `set_config(AccessPointStation)`; `WifiController::new` has already
  started the driver as a station, so that `set_config` is a mode
  change, which stops and restarts the driver (`esp_wifi_stop`,
  `esp_wifi_set_mode`, `esp_wifi_start` in esp-radio). ESP-IDF
  documents the power call as "after WiFi start"; whether the value
  survives the restart is inside the blob and was not verified. The
  station path has no restart. Fix: move the call after `set_config`,
  so the boot line says what is in force.

Nits:

- **The display loop wakes every 64 ms for nothing on the access point
  and while a PIN shows.** The redraw wait is `BLINK_STEP / 4` for every
  view but `Time` and `Ip`, right for the blink and the sweep, but
  `View::Sweep` on the access point is a static `AP` for the whole boot
  and `View::Pin` is static for its minute. Each wake runs
  `carry_time`, a `settings::update` under the watch's critical section
  (which holds off the interrupt executor for its duration), a render
  and the image compare. Fix: wait `PIN_SHOW` for the PIN and a second
  for the sweep when `net::mode()` is the access point.
- **`Store::open` logs inside the flash critical section.** The
  `error!` lines for a missing partition table or `nvs` entry run
  inside `with_flash`'s `critical_section::with`, so the serial write
  stalls everything. Boot-only, before the scan starts; move the match
  outside the closure.
- **A discover from a client that already holds a lease cuts the lease
  to the offer hold.** `Leases::handle` writes `now + OFFER_HOLD_SECS`
  (60 s) over an existing slot when `find` hits. A phone that
  re-discovers and then does not request keeps using the address while
  the slot is free again after a minute and can be offered to the next
  client. Fix: on a discover, keep the later of the two expiries.
- **`dnsd::answer` indexes `out` by the query's length.** `out[HEADER..n]`
  with `n` up to `query.len()` is only checked against `PACKET`; a
  caller passing a query longer than `out` panics. The task's buffers
  are both 512 so it cannot happen on the air; a host test feeding a
  long query would hit it. Fix: `get_mut(HEADER..n)?` like the record
  copy below it.
- **`REQUEST_BUF` is tight for the setup form from a phone.** The head
  and the inline body share 1536 bytes; the setup form at its worst is
  about 457 bytes encoded (two 32-byte SSIDs and a 64-byte password,
  percent-escaped), leaving about 1080 for the head, and a mobile
  browser's POST head to a captive page is about 750 bytes before any
  cookie another gadget at 192.168.4.1 may have set. Over the limit is
  the silent close of the open "oversized requests" item, on the one
  page whose user cannot see the log.
  Since 2026-11-17 over the limit is a `431` the browser shows; the
  room is what it was.
  Checked 2026-11-17: the form also posts the timezone, the hostname
  and the transmit power now, and a body was refused over `BODY_MAX`
  (512) whatever the head left. Measured by a test the same day
  (`forms_fit_the_request_body`): 605 bytes with both names, the
  password and the timezone full and every byte escaped, so
  `BODY_MAX` is 640 now (built and tested on the host, not on a
  clock). What is left of the item is the head: at 605 of body it has
  931 bytes of the 1536, against the 750 or so given above for a
  phone.
- **No error from `ap_task` is ever healed.** A failed `set_config`
  logs once and sleeps for ever; the panel shows `AP` with no network
  and only a power cycle or a hold helps. Unlikely with the
  compile-time-checked SSID and password; a log line is probably all
  that is worth doing.
- **Stale comments.** `net::Mode`'s doc says the driver does not do
  AP+STA, which `ap_task` does for the scan; `http.rs` still says an
  upload is 800 KB where the image is about 650 KB (the second pass's
  `check_rollback` half is fixed).
  Checked 2026-11-17: the `net::Mode` half holds; the image is nearly
  900 KB now, so the 800 KB one does not.
