use lazy_static::lazy_static;
use regex::Regex;

#[derive(Debug, PartialEq)]
pub struct RsyncProgress {
    pub bytes_transferred: u64,
    pub percentage: u8,
    pub speed: String,
    pub estimated_time: String,
}

pub fn parse_rsync_progress(line: &str) -> Option<RsyncProgress> {
    lazy_static! {
        static ref RE: Regex = Regex::new(
            r"^([\d.]+)\s+(\d+)%\s+([\d,]+\w+/\w+)\s+(\d{1,2}:\d{2}:\d{2})"
        ).unwrap();
    }

    let caps = RE.captures(line.trim())?;
    let bytes_str = caps.get(1)?.as_str().replace('.', "");
    let bytes_transferred = bytes_str.parse::<u64>().ok()?;
    let percentage = caps.get(2)?.as_str().parse::<u8>().ok()?;
    let speed = caps.get(3)?.as_str().to_string();
    let estimated_time = caps.get(4)?.as_str().to_string();

    Some(RsyncProgress {
        bytes_transferred,
        percentage,
        speed,
        estimated_time,
    })
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes == 0 {
        return "0 B".to_string();
    }

    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let i = (bytes as f64).log(1024.0).floor() as usize;
    let size = (bytes as f64) / 1024.0_f64.powi(i as i32);

    if i == 0 {
        return format!("{} {}", size, UNITS[i]);
    }

    format!("{:.1} {}", size, UNITS[i])
}