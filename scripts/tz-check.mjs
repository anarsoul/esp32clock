#!/usr/bin/env node
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick
// Runs src/tz.js's posixTz under Node against zones with known rules:
// the one check the host can make of the browser half of the timezone
// setting. Exits 1 on a mismatch. Node's Intl uses the same tz data a
// browser does, so a zone whose rule changes shows up here first.
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import vm from 'node:vm';

const src = readFileSync(join(dirname(fileURLToPath(import.meta.url)), '..', 'src', 'tz.js'), 'utf8');
const ctx = { Intl, Date, Math };
vm.createContext(ctx);
vm.runInContext(src + '\nthis.posixTz = posixTz;', ctx);

const expected = {
  'America/Los_Angeles': 'PST8PDT,M3.2.0,M11.1.0',
  'America/New_York': 'EST5EDT,M3.2.0,M11.1.0',
  'America/St_Johns': 'NST3:30NDT,M3.2.0,M11.1.0',
  'America/Phoenix': 'MST7',
  'Europe/London': 'GMT0BST,M3.5.0/1,M10.5.0',
  'Europe/Berlin': 'CET-1CEST,M3.5.0,M10.5.0/3',
  'Europe/Moscow': '<+03>-3',
  'Asia/Kolkata': 'IST-5:30',
  'Asia/Kathmandu': '<+0545>-5:45',
  'Asia/Tokyo': '<+09>-9',
  'Africa/Johannesburg': 'SAST-2',
  'Australia/Sydney': 'AEST-10AEDT,M10.1.0,M4.1.0/3',
  'Australia/Lord_Howe': 'LHST-10:30LHDT-11,M10.1.0,M4.1.0', // half an hour of daylight time
  'Pacific/Auckland': 'NZST-12NZDT,M9.5.0,M4.1.0/3',
  // Chile changes at Saturday 24:00, which one year alone cannot tell
  // from Sunday 00:00; the check against next year does.
  'America/Santiago': '<-04>4<-03>,M9.1.6/24,M4.1.6/24',
  // Nuuk changes with Europe, at 01:00 UTC: Sunday -01:00 local.
  'America/Nuuk': '<-02>2<-01>,M3.5.0/-1,M10.5.0/0',
  'UTC': 'UTC0',
  // Fixed since 2022 and 2019: no change in the year, not an equal
  // offset in January and July.
  'Asia/Tehran': '<+0330>-3:30',
  'America/Sao_Paulo': '<-03>3',
  // Casablanca falls back an hour for Ramadan, which moves eleven days
  // a year: two changes, but no rule, and the page says so.
  'Africa/Casablanca': 'throws',
};

let bad = 0;
for (const [zone, want] of Object.entries(expected)) {
  let got;
  try {
    got = ctx.posixTz(zone);
  } catch (e) {
    got = `throws: ${e.message}`;
  }
  const ok = want === 'throws' ? got.startsWith('throws: no rule for ') : got === want;
  bad += !ok;
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${zone.padEnd(22)} ${got}${ok ? '' : `  (expected ${want})`}`);
}
process.exit(bad ? 1 : 0);
