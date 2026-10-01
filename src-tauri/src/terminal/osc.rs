/// Events extracted from OSC sequences in raw PTY output.
/// These are maiTerm-specific features that alacritty_terminal doesn't handle natively.
#[derive(Debug, Clone)]
pub enum OscEvent {
    /// OSC 7: CWD report — file://host/path
    Cwd { cwd: String, host: Option<String> },
    /// OSC 133/633: Shell integration (FinalTerm/VS Code)
    ShellIntegration { cmd: char, exit_code: Option<i32> },
    /// OSC 9: Notification request (non-protocol text only)
    Notification { message: String },
    /// OSC 1337: iTerm2 CurrentDir
    CurrentDir { cwd: String },
    /// OSC 1337 `MaitermLineProbe=<pid>`: maiTerm's zsh integration, announcing that this shell
    /// answers a command-line probe (`probe_shell_line`).
    LineProbe { pid: u32 },
    /// OSC 1337 `MaitermLine=<pid>;<len>`: the answer — how many characters are on the shell's
    /// command line right now.
    LineReport { pid: u32, len: u32 },
}

/// Lightweight state machine that scans raw PTY bytes for OSC sequences.
/// Bytes pass through unchanged — alacritty_terminal still sees everything.
pub struct OscInterceptor {
    /// Accumulates bytes when inside an OSC sequence
    osc_buffer: Vec<u8>,
    /// True when we're between ESC] and ST/BEL
    in_osc: bool,
    /// True when we just saw ESC (waiting for ] or \)
    saw_esc: bool,
}

impl OscInterceptor {
    pub fn new() -> Self {
        Self {
            osc_buffer: Vec::with_capacity(256),
            in_osc: false,
            saw_esc: false,
        }
    }

    /// Scan raw bytes, extract OSC events. Returns structured events.
    pub fn process(&mut self, data: &[u8]) -> Vec<OscEvent> {
        let mut events = Vec::new();

        for &byte in data {
            if self.saw_esc {
                self.saw_esc = false;
                if byte == b']' {
                    // ESC ] — start of OSC sequence
                    self.in_osc = true;
                    self.osc_buffer.clear();
                    continue;
                } else if byte == b'\\' && self.in_osc {
                    // ESC \ — String Terminator (ST), end of OSC
                    self.in_osc = false;
                    if let Some(event) = self.parse_osc() {
                        events.push(event);
                    }
                    continue;
                }
                // Not an OSC-related ESC sequence
                continue;
            }

            if byte == 0x1b {
                // ESC
                self.saw_esc = true;
                continue;
            }

            if self.in_osc {
                if byte == 0x07 {
                    // BEL — also terminates OSC
                    self.in_osc = false;
                    if let Some(event) = self.parse_osc() {
                        events.push(event);
                    }
                } else {
                    self.osc_buffer.push(byte);
                    // Safety: don't accumulate forever on malformed input
                    if self.osc_buffer.len() > 4096 {
                        self.in_osc = false;
                        self.osc_buffer.clear();
                    }
                }
            }
        }

        events
    }

    /// Parse the accumulated OSC buffer into an event.
    fn parse_osc(&mut self) -> Option<OscEvent> {
        let payload = String::from_utf8_lossy(&self.osc_buffer).to_string();

        // Split on first ';' to get OSC code
        let (code_str, data) = match payload.find(';') {
            Some(pos) => (&payload[..pos], &payload[pos + 1..]),
            None => (payload.as_str(), ""),
        };

        let code: u32 = code_str.parse().ok()?;

        match code {
            7 => {
                // OSC 7: file://host/path
                self.parse_osc7(data)
            }
            133 | 633 => {
                // OSC 133/633: Shell integration
                self.parse_osc133(data)
            }
            9 => {
                // OSC 9: Notification
                // Skip payloads that are only digits/semicolons (Claude Code protocol data)
                if data.bytes().all(|b| b.is_ascii_digit() || b == b';') {
                    return None;
                }
                if data.is_empty() {
                    return None;
                }
                Some(OscEvent::Notification {
                    message: data.to_string(),
                })
            }
            1337 => {
                // OSC 1337: iTerm2 extensions — CurrentDir, and maiTerm's own line probe
                if let Some(cwd) = data.strip_prefix("CurrentDir=") {
                    if !cwd.is_empty() {
                        return Some(OscEvent::CurrentDir {
                            cwd: cwd.to_string(),
                        });
                    }
                }
                if let Some(pid) = data.strip_prefix("MaitermLineProbe=") {
                    return pid.trim().parse().ok().map(|pid| OscEvent::LineProbe { pid });
                }
                if let Some(rest) = data.strip_prefix("MaitermLine=") {
                    let (pid, len) = rest.split_once(';')?;
                    return Some(OscEvent::LineReport { pid: pid.trim().parse().ok()?, len: len.trim().parse().ok()? });
                }
                None
            }
            _ => None,
        }
    }

    fn parse_osc7(&self, data: &str) -> Option<OscEvent> {
        // Parse file://host/path format
        if let Some(rest) = data.strip_prefix("file://") {
            let (host, path) = if let Some(slash_pos) = rest.find('/') {
                let h = &rest[..slash_pos];
                let p = &rest[slash_pos..];
                (
                    if h.is_empty() { None } else { Some(h.to_string()) },
                    percent_decode(p),
                )
            } else {
                (None, String::new())
            };
            if !path.is_empty() {
                return Some(OscEvent::Cwd { cwd: path, host });
            }
        }
        None
    }

    fn parse_osc133(&self, data: &str) -> Option<OscEvent> {
        let parts: Vec<&str> = data.split(';').collect();
        let cmd_str = parts.first()?;
        let cmd = cmd_str.chars().next()?;

        match cmd {
            'A' | 'B' | 'C' => Some(OscEvent::ShellIntegration {
                cmd,
                exit_code: None,
            }),
            'D' => {
                let exit_code = parts.get(1).and_then(|s| s.parse().ok());
                Some(OscEvent::ShellIntegration { cmd, exit_code })
            }
            _ => None,
        }
    }
}

/// Simple percent-decoding for file:// URLs
fn percent_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next();
            let lo = chars.next();
            if let (Some(h), Some(l)) = (hi, lo) {
                let hex = [h, l];
                if let Ok(s) = std::str::from_utf8(&hex) {
                    if let Ok(val) = u8::from_str_radix(s, 16) {
                        result.push(val as char);
                        continue;
                    }
                }
            }
            // Malformed percent encoding — pass through
            result.push('%');
        } else {
            result.push(b as char);
        }
    }
    result
}

#[cfg(test)]
mod line_probe_tests {
    use super::*;

    #[test]
    fn the_zsh_line_probe_announcement_and_answer_parse() {
        let mut i = OscInterceptor::new();
        let ev = i.process(b"\x1b]1337;MaitermLineProbe=4242\x07\x1b]1337;MaitermLine=4242;18\x07");
        assert!(matches!(ev[0], OscEvent::LineProbe { pid: 4242 }));
        assert!(matches!(ev[1], OscEvent::LineReport { pid: 4242, len: 18 }));
        // Malformed answers are not answers — never read as an empty line.
        assert!(i.process(b"\x1b]1337;MaitermLine=4242\x07").is_empty());
        assert!(i.process(b"\x1b]1337;MaitermLine=x;0\x07").is_empty());
    }
}
