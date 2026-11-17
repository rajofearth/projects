//! Figures and times in the one format the app uses everywhere.

/// "today", "yesterday", "3d ago", "5w ago", "4mo ago", "2y ago"; `None` is "no commits".
pub fn days_ago(days: Option<u64>) -> String {
    match days {
        None => "no commits".into(),
        Some(0) => "today".into(),
        Some(1) => "yesterday".into(),
        Some(d) if d < 14 => format!("{d}d ago"),
        Some(d) if d < 60 => format!("{}w ago", d / 7),
        Some(d) if d < 730 => format!("{}mo ago", d / 30),
        Some(d) => format!("{}y ago", d / 365),
    }
}

/// Seconds since an event as "just now", "12m ago", "3h ago", then days.
pub fn secs_ago(secs: i64) -> String {
    match secs.max(0) {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s => days_ago(Some((s / 86_400) as u64)),
    }
}

/// 1.2B / 3.4M / 120k token counts.
pub fn count(n: u64) -> String {
    match n {
        n if n >= 1_000_000_000 => format!("{:.1}B", n as f64 / 1e9),
        n if n >= 10_000_000 => format!("{:.0}M", n as f64 / 1e6),
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n if n >= 1_000 => format!("{:.0}k", n as f64 / 1e3),
        n => n.to_string(),
    }
}

/// Dollars: cents under $100, whole dollars above.
pub fn usd(c: f64) -> String {
    if c >= 100.0 {
        format!("${:.0}", c)
    } else if c >= 0.005 || c == 0.0 {
        format!("${c:.2}")
    } else {
        "<$0.01".into()
    }
}

/// "1 project" / "3 projects".
pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_read_naturally() {
        assert_eq!(days_ago(Some(0)), "today");
        assert_eq!(days_ago(Some(3)), "3d ago");
        assert_eq!(days_ago(Some(21)), "3w ago");
        assert_eq!(days_ago(Some(90)), "3mo ago");
        assert_eq!(days_ago(None), "no commits");
        assert_eq!(secs_ago(30), "just now");
        assert_eq!(secs_ago(7200), "2h ago");
        assert_eq!(count(1_500_000), "1.5M");
        assert_eq!(usd(0.0), "$0.00");
        assert_eq!(usd(123.4), "$123");
    }
}
