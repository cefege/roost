//! Shared validation of the pi-env daemon's fixed serve arguments.

use crate::ProtocolError;

pub fn validated_daemon_args(args: &[String]) -> Result<[String; 3], ProtocolError> {
    let valid = args.len() == 3 && args[0] == "serve" && args[1] == "--token"
        && args[2].len() == 32
        && args[2].bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !valid { return Err(ProtocolError::new("args", "must be serve --token followed by 32 lowercase hexadecimal characters")); }
    Ok([args[0].clone(), args[1].clone(), args[2].clone()])
}

#[cfg(test)]
mod tests {
    use super::validated_daemon_args;

    #[test]
    fn accepts_exact_daemon_arguments() {
        let args = ["serve", "--token", "0123456789abcdef0123456789abcdef"].map(str::to_owned);
        assert!(validated_daemon_args(&args).is_ok());
    }

    #[test]
    fn rejects_short_uppercase_and_extra_tokens() {
        for token in ["0123456789abcdef0123456789abcde", "0123456789abcdef0123456789abcdeF"] {
            let args = ["serve".to_owned(), "--token".to_owned(), token.to_owned()];
            assert!(validated_daemon_args(&args).is_err());
        }
        let args = ["serve", "--token", "0123456789abcdef0123456789abcdef", "extra"].map(str::to_owned);
        assert!(validated_daemon_args(&args).is_err());
    }
}
