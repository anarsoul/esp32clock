//! What the two front buttons report. The debouncing task that reads the
//! pins is in the firmware; these are the events it sends.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    /// S1, the upper button.
    Top,
    /// S2, the lower button.
    Bottom,
    /// Both held together. Only ever `Long`.
    Both,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Released before the long-press threshold.
    Short,
    /// Held past the threshold. Emitted while still held.
    Long,
    /// Both buttons held for the hold time, long after their `Long`.
    /// Only ever for `Both`, once per hold.
    Hold,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Event {
    pub button: Button,
    pub kind: Kind,
}
