//! An ssh command as maiTerm stores and passes it: one string of shell words.
//!
//! A tab's ssh command is read off the process table, stored on the tab, typed back into the
//! user's shell by `buildSshCommand`, and handed to Rust as `ssh_args` for every ssh maiTerm makes
//! for itself (the bridge tunnel, Files' helper, the mirror, watch scripts, setup). Until
//! 2026-11-15 every one of those split the string on whitespace, and the process table joined
//! argv with spaces, so `ssh -i "/Users/me/My Keys/id" ews@nova` became the words
//! `-i /Users/me/My Keys/id ews@nova` and every maiTerm-made ssh connected to the host `Keys/id`.
//!
//! The string is now shell words: `join` quotes a word only when a whitespace split (or the
//! user's shell, when the command is typed back) would change it, and `split` reads quotes back.
//! **Never split a stored command on whitespace again: go through `split`.**
//!
//! Compatible both ways with what was stored before:
//! - a word with nothing special stays bare (`-o Port=2222`, `~/.ssh/k`, `ews@nova`), so every
//!   key, `due.host` and approval spelled before this still compares equal;
//! - an old string that a quote char leaves unbalanced (a lossy `ps` line naming `Bob's Keys`)
//!   is read the old way, on whitespace, rather than refused;
//! - a backslash outside quotes escapes only whitespace and quote chars, so a Windows path
//!   stored bare (`C:\Users\me\.ssh\id`) still reads as itself.
//!
//! The TypeScript twin is `src/lib/utils/shellWords.ts`; the two must read and write alike,
//! since a key built on either side is compared with one built on the other.

use std::borrow::Cow;

/// The words of a stored or typed ssh command. Never fails: a string with an unbalanced quote
/// is read on whitespace, as everything was before quoting (`parse` says which happened).
pub fn split(s: &str) -> Vec<String> {
    parse(s).unwrap_or_else(|| s.split_whitespace().map(str::to_string).collect())
}

/// The shell-words reading of `s`, or None when a quote is left open.
pub fn parse(s: &str) -> Option<Vec<String>> {
    #[derive(PartialEq)]
    enum St {
        Between,
        Word,
        Single,
        Double,
    }
    let mut words = Vec::new();
    let mut word = String::new();
    let mut st = St::Between;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match st {
            St::Between | St::Word => match c {
                ' ' | '\t' | '\n' | '\r' => {
                    if st == St::Word {
                        words.push(std::mem::take(&mut word));
                        st = St::Between;
                    }
                }
                '\'' => st = St::Single,
                '"' => st = St::Double,
                '\\' => {
                    st = St::Word;
                    match chars.peek().copied() {
                        Some(n @ (' ' | '\t' | '\'' | '"')) => {
                            word.push(n);
                            chars.next();
                        }
                        // A line continuation.
                        Some('\n') => {
                            chars.next();
                        }
                        _ => word.push('\\'),
                    }
                }
                c => {
                    word.push(c);
                    st = St::Word;
                }
            },
            St::Single => match c {
                '\'' => st = St::Word,
                c => word.push(c),
            },
            St::Double => match c {
                '"' => st = St::Word,
                '\\' => match chars.peek().copied() {
                    Some(n @ ('$' | '`' | '"' | '\\')) => {
                        word.push(n);
                        chars.next();
                    }
                    Some('\n') => {
                        chars.next();
                    }
                    _ => word.push('\\'),
                },
                c => word.push(c),
            },
        }
    }
    match st {
        St::Single | St::Double => None,
        St::Word => {
            words.push(word);
            Some(words)
        }
        St::Between => Some(words),
    }
}

/// Characters a word may hold and still go bare: they mean nothing to `parse`, and nothing to a
/// shell in the places ssh args put them. `=`, `~`, `%`, `,` and `:` are here so `-o Port=2222`,
/// `~/.ssh/k` and `-W %h:%p` keep the spelling they always had.
fn is_bare(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '@' | '%' | '+' | '=' | ',' | '~')
}

/// One word, quoted only if it needs it: `'…'`, with a `'` inside written `'\''`.
pub fn quote(w: &str) -> Cow<'_, str> {
    if !w.is_empty() && w.chars().all(is_bare) {
        return Cow::Borrowed(w);
    }
    Cow::Owned(format!("'{}'", w.replace('\'', r"'\''")))
}

