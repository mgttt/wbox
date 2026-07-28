//! Length-prefixed transport for the Linux guest environment.
//!
//! On Windows, `wbox-linux.exe` needs a small Win32 environment to start, but
//! that environment must not become the Linux guest's `envp`. The supervisor
//! therefore transports the guest environment in one internal variable.

pub const ENV_NAME: &str = "WBOX_GUEST_ENV";

pub fn encode(env: &[(String, String)]) -> String {
    let mut out = String::new();
    for (key, value) in env {
        out.push_str(&key.len().to_string());
        out.push(':');
        out.push_str(&value.len().to_string());
        out.push(':');
        out.push_str(key);
        out.push_str(value);
    }
    out
}

pub fn decode(mut payload: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    while !payload.is_empty() {
        let (key_len, rest) = take_len(payload)?;
        let (value_len, rest) = take_len(rest)?;
        let key = rest
            .get(..key_len)
            .ok_or_else(|| "guest environment key is truncated".to_string())?;
        let rest = rest
            .get(key_len..)
            .ok_or_else(|| "guest environment key ends inside UTF-8".to_string())?;
        let value = rest
            .get(..value_len)
            .ok_or_else(|| "guest environment value is truncated".to_string())?;
        payload = rest
            .get(value_len..)
            .ok_or_else(|| "guest environment value ends inside UTF-8".to_string())?;
        if key.is_empty() || key.contains('=') || key.contains('\0') || value.contains('\0') {
            return Err("guest environment contains an invalid entry".to_string());
        }
        out.push((key.to_string(), value.to_string()));
    }
    Ok(out)
}

fn take_len(input: &str) -> Result<(usize, &str), String> {
    let (digits, rest) = input
        .split_once(':')
        .ok_or_else(|| "guest environment length is missing ':'".to_string())?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("guest environment length is invalid".to_string());
    }
    let len = digits
        .parse()
        .map_err(|_| "guest environment length overflows usize".to_string())?;
    Ok((len, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_preserves_case_duplicates_and_unicode() {
        let env = vec![
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("path".to_string(), "/custom".to_string()),
            ("LANG".to_string(), "zh_CN.UTF-8".to_string()),
            ("GREETING".to_string(), "你好".to_string()),
        ];
        assert_eq!(decode(&encode(&env)).unwrap(), env);
    }

    #[test]
    fn malformed_payload_is_rejected() {
        for payload in ["x:1:ab", "1:x:ab", "2:1:ab", "1:2:ab"] {
            assert!(decode(payload).is_err(), "{payload}");
        }
    }
}
