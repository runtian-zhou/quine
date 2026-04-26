use std::time::Duration;

pub(crate) fn parse_duration_literal(input: &str) -> Result<Duration, String> {
    if input.is_empty() {
        return Err("Duration cannot be empty".into());
    }
    let split_at = input
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| "Duration must include a unit like s, sec, min, h, or d".to_string())?;
    let (value, unit) = input.split_at(split_at);
    let amount = value
        .parse::<u64>()
        .map_err(|_| format!("Invalid duration value: {input}"))?;

    let multiplier = match unit.to_ascii_lowercase().as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => 60 * 60,
        "d" | "day" | "days" => 60 * 60 * 24,
        _ => return Err(format!("Unsupported duration unit: {unit}")),
    };

    Ok(Duration::from_secs(amount.saturating_mul(multiplier)))
}

#[cfg(test)]
mod tests {
    use super::parse_duration_literal;
    use std::time::Duration;

    #[test]
    fn parse_duration_supports_minute_aliases() {
        assert_eq!(
            parse_duration_literal("5m").unwrap(),
            Duration::from_secs(300)
        );
        assert_eq!(
            parse_duration_literal("5min").unwrap(),
            Duration::from_secs(300)
        );
        assert_eq!(
            parse_duration_literal("5minutes").unwrap(),
            Duration::from_secs(300)
        );
    }

    #[test]
    fn parse_duration_supports_case_insensitive_units() {
        assert_eq!(
            parse_duration_literal("2MIN").unwrap(),
            Duration::from_secs(120)
        );
        assert_eq!(
            parse_duration_literal("3Hours").unwrap(),
            Duration::from_secs(10_800)
        );
    }

    #[test]
    fn parse_duration_rejects_missing_or_unknown_units() {
        assert!(parse_duration_literal("5").is_err());
        assert!(parse_duration_literal("5fortnights").is_err());
    }
}
