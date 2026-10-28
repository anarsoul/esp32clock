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
// so pitches from about G6 up come out much louder than the rest; the
// tunes stay at or below E6 (1319 Hz) to sound even. The same resonance
// catches the third harmonic of the pitches below G5, which is what
// `duty_pct` is for; the tunes avoid them where the music allows.
pub const REST: u32 = 0;
pub const C5: u32 = 523;
pub const D5: u32 = 587;
pub const E5: u32 = 659;
pub const G5: u32 = 784;
pub const GS5: u32 = 831;
pub const A5: u32 = 880;
pub const B5: u32 = 988;
pub const C6: u32 = 1047;
pub const D6: u32 = 1175;
pub const DS6: u32 = 1245;
pub const E6: u32 = 1319;
pub const F6: u32 = 1397;
pub const G6: u32 = 1568;

/// The tone of the plain beeps, near the buzzer's resonance.
pub const BEEP_HZ: u32 = 2500;

/// Pitches below this get the one-third duty cycle.
pub const LOW_DUTY_BELOW_HZ: u32 = 700;

/// The nominal high time of the square wave, in percent of the period.
pub const SQUARE_PCT: u8 = 50;

/// The nominal high time for the pitches below `LOW_DUTY_BELOW_HZ`.
pub const LOW_PCT: u8 = 33;

/// The pulse duty for a pitch, in percent of the period the line is
/// high. A 50 % square wave has a third harmonic a third as strong as
/// the fundamental, and for the pitches below G5 (C5 to F5, 523 to
/// 698 Hz) it lands between 1.6 and 2.1 kHz, on the buzzer's resonance,
/// so those notes came out louder than their neighbours (heard on the
/// bench 2026-10-28). A duty of one third has no third harmonic at all
/// and keeps 87 % of the fundamental, so it is used below
/// `LOW_DUTY_BELOW_HZ`; everything else keeps the square wave, which
/// a sweep by ear from 25 to 75 % confirmed as the cleanest: a few
/// points either side added a second harmonic the ear picked up, so
/// the transistor's turn-off is not stretching the pulse.
pub const fn duty_pct(hz: u32) -> u8 {
    if hz < LOW_DUTY_BELOW_HZ { LOW_PCT } else { SQUARE_PCT }
}

const fn n(hz: u32, len: u8) -> Note {
    Note { hz, len }
}

// Lengths in sixteenths.
const S: u8 = 1;
const E: u8 = 2;
const DE: u8 = 3;
const Q: u8 = 4;
const DQ: u8 = 6;
const H: u8 = 8;
const W: u8 = 16;

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

