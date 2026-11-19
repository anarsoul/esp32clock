# Sizes reference

Every buffer, budget and limit with its measured figures. `AGENTS.md`
names the limits; update the figures here when a test prints new ones.

## Pages and the response buffer

`http::RESPONSE_BUF` is 8 KB, one per listener task (6 KB before
2026-11-15, 4096 before 2026-11-02). It holds the whole page plus
headers, and overflow is a 500, not a truncation.

- The headers are 126 bytes for a page (`HEADERS_MAX`).
- `core::http::PAGE_MAX` (8032) is what a page may take. The `form`
  tests build each page at its worst case and assert it: run `cargo
  host-test` after adding a field or label; the test prints the sizes
  with `-- --nocapture`.
- `src/tz.js` (5.8 KB) is sent from one response buffer, and a `const`
  assert in `src/http.rs` keeps it under `RESPONSE_BUF` with its
  header; it was trimmed to comments of a line each to fit.

Worst cases since 2026-11-15 (the PIN asked for at the click, the
notices, the day checkboxes, the forms' own post):

| page | bytes | worst case |
| --- | --- | --- |
| clock | 5989; 7205 with a full pack of sixteen clips at their longest, which leaves 827 | the status lines, a stored SSID of 32 `"` and a 64-byte broker host in them |
| settings | 3812 (3138 before the transmit power) | a 48-byte timezone rule of `<`, four bytes each through `HtmlStr` |
| Home Assistant | 3110 | a user of 32 `"` |
| clips | 3686 | |
| system | 4828 | |
| setup, without a list | 3084 | |

Earlier figures, for scale: on 2026-11-14, after the split in four and
with the three clips of `clips/`, the clock page was 4353, settings
2433, Home Assistant 2253, system 3876, and setup with twelve
32-character networks about 3200. The single page before the split
was 5564, and 5865 with nine clips.

The setup page's network list:

- It stops at `form::NETWORK_LIST_BUDGET` (1900) bytes, strongest
  first, with a "N more not shown" line. A listed SSID of 32 `"` is
  417 bytes of `<option>` through `HtmlStr` (twice per network), and
  five of them would pass the buffer.
- The rest of the page at its worst is 2324, and
  `provision_page_budget_is_covered` locks the sum under `PAGE_MAX`.
  The budget was set when the page had 3968 and stays, since twelve
  networks is plenty.

## Requests

- `http::RX_BUF` (4096) is the TCP receive window; 1 KB made an upload
  one segment per round trip.
- `REQUEST_BUF` (1536) bounds the head; an upload's body stays on the
  socket.
- A form's or `/api`'s body is read into what the head leaves of that
  buffer and may be `http::BODY_MAX` (640) at most; over either, the
  socket is closed with no answer. `forms_fit_the_request_body`
  measures each form at its worst (`-- --nocapture` prints them): the
  setup form 605, the Home Assistant form 404, the settings form 255,
  the clock page's 165. With the setup form at 605 the head has 931.
- `form::apply` decodes each key into 16 bytes and each value into 64
  (`form::VALUE_MAX`, the password); longer input is reported as a bad
  key or value before parsing.

## Flash

| what | limit | figures |
| --- | --- | --- |
| an OTA image | a slot, 0x160000 = 1408 KB (`partitions.csv`) since the `clips` partition took half a megabyte of each, 2026-11-14; 1920 KB on a clock with the old table | the release image is 877 KB; 1045 KB with the first three clips of `clips/` built in, and Synchronization, the fourth since 2026-11-16, is 11 KB more. A build with Futurama in `ALARM_CLIP` as well no longer fits. |
| a clip pack | its partition, 0x120000 = 1152 KB to the end of the 4 MB flash; `scripts/mkpack.py` refuses a larger one | its SHA-256 is checked in software at every boot before the settings load: 119 ms for the default pack's 172 KB, 482 ms for 700 KB (test-clock, 2026-11-14) |
| the settings record | `settings::RECORD_LEN`, 387 bytes | the map scratch in `storage.rs` is 1024 and must hold the item (record plus 9 bytes, padded to a word); `storage::ITEM_MAX`, 1000, is the longest item any firmware may write, see "Compatibility" in `AGENTS.md` |

Clip sizes:

- PCM is 16 KB of flash a second at 16 kHz, 8 KB at 8 kHz, and half
  that as ADPCM. The 27.6 s test clip is 442 KB of `.rodata`, held
  once: `melody::CLIP_DATA` is a `static`.
- A beeper clip is half a KB to 1.5 KB a second packed, twice that as
  plain runs.
- A tracker module is some 10 KB whatever its length.

## Static RAM and the main stack

What the statics leave of DRAM is the main stack. Check `.stack` in
`llvm-readelf -S` after adding a static, and compare with a build of
`HEAD` in a worktree when in doubt.

| date | main stack | change |
| --- | --- | --- |
| 2026-11-07 | 84.5 → 69.6 KB | the MQTT task |
| 2026-11-07 | 67.1 KB | |
| 2026-11-08 | 66.9 KB | the ring's descriptors and conf words are 160 bytes; the gap statistics and buffer channels went |
| 2026-11-14 | 49.9 KB | the clip ring, 16 KB, in every build |
| 2026-11-15 | 43.7 KB | `RESPONSE_BUF` at 8 KB |
| 2026-11-17 | 42.2 KB | the settings map's scratch at 1 KB |
| 2026-11-18 | 39.9 → 39.3 KB | `xm::MAX_CHANNELS` from 8 to 16, 640 bytes of the clip task's future (both figures with the line output's setting in the tree) |

