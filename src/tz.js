// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick
// The browser's IANA zone as a POSIX TZ rule for the clock, from next
// year's two offset changes checked against the six years after; throws
// for a zone no such rule describes. PLAN.md has the why; the Node check
// is scripts/tz-check.mjs.
function posixTz(zone) {
  var y = new Date().getUTCFullYear() + 1;
  var fmt = new Intl.DateTimeFormat('en-US', {timeZone: zone, hourCycle: 'h23', year: 'numeric', month: 'numeric', day: 'numeric', hour: 'numeric', minute: 'numeric', second: 'numeric'});
  // Minutes east of UTC at instant t (ms).
  function off(t) {
    var o = {};
    fmt.formatToParts(new Date(t)).forEach(function (p) { o[p.type] = +p.value; });
    return Math.round((Date.UTC(o.year, o.month - 1, o.day, o.hour % 24, o.minute, o.second) - t) / 60000);
  }
  // The name at t: letters from the first locale that has them (en-US
  // PST, en-GB CET, en-AU AEST, en-CA NST, en-IN IST, en-ZA SAST), else <+hhmm>.
  function name(t, o) {
    var locales = ['en-US', 'en-GB', 'en-AU', 'en-CA', 'en-IN', 'en-ZA'];
    if (typeof navigator != 'undefined' && navigator.language) locales.unshift(navigator.language);
    for (var i = 0; i < locales.length; i++) {
      var n = new Intl.DateTimeFormat(locales[i], {timeZone: zone, timeZoneName: 'short'}).formatToParts(new Date(t)).filter(function (p) { return p.type == 'timeZoneName'; })[0].value;
      if (/^[A-Za-z]{3,12}$/.test(n)) return n;
    }
    var a = Math.abs(o), h = Math.floor(a / 60), m = a % 60;
    return '<' + (o < 0 ? '-' : '+') + (h < 10 ? '0' : '') + h + (m ? (m < 10 ? '0' : '') + m : '') + '>';
  }
  // A POSIX offset, positive west, of o minutes east.
  function west(o) {
    var a = Math.abs(o), h = Math.floor(a / 60), m = a % 60;
    return (o > 0 ? '-' : '') + h + (m ? ':' + (m < 10 ? '0' : '') + m : '');
  }
  // The first minute in (lo, hi] whose offset differs from lo's.
  function change(lo, hi) {
    var o = off(lo);
    while (hi - lo > 60000) {
      var mid = lo + Math.floor((hi - lo) / 120000) * 60000;
      if (off(mid) == o) lo = mid; else hi = mid;
    }
    return hi;
  }
  // The instant of rule r = {m, w, d, t} (t minutes after midnight, may be
  // negative or past the day) in year yr, local time `before` minutes east.
  function when(r, yr, before) {
    var first = Date.UTC(yr, r.m - 1, 1), days = new Date(Date.UTC(yr, r.m, 0)).getUTCDate();
    var day = (r.d - new Date(first).getUTCDay() + 7) % 7 + (r.w - 1) * 7;
    while (day >= days) day -= 7;
    return first + (day * 1440 + r.t - before) * 60000;
  }
  // Rules that could describe the change at t: the day itself, the day
  // before at 24:00+ or the day after at a negative time when it is near
  // midnight, each as the n-th or the last such weekday.
  function candidates(t, before) {
    var l = new Date(t + before * 60000), out = [];
    function add(date, min) {
      var d = date.getUTCDate(), days = new Date(Date.UTC(date.getUTCFullYear(), date.getUTCMonth() + 1, 0)).getUTCDate();
      var r = {m: date.getUTCMonth() + 1, w: Math.ceil(d / 7), d: date.getUTCDay(), t: min};
      if (d + 7 > days) out.push({m: r.m, w: 5, d: r.d, t: min});
      out.push(r);
    }
    var min = l.getUTCHours() * 60 + l.getUTCMinutes();
    add(l, min);
    if (min < 180) add(new Date(t + before * 60000 - 86400000), min + 1440);
    if (min >= 1260) add(new Date(t + before * 60000 + 86400000), min - 1440);
    return out;
  }
  // Mm.w.d/time as text.
  function text(r) {
    var a = Math.abs(r.t), h = Math.floor(a / 60), m = a % 60;
    return 'M' + r.m + '.' + r.w + '.' + r.d + (r.t == 120 ? '' : '/' + (r.t < 0 ? '-' : '') + h + (m ? ':' + (m < 10 ? '0' : '') + m : ''));
  }
  // Every change of year yr, in order, from monthly samples bisected.
  function changes(yr) {
    var out = [], t = Date.UTC(yr, 0, 1);
    for (var m = 1; m <= 12; m++) {
      var n = Date.UTC(yr, m, 1);
      if (off(n) != off(t)) out.push(change(t, n));
      t = n;
    }
    return out;
  }
  function unruly() { return new Error('no rule for ' + zone); }
  // The candidate that also predicts later[k], year y + 1 + k's change.
  function rule(t, before, later) {
    var c = candidates(t, before);
    for (var i = 0; i < c.length; i++) {
      var ok = true;
      for (var k = 0; k < later.length; k++) if (when(c[i], y + 1 + k, before) != later[k]) ok = false;
      if (ok) return text(c[i]);
    }
    throw unruly();
  }
  var jan = Date.UTC(y, 0, 1), now = changes(y), a = off(jan);
  if (now.length == 0) return name(jan, a) + west(a);
  var b = now.length == 2 ? off(now[0]) : a;
  if (b == a || off(now[1]) != a) throw unruly();
  // Standard time is the smaller offset; the south has it mid-year.
  var south = a > b, std = south ? b : a, dst = south ? a : b, starts = [], ends = [];
  for (var k = 1; k <= 6; k++) {
    var c = changes(y + k);
    if (c.length != 2) throw unruly();
    starts.push(south ? c[1] : c[0]);
    ends.push(south ? c[0] : c[1]);
  }
  return name(south ? now[0] : jan, std) + west(std) + name(south ? jan : now[0], dst) + (dst - std == 60 ? '' : west(dst)) + ',' + rule(south ? now[1] : now[0], std, starts) + ',' + rule(south ? now[0] : now[1], dst, ends);
}
if (typeof document != 'undefined') (function () {
  var b = document.getElementById('b'), z = document.getElementById('z'), zn = document.getElementById('zn');
  if (!b) return;
  b.onclick = function () {
    var zone = Intl.DateTimeFormat().resolvedOptions().timeZone;
    try {
      z.value = posixTz(zone);
      zn.textContent = zone;
      var f = z.form;
      if (f && f.oninput) f.oninput();
    } catch (e) {
      zn.textContent = e.message || 'could not read ' + zone;
    }
  };
})();
