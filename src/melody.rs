//! Alarm melodies as data. Pure code: pitches, lengths and tempo; the
//! buzzer turns them into sound.
//!
//! A note's length is in sixteenths of a whole note, so a quarter is 4, a
//! half 8, an eighth 2, a dotted quarter 6. With the tempo in quarter
//! notes per minute, `Note::ms` gives the duration. Rests are notes at
//! 0 Hz, written out so the rhythm is on the page and not in the player.

/// One note, or a rest at 0 Hz.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Note {
    pub hz: u32,
    /// Length in sixteenths.
    pub len: u8,
}

impl Note {
    /// Duration at `bpm` quarter notes per minute.
    pub const fn ms(&self, bpm: u16) -> u32 {
        self.len as u32 * 15_000 / bpm as u32
    }

    /// How long the tone sounds: the note minus an articulation gap of an
    /// eighth of its length, clamped to 15 to 60 ms, so a repeated pitch
    /// is heard as two notes. Rests sound for nothing.
    pub const fn sounding_ms(&self, bpm: u16) -> u32 {
        if self.hz == 0 {
            return 0;
        }
        let ms = self.ms(bpm);
        let mut gap = ms / 8;
        if gap < 15 {
            gap = 15;
        }
        if gap > 60 {
            gap = 60;
        }
        ms.saturating_sub(gap)
    }
}

/// A tune for the alarm.
#[derive(Clone, Copy, Debug)]
pub struct Melody {
    /// Name on the API.
    pub name: &'static str,
    /// Label on the page.
    pub label: &'static str,
    /// Quarter notes per minute.
    pub bpm: u16,
    pub notes: &'static [Note],
    /// Silence before the melody repeats while ringing.
    pub repeat_gap_ms: u16,
}

impl Melody {
    /// Length of one pass, without the repeat gap.
    pub const fn duration_ms(&self) -> u32 {
        let mut total = 0;
        let mut i = 0;
        while i < self.notes.len() {
            total += self.notes[i].ms(self.bpm);
            i += 1;
        }
        total
    }
}

// Equal-tempered pitches, A4 = 440 Hz. The buzzer resonates near 2 kHz,
// so pitches from about A6 up come out much louder than the rest; the
// tunes stay at or below E6 (1319 Hz) to sound even.
pub const REST: u32 = 0;
pub const D5: u32 = 587;
pub const G5: u32 = 784;
pub const A5: u32 = 880;
pub const B5: u32 = 988;
pub const C6: u32 = 1047;
pub const D6: u32 = 1175;
pub const E6: u32 = 1319;
pub const F6: u32 = 1397;
pub const G6: u32 = 1568;

/// The tone of the plain beeps, near the buzzer's resonance.
pub const BEEP_HZ: u32 = 2500;

const fn n(hz: u32, len: u8) -> Note {
    Note { hz, len }
}

// Lengths in sixteenths.
const Q: u8 = 4;
const H: u8 = 8;
const E: u8 = 2;
const DQ: u8 = 6;

/// The M4 alarm: four 100 ms tones 100 ms apart. At 150 bpm a sixteenth
/// is 100 ms.
pub const BEEPS: Melody = Melody {
    name: "beeps",
    label: "Beeps",
    bpm: 150,
    notes: &[
        n(BEEP_HZ, 1), n(REST, 1), n(BEEP_HZ, 1), n(REST, 1),
        n(BEEP_HZ, 1), n(REST, 1), n(BEEP_HZ, 1),
    ],
    repeat_gap_ms: 800,
};

/// Twinkle Twinkle Little Star, the whole verse, in G so the top note is
/// E6 and clear of the resonance. Traditional.
pub const TWINKLE: Melody = Melody {
    name: "twinkle",
    label: "Twinkle Twinkle Little Star",
    bpm: 120,
    notes: &[
        n(G5, Q), n(G5, Q), n(D6, Q), n(D6, Q), n(E6, Q), n(E6, Q), n(D6, H),
        n(C6, Q), n(C6, Q), n(B5, Q), n(B5, Q), n(A5, Q), n(A5, Q), n(G5, H),
        n(D6, Q), n(D6, Q), n(C6, Q), n(C6, Q), n(B5, Q), n(B5, Q), n(A5, H),
        n(D6, Q), n(D6, Q), n(C6, Q), n(C6, Q), n(B5, Q), n(B5, Q), n(A5, H),
        n(G5, Q), n(G5, Q), n(D6, Q), n(D6, Q), n(E6, Q), n(E6, Q), n(D6, H),
        n(C6, Q), n(C6, Q), n(B5, Q), n(B5, Q), n(A5, Q), n(A5, Q), n(G5, H),
    ],
    repeat_gap_ms: 2000,
};

/// Ode to Joy, the first two phrases of the theme. Beethoven, 1824.
pub const ODE_TO_JOY: Melody = Melody {
    name: "ode",
    label: "Ode to Joy",
    bpm: 120,
    notes: &[
        n(E6, Q), n(E6, Q), n(F6, Q), n(G6, Q), n(G6, Q), n(F6, Q), n(E6, Q), n(D6, Q),
        n(C6, Q), n(C6, Q), n(D6, Q), n(E6, Q), n(E6, DQ), n(D6, E), n(D6, H),
        n(E6, Q), n(E6, Q), n(F6, Q), n(G6, Q), n(G6, Q), n(F6, Q), n(E6, Q), n(D6, Q),
        n(C6, Q), n(C6, Q), n(D6, Q), n(E6, Q), n(D6, DQ), n(C6, E), n(C6, H),
    ],
    repeat_gap_ms: 2000,
};

/// Frère Jacques, one round, in G for the same reason; the "ding dang
/// dong" drops to D5. Traditional.
pub const FRERE_JACQUES: Melody = Melody {
    name: "frere",
    label: "Frère Jacques",
    bpm: 120,
    notes: &[
        n(G5, Q), n(A5, Q), n(B5, Q), n(G5, Q),
        n(G5, Q), n(A5, Q), n(B5, Q), n(G5, Q),
        n(B5, Q), n(C6, Q), n(D6, H),
        n(B5, Q), n(C6, Q), n(D6, H),
        n(D6, E), n(E6, E), n(D6, E), n(C6, E), n(B5, Q), n(G5, Q),
        n(D6, E), n(E6, E), n(D6, E), n(C6, E), n(B5, Q), n(G5, Q),
        n(G5, Q), n(D5, Q), n(G5, H),
        n(G5, Q), n(D5, Q), n(G5, H),
    ],
    repeat_gap_ms: 2000,
};

/// Every melody, in the order the page lists them. The settings index
/// points into this table; `beeps` first so the default is index 0.
pub const MELODIES: [Melody; 4] = [BEEPS, TWINKLE, ODE_TO_JOY, FRERE_JACQUES];

/// The melody at a settings index, falling back to the beeps.
pub fn get(index: usize) -> &'static Melody {
    MELODIES.get(index).unwrap_or(&MELODIES[0])
}

/// The index of a melody by API name.
pub fn index_of(name: &str) -> Option<usize> {
    MELODIES.iter().position(|m| m.name == name)
}
