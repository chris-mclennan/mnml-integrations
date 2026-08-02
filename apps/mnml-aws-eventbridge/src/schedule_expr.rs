//! Human-readable rendering of EventBridge Scheduler expressions.
//!
//! Supports the three expression kinds Scheduler accepts:
//!   - `at(<iso>)`             — one-shot
//!   - `rate(<n> <unit>)`      — recurring every n unit(s)
//!   - `cron(m h dom mon dow year)` — recurring on a cron mask
//!
//! Only common shapes get a rich translation; anything unusual
//! falls back to the raw expression so the user still sees
//! something (rather than nothing). Timezone is passed through
//! so daily/weekly rows say "at 07:00 America/New_York" instead
//! of guessing UTC.

pub fn humanize(expr: &str, tz: &str) -> String {
    let e = expr.trim();
    if let Some(inner) = strip("at(", ")", e) {
        return humanize_at(inner, tz);
    }
    if let Some(inner) = strip("rate(", ")", e) {
        return humanize_rate(inner);
    }
    if let Some(inner) = strip("cron(", ")", e) {
        return humanize_cron(inner, tz);
    }
    // Unknown — return the raw expression so the caller still has
    // something to show alongside it.
    expr.to_string()
}

fn strip<'a>(prefix: &str, suffix: &str, s: &'a str) -> Option<&'a str> {
    s.strip_prefix(prefix).and_then(|s| s.strip_suffix(suffix))
}

fn humanize_at(inner: &str, tz: &str) -> String {
    // AWS format: `at(2026-07-21T09:30:00)` (no zone in the string;
    // the schedule's own `ScheduleExpressionTimezone` supplies it).
    let zone = if tz.is_empty() { "" } else { " " };
    format!("Once on {inner}{zone}{tz}")
}

fn humanize_rate(inner: &str) -> String {
    // `rate(5 minutes)` — unit is already plural or singular.
    let parts: Vec<&str> = inner.split_whitespace().collect();
    match parts.as_slice() {
        [n, unit] => {
            if *n == "1" {
                // "rate(1 minutes)" is accepted → strip trailing s.
                let unit = unit.trim_end_matches('s');
                format!("Every {unit}")
            } else {
                format!("Every {n} {unit}")
            }
        }
        _ => format!("rate({inner})"),
    }
}

fn humanize_cron(inner: &str, tz: &str) -> String {
    // AWS Scheduler cron: `<min> <hr> <dom> <mon> <dow> <year>`
    // — six fields, with `?` meaning "no specific value" for
    // whichever of DOM / DOW isn't being constrained.
    let f: Vec<&str> = inner.split_whitespace().collect();
    if f.len() != 6 {
        return format!("cron({inner})");
    }
    let (min, hr, dom, mon, dow, _year) = (f[0], f[1], f[2], f[3], f[4], f[5]);
    let zone_suffix = if tz.is_empty() {
        String::new()
    } else {
        format!(" {tz}")
    };

    // Recognized shapes:
    //   Every day             (dom in {*,?}, mon=*, dow in {*,?})
    //   Every weekday         (dow=MON-FRI or 2-6)
    //   Every <weekday>       (dow=single named day)
    //   Every N minutes       (min="0/N" or "*/N", hr=*)
    //   Every N hours         (hr="0/N" or "*/N", min=fixed)
    //   Monthly on day N      (dom=<n>, dow=?)
    // Fallback: raw cron.

    // Every N minutes / hours.
    if let Some(n) = every_interval(min)
        && hr == "*" && (dom == "*" || dom == "?") && (dow == "*" || dow == "?") {
            return if n == 1 {
                "Every minute".to_string()
            } else {
                format!("Every {n} minutes")
            };
        }
    if let Some(n) = every_interval(hr)
        && let Some(m) = single_value(min)
            && (dom == "*" || dom == "?") && (dow == "*" || dow == "?") && mon == "*" {
                return if n == 1 {
                    format!("Every hour at :{m:02}")
                } else {
                    format!("Every {n} hours at :{m:02}")
                };
            }

    let Some(h) = single_value(hr) else {
        return format!("cron({inner})");
    };
    let Some(m) = single_value(min) else {
        return format!("cron({inner})");
    };
    let time = format_time(h, m);

    // Daily.
    let daily = (dom == "*" || dom == "?") && mon == "*" && (dow == "*" || dow == "?");
    if daily {
        return format!("Every day at {time}{zone_suffix}");
    }

    // Weekly on named day(s).
    if (dom == "*" || dom == "?") && mon == "*"
        && let Some(days) = named_days(dow) {
            return format!("Every {days} at {time}{zone_suffix}");
        }

    // Monthly on day N.
    if (dow == "*" || dow == "?") && mon == "*"
        && let Some(day) = single_value(dom) {
            let suffix = ordinal_suffix(day);
            return format!("On the {day}{suffix} of every month at {time}{zone_suffix}");
        }

    format!("cron({inner})")
}

