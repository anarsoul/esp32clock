// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! The library for the web page (`docs/pack/`): `web::Session` over a
//! C ABI, with no binding generator between. The page writes its
//! arguments into memory it asks `obd_alloc` for, and reads an answer
//! at `obd_answer` for `obd_answer_len` bytes: text, or a pack.
//!
//! One thread and one session: a page.

use std::sync::Mutex;

use crate::web::{Session, limits};

struct State {
    session: Option<Session>,
    answer: Vec<u8>,
}

static STATE: Mutex<State> = Mutex::new(State { session: None, answer: Vec::new() });

fn with<T>(f: impl FnOnce(&mut Session, &mut Vec<u8>) -> T) -> T {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let State { session, answer } = &mut *state;
    f(session.get_or_insert_with(Session::default), answer)
}

/// The bytes the page wrote at `at`.
///
/// # Safety
/// `at` is from `obd_alloc(len)` or `len` is 0.
unsafe fn bytes<'a>(at: *const u8, len: usize) -> &'a [u8] {
    if len == 0 { &[] } else { unsafe { core::slice::from_raw_parts(at, len) } }
}

unsafe fn text<'a>(at: *const u8, len: usize) -> &'a str {
    core::str::from_utf8(unsafe { bytes(at, len) }).unwrap_or("")
}

/// 0 and the answer, or -1 and why not.
fn answer(result: Result<Vec<u8>, String>, into: &mut Vec<u8>) -> i32 {
    match result {
        Ok(bytes) => {
            *into = bytes;
            0
        }
        Err(why) => {
            *into = why.into_bytes();
            -1
        }
    }
}

/// `len` bytes for the page to write into; `obd_free` gives them back.
#[unsafe(no_mangle)]
pub extern "C" fn obd_alloc(len: usize) -> *mut u8 {
    let mut buf = Vec::<u8>::with_capacity(len.max(1));
    let at = buf.as_mut_ptr();
    core::mem::forget(buf);
    at
}

/// # Safety
/// `at` and `len` are an `obd_alloc`'s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obd_free(at: *mut u8, len: usize) {
    drop(unsafe { Vec::from_raw_parts(at, 0, len.max(1)) });
}

#[unsafe(no_mangle)]
pub extern "C" fn obd_answer() -> *const u8 {
    with(|_, answer| answer.as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn obd_answer_len() -> usize {
    with(|_, answer| answer.len())
}

/// The pack's limits as JSON, in the answer.
#[unsafe(no_mangle)]
pub extern "C" fn obd_limits() -> i32 {
    with(|_, into| answer(Ok(limits().into_bytes()), into))
}

/// A clip of a file: its JSON in the answer.
///
/// # Safety
/// Each pointer is an `obd_alloc`'s of its length or more.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obd_convert(file: *const u8, file_len: usize, data: *const u8, data_len: usize, options: *const u8, options_len: usize, speaker: u32) -> i32 {
    let (file, data, options) = unsafe { (text(file, file_len), bytes(data, data_len), text(options, options_len)) };
    with(|session, into| answer(session.convert(file, data, options, speaker != 0).map(String::into_bytes), into))
}

#[unsafe(no_mangle)]
pub extern "C" fn obd_forget(id: usize) {
    with(|session, _| session.forget(id));
}

/// The pack of the clips the lines name, in the answer.
///
/// # Safety
/// `lines` is an `obd_alloc`'s of `len` or more.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obd_build(lines: *const u8, len: usize) -> i32 {
    let lines = unsafe { text(lines, len) };
    with(|session, into| answer(session.build(lines), into))
}

/// Start hearing a clip: its samples a second, 0 for none.
#[unsafe(no_mangle)]
pub extern "C" fn obd_preview(id: usize) -> u32 {
    with(|session, _| session.preview(id))
}

/// The next `count` samples as `f32`s at `out`: how many, 0 at the end.
///
/// # Safety
/// `out` is an `obd_alloc`'s of `4 * count` bytes or more.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obd_samples(out: *mut f32, count: usize) -> usize {
    if count == 0 || !out.is_aligned() {
        return 0;
    }
    let out = unsafe { core::slice::from_raw_parts_mut(out, count) };
    with(|session, _| session.samples(out))
}
