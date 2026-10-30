//! Monthly allowance periods (issue #145).
//!
//! A period is one UTC calendar month: it starts at `00:00:00Z` on the first day of the month
//! and ends **exclusive** at `00:00:00Z` on the first day of the next month. An instant exactly
//! at a boundary belongs to the period that starts there. Local time zones (including
//! `Africa/Johannesburg`) are not used for accounting; the user's timezone is presentation only.
//!
//! Flow meters reset at each period start. Level meters are capped per period and do not carry
//! over. Mid-period plan changes do not split a period in this model: see
//! `docs/BILLING_PLAN_DOMAIN.md`.

use chrono::{DateTime, Datelike, NaiveDate, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AllowancePeriod {
    /// First day of the month, UTC.
    start: NaiveDate,
}

impl AllowancePeriod {
    /// The UTC calendar month that contains `instant`.
    pub fn containing(instant: DateTime<Utc>) -> Self {
        let date = instant.date_naive();
        // Day 1 exists for every valid year and month, so this cannot fail for a valid date.
        let start = NaiveDate::from_ymd_opt(date.year(), date.month(), 1)
            .expect("day 1 exists for every valid year and month");
        Self { start }
    }

    pub fn start_date(&self) -> NaiveDate {
        self.start
    }

    /// Inclusive period start, `00:00:00Z` on the first day.
    pub fn start_instant(&self) -> DateTime<Utc> {
        self.start
            .and_hms_opt(0, 0, 0)
            .expect("midnight exists for every date")
            .and_utc()
    }

    /// The first day of the following month. Exclusive upper bound of this period.
    pub fn end_exclusive_date(&self) -> NaiveDate {
        self.next().start
    }

    /// Exclusive period end, `00:00:00Z` on the first day of the next month.
    pub fn end_exclusive_instant(&self) -> DateTime<Utc> {
        self.next().start_instant()
    }

    /// The period that immediately follows this one.
    pub fn next(&self) -> Self {
        let (year, month) = if self.start.month() == 12 {
            (self.start.year() + 1, 1)
        } else {
            (self.start.year(), self.start.month() + 1)
        };
        let start = NaiveDate::from_ymd_opt(year, month, 1)
            .expect("day 1 exists for every valid year and month");
        Self { start }
    }

    /// True when `instant` is in `[start, end)`.
    pub fn contains(&self, instant: DateTime<Utc>) -> bool {
        instant >= self.start_instant() && instant < self.end_exclusive_instant()
    }

    /// Stable `YYYY-MM` label, used for keys and documentation.
    pub fn label(&self) -> String {
        format!("{:04}-{:02}", self.start.year(), self.start.month())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, s)
            .single()
            .expect("valid instant")
    }

    #[test]
    fn last_second_of_month_belongs_to_that_month() {
        let p = AllowancePeriod::containing(utc(2026, 1, 31, 23, 59, 59));
        assert_eq!(p.label(), "2026-01");
        assert!(p.contains(utc(2026, 1, 31, 23, 59, 59)));
    }

    #[test]
    fn first_instant_of_next_month_starts_a_new_period() {
        let p = AllowancePeriod::containing(utc(2026, 2, 1, 0, 0, 0));
        assert_eq!(p.label(), "2026-02");
        assert_eq!(p.start_instant(), utc(2026, 2, 1, 0, 0, 0));
        assert!(
            !AllowancePeriod::containing(utc(2026, 1, 31, 23, 59, 59))
                .contains(utc(2026, 2, 1, 0, 0, 0))
        );
    }

    #[test]
    fn end_is_exclusive() {
        let p = AllowancePeriod::containing(utc(2026, 3, 15, 12, 0, 0));
        assert!(p.contains(utc(2026, 3, 31, 23, 59, 59)));
        assert!(!p.contains(utc(2026, 4, 1, 0, 0, 0)));
        assert_eq!(p.end_exclusive_instant(), utc(2026, 4, 1, 0, 0, 0));
    }

    #[test]
    fn december_rolls_over_to_january_of_next_year() {
        let p = AllowancePeriod::containing(utc(2026, 12, 20, 9, 0, 0));
        assert_eq!(p.next().label(), "2027-01");
        assert_eq!(
            p.end_exclusive_date(),
            NaiveDate::from_ymd_opt(2027, 1, 1).expect("valid")
        );
    }

    #[test]
    fn leap_february_has_twenty_nine_days() {
        let p = AllowancePeriod::containing(utc(2028, 2, 10, 0, 0, 0));
        assert_eq!(
            p.end_exclusive_date(),
            NaiveDate::from_ymd_opt(2028, 3, 1).expect("valid")
        );
        assert!(p.contains(utc(2028, 2, 29, 23, 59, 59)));
    }

    #[test]
    fn non_leap_february_ends_on_the_first_of_march() {
        let p = AllowancePeriod::containing(utc(2026, 2, 28, 23, 59, 59));
        assert_eq!(
            p.end_exclusive_date(),
            NaiveDate::from_ymd_opt(2026, 3, 1).expect("valid")
        );
    }

    #[test]
    fn a_johannesburg_local_midnight_is_not_a_period_boundary() {
        // 2026-03-01 00:30 in Africa/Johannesburg (UTC+2) is 2026-02-28 22:30 UTC.
        let p = AllowancePeriod::containing(utc(2026, 2, 28, 22, 30, 0));
        assert_eq!(p.label(), "2026-02");
    }
}
