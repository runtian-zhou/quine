use std::collections::BTreeMap;

pub(crate) fn format_session_summary<'a, I>(statuses: I) -> String
where
    I: IntoIterator<Item = &'a str>,
{
    let mut total = 0usize;
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();

    for status in statuses {
        total += 1;
        *counts.entry(status).or_default() += 1;
    }

    let mut parts = vec![format!("{total} sessions")];
    for (status, count) in counts {
        parts.push(format!("{count} {status}"));
    }
    parts.join(" · ")
}

pub(crate) fn prepend_summary(summary: &str, body: &str) -> String {
    if body.is_empty() {
        summary.to_string()
    } else {
        format!("{summary}\n\n{body}")
    }
}

#[cfg(test)]
mod tests {
    use super::{format_session_summary, prepend_summary};

    #[test]
    fn formats_empty_summary() {
        let statuses: [&str; 0] = [];
        assert_eq!(format_session_summary(statuses), "0 sessions");
    }

    #[test]
    fn formats_sorted_status_counts() {
        let statuses = ["waiting", "active", "active", "idle"];
        assert_eq!(
            format_session_summary(statuses),
            "4 sessions · 2 active · 1 idle · 1 waiting"
        );
    }

    #[test]
    fn prepends_summary_to_body() {
        assert_eq!(
            prepend_summary("2 sessions", "row1\nrow2"),
            "2 sessions\n\nrow1\nrow2"
        );
    }
}