/// Words joined so `split` reads them back exactly, and so does a POSIX shell.
pub fn join<I, S>(words: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    words.into_iter().map(|w| quote(w.as_ref()).into_owned()).collect::<Vec<_>>().join(" ")
}

/// Words that a whitespace split mangles, and some that must stay bare.
#[cfg(test)]
pub(crate) const HARD_WORDS: [&str; 10] = [
    "/Users/me/My Keys/id",
    "/Users/me/Bob's \"Keys\"/id",
    "/Users/me/Clés privées/id_ed25519",
    "/Users/me/鍵/id",
    "IdentityFile=/Users/me/a b=c/id",
    "ProxyCommand=ssh -W %h:%p bastion",
    "two  spaces\tand a tab",
    "C:\\Users\\Me\\My Keys\\id",
    "$HOME/`x`/\\",
    "",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_hard_word_round_trips() {
        for w in HARD_WORDS {
            let joined = join(["-i", w, "ews@nova"]);
            assert_eq!(split(&joined), ["-i", w, "ews@nova"], "{joined}");
        }
        let all = join(HARD_WORDS);
        assert_eq!(split(&all), HARD_WORDS, "{all}");
    }

    /// The spelling both sides must give: the same table is asserted in `shellWords.test.ts`,
    /// since a key built in TypeScript is compared with one built here.
    #[test]
    fn spells_every_word_as_typescript_does() {
        let table: [(&[&str], &str); 6] = [
            (&["-i", "/Users/me/My Keys/id", "ews@nova"], "-i '/Users/me/My Keys/id' ews@nova"),
            (&["/Users/me/Bob's \"Keys\"/id"], r#"'/Users/me/Bob'\''s "Keys"/id'"#),
            (&["-o", "Port=2222", "-i", "~/.ssh/k", "ews@nova"], "-o Port=2222 -i ~/.ssh/k ews@nova"),
            (&["/Users/me/Clés/id", "-W", "%h:%p"], "/Users/me/Clés/id -W %h:%p"),
            (&["C:\\Users\\me\\id", ""], r"'C:\Users\me\id' ''"),
            (&["a#b", "$x", "ews@[::1]"], "'a#b' '$x' 'ews@[::1]'"),
        ];
        for (words, joined) in table {
            assert_eq!(join(words), joined);
            assert_eq!(split(joined), words);
        }
    }

    #[test]
    fn plain_words_keep_their_old_spelling() {
        // Keys, `due.host` and approvals spelled before quoting must still compare equal.
        for s in ["-o Port=2222 ews@nova", "-p 2222 -i ~/.ssh/k -J bastion ews@nova", "-x -C ews@nova", "-W %h:%p", "-oUser=ews nova"] {
            assert_eq!(join(split(s)), s);
        }
        assert_eq!(join(["/Users/me/Clés/id"]), "/Users/me/Clés/id", "a letter is a letter in any script");
    }

    #[test]
    fn what_a_human_typed_reads_as_their_shell_reads_it() {
        assert_eq!(split(r#"-i "/Users/me/My Keys/id" ews@nova"#), ["-i", "/Users/me/My Keys/id", "ews@nova"]);
        assert_eq!(split(r"-i /Users/me/My\ Keys/id ews@nova"), ["-i", "/Users/me/My Keys/id", "ews@nova"]);
        assert_eq!(split(r#"-o "ProxyCommand=ssh -W %h:%p \"b\"" h"#), ["-o", r#"ProxyCommand=ssh -W %h:%p "b""#, "h"]);
        assert_eq!(split("a'b c'd"), ["ab cd"], "adjacent quoting is one word");
        assert_eq!(split("ews@nova ''"), ["ews@nova", ""]);
    }

    #[test]
    fn old_stored_strings_still_read() {
        // A lossy ps line with an apostrophe: an open quote, so read the old way.
        assert_eq!(split("-i /Users/me/Bob's/id ews@nova"), ["-i", "/Users/me/Bob's/id", "ews@nova"]);
        assert!(parse("-i /Users/me/Bob's/id ews@nova").is_none());
        // A Windows path stored bare.
        assert_eq!(split(r"-i C:\Users\me\.ssh\id ews@nova"), ["-i", r"C:\Users\me\.ssh\id", "ews@nova"]);
        // maiTerm's own remote command as ps showed it.
        assert_eq!(split("-t ews@nova export MAITERM_TAB_ID=x; cd /a && exec $SHELL -l").len(), 10);
    }
}
