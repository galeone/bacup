// Copyright 2022-2026 Paolo Galeone <nessuno@nerdz.eu>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use chrono::Weekday;
use croner::parser::{CronParser, Seconds};
use croner::Cron;
use regex::Regex;

fn get_hours_and_minutes(when: &str) -> Option<(i8, i8)> {
    let re = Regex::new(r"(\d{2}):(\d{2})").unwrap();
    let caps = re.captures(when)?;
    let hours: i8 = caps.get(1)?.as_str().parse().ok()?;
    let minutes: i8 = caps.get(2)?.as_str().parse().ok()?;
    if hours < 24 && minutes < 60 {
        Some((hours, minutes))
    } else {
        None
    }
}

pub fn parse_daily(input: &str) -> Result<String, String> {
    // Daily 12:40
    let daily = "daily";
    if input.contains(daily) {
        let input = input.replace(daily, "").trim().to_string();
        let hm = get_hours_and_minutes(&input);
        if hm.is_none() {
            return Err(String::from("Unable to find hours:minutes"));
        }
        let hm = hm.unwrap();
        let input = input.replace(&format!("{:02}:{:02}", hm.0, hm.1), "");
        let input = input.trim();
        if !input.is_empty() {
            return Err(format!(
                "Expected to consume all the when string, unable to parse remaining part: {}",
                input
            ));
        }
        // sec   min   hour   day of month   month   day of week
        return Ok(format!("{} {} {} {} {} {}", 0, hm.1, hm.0, "*", "*", "*"));
    }
    Err(String::from("Invalid format for when: daily HH:MM"))
}

pub fn parse_weekly(input: &str) -> Result<String, String> {
    // Monday 15:40 or Weekly Monday 15:40
    let weekdays = [
        (Weekday::Mon, "Monday"),
        (Weekday::Tue, "Tuesday"),
        (Weekday::Wed, "Wednesday"),
        (Weekday::Thu, "Thursday"),
        (Weekday::Fri, "Friday"),
        (Weekday::Sat, "Saturday"),
        (Weekday::Sun, "Sunday"),
    ];

    let weekdays = weekdays.iter().map(|d| {
        (
            d.0.to_string().to_lowercase(),
            String::from(d.1).to_lowercase(),
        )
    });
    // Match day names as whole words, not as substrings of other words.
    let words: Vec<&str> = input.split_whitespace().collect();
    for day in weekdays {
        let short = words.contains(&day.0.as_str());
        let long = words.contains(&day.1.as_str());
        if short || long {
            let word = if long { &day.1 } else { &day.0 };
            let input = words
                .iter()
                .filter(|w| *w != word)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            let hm = get_hours_and_minutes(&input);
            if hm.is_none() {
                return Err(String::from("Unable to find hours:minutes"));
            }
            let hm = hm.unwrap();
            let input = input.replace(&format!("{:02}:{:02}", hm.0, hm.1), "");
            let input = input.trim();
            if !["", "weekly"].contains(&input) {
                return Err(format!(
                    "Expected to consume all the when string, unable to parse \
                    remaining part: {}",
                    input
                ));
            }
            // Use the day name, not a number: it can't be misread whatever
            // the numbering convention.
            let day = day.0.to_uppercase();

            // sec   min   hour   day of month   month   day of week
            return Ok(format!("{} {} {} {} {} {}", 0, hm.1, hm.0, "*", "*", day));
        }
    }
    Err(String::from("Unable to find any weekday identifier"))
}

