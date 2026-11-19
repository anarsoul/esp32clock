// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! One argument of the command: `file[:option,…][=Label]`, an option
//! being `key` or `key=value`.

/// An argument taken apart.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Spec {
    pub file: String,
    pub options: Vec<(String, String)>,
    pub label: Option<String>,
}

/// The options that are there or not; every other takes a value.
pub const FLAGS: [&str; 6] = ["loop", "once", "restart", "flat", "limiter", "plain"];

/// `:key[=value][,key[=value]…]` at the head of `rest`, and what
/// follows it. A key is lower-case letters and `-`; a value has no `,`
/// and no `=`. A flag has no value, so an `=` after one is the
/// label's: `tune.pcm:loop=My Tune`.
fn options(rest: &str) -> Option<(Vec<(String, String)>, &str)> {
    let mut rest = rest.strip_prefix(':')?;
    let mut found = Vec::new();
    loop {
        let end = rest.find(|c: char| !(c.is_ascii_lowercase() || c == '-')).unwrap_or(rest.len());
        if end == 0 {
            return None;
        }
        let key = &rest[..end];
        rest = &rest[end..];
        let mut value = "";
        if !FLAGS.contains(&key) {
            let after = rest.strip_prefix('=')?;
            let end = after.find([',', '=']).unwrap_or(after.len());
            if end == 0 {
                return None;
            }
            value = &after[..end];
            rest = &after[end..];
        }
        found.push((key.to_string(), value.to_string()));
        match rest.strip_prefix(',') {
            Some(more) => rest = more,
            None => return Some((found, rest)),
        }
    }
}

/// Options alone, as the page sends them: `key[=value][,…]`, or
/// nothing. `None` for what is not that.
pub fn parse_options(text: &str) -> Option<Vec<(String, String)>> {
    let text = text.trim();
    if text.is_empty() {
        return Some(Vec::new());
    }
    match options(&format!(":{text}"))? {
        (found, "") => Some(found),
        _ => None,
    }
}

impl Spec {
    /// The file's name is as much of the argument as it takes for the
    /// rest to be options and a label: a colon in a path
    /// (`C:\tunes\a.xm`) is not the options'.
    pub fn parse(arg: &str) -> Spec {
        for (at, c) in arg.char_indices() {
            let rest = &arg[at..];
            let (found, rest) = match c {
                ':' => match options(rest) {
                    Some(parsed) => parsed,
                    None => continue,
                },
                '=' => (Vec::new(), rest),
                _ => continue,
            };
            let label = match rest.strip_prefix('=') {
                Some(label) => Some(label.to_string()),
                None if rest.is_empty() => None,
                None => continue,
            };
            if at > 0 {
                return Spec { file: arg[..at].to_string(), options: found, label };
            }
        }
        Spec { file: arg.to_string(), options: Vec::new(), label: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(file: &str, options: &[(&str, &str)], label: Option<&str>) -> Spec {
        Spec {
            file: file.into(),
            options: options.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            label: label.map(str::to_string),
        }
    }

    #[test]
    fn an_argument_comes_apart() {
        assert_eq!(Spec::parse("clips/trantor.bpr"), spec("clips/trantor.bpr", &[], None));
        assert_eq!(Spec::parse("wav/tune.pcm:loop=My Tune"), spec("wav/tune.pcm", &[("loop", "")], Some("My Tune")));
        assert_eq!(Spec::parse("a.wav=My: Tune"), spec("a.wav", &[], Some("My: Tune")));
        assert_eq!(Spec::parse("a.sid:song=2,seconds=90"), spec("a.sid", &[("song", "2"), ("seconds", "90")], None));
        assert_eq!(Spec::parse("a.sid:song=2=Monty"), spec("a.sid", &[("song", "2")], Some("Monty")));
        assert_eq!(Spec::parse("a.sid:song=2,once=Monty on the Run"), spec("a.sid", &[("song", "2"), ("once", "")], Some("Monty on the Run")));
        assert_eq!(Spec::parse("a.pt3:machine=pentagon,restart"), spec("a.pt3", &[("machine", "pentagon"), ("restart", "")], None));
        assert_eq!(Spec::parse("a.xm:raise=1:2=Tune"), spec("a.xm", &[("raise", "1:2")], Some("Tune")));
        // Not options: the argument is a file's name, there or not.
        assert_eq!(Spec::parse("a.xm:song"), spec("a.xm:song", &[], None));
        assert_eq!(Spec::parse("a.wav:skip=1.5,seconds=20=Ring"), spec("a.wav", &[("skip", "1.5"), ("seconds", "20")], Some("Ring")));
    }

    #[test]
    fn options_alone() {
        assert_eq!(parse_options(" "), Some(Vec::new()));
        assert_eq!(parse_options("song=2,once"), Some(vec![("song".into(), "2".into()), ("once".into(), String::new())]));
        assert_eq!(parse_options("song"), None);
        assert_eq!(parse_options("once=Label"), None);
    }

    #[test]
    fn a_colon_in_a_path_is_the_paths() {
        assert_eq!(Spec::parse("C:\\tunes\\a.xm"), spec("C:\\tunes\\a.xm", &[], None));
        assert_eq!(Spec::parse("C:\\tunes\\a.xm:once"), spec("C:\\tunes\\a.xm", &[("once", "")], None));
        assert_eq!(Spec::parse("C:\\tunes\\a.xm:once=Tune"), spec("C:\\tunes\\a.xm", &[("once", "")], Some("Tune")));
        assert_eq!(Spec::parse("dir:name/a.xm"), spec("dir:name/a.xm", &[], None));
    }
}
