use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".into());
    let suffix = match (profile.as_str(), env::var_os("NTSC_PACKAGED_BUILD")) {
        ("debug", _) => "d",
        ("release", Some(_)) => "r",
        ("release", None) => "t",
        _ => "t",
    };
    let build_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = build_time.as_secs();
    let unique_stamp = build_time.as_nanos();
    let commit = env::var("VERGEN_GIT_SHA").unwrap_or_default();
    // Compact base-36 encoding keeps the build ID short while remaining
    // unique per build. A two-build-per-nanosecond collision is impossible
    // within the build-time window.
    let id = if commit.is_empty() {
        format!("{}{suffix}", encode_base36(unique_stamp))
    } else {
        format!(
            "{}-{}{suffix}",
            &commit[..commit.len().min(7)],
            encode_base36(unique_stamp)
        )
    };
    println!("cargo:rustc-env=NTSC_BUILD_ID={id}");
    let date = std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|date| date.trim().to_string())
        .filter(|date| !date.is_empty())
        .unwrap_or_else(|| seconds.to_string());
    println!("cargo:rustc-env=NTSC_BUILD_DATE={date}");
    println!("cargo:rerun-if-env-changed=NTSC_PACKAGED_BUILD");
    println!("cargo:rerun-if-env-changed=VERGEN_GIT_SHA");
}

fn encode_base36(mut value: u128) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while value > 0 {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_else(|_| "0".into())
}