What takes it:

- `ota.rs` holds 11 KB of buffers.
- `Settings` is a 387-byte record and sits in several task futures:
  the 49-byte timezone rule cost 2.2 KB of stack.
- One `Response` per listener loop. A second one in another arm of the
  same loop was not overlapped by the compiler and cost 6 KB per
  listener until it was hoisted (2026-11-07).
- The MQTT task's future holds its socket buffers (1 KB receive, 2 KB
  send), the 1 KB `BumpBuffer`, the state JSON (`form::JSON_MAX`,
  1536; the worst case is 1467, printed by the `form` test, about 640
  on a clock) and the
  discovery payload (`mqtt::DISCOVERY_MAX`, 5120; the worst case is
  4771, printed by the `mqtt` test): about 15 KB in all.
- The clip ring is 16 KB in every build, since any build may meet a
  pack.

## The clip ring's timing

- A chunk is 16 ms at 512 kbit/s (a beeper clip) and the ring holds
  fourteen ahead of the DMA. So the clip task not running for about
  220 ms (twice that for the 16-bit samples of a 16 kHz clip under the
  PDM converter) is an underrun, logged when the clip ends with the
  count of DMA positions the pacer did not believe.
- The task is on the interrupt executor for that reason, and a chunk's
  modulating, a millisecond or two, runs there above the Wi-Fi driver.

## Counts

- `settings::RECEIVERS` is the number of `Watch` receivers (display,
  persist, hostname, MQTT). Add one when a new task subscribes, or
  `receiver()` panics at boot.
- `StackResources<11>` in `main.rs` covers DHCP, DNS, SNTP, the
  gateway probe, the MQTT connection and the HTTP listeners with
  headroom (on the access point: the DHCP and DNS servers and the
  listeners). Adding sockets means raising it.
- `HOSTNAME_MAX` (24) stays under embassy-net's 32-byte DHCP option.

## Network

`ESP_RADIO_CONFIG_WIFI_MTU` (540, in `.cargo/config.toml`'s `[env]`,
read back as `config::WIFI_MTU`) bounds every frame the clock sends:
TCP segments carry 486 bytes, the probe 498, and a page is seven
segments instead of two.

- It is a transmit-side limit only: smoltcp applies it when emitting,
  and the driver passes received frames whole.
- It was chosen because the access point hears the clock 20 dB below
  what the clock hears of it, and short frames pass where full ones
  fail (2026-11-01, the adapter-built clocks at −52 to −65 dBm).
- A TCP peer is told the 486-byte segment size and sends an upload in
  486-byte pieces.
- A ping larger than 498 bytes from the LAN gets no reply, since the
  reply would not fit; `scripts/watch-clock.sh` pings with 498.
- Raising it means the probe grows with it; nothing else depends on
  the number.

`config::WIFI_RX_QUEUE_FRAMES` (32) is the driver's receive queue;
esp-radio's default of 5 dropped bursts of LAN broadcasts, visible as
`RX QUEUE FULL` with `esp_radio=debug`. The stack answers ping only
because embassy-net's `auto-icmp-echo-reply` feature is on.