/// Ode to Joy, the first two phrases of the theme, in G so it spans
/// G5 to D6 like the others; in C its top note G6 rang out louder than
/// the rest. Beethoven, 1824.
pub const ODE_TO_JOY: Melody = Melody {
    name: "ode",
    label: "Ode to Joy",
    bpm: 120,
    notes: &[
        n(B5, Q), n(B5, Q), n(C6, Q), n(D6, Q), n(D6, Q), n(C6, Q), n(B5, Q), n(A5, Q),
        n(G5, Q), n(G5, Q), n(A5, Q), n(B5, Q), n(B5, DQ), n(A5, E), n(A5, H),
        n(B5, Q), n(B5, Q), n(C6, Q), n(D6, Q), n(D6, Q), n(C6, Q), n(B5, Q), n(A5, Q),
        n(G5, Q), n(G5, Q), n(A5, Q), n(B5, Q), n(A5, DQ), n(G5, E), n(G5, H),
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

/// Morning Mood from Peer Gynt, the first four bars of the theme, in
/// 6/8: sol mi re do re mi; the same with a turn of sixteenths on the
/// way back up; sol mi sol la mi la; sol mi re and do held. From the
/// score the user supplied (an arrangement in C, 2026-10-28); the first
/// cut had the figure a third too low. In G, so do is G5 and the tune
/// uses G5, A5, B5, D6 and E6. Grieg, 1875.
pub const MORNING_MOOD: Melody = Melody {
    name: "morning",
    label: "Morning Mood",
    bpm: 75,
    notes: &[
        n(D6, E), n(B5, E), n(A5, E), n(G5, E), n(A5, E), n(B5, E),
        n(D6, E), n(B5, E), n(A5, E), n(G5, E), n(A5, S), n(B5, S), n(A5, S), n(B5, S),
        n(D6, E), n(B5, E), n(D6, E), n(E6, E), n(B5, E), n(E6, E),
        n(D6, E), n(B5, E), n(A5, E), n(G5, DQ),
    ],
    repeat_gap_ms: 2000,
};

/// The Westminster Quarters, the four changes rung on the hour, each
/// four notes and a pause. In C, so the low note of the changes is G5:
/// G5, C6, D6 and E6. Traditional, Cambridge, 1793.
pub const WESTMINSTER: Melody = Melody {
    name: "westminster",
    label: "Westminster Quarters",
    bpm: 72,
    notes: &[
        n(C6, Q), n(E6, Q), n(D6, Q), n(G5, H), n(REST, Q),
        n(C6, Q), n(D6, Q), n(E6, Q), n(C6, H), n(REST, Q),
        n(E6, Q), n(C6, Q), n(D6, Q), n(G5, H), n(REST, Q),
        n(G5, Q), n(D6, Q), n(E6, Q), n(C6, H),
    ],
    repeat_gap_ms: 2000,
};

/// Reveille, the first strain of the bugle call, played twice as it is
/// in the call: a pickup and the dotted figure on the bugle's notes,
/// which in C are G5, C6 and E6. Traditional, US Army.
pub const REVEILLE: Melody = Melody {
    name: "reveille",
    label: "Reveille",
    bpm: 120,
    notes: &[
        n(G5, E),
        n(C6, DE), n(E6, S), n(C6, E), n(G5, E),
        n(C6, DE), n(E6, S), n(C6, E), n(G5, E),
        n(C6, DE), n(E6, S), n(C6, E), n(E6, E),
        n(C6, Q), n(REST, E), n(G5, E),
        n(C6, DE), n(E6, S), n(C6, E), n(G5, E),
        n(C6, DE), n(E6, S), n(C6, E), n(G5, E),
        n(C6, DE), n(E6, S), n(C6, E), n(E6, E),
        n(C6, Q), n(REST, Q),
    ],
    repeat_gap_ms: 2000,
};

/// Für Elise, the first eight bars of the melody in 3/8, with the left
/// hand's arpeggios where the right hand rests. Kept at pitch: the
/// phrase lives on E6 and D#6 and cannot go up, so the arpeggios reach
/// down to C5 and E5, the only notes below G5 any tune still uses;
/// `duty_pct` keeps them from ringing out. At 75 bpm a sixteenth is
/// 200 ms, five notes a second, the usual pace; at 100 the sixteenths
/// were blips and the eighths between them stood out. Beethoven, 1810.
pub const FUR_ELISE: Melody = Melody {
    name: "elise",
    label: "Für Elise",
    bpm: 75,
    notes: &[
        n(E6, S), n(DS6, S),
        n(E6, S), n(DS6, S), n(E6, S), n(B5, S), n(D6, S), n(C6, S),
        n(A5, E), n(REST, S), n(C5, S), n(E5, S), n(A5, S),
        n(B5, E), n(REST, S), n(E5, S), n(GS5, S), n(B5, S),
        n(C6, E), n(REST, S), n(E5, S), n(E6, S), n(DS6, S),
        n(E6, S), n(DS6, S), n(E6, S), n(B5, S), n(D6, S), n(C6, S),
        n(A5, E), n(REST, S), n(C5, S), n(E5, S), n(A5, S),
        n(B5, E), n(REST, S), n(E5, S), n(C6, S), n(B5, S),
        n(A5, DQ),
    ],
    repeat_gap_ms: 2000,
};

/// Jingle Bells, the chorus with both endings, in G so it spans G5 to
/// D6, at a sleigh's pace. Pierpont, 1857.
pub const JINGLE_BELLS: Melody = Melody {
    name: "jingle",
    label: "Jingle Bells",
    bpm: 160,
    notes: &[
        n(B5, Q), n(B5, Q), n(B5, H),
        n(B5, Q), n(B5, Q), n(B5, H),
        n(B5, Q), n(D6, Q), n(G5, DQ), n(A5, E),
        n(B5, W),
        n(C6, Q), n(C6, Q), n(C6, Q), n(C6, Q),
        n(C6, Q), n(B5, Q), n(B5, Q), n(B5, E), n(B5, E),
        n(B5, Q), n(A5, Q), n(A5, Q), n(B5, Q),
        n(A5, H), n(D6, H),
        n(B5, Q), n(B5, Q), n(B5, H),
        n(B5, Q), n(B5, Q), n(B5, H),
        n(B5, Q), n(D6, Q), n(G5, DQ), n(A5, E),
        n(B5, W),
        n(C6, Q), n(C6, Q), n(C6, Q), n(C6, Q),
        n(C6, Q), n(B5, Q), n(B5, Q), n(B5, E), n(B5, E),
        n(D6, Q), n(D6, Q), n(C6, Q), n(A5, Q),
        n(G5, W),
    ],
    repeat_gap_ms: 2000,
};

/// Every melody, in the order the page lists them. The settings index
/// points into this table; `beeps` first so the default is index 0,
/// and new tunes go at the end so stored indexes keep their meaning.
pub const MELODIES: [Melody; 9] = [
    BEEPS,
    TWINKLE,
    ODE_TO_JOY,
    FRERE_JACQUES,
    MORNING_MOOD,
    WESTMINSTER,
    REVEILLE,
    FUR_ELISE,
    JINGLE_BELLS,
];

/// The setting value for "a different tune each time": one past the
/// table. `pick` turns it into a tune when the buzzer starts.
pub const RANDOM: usize = MELODIES.len();

/// The API name of `RANDOM`.
pub const RANDOM_NAME: &str = "random";

/// The melody at a settings index, falling back to the beeps.
pub fn get(index: usize) -> &'static Melody {
    MELODIES.get(index).unwrap_or(&MELODIES[0])
}

/// The API name of a setting value: a melody's name, or `random`.
pub fn name(index: usize) -> &'static str {
    if index == RANDOM { RANDOM_NAME } else { get(index).name }
}

/// The setting value for an API name: a melody's index, or `RANDOM`.
pub fn index_of(name: &str) -> Option<usize> {
    if name == RANDOM_NAME {
        return Some(RANDOM);
    }
    MELODIES.iter().position(|m| m.name == name)
}

/// The melody to play for a setting value. A melody's index is itself.
/// `RANDOM` is one of the tunes, every melody but the beeps at index 0,
/// chosen by `random`, and not `avoid` (the last pick) when there is
/// another: the next tune round is taken instead.
pub fn pick(index: usize, random: u32, avoid: Option<usize>) -> usize {
    if index != RANDOM {
        return index;
    }
    let tunes = MELODIES.len() - 1;
    let mut i = 1 + random as usize % tunes;
    if Some(i) == avoid {
        i = 1 + i % tunes;
    }
    i
}
