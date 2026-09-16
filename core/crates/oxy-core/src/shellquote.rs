//! POSIX single-quoting, the way `Util.shellQuote` did it: wrap in `'…'` and
//! write an embedded quote as `'\''`. The result is one shell word, always.

pub fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain() {
        assert_eq!(quote("hello"), "'hello'");
        assert_eq!(quote(""), "''");
    }

    #[test]
    fn embedded_quote() {
        assert_eq!(quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn hostile() {
        assert_eq!(quote("$(rm -rf ~)"), "'$(rm -rf ~)'");
    }
}