pub fn parse_monthly(input: &str) -> Result<String, String> {
    // Monthly 1 12:40
    let monthly = "monthly";
    if input.contains(monthly) {
        let input = input.replace(monthly, "");
        let hm = get_hours_and_minutes(&input);
        if hm.is_none() {
            return Err(String::from("Unable to find hours:minutes"));
        }
        let hm = hm.unwrap();
        let input = input.replace(&format!("{:02}:{:02}", hm.0, hm.1), "");
        let input = input.trim();
        // Input should now contain only the "day of the month"

        let day: i8 = match input.parse() {
            Ok(day) => day,
            Err(error) => {
                return Err(format!(
                    "Unable to correctly parse the string for the day of the month. \
                    Given input: {}. Error: {}",
                    input, error
                ))
            }
        };

        let valid_days = 1..32;
        if !valid_days.contains(&day) {
            return Err(String::from(
                "Invalid day of the month specified, out of range [1,31]",
            ));
        }

        // sec   min   hour   day of month   month   day of week
        return Ok(format!("{} {} {} {} {} {}", 0, hm.1, hm.0, day, "*", "*"));
    }
    Err(String::from("Unable to find monthly identifier"))
}

/// The schedule a backup runs on: the cron expression handed to the
/// scheduler, and the same expression parsed exactly as the scheduler
/// parses it, used for validation and to compute the next run.
#[derive(Debug, Clone)]
pub struct Schedule {
    pub expression: String,
    pub cron: Cron,
}

impl Schedule {
    /// Parses a `when` field: one of the human friendly formats of
    /// [parse_when], or a raw cron expression with a seconds field
    /// (`sec min hour dom month dow`, Sunday is 0 or 7).
    pub fn parse(when: &str) -> Result<Schedule, String> {
        let expression = parse_when(when).unwrap_or_else(|_| when.trim().to_string());
        // The parser configuration of tokio-cron-scheduler, which runs the
        // jobs, so that the logged next run is the run that happens.
        let cron = CronParser::builder()
            .seconds(Seconds::Required)
            .dom_and_dow(true)
            .build()
            .parse(&expression)
            .map_err(|e| {
                format!(
                    "invalid when \"{}\": not daily/weekly/monthly, nor a cron \
                    expression with seconds (sec min hour dom month dow): {}",
                    when, e
                )
            })?;
        Ok(Schedule { expression, cron })
    }

    /// The first run strictly after `after`.
    pub fn next_after(
        &self,
        after: &chrono::DateTime<chrono::Utc>,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        self.cron.find_next_occurrence(after, false).ok()
    }

    /// The next run from now.
    pub fn next(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.next_after(&chrono::Utc::now())
    }
}