/// Match `0/N` or `*/N` — "every N units". Returns N.
fn every_interval(field: &str) -> Option<u32> {
    let rest = field.strip_prefix("0/").or_else(|| field.strip_prefix("*/"))?;
    rest.parse().ok()
}

fn single_value(field: &str) -> Option<u32> {
    field.parse().ok()
}

fn format_time(h: u32, m: u32) -> String {
    // AWS cron hours are 0-23. Render as 12-hour with AM/PM since
    // that's the shape most people speak.
    let (h12, ampm) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    if m == 0 {
        format!("{h12}:00 {ampm}")
    } else {
        format!("{h12}:{m:02} {ampm}")
    }
}

fn ordinal_suffix(n: u32) -> &'static str {
    if (11..=13).contains(&(n % 100)) {
        return "th";
    }
    match n % 10 {
        1 => "st",
        2 => "nd",
        3 => "rd",
        _ => "th",
    }
}

/// Translate a DOW field into a human phrase. Handles a single
/// named day (`MON`), a numeric single day (`2`), the classic
/// weekday range (`MON-FRI` / `2-6`), and comma lists (`MON,WED`).
fn named_days(dow: &str) -> Option<String> {
    if dow == "MON-FRI" || dow == "2-6" {
        return Some("weekday".to_string());
    }
    if dow == "SUN,SAT" || dow == "SAT,SUN" || dow == "1,7" || dow == "7,1" {
        return Some("weekend day".to_string());
    }
    if dow.contains(',') {
        let names: Vec<String> = dow
            .split(',')
            .filter_map(day_name)
            .map(str::to_string)
            .collect();
        if names.len() == dow.split(',').count() {
            return Some(names.join(" / "));
        }
        return None;
    }
    day_name(dow).map(str::to_string)
}

fn day_name(d: &str) -> Option<&'static str> {
    match d.trim().to_uppercase().as_str() {
        "SUN" | "1" => Some("Sunday"),
        "MON" | "2" => Some("Monday"),
        "TUE" | "3" => Some("Tuesday"),
        "WED" | "4" => Some("Wednesday"),
        "THU" | "5" => Some("Thursday"),
        "FRI" | "6" => Some("Friday"),
        "SAT" | "7" => Some("Saturday"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_at_time() {
        assert_eq!(
            humanize("cron(00 19 * * ? *)", "America/New_York"),
            "Every day at 7:00 PM America/New_York"
        );
        assert_eq!(
            humanize("cron(30 6 * * ? *)", ""),
            "Every day at 6:30 AM"
        );
    }

    #[test]
    fn every_n_minutes() {
        assert_eq!(humanize("cron(0/5 * * * ? *)", ""), "Every 5 minutes");
    }

    #[test]
    fn weekly() {
        assert_eq!(
            humanize("cron(0 9 ? * MON *)", ""),
            "Every Monday at 9:00 AM"
        );
        assert_eq!(
            humanize("cron(0 9 ? * MON-FRI *)", ""),
            "Every weekday at 9:00 AM"
        );
    }

    #[test]
    fn monthly() {
        assert_eq!(
            humanize("cron(0 12 1 * ? *)", ""),
            "On the 1st of every month at 12:00 PM"
        );
        assert_eq!(
            humanize("cron(0 12 22 * ? *)", ""),
            "On the 22nd of every month at 12:00 PM"
        );
    }

    #[test]
    fn rate() {
        assert_eq!(humanize("rate(5 minutes)", ""), "Every 5 minutes");
        assert_eq!(humanize("rate(1 hour)", ""), "Every hour");
    }

    #[test]
    fn at() {
        assert_eq!(
            humanize("at(2026-07-21T09:30:00)", "UTC"),
            "Once on 2026-07-21T09:30:00 UTC"
        );
    }

    #[test]
    fn unknown_falls_back() {
        assert_eq!(humanize("cron(0 12 15 6 ? 2027)", ""), "cron(0 12 15 6 ? 2027)");
    }
}
