// Ported from pi_agent_rust b6fa9e18b, src/tools.rs. See licenses/pi_agent_rust.LICENSE.
pub(crate) fn strip_bom(s: &str) -> (&str, bool) {
    s.strip_prefix('\u{FEFF}')
        .map_or_else(|| (s, false), |stripped| (stripped, true))
}

pub(crate) fn detect_line_ending(content: &str) -> &'static str {
    let bytes = content.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        match bytes[idx] {
            b'\r' => {
                return if bytes.get(idx + 1) == Some(&b'\n') {
                    "\r\n"
                } else {
                    "\r"
                };
            }
            b'\n' => return "\n",
            _ => idx += 1,
        }
    }
    "\n"
}

pub(crate) fn normalize_to_lf(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            out.push('\n');
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}
