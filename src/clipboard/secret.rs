//! Deciding whether a clipboard entry is a password or token.

/// Target that KeePassXC, KDE Wallet and others offer alongside a copied password.
pub const PASSWORD_MANAGER_HINT: &str = "x-kde-passwordManagerHint";

/// Prefixes of well-known API token formats.
const TOKEN_PREFIXES: &[&str] = &[
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "sk-",
    "xoxb-",
    "xoxp-",
    "xapp-",
    "AKIA",
    "ASIA",
    "AIza",
    "npm_",
    "pypi-",
];

/// True if `targets` (the clipboard's offered formats) mark the content as a password.
pub fn has_password_hint(targets: &[String]) -> bool {
    targets.iter().any(|target| target == PASSWORD_MANAGER_HINT)
}

/// Heuristic: private keys, known token formats, JWTs and short random-looking strings
/// that mix at least three character classes. A false positive only means the entry is
/// dropped after the secret TTL, so this errs on the side of caution.
pub fn looks_secret(text: &str) -> bool {
    let text = text.trim();
    if text.contains("PRIVATE KEY-----") {
        return true;
    }
    if text.is_empty() || text.contains(char::is_whitespace) {
        return false;
    }

    let len = text.chars().count();
    if len >= 16 && TOKEN_PREFIXES.iter().any(|prefix| text.starts_with(prefix)) {
        return true;
    }
    if text.starts_with("eyJ") && text.matches('.').count() == 2 && len >= 32 {
        return true;
    }

    if !(8..=64).contains(&len) || is_url_or_path(text) || is_email(text) || is_hex_id(text) {
        return false;
    }

    let lower = text.chars().any(|c| c.is_lowercase());
    let upper = text.chars().any(|c| c.is_uppercase());
    let digit = text.chars().any(|c| c.is_ascii_digit());
    let symbol = text.chars().any(|c| !c.is_alphanumeric());
    [lower, upper, digit, symbol]
        .into_iter()
        .filter(|&b| b)
        .count()
        >= 3
}

fn is_url_or_path(text: &str) -> bool {
    text.contains("://") || text.starts_with(['/', '~', '.', '#', '$', '-'])
}

fn is_email(text: &str) -> bool {
    let Some((user, domain)) = text.split_once('@') else {
        return false;
    };
    !user.is_empty()
        && domain.contains('.')
        && text
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '@' | '.' | '_' | '-' | '+'))
}

/// Hashes, UUIDs and similar hex identifiers.
fn is_hex_id(text: &str) -> bool {
    text.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_passwords() {
        for secret in [
            "Hunter2!x",
            "c0rrect-H0rse",
            "aB3$dE6^gH9*",
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "AKIAIOSFODNN7EXAMPLE",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.abcdefghijk",
            "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----",
        ] {
            assert!(looks_secret(secret), "{secret:?} should be a secret");
        }
    }

    #[test]
    fn ignores_ordinary_text() {
        for text in [
            "hello world",
            "password",
            "Hello, World! 123",
            "https://Example.com/Path?a=1",
            "/home/user/File_1.txt",
            "John.Doe1@example.com",
            "550e8400-e29b-41d4-a716-446655440000",
            "3f786850e387550fdab836ed7e6dc881de23001b",
            "2026-11-09",
            "fn main() {}",
            "",
        ] {
            assert!(!looks_secret(text), "{text:?} should not be a secret");
        }
    }

    #[test]
    fn detects_hint() {
        let targets = vec!["UTF8_STRING".to_string(), PASSWORD_MANAGER_HINT.to_string()];
        assert!(has_password_hint(&targets));
        assert!(!has_password_hint(&targets[..1]));
    }
}