/// Parse a human friendly `when` expression into a cron expression
/// (sec min hour dom month dow).
///
/// Accepted formats:
/// - `daily HH:MM` (e.g. `daily 03:00`)
/// - `monthly D HH:MM` (e.g. `monthly 1 01:00`)
/// - `weekly <day> HH:MM` or `<day> HH:MM` (e.g. `weekly monday 12:00`, `sun 12:00`)
pub fn parse_when(when: &str) -> Result<String, String> {
    // sec   min   hour   day of month   month   day of week
    // *     *     *      *              *       *
    let input = when.to_lowercase();
    let daily = parse_daily(&input);
    if daily.is_ok() {
        return daily;
    }

    let monthly = parse_monthly(&input);
    if monthly.is_ok() {
        return monthly;
    }

    let weekly = parse_weekly(&input);
    if weekly.is_ok() {
        return weekly;
    }

    Err(format!(
        "Unable to parse for:\n\
        Daily: {}\n
        Weekly: {}\n
        Monthly: {}",
        daily.unwrap_err(),
        weekly.unwrap_err(),
        monthly.unwrap_err()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate_cron_expression(when: &str) {
        parse_when(when).unwrap_or_else(|e| panic!("Unable to parse when: {}: {}", when, e));
        // Parsed with the same configuration as tokio-cron-scheduler.
        Schedule::parse(when).unwrap_or_else(|e| panic!("{}", e));
    }

    #[test]
    fn test_parse_when_daily() {
        validate_cron_expression("daily 00:00");
        validate_cron_expression("Daily 00:00");
        validate_cron_expression("daily 12:30");
        validate_cron_expression("DAILY 12:30");

        assert!(parse_when("dayly 00:00").is_err());
        assert!(parse_when("daily 55:00").is_err());
        assert!(parse_when("daily 00:61").is_err());
        assert!(parse_when("daily 00:60").is_err());
        assert!(parse_when("daily 24:01").is_err());
    }

    #[test]
    fn test_parse_when_weekly() {
        for day in ["mon", "tue", "wed", "thu", "fri", "sat", "sun"] {
            let when = format!("{day} 12:30");
            validate_cron_expression(&when);
        }

        validate_cron_expression(" SUN 12:30");
        validate_cron_expression(" sunday 12:30");

        assert!(parse_when("Sundays 1:00").is_err());
        assert!(parse_when("Today 00:00").is_err());
        assert!(parse_when("Tomorrow 00:00").is_err());
        assert!(parse_when("Toyota -1:00").is_err());
    }

    #[test]
    fn test_parse_when_weekly_day_mapping() {
        for (day, dow) in [
            ("mon", "MON"),
            ("tue", "TUE"),
            ("wed", "WED"),
            ("thu", "THU"),
            ("fri", "FRI"),
            ("sat", "SAT"),
            ("sun", "SUN"),
            ("weekly sunday", "SUN"),
        ] {
            let cron = parse_when(&format!("{day} 12:30")).unwrap();
            let fields: Vec<&str> = cron.split_whitespace().collect();
            assert_eq!(fields[5], dow, "wrong day-of-week for {day}: {cron}");
        }
    }

    fn next_weekday(when: &str) -> Weekday {
        use chrono::Datelike;
        // Sunday 2026-11-22 11:00 UTC
        let from = chrono::DateTime::parse_from_rfc3339("2026-11-22T11:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        Schedule::parse(when)
            .unwrap_or_else(|e| panic!("{}", e))
            .next_after(&from)
            .unwrap()
            .weekday()
    }

    #[test]
    fn test_weekly_next_run_weekday() {
        assert_eq!(next_weekday("weekly monday 10:00"), Weekday::Mon);
        assert_eq!(next_weekday("weekly saturday 10:00"), Weekday::Sat);
        assert_eq!(next_weekday("weekly sunday 10:00"), Weekday::Sun);
    }

    #[test]
    fn test_raw_cron_weekdays_use_the_scheduler_convention() {
        // Standard cron numbering, as tokio-cron-scheduler (croner) runs it:
        // 0 and 7 are Sunday, 1 is Monday, 6 is Saturday.
        assert_eq!(next_weekday("0 0 10 * * 0"), Weekday::Sun);
        assert_eq!(next_weekday("0 0 10 * * 7"), Weekday::Sun);
        assert_eq!(next_weekday("0 0 10 * * 1"), Weekday::Mon);
        assert_eq!(next_weekday("0 0 10 * * 6"), Weekday::Sat);
        assert_eq!(next_weekday("0 0 10 * * SUN"), Weekday::Sun);
    }

    #[test]
    fn test_raw_cron_needs_seconds() {
        assert!(Schedule::parse("0 5 0 * 8 *").is_ok());
        let err = Schedule::parse("5 0 * 8 *").unwrap_err();
        assert!(err.contains("with seconds"), "{}", err);
        assert!(Schedule::parse("every day").is_err());
    }

    #[test]
    fn test_schedule_expression_is_what_the_scheduler_gets() {
        assert_eq!(
            Schedule::parse("weekly sunday 10:00").unwrap().expression,
            "0 0 10 * * SUN"
        );
        assert_eq!(
            Schedule::parse(" 0 5 0 * 8 * ").unwrap().expression,
            "0 5 0 * 8 *"
        );
    }

    #[test]
    fn test_parse_when_monthly() {
        validate_cron_expression("Monthly 1 02:30");
        validate_cron_expression("Monthly 31 02:30");

        assert!(parse_when("Monthly 0 01:00").is_err());
        assert!(parse_when("Monthly 32 01:00").is_err());
        assert!(parse_when("Monthly 01:00").is_err());
    }

    #[test]
    fn test_croner_accepts_human_format() {
        // This is what zfs full_when relies on: the human format must
        // produce an expression croner can parse.
        validate_cron_expression("monthly 1 01:00");
        validate_cron_expression("weekly monday 12:00");
    }
}
