//! Vietnamese lunar calendar (UTC+7), Hồ Ngọc Đức's astronomical algorithm.
use std::f64::consts::PI;

fn jd_from_date(dd: i32, mm: i32, yy: i32) -> i32 {
    let a = (14 - mm) / 12;
    let y = yy + 4800 - a;
    let m = mm + 12 * a - 3;
    let jd = dd + (153 * m + 2) / 5 + 365 * y + y / 4 - y / 100 + y / 400 - 32045;
    if jd < 2299161 { dd + (153 * m + 2) / 5 + 365 * y + y / 4 - 32083 } else { jd }
}

fn new_moon(k: i32) -> f64 {
    let k = k as f64;
    let t = k / 1236.85;
    let t2 = t * t;
    let t3 = t2 * t;
    let dr = PI / 180.0;
    let mut jd1 = 2415020.75933 + 29.53058868 * k + 0.0001178 * t2 - 0.000000155 * t3;
    jd1 += 0.00033 * ((166.56 + 132.87 * t - 0.009173 * t2) * dr).sin();
    let m = 359.2242 + 29.10535608 * k - 0.0000333 * t2 - 0.00000347 * t3;
    let mpr = 306.0253 + 385.81691806 * k + 0.0107306 * t2 + 0.00001236 * t3;
    let f = 21.2964 + 390.67050646 * k - 0.0016528 * t2 - 0.00000239 * t3;
    let mut c1 = (0.1734 - 0.000393 * t) * (m * dr).sin() + 0.0021 * (2.0 * dr * m).sin();
    c1 += -0.4068 * (mpr * dr).sin() + 0.0161 * (dr * 2.0 * mpr).sin();
    c1 += -0.0004 * (dr * 3.0 * mpr).sin();
    c1 += 0.0104 * (dr * 2.0 * f).sin() - 0.0051 * (dr * (m + mpr)).sin();
    c1 += -0.0074 * (dr * (m - mpr)).sin() + 0.0004 * (dr * (2.0 * f + m)).sin();
    c1 += -0.0004 * (dr * (2.0 * f - m)).sin() - 0.0006 * (dr * (2.0 * f + mpr)).sin();
    c1 += 0.0010 * (dr * (2.0 * f - mpr)).sin() + 0.0005 * (dr * (2.0 * mpr + m)).sin();
    let dt = if t < -11.0 {
        0.001 + 0.000839 * t + 0.0002261 * t2 - 0.00000845 * t3 - 0.000000081 * t * t3
    } else {
        -0.000278 + 0.000265 * t + 0.000262 * t2
    };
    jd1 + c1 - dt
}

fn sun_longitude(jdn: f64) -> f64 {
    let t = (jdn - 2451545.0) / 36525.0;
    let t2 = t * t;
    let dr = PI / 180.0;
    let m = 357.5291 + 35999.0503 * t - 0.0001559 * t2 - 0.00000048 * t * t2;
    let l0 = 280.46645 + 36000.76983 * t + 0.0003032 * t2;
    let dl = (1.9146 - 0.004817 * t - 0.000014 * t2) * (dr * m).sin()
        + (0.019993 - 0.000101 * t) * (dr * 2.0 * m).sin()
        + 0.00029 * (dr * 3.0 * m).sin();
    let l = (l0 + dl) * dr;
    let l = l - PI * 2.0 * (l / (PI * 2.0)).floor();
    l
}

fn new_moon_day(k: i32, tz: f64) -> i32 {
    (new_moon(k) + 0.5 + tz / 24.0).floor() as i32
}

fn sun_longitude_day(jdn: i32, tz: f64) -> i32 {
    (sun_longitude(jdn as f64 - 0.5 - tz / 24.0) / PI * 6.0).floor() as i32
}

fn lunar_month_11(yy: i32, tz: f64) -> i32 {
    let off = jd_from_date(31, 12, yy) as f64 - 2415021.0;
    let k = (off / 29.530588853).floor() as i32;
    let mut nm = new_moon_day(k, tz);
    if sun_longitude_day(nm, tz) >= 9 {
        nm = new_moon_day(k - 1, tz);
    }
    nm
}

fn leap_month_offset(a11: i32, tz: f64) -> i32 {
    let k = ((a11 as f64 - 2415021.076998695) / 29.530588853 + 0.5).floor() as i32;
    let mut i = 1;
    let mut arc = sun_longitude_day(new_moon_day(k + i, tz), tz);
    loop {
        let last = arc;
        i += 1;
        arc = sun_longitude_day(new_moon_day(k + i, tz), tz);
        if arc != last && i < 14 {
            continue;
        }
        break;
    }
    i - 1
}

/// Solar date → (lunar day, lunar month, is_leap_month).
pub fn solar_to_lunar(dd: i32, mm: i32, yy: i32) -> (i32, i32, bool) {
    let tz = 7.0;
    let day = jd_from_date(dd, mm, yy);
    let k = ((day as f64 - 2415021.076998695) / 29.530588853).floor() as i32;
    let mut month_start = new_moon_day(k + 1, tz);
    if month_start > day {
        month_start = new_moon_day(k, tz);
    }
    let mut a11 = lunar_month_11(yy, tz);
    let mut b11 = a11;
    let lunar_year;
    if a11 >= month_start {
        lunar_year = yy;
        a11 = lunar_month_11(yy - 1, tz);
    } else {
        lunar_year = yy + 1;
        b11 = lunar_month_11(yy + 1, tz);
    }
    let _ = lunar_year;
    let lunar_day = day - month_start + 1;
    let diff = (month_start - a11) / 29;
    let mut leap = false;
    let mut lunar_month = diff + 11;
    if b11 - a11 > 365 {
        let leap_off = leap_month_offset(a11, tz);
        if diff >= leap_off {
            lunar_month = diff + 10;
            if diff == leap_off {
                leap = true;
            }
        }
    }
    if lunar_month > 12 {
        lunar_month -= 12;
    }
    (lunar_day, lunar_month, leap)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tet_2025_and_known_dates() {
        assert_eq!(solar_to_lunar(29, 1, 2025), (1, 1, false)); // Tết Ất Tỵ
        assert_eq!(solar_to_lunar(17, 2, 2026), (1, 1, false)); // Tết Bính Ngọ
        assert_eq!(solar_to_lunar(3, 10, 2026).0 >= 1, true);
    }
}
